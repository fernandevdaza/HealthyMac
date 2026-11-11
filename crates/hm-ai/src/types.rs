//! Public data types. The UI codes against these: fields may be added, but never renamed or removed.

use std::path::PathBuf;
use std::time::SystemTime;

use hm_core::t;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Ollama,
    LmStudio,
    HuggingFace,
    Mlx,
    Whisper,
    LlamaCpp,
    Jan,
    Gpt4All,
    ComfyUi,
    Automatic1111,
    DrawThings,
    TorchHub,
    Other,
    /// oMLX keeps its own model folders (`~/.omlx/models`) and also serves whatever is in the
    /// Hugging Face cache. Models found there are reported under [`RuntimeKind::HuggingFace`].
    #[serde(rename = "omlx")]
    OMlx,
    /// The pre-Hugging Face cache layout (`~/.cache/torch/sentence_transformers`). Current
    /// versions store their models in the Hugging Face cache instead.
    SentenceTransformers,
}

impl RuntimeKind {
    /// Whether this is something that runs and can be stopped, rather than a folder of files that
    /// other tools read from.
    pub fn has_server(self) -> bool {
        !matches!(
            self,
            RuntimeKind::HuggingFace
                | RuntimeKind::Mlx
                | RuntimeKind::Whisper
                | RuntimeKind::TorchHub
                | RuntimeKind::SentenceTransformers
                | RuntimeKind::Other
        )
    }

    /// Stable identifier, the same text the enum serialises to. Used in [`LocalModel::id`].
    pub fn key(self) -> &'static str {
        match self {
            RuntimeKind::Ollama => "ollama",
            RuntimeKind::LmStudio => "lm_studio",
            RuntimeKind::HuggingFace => "hugging_face",
            RuntimeKind::Mlx => "mlx",
            RuntimeKind::Whisper => "whisper",
            RuntimeKind::LlamaCpp => "llama_cpp",
            RuntimeKind::Jan => "jan",
            RuntimeKind::Gpt4All => "gpt4_all",
            RuntimeKind::ComfyUi => "comfy_ui",
            RuntimeKind::Automatic1111 => "automatic1111",
            RuntimeKind::DrawThings => "draw_things",
            RuntimeKind::TorchHub => "torch_hub",
            RuntimeKind::Other => "other",
            RuntimeKind::OMlx => "omlx",
            RuntimeKind::SentenceTransformers => "sentence_transformers",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RuntimeKind::Ollama => "Ollama",
            RuntimeKind::LmStudio => "LM Studio",
            RuntimeKind::HuggingFace => "Hugging Face",
            RuntimeKind::Mlx => "MLX",
            RuntimeKind::Whisper => "Whisper",
            RuntimeKind::LlamaCpp => "llama.cpp",
            RuntimeKind::Jan => "Jan",
            RuntimeKind::Gpt4All => "GPT4All",
            RuntimeKind::ComfyUi => "ComfyUI",
            RuntimeKind::Automatic1111 => "Automatic1111",
            RuntimeKind::DrawThings => "Draw Things",
            RuntimeKind::TorchHub => "PyTorch Hub",
            RuntimeKind::Other => t!("Otro", "Other"),
            RuntimeKind::OMlx => "oMLX",
            RuntimeKind::SentenceTransformers => "Sentence Transformers",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFormat {
    Gguf,
    Mlx,
    Safetensors,
    PyTorch,
    CoreMl,
    Onnx,
    #[default]
    Unknown,
    /// faster-whisper and friends: a `model.bin` that is not a PyTorch pickle.
    #[serde(rename = "ctranslate2")]
    CTranslate2,
    /// whisper.cpp's `ggml-*.bin`.
    Ggml,
}

/// What [`LocalModel::last_used`] actually means. Only [`UsageSignal::LoadedNow`] and
/// [`UsageSignal::RuntimeRecord`] say anything about use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSignal {
    /// A running runtime has it loaded right now.
    LoadedNow,
    /// The runtime itself recorded this: LM Studio's last load, oMLX's usage history, or an Ollama
    /// server log line.
    RuntimeRecord,
    /// No record of use exists. The time is the newest file modification, which in practice means
    /// when it was downloaded. It must be shown as such, never as "last used".
    Modified,
    /// Nothing is known.
    #[default]
    None,
}

impl UsageSignal {
    pub fn label(self) -> &'static str {
        match self {
            UsageSignal::LoadedNow => t!("Cargado ahora", "Loaded now"),
            UsageSignal::RuntimeRecord => t!("Último uso", "Last used"),
            UsageSignal::Modified => t!("Descargado", "Downloaded"),
            UsageSignal::None => t!("Sin datos de uso", "No usage data"),
        }
    }
}

/// Whether a model can run in this machine's unified memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFit {
    /// Fits on the GPU with room to spare.
    Comfortable,
    /// Fits on the GPU, but expect pressure with other apps open.
    Tight,
    /// Above the GPU limit but within RAM: runs only partly offloaded, or very slowly.
    CpuOnly,
    /// Does not fit in memory at all.
    TooLarge,
    #[default]
    Unknown,
}

impl ModelFit {
    pub fn label(self) -> &'static str {
        match self {
            ModelFit::Comfortable => t!("Cabe de sobra", "Fits easily"),
            ModelFit::Tight => t!("Justo", "Tight fit"),
            ModelFit::CpuOnly => t!("Excede la GPU", "Exceeds GPU"),
            ModelFit::TooLarge => t!("No cabe", "Won't fit"),
            ModelFit::Unknown => t!("Desconocido", "Unknown"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalModel {
    /// Stable: `"{runtime}:{name}"`.
    pub id: String,
    /// E.g. `"qwen3:32b"` or `"mlx-community/whisper-large-v3"`.
    pub name: String,
    pub runtime: RuntimeKind,
    pub format: ModelFormat,
    /// E.g. `"Q4_K_M"` or `"4bit"`.
    pub quantization: Option<String>,
    /// E.g. `"llama"` or `"qwen3"`.
    pub architecture: Option<String>,
    pub parameter_count: Option<u64>,
    pub context_length: Option<u64>,
    /// On-disk bytes. Blobs shared with other models of the same runtime are counted once in
    /// [`ModelInventory::total_bytes`] but in full here, since the model needs all of them.
    pub size: u64,
    /// Bytes shared with another model of the same runtime, included in `size`.
    pub shared_bytes: u64,
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
    /// The best honest signal available. **Read [`LocalModel::last_used_source`] before calling
    /// this "last used"**: when the runtime keeps no record this is the newest modification time,
    /// which only says when the files were downloaded. Access time is deliberately not used; see
    /// the `usage` module for why.
    pub last_used: Option<SystemTime>,
    /// What `last_used` means.
    pub last_used_source: UsageSignal,
    /// Currently loaded in a running runtime.
    pub loaded: bool,
    pub fit: ModelFit,
    /// Files are missing, or the folder lacks what its runtime needs to load it.
    pub incomplete: bool,
}

/// The same bytes stored more than once, typically across runtimes.
#[derive(Clone, Debug, Serialize)]
pub struct DuplicateGroup {
    /// Empty only when every copy is already a clone of the others and no digest was known, since
    /// hashing them would have been pointless.
    pub sha256: String,
    pub file_size: u64,
    /// What deduplicating this group would really release. Already-cloned copies do not count.
    pub reclaimable: u64,
    pub paths: Vec<PathBuf>,
    /// Without repeats, so two runtimes that call a model the same thing appear once here. See
    /// `runtimes` for who holds a copy.
    pub model_names: Vec<String>,
    /// Every runtime holding a copy.
    pub runtimes: Vec<RuntimeKind>,
    /// Id of the [`hm_core::Finding`] that deduplicates this group. That finding only exists when
    /// `reclaimable` is above zero.
    pub finding_id: String,
    /// The copy the others would become clones of. `None` if no copy may be touched, for instance
    /// because they are all outside the home folder.
    pub keep: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LoadedModel {
    pub name: String,
    pub memory_bytes: Option<u64>,
    pub gpu_memory_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeStatus {
    pub kind: RuntimeKind,
    pub installed: bool,
    pub running: bool,
    /// E.g. `"http://127.0.0.1:11434"`.
    pub endpoint: Option<String>,
    pub version: Option<String>,
    pub loaded: Vec<LoadedModel>,
    pub storage_path: Option<PathBuf>,
    /// Always 0 from [`crate::runtime::status`], which must stay fast. Filled in by
    /// [`crate::inventory::scan`].
    pub storage_bytes: u64,
    /// How many models are loaded, when known even if their names are not: oMLX reports the count
    /// without authentication but the names only with an API key.
    pub loaded_count: Option<u64>,
    /// Memory used by the loaded models, when the runtime reports only a total.
    pub loaded_memory_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MemoryBudget {
    pub total_ram: u64,
    /// What the GPU may wire. See `iogpu.wired_limit_mb`.
    pub gpu_limit: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ModelInventory {
    /// Largest first.
    pub models: Vec<LocalModel>,
    /// Most reclaimable first.
    pub duplicates: Vec<DuplicateGroup>,
    pub runtimes: Vec<RuntimeStatus>,
    /// Real disk usage, with shared blobs counted once.
    pub total_bytes: u64,
    /// What deduplicating every group would release. Lossless, unlike removing a model.
    pub reclaimable_bytes: u64,
    pub memory: MemoryBudget,
    /// Model folders that exist or are configured but were not read, because reading them would
    /// raise a permission prompt without Full Disk Access.
    pub skipped_roots: Vec<PathBuf>,
    /// Files hashed in full by this scan, and their total size. Zero when known digests and the
    /// hash cache were enough.
    pub hashed_files: u64,
    pub hashed_bytes: u64,
}
