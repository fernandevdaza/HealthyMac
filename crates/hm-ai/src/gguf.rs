//! Reads GGUF header metadata without loading the weights.
//!
//! The header can be tens of megabytes, almost all of it tokenizer arrays, in front of weights of
//! tens of gigabytes. Everything that isn't wanted is seeked past, so the cost is one pass over the
//! header and never the weights.
//!
//! Model files come from the internet, so the header is treated as hostile: every length is checked
//! against the file size before it is believed, nothing is allocated from a declared length beyond
//! a small cap, and the walk has a byte budget so it always ends quickly.

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use anyhow::{bail, Context, Result};
use hm_core::{t, tf};

const MAGIC: &[u8; 4] = b"GGUF";
const MAX_KEY_BYTES: u64 = 64 * 1024;
/// Longer strings are skipped. Nothing we read is anywhere near this.
const MAX_KEPT_STRING_BYTES: u64 = 64 * 1024;
/// How far into a file the header may reach. Real ones are tens of megabytes. Each element walked
/// advances at least 8 bytes, so this also bounds the number of iterations.
const MAX_HEADER_BYTES: u64 = 256 * 1024 * 1024;
/// `GGML_MAX_DIMS`.
const MAX_TENSOR_DIMS: u32 = 8;
const MAX_ARRAY_DEPTH: u32 = 1;
const MAX_CONTEXT_KEYS: usize = 16;
/// Smallest possible encodings, used to reject counts that could not fit in the file.
const MIN_KV_BYTES: u64 = 8 + 4 + 1;
const MIN_TENSOR_BYTES: u64 = 8 + 4 + 4 + 8;

const TYPE_U8: u32 = 0;
const TYPE_I8: u32 = 1;
const TYPE_U16: u32 = 2;
const TYPE_I16: u32 = 3;
const TYPE_U32: u32 = 4;
const TYPE_I32: u32 = 5;
const TYPE_F32: u32 = 6;
const TYPE_BOOL: u32 = 7;
const TYPE_STRING: u32 = 8;
const TYPE_ARRAY: u32 = 9;
const TYPE_U64: u32 = 10;
const TYPE_I64: u32 = 11;
const TYPE_F64: u32 = 12;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GgufInfo {
    pub architecture: Option<String>,
    pub name: Option<String>,
    pub quantization: Option<String>,
    /// From `general.parameter_count` if present, otherwise the sum of the tensor shapes. For a
    /// split model this covers only this shard: see `split_count`.
    pub parameter_count: Option<u64>,
    pub context_length: Option<u64>,
    /// GGUF format version: 2 or 3.
    pub version: u32,
    pub tensor_count: u64,
    pub kv_count: u64,
    /// The raw `general.file_type`.
    pub file_type: Option<u32>,
    pub quantization_version: Option<u32>,
    /// `split.count`, when the model is split across several files.
    pub split_count: Option<u32>,
}

/// Parses only the header, so it is cheap even for files of tens of gigabytes. Returns an error for
/// anything that isn't a complete, sane GGUF v2 or v3 header.
pub fn read_info(path: &Path) -> Result<GgufInfo> {
    let file = File::open(path)
        .with_context(|| tf!("no se pudo abrir {}", "couldn't open {}", path.display()))?;
    let len = file.metadata()?.len();
    let mut info = parse(file, len)?;
    if info.quantization.is_none() {
        info.quantization = path
            .file_name()
            .and_then(|name| quantization_from_name(&name.to_string_lossy()));
    }
    Ok(info)
}

/// Maps `general.file_type`, which is `llama_ftype` from `llama.h`, to the usual name.
pub fn quantization_name(file_type: u32) -> Option<&'static str> {
    const LLAMA_FTYPE_GUESSED: u32 = 1024;
    Some(match file_type & !LLAMA_FTYPE_GUESSED {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        4 => "Q4_1_SOME_F16",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        33 => "Q4_0_4_4",
        34 => "Q4_0_4_8",
        35 => "Q4_0_8_8",
        36 => "TQ1_0",
        37 => "TQ2_0",
        38 => "MXFP4_MOE",
        39 => "NVFP4",
        40 => "Q1_0",
        41 => "Q2_0",
        _ => return None,
    })
}

/// Finds a quantization name in a file name, for files whose header doesn't say. Prefers the longest
/// match, so `Q4_K_M` wins over `Q4_K`-like prefixes.
pub fn quantization_from_name(name: &str) -> Option<String> {
    let upper = name.to_uppercase();
    (0..=41)
        .filter_map(quantization_name)
        .filter(|quant| upper.contains(quant))
        .max_by_key(|quant| quant.len())
        .map(str::to_string)
}

struct Reader<R> {
    inner: BufReader<R>,
    pos: u64,
    len: u64,
}

impl<R: Read + Seek> Reader<R> {
    fn advance(&mut self, count: u64) -> Result<()> {
        let end = self
            .pos
            .checked_add(count)
            .context(t!("longitud desbordada", "length overflow"))?;
        if end > self.len {
            bail!(t!("cabecera GGUF truncada", "truncated GGUF header"));
        }
        if end > MAX_HEADER_BYTES {
            bail!(t!(
                "cabecera GGUF demasiado grande",
                "GGUF header too large"
            ));
        }
        self.pos = end;
        Ok(())
    }

    fn skip(&mut self, count: u64) -> Result<()> {
        self.advance(count)?;
        // `advance` capped it at MAX_HEADER_BYTES, so this cannot truncate.
        self.inner.seek_relative(count as i64)?;
        Ok(())
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.advance(N as u64)?;
        let mut buffer = [0u8; N];
        self.inner.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }

    fn remaining(&self) -> u64 {
        self.len.saturating_sub(self.pos)
    }

    /// Reads a string only after confirming the declared length is small and within the file.
    fn string(&mut self, cap: u64) -> Result<Option<String>> {
        let length = self.u64()?;
        if length > cap {
            self.skip(length)?;
            return Ok(None);
        }
        self.advance(length)?;
        let mut buffer = vec![0u8; length as usize];
        self.inner.read_exact(&mut buffer)?;
        Ok(Some(String::from_utf8_lossy(&buffer).into_owned()))
    }

    fn skip_string(&mut self) -> Result<()> {
        let length = self.u64()?;
        self.skip(length)
    }

    fn skip_value(&mut self, kind: u32, depth: u32) -> Result<()> {
        match kind {
            TYPE_STRING => self.skip_string(),
            TYPE_ARRAY => {
                if depth >= MAX_ARRAY_DEPTH {
                    bail!(t!(
                        "arrays anidados en la cabecera GGUF",
                        "nested arrays in the GGUF header"
                    ));
                }
                let element = self.u32()?;
                let count = self.u64()?;
                self.skip_array(element, count)
            }
            other => {
                let size = scalar_size(other).context(t!(
                    "tipo de valor GGUF desconocido",
                    "unknown GGUF value type"
                ))?;
                self.skip(size)
            }
        }
    }

    fn skip_array(&mut self, element: u32, count: u64) -> Result<()> {
        if element == TYPE_ARRAY {
            bail!(t!(
                "arrays anidados en la cabecera GGUF",
                "nested arrays in the GGUF header"
            ));
        }
        if element == TYPE_STRING {
            if count
                .checked_mul(8)
                .is_none_or(|min| min > self.remaining())
            {
                bail!(t!(
                    "array de cadenas mayor que el archivo",
                    "string array larger than the file"
                ));
            }
            for _ in 0..count {
                self.skip_string()?;
            }
            return Ok(());
        }
        let size = scalar_size(element).context(t!(
            "tipo de array GGUF desconocido",
            "unknown GGUF array type"
        ))?;
        let total = count
            .checked_mul(size)
            .context(t!("longitud de array desbordada", "array length overflow"))?;
        self.skip(total)
    }

    /// Reads an integer of any width, or skips the value if it isn't a non-negative integer.
    fn number(&mut self, kind: u32) -> Result<Option<u64>> {
        Ok(match kind {
            TYPE_U8 => Some(u8::from_le_bytes(self.bytes()?).into()),
            TYPE_U16 => Some(u16::from_le_bytes(self.bytes()?).into()),
            TYPE_U32 => Some(self.u32()?.into()),
            TYPE_U64 => Some(self.u64()?),
            TYPE_I8 => u64::try_from(i8::from_le_bytes(self.bytes()?)).ok(),
            TYPE_I16 => u64::try_from(i16::from_le_bytes(self.bytes()?)).ok(),
            TYPE_I32 => u64::try_from(i32::from_le_bytes(self.bytes()?)).ok(),
            TYPE_I64 => u64::try_from(i64::from_le_bytes(self.bytes()?)).ok(),
            other => {
                self.skip_value(other, 0)?;
                None
            }
        })
    }

    fn text(&mut self, kind: u32) -> Result<Option<String>> {
        if kind == TYPE_STRING {
            self.string(MAX_KEPT_STRING_BYTES)
        } else {
            self.skip_value(kind, 0)?;
            Ok(None)
        }
    }

    /// Sums the tensor shapes. Returns `None` when the total overflows, which no real model does.
    fn tensor_parameters(&mut self, count: u64) -> Result<u64> {
        let mut total: u64 = 0;
        for _ in 0..count {
            self.skip_string()?;
            let dims = self.u32()?;
            if dims > MAX_TENSOR_DIMS {
                bail!(t!(
                    "tensor con demasiadas dimensiones",
                    "tensor with too many dimensions"
                ));
            }
            let mut elements: u64 = 1;
            for _ in 0..dims {
                elements = elements.checked_mul(self.u64()?).context(t!(
                    "dimensiones de tensor desbordadas",
                    "tensor dimensions overflow"
                ))?;
            }
            total = total.checked_add(elements).context(t!(
                "recuento de parámetros desbordado",
                "parameter count overflow"
            ))?;
            self.skip(4 + 8)?; // Type and data offset.
        }
        Ok(total)
    }
}

fn scalar_size(kind: u32) -> Option<u64> {
    Some(match kind {
        TYPE_U8 | TYPE_I8 | TYPE_BOOL => 1,
        TYPE_U16 | TYPE_I16 => 2,
        TYPE_U32 | TYPE_I32 | TYPE_F32 => 4,
        TYPE_U64 | TYPE_I64 | TYPE_F64 => 8,
        _ => return None,
    })
}

fn parse<R: Read + Seek>(source: R, len: u64) -> Result<GgufInfo> {
    let mut reader = Reader {
        inner: BufReader::with_capacity(256 * 1024, source),
        pos: 0,
        len,
    };

    let magic: [u8; 4] = reader.bytes()?;
    if &magic != MAGIC {
        bail!(t!("no es un archivo GGUF", "not a GGUF file"));
    }
    // A big-endian file reads as a huge version and is refused here, as is v1 with its 32-bit counts.
    let version = reader.u32()?;
    if version != 2 && version != 3 {
        bail!(tf!(
            "versión de GGUF no admitida: {version}",
            "unsupported GGUF version: {version}"
        ));
    }
    let tensor_count = reader.u64()?;
    let kv_count = reader.u64()?;
    let declared = kv_count
        .checked_mul(MIN_KV_BYTES)
        .and_then(|kv| tensor_count.checked_mul(MIN_TENSOR_BYTES)?.checked_add(kv));
    if declared.is_none_or(|bytes| bytes > reader.remaining()) {
        bail!(t!(
            "la cabecera GGUF declara más entradas de las que caben en el archivo",
            "the GGUF header declares more entries than the file can hold"
        ));
    }

    let mut info = GgufInfo {
        version,
        tensor_count,
        kv_count,
        ..GgufInfo::default()
    };
    let mut declared_parameters = None;
    // The architecture usually comes first, but nothing requires it to.
    let mut context_lengths: Vec<(String, u64)> = Vec::new();

    for _ in 0..kv_count {
        let key = reader
            .string(MAX_KEY_BYTES)?
            .context(t!("clave GGUF demasiado larga", "GGUF key too long"))?;
        let kind = reader.u32()?;
        match key.as_str() {
            "general.architecture" => info.architecture = reader.text(kind)?,
            "general.name" => info.name = reader.text(kind)?,
            "general.file_type" => {
                info.file_type = reader.number(kind)?.and_then(|v| u32::try_from(v).ok());
            }
            "general.quantization_version" => {
                info.quantization_version =
                    reader.number(kind)?.and_then(|v| u32::try_from(v).ok());
            }
            "general.parameter_count" => declared_parameters = reader.number(kind)?,
            "split.count" => {
                info.split_count = reader.number(kind)?.and_then(|v| u32::try_from(v).ok());
            }
            other if other.ends_with(".context_length") => {
                let value = reader.number(kind)?;
                if let Some(value) = value {
                    if context_lengths.len() < MAX_CONTEXT_KEYS {
                        context_lengths.push((key.clone(), value));
                    }
                }
            }
            _ => reader.skip_value(kind, 0)?,
        }
    }

    let summed = reader.tensor_parameters(tensor_count)?;
    info.parameter_count = declared_parameters.or((tensor_count > 0).then_some(summed));

    info.context_length = match &info.architecture {
        Some(architecture) => {
            let wanted = format!("{architecture}.context_length");
            context_lengths
                .iter()
                .find(|(key, _)| *key == wanted)
                .map(|(_, value)| *value)
        }
        None => match context_lengths.as_slice() {
            [(_, only)] => Some(*only),
            _ => None,
        },
    };
    info.quantization = info
        .file_type
        .and_then(quantization_name)
        .map(str::to_string);
    Ok(info)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{Duration, Instant};

    /// Builds GGUF headers byte by byte.
    #[derive(Default)]
    pub(crate) struct Builder {
        kv: Vec<u8>,
        kv_count: u64,
        tensors: Vec<u8>,
        tensor_count: u64,
    }

    impl Builder {
        fn key(&mut self, key: &str, kind: u32) -> &mut Self {
            self.kv.extend((key.len() as u64).to_le_bytes());
            self.kv.extend(key.as_bytes());
            self.kv.extend(kind.to_le_bytes());
            self.kv_count += 1;
            self
        }

        pub(crate) fn string(&mut self, key: &str, value: &str) -> &mut Self {
            self.key(key, TYPE_STRING);
            self.kv.extend((value.len() as u64).to_le_bytes());
            self.kv.extend(value.as_bytes());
            self
        }

        pub(crate) fn u32(&mut self, key: &str, value: u32) -> &mut Self {
            self.key(key, TYPE_U32);
            self.kv.extend(value.to_le_bytes());
            self
        }

        fn strings(&mut self, key: &str, values: &[&str]) -> &mut Self {
            self.key(key, TYPE_ARRAY);
            self.kv.extend(TYPE_STRING.to_le_bytes());
            self.kv.extend((values.len() as u64).to_le_bytes());
            for value in values {
                self.kv.extend((value.len() as u64).to_le_bytes());
                self.kv.extend(value.as_bytes());
            }
            self
        }

        fn floats(&mut self, key: &str, count: usize) -> &mut Self {
            self.key(key, TYPE_ARRAY);
            self.kv.extend(TYPE_F32.to_le_bytes());
            self.kv.extend((count as u64).to_le_bytes());
            self.kv.extend(vec![0u8; count * 4]);
            self
        }

        pub(crate) fn tensor(&mut self, name: &str, dims: &[u64]) -> &mut Self {
            self.tensors.extend((name.len() as u64).to_le_bytes());
            self.tensors.extend(name.as_bytes());
            self.tensors.extend((dims.len() as u32).to_le_bytes());
            for dim in dims {
                self.tensors.extend(dim.to_le_bytes());
            }
            self.tensors.extend(0u32.to_le_bytes());
            self.tensors.extend(0u64.to_le_bytes());
            self.tensor_count += 1;
            self
        }

        pub(crate) fn build(&self, version: u32) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend(MAGIC);
            out.extend(version.to_le_bytes());
            out.extend(self.tensor_count.to_le_bytes());
            out.extend(self.kv_count.to_le_bytes());
            out.extend(&self.kv);
            out.extend(&self.tensors);
            out
        }
    }

    fn realistic() -> Builder {
        let mut builder = Builder::default();
        builder
            .string("general.architecture", "llama")
            .string("general.name", "Test Llama")
            .u32("general.file_type", 15)
            .u32("general.quantization_version", 2)
            .u32("llama.context_length", 8192)
            .u32("whisper.context_length", 99)
            .strings("tokenizer.ggml.tokens", &["<s>", "</s>", "hola", "mundo"])
            .floats("tokenizer.ggml.scores", 4)
            .tensor("a.weight", &[10, 20])
            .tensor("b.weight", &[3]);
        builder
    }

    fn parse_bytes(bytes: &[u8]) -> Result<GgufInfo> {
        parse(Cursor::new(bytes.to_vec()), bytes.len() as u64)
    }

    #[test]
    fn reads_v2_and_v3_headers() {
        for version in [2, 3] {
            let info = parse_bytes(&realistic().build(version)).unwrap();
            assert_eq!(info.version, version);
            assert_eq!(info.architecture.as_deref(), Some("llama"));
            assert_eq!(info.name.as_deref(), Some("Test Llama"));
            assert_eq!(info.quantization.as_deref(), Some("Q4_K_M"));
            assert_eq!(info.quantization_version, Some(2));
            assert_eq!(info.context_length, Some(8192));
            assert_eq!(info.parameter_count, Some(203));
            assert_eq!(info.tensor_count, 2);
        }
    }

    #[test]
    fn context_length_is_found_whatever_the_key_order() {
        let mut builder = Builder::default();
        builder
            .u32("qwen3.context_length", 4096)
            .string("general.architecture", "qwen3");
        assert_eq!(
            parse_bytes(&builder.build(3)).unwrap().context_length,
            Some(4096)
        );
    }

    #[test]
    fn declared_parameter_count_wins_over_the_tensor_sum() {
        let mut builder = realistic();
        builder.key("general.parameter_count", TYPE_U64);
        builder.kv.extend(7_000_000_000u64.to_le_bytes());
        assert_eq!(
            parse_bytes(&builder.build(3)).unwrap().parameter_count,
            Some(7_000_000_000)
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_is_ignored_not_fatal() {
        let mut builder = Builder::default();
        builder
            .u32("general.architecture", 7)
            .string("general.file_type", "Q4_K_M")
            .string("llama.context_length", "mucho")
            .string("general.name", "ok");
        let info = parse_bytes(&builder.build(3)).unwrap();
        assert_eq!(info.architecture, None);
        assert_eq!(info.quantization, None);
        assert_eq!(info.context_length, None);
        assert_eq!(info.name.as_deref(), Some("ok"));
    }

    #[test]
    fn guessed_file_types_and_unknown_ones_map_sensibly() {
        assert_eq!(quantization_name(1024 + 15), Some("Q4_K_M"));
        assert_eq!(quantization_name(5), None); // Removed from llama.cpp.
        assert_eq!(quantization_name(9999), None);
    }

    #[test]
    fn quantization_falls_back_to_the_file_name() {
        assert_eq!(
            quantization_from_name("model-q4_k_m.gguf").as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(quantization_from_name("model.gguf"), None);
    }

    #[test]
    fn every_truncation_is_an_error_and_never_a_panic() {
        let bytes = realistic().build(3);
        for cut in 0..bytes.len() {
            assert!(parse_bytes(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }

    fn header(tensors: u64, kvs: u64) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(MAGIC);
        out.extend(3u32.to_le_bytes());
        out.extend(tensors.to_le_bytes());
        out.extend(kvs.to_le_bytes());
        out
    }

    /// Pads so length checks against the file size pass and the code under test has to be careful.
    fn padded(mut bytes: Vec<u8>) -> Vec<u8> {
        bytes.resize(bytes.len() + 4096, 0);
        bytes
    }

    #[track_caller]
    fn assert_refused_quickly(bytes: Vec<u8>) {
        let started = Instant::now();
        assert!(parse_bytes(&bytes).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn refuses_bad_magic_old_and_foreign_endian_versions() {
        assert_refused_quickly(b"NOPE".to_vec());
        for version in [0u32, 1, 4, 0x0300_0000] {
            let mut bytes = Vec::new();
            bytes.extend(MAGIC);
            bytes.extend(version.to_le_bytes());
            bytes.extend([0u8; 16]);
            assert_refused_quickly(bytes);
        }
    }

    #[test]
    fn refuses_counts_that_cannot_fit_in_the_file() {
        assert_refused_quickly(padded(header(u64::MAX, 0)));
        assert_refused_quickly(padded(header(0, u64::MAX)));
        assert_refused_quickly(padded(header(u64::MAX / 2, u64::MAX / 2)));
    }

    #[test]
    fn refuses_absurd_string_lengths_without_allocating() {
        for length in [u64::MAX, u64::MAX / 2, 1 << 40] {
            let mut bytes = header(0, 1);
            bytes.extend(length.to_le_bytes());
            assert_refused_quickly(padded(bytes));

            let mut bytes = header(0, 1);
            let key = "general.name";
            bytes.extend((key.len() as u64).to_le_bytes());
            bytes.extend(key.as_bytes());
            bytes.extend(TYPE_STRING.to_le_bytes());
            bytes.extend(length.to_le_bytes());
            assert_refused_quickly(padded(bytes));
        }
    }

    fn array_header(element: u32, count: u64) -> Vec<u8> {
        let mut bytes = header(0, 1);
        let key = "tokenizer.ggml.tokens";
        bytes.extend((key.len() as u64).to_le_bytes());
        bytes.extend(key.as_bytes());
        bytes.extend(TYPE_ARRAY.to_le_bytes());
        bytes.extend(element.to_le_bytes());
        bytes.extend(count.to_le_bytes());
        bytes
    }

    #[test]
    fn refuses_hostile_arrays() {
        for count in [u64::MAX, u64::MAX / 8, 1 << 40] {
            assert_refused_quickly(padded(array_header(TYPE_STRING, count)));
            assert_refused_quickly(padded(array_header(TYPE_U64, count)));
        }
        assert_refused_quickly(padded(array_header(99, 1)));
        // Nested arrays could otherwise recurse as deep as the file allows.
        assert_refused_quickly(padded(array_header(TYPE_ARRAY, 1)));
    }

    #[test]
    fn a_flood_of_empty_strings_is_cut_off_by_the_byte_budget() {
        // 512 MiB of zeros decodes as tens of millions of empty strings. This must end promptly,
        // on a sparse file so the test itself stays cheap.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flood.gguf");
        std::fs::write(&path, array_header(TYPE_STRING, 1 << 40)).unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        file.set_len(16 * (1 << 40)).unwrap();
        let started = Instant::now();
        assert!(read_info(&path).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn refuses_hostile_tensor_entries() {
        let tensor = |dims: u32, values: &[u64]| {
            let mut bytes = header(1, 0);
            bytes.extend(1u64.to_le_bytes());
            bytes.push(b't');
            bytes.extend(dims.to_le_bytes());
            for value in values {
                bytes.extend(value.to_le_bytes());
            }
            bytes.extend(0u32.to_le_bytes());
            bytes.extend(0u64.to_le_bytes());
            padded(bytes)
        };
        assert_refused_quickly(tensor(u32::MAX, &[]));
        assert_refused_quickly(tensor(9, &[1; 9]));
        assert_refused_quickly(tensor(3, &[u64::MAX, u64::MAX, 2]));
    }

    #[test]
    fn random_corruption_never_panics() {
        let original = realistic().build(3);
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..5_000 {
            let mut bytes = original.clone();
            for _ in 0..(next() % 8 + 1) {
                let at = next() as usize % bytes.len();
                bytes[at] = next() as u8;
            }
            let _ = parse_bytes(&bytes);
        }
    }

    #[test]
    fn a_50_gb_file_costs_only_its_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.gguf");
        std::fs::write(&path, realistic().build(3)).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(50 * 1024 * 1024 * 1024)
            .unwrap();
        let started = Instant::now();
        let info = read_info(&path).unwrap();
        assert_eq!(info.context_length, Some(8192));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
