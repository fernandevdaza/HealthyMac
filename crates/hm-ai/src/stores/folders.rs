//! Runtimes that keep models as plain files: a folder per model (LM Studio, oMLX, Jan) or one file
//! per model (Whisper, ComfyUI, GPT4All and others).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use hm_core::{t, ScanContext};

use super::{newest, Leftover, LeftoverKind, RawModel, StoreScan, ABANDONED_MIN_DAYS};
use crate::files::{link_target, name_of, older_than, relative, walk, FileRec};
use crate::locate::Roots;
use crate::metadata::{self, extension, gguf_variant, is_gguf, is_projector, is_weight};
use crate::types::RuntimeKind;
use crate::usage::UsageRecords;

/// Smaller than this it isn't a model: ComfyUI ships `put_checkpoints_here` placeholders, and so on.
const MIN_FLAT_MODEL_BYTES: u64 = 1024 * 1024;

fn is_partial_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("downloading_")
        || lower.starts_with("incomplete-")
        || [
            ".part",
            ".partial",
            ".incomplete",
            ".tmp",
            ".download",
            ".downloadinprogress",
            ".crdownload",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

#[derive(Debug, Default)]
struct FolderFiles {
    files: Vec<FileRec>,
    partials: Vec<FileRec>,
}

/// Everything in a model's folder. A file linked in from elsewhere counts, as linking models from
/// the Hugging Face cache into LM Studio is a common trick, and whoever adds up bytes compares
/// inodes, so it isn't counted twice.
fn read_folder(dir: &Path, roots: &Roots, ctx: &ScanContext) -> FolderFiles {
    let mut found = FolderFiles::default();
    walk(dir, 8, ctx, &mut |path, meta| {
        let name = name_of(path);
        if name.starts_with('.') {
            return false;
        }
        if meta.is_dir() {
            return true;
        }
        let logical = relative(path, dir);
        if is_partial_name(&name) {
            found.partials.extend(FileRec::stat(path, logical));
        } else if meta.file_type().is_symlink() {
            let target = link_target(path).filter(|target| roots.allowed(target));
            found
                .files
                .extend(target.and_then(|target| FileRec::stat(&target, logical)));
        } else {
            found.files.extend(FileRec::stat(path, logical));
        }
        false
    });
    found
}

/// Keyed by folder, since two folders can hold models of the same name.
fn partial_leftover(
    runtime: RuntimeKind,
    label: String,
    dir: &Path,
    partials: Vec<FileRec>,
    ctx: &ScanContext,
) -> Option<Leftover> {
    if partials.is_empty() {
        return None;
    }
    let newest_write = newest(partials.iter().map(|file| file.modified));
    older_than(newest_write, ctx.now, ABANDONED_MIN_DAYS).then(|| Leftover {
        kind: LeftoverKind::Partial,
        runtime,
        key: format!("{}:{}", runtime.key(), dir.display()),
        label,
        paths: partials.iter().map(|file| file.path.clone()).collect(),
        records: partials,
        newest: newest_write,
    })
}

/// Turns one model folder into models. A folder of several GGUF quantizations is several models,
/// as it is for LM Studio: each one is loaded and removed by itself. What they have in common, like
/// a vision projector, goes with each, and shows up as shared.
pub(crate) fn scan_model_folder(
    runtime: RuntimeKind,
    name: &str,
    dir: &Path,
    requires_config: bool,
    roots: &Roots,
    ctx: &ScanContext,
    out: &mut StoreScan,
) {
    let found = read_folder(dir, roots, ctx);
    out.leftovers.extend(partial_leftover(
        runtime,
        name.to_string(),
        dir,
        found.partials,
        ctx,
    ));
    let files = found.files;
    if !files.iter().any(|file| is_weight(&file.logical, file.len)) {
        return;
    }

    let mut variants: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        if is_gguf(&file.logical) && !is_projector(&file.logical) {
            variants
                .entry(gguf_variant(&file.logical))
                .or_default()
                .push(index);
        }
    }
    let assume_mlx = runtime == RuntimeKind::OMlx;

    if variants.len() >= 2 {
        for shards in variants.values() {
            let mut model_files: Vec<FileRec> = files
                .iter()
                .enumerate()
                .filter(|(index, file)| {
                    shards.contains(index) || !is_gguf(&file.logical) || is_projector(&file.logical)
                })
                .map(|(_, file)| file.clone())
                .collect();
            model_files.sort_by(|a, b| a.logical.cmp(&b.logical));
            let first = shards.first().and_then(|index| files.get(*index));
            let Some(first) = first else {
                continue;
            };
            let variant_name = format!("{name}/{}", first.logical);
            out.models.push(RawModel {
                detected: metadata::detect(&variant_name, &model_files, false),
                name: variant_name,
                runtime,
                root: dir.to_path_buf(),
                incomplete: metadata::missing_parts(&model_files),
                files: model_files,
                used: None,
            });
        }
        return;
    }

    let has_gguf = !variants.is_empty();
    let has_config = files.iter().any(|file| file.logical == "config.json");
    out.models.push(RawModel {
        detected: metadata::detect(name, &files, assume_mlx),
        name: name.to_string(),
        runtime,
        root: dir.to_path_buf(),
        incomplete: (requires_config && !has_gguf && !has_config)
            || metadata::missing_parts(&files),
        files,
        used: None,
    });
}

/// The subfolders of `dir`, including links to folders if the gate allows them. They're returned as
/// found and not resolved: removing a linked model must take the link, never what it points at, and
/// reading through it works just the same.
fn subfolders(dir: &Path, roots: &Roots) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut folders: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = name_of(&path);
            if name.starts_with('.') {
                return None;
            }
            roots.follow_root(&path)?;
            Some((name, path))
        })
        .collect();
    folders.sort();
    folders
}

/// `<root>/<publisher>/<model>/`.
pub(crate) fn scan_lm_studio(
    root: &Path,
    roots: &Roots,
    ctx: &ScanContext,
    usage: &UsageRecords,
) -> StoreScan {
    let mut out = StoreScan::default();
    for (publisher, publisher_dir) in subfolders(root, roots) {
        for (model, model_dir) in subfolders(&publisher_dir, roots) {
            if ctx.is_cancelled() {
                return out;
            }
            let folder_name = format!("{publisher}/{model}");
            let first_new = out.models.len();
            scan_model_folder(
                RuntimeKind::LmStudio,
                &folder_name,
                &model_dir,
                true,
                roots,
                ctx,
                &mut out,
            );
            for found in &mut out.models[first_new..] {
                // LM Studio's ids are the folder for MLX, and include the file for GGUF.
                let by_file = found
                    .files
                    .iter()
                    .filter(|file| is_gguf(&file.logical))
                    .filter_map(|file| {
                        usage
                            .lm_studio
                            .get(&format!("{folder_name}/{}", file.logical))
                    });
                let by_name = usage.lm_studio.get(&found.name);
                let exact_variant = found.name != folder_name;
                let candidates: Vec<_> = if exact_variant {
                    by_name.into_iter().copied().collect()
                } else {
                    by_name.into_iter().chain(by_file).copied().collect()
                };
                found.used = candidates.into_iter().max();
            }
        }
    }
    out
}

/// LM Studio's temporary downloads folder.
pub(crate) fn scan_lm_temp(roots: &Roots, ctx: &ScanContext) -> StoreScan {
    let mut out = StoreScan::default();
    let dir = roots.lm_home.join(".internal/temp-downloads");
    if !roots.allowed_quietly(&dir) {
        return out;
    }
    let mut partials = Vec::new();
    walk(&dir, 4, ctx, &mut |path, meta| {
        if meta.is_file() && is_partial_name(&name_of(path)) {
            partials.extend(FileRec::stat(path, name_of(path)));
        }
        true
    });
    out.leftovers.extend(partial_leftover(
        RuntimeKind::LmStudio,
        t!("descargas temporales", "temporary downloads").to_string(),
        &dir,
        partials,
        ctx,
    ));
    out
}

/// Mirrors `omlx/model_discovery.py`: a first-level folder is a model if it has a `config.json` or
/// GGUFs, otherwise an organization folder whose children may be. Folders with weights and neither
/// are the runtime's blind spot, and get reported as incomplete.
pub(crate) fn scan_omlx(
    model_dir: &Path,
    roots: &Roots,
    ctx: &ScanContext,
    usage: &UsageRecords,
) -> StoreScan {
    let mut out = StoreScan::default();
    let looks_like_model = |dir: &Path| {
        let mut any = false;
        walk(dir, 1, ctx, &mut |path, meta| {
            let name = name_of(path);
            any |= meta.is_file() && (name == "config.json" || is_weight(&name, meta.len()));
            false
        });
        any
    };
    for (name, dir) in subfolders(model_dir, roots) {
        if dir.join("adapter_config.json").exists() {
            continue;
        }
        if looks_like_model(&dir) {
            scan_model_folder(RuntimeKind::OMlx, &name, &dir, true, roots, ctx, &mut out);
            continue;
        }
        for (child, child_dir) in subfolders(&dir, roots) {
            if !child_dir.join("adapter_config.json").exists() && looks_like_model(&child_dir) {
                scan_model_folder(
                    RuntimeKind::OMlx,
                    &child,
                    &child_dir,
                    true,
                    roots,
                    ctx,
                    &mut out,
                );
            }
        }
    }
    for found in &mut out.models {
        found.used = usage.omlx.get(&found.name).copied();
    }
    out
}

/// One model per file below `dir`. The name is its path under `dir`, after `prefix`.
pub(crate) fn scan_flat(
    runtime: RuntimeKind,
    prefix: &str,
    dir: &Path,
    max_depth: usize,
    roots: &Roots,
    ctx: &ScanContext,
) -> StoreScan {
    let mut out = StoreScan::default();
    let mut partials = Vec::new();
    let mut found: Vec<(PathBuf, FileRec)> = Vec::new();
    let mut packages: Vec<PathBuf> = Vec::new();
    walk(dir, max_depth, ctx, &mut |path, meta| {
        let name = name_of(path);
        if name.starts_with('.') {
            return false;
        }
        if meta.is_dir() {
            if matches!(extension(&name).as_str(), "mlmodelc" | "mlpackage") {
                packages.push(path.to_path_buf());
                return false;
            }
            return true;
        }
        if is_partial_name(&name) {
            partials.extend(FileRec::stat(path, name));
            return false;
        }
        let record = if meta.file_type().is_symlink() {
            link_target(path)
                .filter(|target| roots.allowed(target))
                .and_then(|target| FileRec::stat(&target, name.clone()))
        } else {
            FileRec::stat(path, name.clone())
        };
        if let Some(record) = record {
            if is_weight(&name, record.len) && record.len >= MIN_FLAT_MODEL_BYTES {
                found.push((path.to_path_buf(), record));
            }
        }
        false
    });

    let label = if prefix.is_empty() {
        name_of(dir)
    } else {
        prefix.trim_end_matches('/').to_string()
    };
    out.leftovers
        .extend(partial_leftover(runtime, label, dir, partials, ctx));
    for (path, record) in found {
        let name = format!("{prefix}{}", relative(&path, dir));
        let files = vec![record];
        out.models.push(RawModel {
            detected: metadata::detect(&name, &files, false),
            name,
            runtime,
            root: path,
            files,
            used: None,
            incomplete: false,
        });
    }
    for package in packages {
        let folder = read_folder(&package, roots, ctx);
        let package_name = name_of(&package);
        let files: Vec<FileRec> = folder
            .files
            .into_iter()
            .map(|mut file| {
                file.logical = format!("{package_name}/{}", file.logical);
                file
            })
            .collect();
        if files.is_empty() {
            continue;
        }
        let name = format!("{prefix}{}", relative(&package, dir));
        out.models.push(RawModel {
            detected: metadata::detect(&name, &files, false),
            name,
            runtime,
            root: package,
            files,
            used: None,
            incomplete: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::tests::Builder;
    use crate::locate::Detail;
    use crate::test_support::Fixture;
    use crate::types::ModelFormat;
    use std::collections::HashMap;
    use std::time::{Duration, UNIX_EPOCH};

    fn gguf(file_type: u32) -> Vec<u8> {
        let mut builder = Builder::default();
        builder
            .string("general.architecture", "llama")
            .u32("general.file_type", file_type)
            .u32("llama.context_length", 2048)
            .tensor("t", &[4]);
        builder.build(3)
    }

    fn roots(fx: &Fixture) -> Roots {
        Roots::resolve_with(&fx.home, false, Detail::Light, &|_| None)
    }

    fn lm(fx: &Fixture, usage: &UsageRecords) -> StoreScan {
        let ctx = fx.ctx();
        scan_lm_studio(&fx.home.join(".lmstudio/models"), &roots(fx), &ctx, usage)
    }

    #[test]
    fn a_single_gguf_folder_is_one_model() {
        let fx = Fixture::new();
        fx.write(".lmstudio/models/org/m-GGUF/m-Q4_K_M.gguf", &gguf(15));
        let found = lm(&fx, &UsageRecords::default());
        assert_eq!(found.models.len(), 1);
        assert_eq!(found.models[0].name, "org/m-GGUF");
        assert_eq!(found.models[0].detected.format, ModelFormat::Gguf);
        assert_eq!(
            found.models[0].detected.quantization.as_deref(),
            Some("Q4_K_M")
        );
        assert!(!found.models[0].incomplete);
    }

    #[test]
    fn several_quantizations_in_a_folder_are_separate_models_sharing_the_projector() {
        let fx = Fixture::new();
        fx.write(".lmstudio/models/org/m-GGUF/m-Q4_K_M.gguf", &gguf(15));
        fx.write(
            ".lmstudio/models/org/m-GGUF/m-Q8_0-00001-of-00002.gguf",
            &gguf(7),
        );
        fx.write(
            ".lmstudio/models/org/m-GGUF/m-Q8_0-00002-of-00002.gguf",
            &gguf(7),
        );
        fx.write(".lmstudio/models/org/m-GGUF/mmproj-m-f16.gguf", &gguf(1));
        let mut models = lm(&fx, &UsageRecords::default()).models;
        models.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].name, "org/m-GGUF/m-Q4_K_M.gguf");
        assert_eq!(models[1].name, "org/m-GGUF/m-Q8_0-00001-of-00002.gguf");
        assert_eq!(models[0].files.len(), 2);
        assert_eq!(models[1].files.len(), 3);
        for model in &models {
            assert!(model
                .files
                .iter()
                .any(|file| file.logical.starts_with("mmproj")));
        }
    }

    #[test]
    fn weights_without_a_config_are_incomplete_like_the_real_splitted_model_folder() {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/models/org/roto/splitted_model/model.layers.0.mlx.npz",
            &[1u8; 4096],
        );
        fx.write(
            ".lmstudio/models/org/roto/splitted_model/model.layers.0.mlx.done",
            b"",
        );
        let found = lm(&fx, &UsageRecords::default());
        assert_eq!(found.models.len(), 1);
        assert!(found.models[0].incomplete);
        assert_eq!(found.models[0].detected.format, ModelFormat::Mlx);
    }

    #[test]
    fn a_complete_mlx_folder_is_not_incomplete() {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/models/org/bien/config.json",
            br#"{"model_type":"qwen3","quantization":{"bits":4}}"#,
        );
        fx.write(".lmstudio/models/org/bien/model.safetensors", &[1u8; 4096]);
        let found = lm(&fx, &UsageRecords::default());
        assert!(!found.models[0].incomplete);
        assert_eq!(
            found.models[0].detected.quantization.as_deref(),
            Some("4bit")
        );
    }

    #[test]
    fn folders_without_weights_are_not_models_and_junk_is_skipped() {
        let fx = Fixture::new();
        fx.write(".lmstudio/models/org/solo-config/config.json", b"{}");
        fx.write(".lmstudio/models/.DS_Store", b"x");
        fx.write(".lmstudio/models/org/.DS_Store", b"x");
        fx.write(".lmstudio/models/suelto.gguf", &gguf(15));
        assert!(lm(&fx, &UsageRecords::default()).models.is_empty());
    }

    #[test]
    fn usage_is_matched_exactly_by_folder_or_file_id_and_remote_ids_do_not_count() {
        let fx = Fixture::new();
        fx.write(".lmstudio/models/org/mlx/config.json", b"{}");
        fx.write(".lmstudio/models/org/mlx/model.safetensors", &[1u8; 2048]);
        fx.write(".lmstudio/models/org/one-GGUF/one.gguf", &gguf(15));
        fx.write(".lmstudio/models/org/two-GGUF/a.gguf", &gguf(15));
        fx.write(".lmstudio/models/org/two-GGUF/b.gguf", &gguf(7));
        fx.write(".lmstudio/models/org/remote-GGUF/r.gguf", &gguf(7));
        let time = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        let usage = UsageRecords {
            lm_studio: HashMap::from([
                ("org/mlx".to_string(), time(1)),
                ("org/one-GGUF/one.gguf".to_string(), time(2)),
                ("org/two-GGUF/b.gguf".to_string(), time(3)),
                ("4fde2ea4:org/remote-GGUF/r.gguf".to_string(), time(4)),
            ]),
            ..UsageRecords::default()
        };
        let models = lm(&fx, &usage).models;
        let used = |name: &str| models.iter().find(|m| m.name == name).unwrap().used;
        assert_eq!(used("org/mlx"), Some(time(1)));
        assert_eq!(used("org/one-GGUF"), Some(time(2)));
        assert_eq!(used("org/two-GGUF/a.gguf"), None);
        assert_eq!(used("org/two-GGUF/b.gguf"), Some(time(3)));
        assert_eq!(used("org/remote-GGUF"), None);
    }

    #[test]
    fn downloads_in_progress_are_left_alone_and_abandoned_ones_are_reported_without_counting_as_weights(
    ) {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/models/org/m/downloading_model.gguf.part",
            &[1u8; 8192],
        );
        let found = lm(&fx, &UsageRecords::default());
        assert!(found.models.is_empty());
        assert!(found.leftovers.is_empty());
        fx.age_tree(".lmstudio/models", 8);
        let found = lm(&fx, &UsageRecords::default());
        assert_eq!(found.leftovers.len(), 1);
        assert_eq!(found.leftovers[0].kind, LeftoverKind::Partial);
    }

    #[test]
    fn a_file_linked_in_from_the_hf_cache_is_measured_at_its_real_path() {
        let fx = Fixture::new();
        let blob = fx.write(
            ".cache/huggingface/hub/models--o--n/blobs/abc",
            &[7u8; 4096],
        );
        let link = fx.home.join(".lmstudio/models/org/enlace/model.gguf");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&blob, &link).unwrap();
        let found = lm(&fx, &UsageRecords::default());
        assert_eq!(found.models[0].files[0].path, blob);
        assert_eq!(found.models[0].files[0].logical, "model.gguf");
    }

    #[test]
    fn links_into_prompting_folders_are_not_followed() {
        let fx = Fixture::new();
        let private = fx.write("Documents/m.gguf", &gguf(15));
        let link = fx.home.join(".lmstudio/models/org/enlace/model.gguf");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&private, &link).unwrap();
        let folder_link = fx.home.join(".lmstudio/models/org/carpeta");
        std::os::unix::fs::symlink(private.parent().unwrap(), &folder_link).unwrap();
        assert!(lm(&fx, &UsageRecords::default()).models.is_empty());
    }

    #[test]
    fn a_linked_model_folder_keeps_the_link_as_its_root() {
        let fx = Fixture::new();
        fx.write("elsewhere/m/config.json", b"{}");
        fx.write("elsewhere/m/model.safetensors", &[1u8; 4096]);
        let link = fx.home.join(".lmstudio/models/org/enlazado");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(fx.home.join("elsewhere/m"), &link).unwrap();
        let found = lm(&fx, &UsageRecords::default());
        assert_eq!(found.models[0].root, link);
        assert_eq!(found.models[0].files.len(), 2);
    }

    #[test]
    fn a_split_gguf_missing_a_shard_is_incomplete() {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/models/org/m/m-Q8_0-00001-of-00002.gguf",
            &gguf(7),
        );
        assert!(lm(&fx, &UsageRecords::default()).models[0].incomplete);
    }

    #[test]
    fn omlx_handles_flat_and_organized_layouts_and_skips_adapters() {
        let fx = Fixture::new();
        fx.write(".omlx/models/plano/config.json", b"{}");
        fx.write(".omlx/models/plano/model.safetensors", &[1u8; 2048]);
        fx.write(".omlx/models/org/hijo/config.json", b"{}");
        fx.write(".omlx/models/org/hijo/model.safetensors", &[1u8; 2048]);
        fx.write(".omlx/models/lora/adapter_config.json", b"{}");
        fx.write(".omlx/models/lora/adapters.safetensors", &[1u8; 2048]);
        let when = UNIX_EPOCH + Duration::from_secs(99);
        let usage = UsageRecords {
            omlx: HashMap::from([("plano".to_string(), when)]),
            ..UsageRecords::default()
        };
        let ctx = fx.ctx();
        let mut models = scan_omlx(&fx.home.join(".omlx/models"), &roots(&fx), &ctx, &usage).models;
        models.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<_> = models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["hijo", "plano"]);
        assert_eq!(models[1].used, Some(when));
        assert_eq!(
            models[1].detected.format,
            ModelFormat::Mlx,
            "oMLX only runs MLX"
        );
    }

    #[test]
    fn flat_stores_name_each_file_and_skip_placeholders() {
        let fx = Fixture::new();
        let big = vec![1u8; 2 * 1024 * 1024];
        fx.write("ComfyUI/models/checkpoints/sdxl.safetensors", &big);
        fx.write("ComfyUI/models/loras/estilo.safetensors", &big);
        fx.write("ComfyUI/models/checkpoints/put_checkpoints_here", b"");
        fx.write("ComfyUI/models/configs/v1.yaml", b"a: 1");
        fx.write("ComfyUI/models/checkpoints/parcial.safetensors.tmp", &big);
        fx.write("ComfyUI/models/tiny.safetensors", b"pequeno");
        let ctx = fx.ctx();
        let mut found = scan_flat(
            RuntimeKind::ComfyUi,
            "",
            &fx.home.join("ComfyUI/models"),
            6,
            &roots(&fx),
            &ctx,
        );
        found.models.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<_> = found.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            ["checkpoints/sdxl.safetensors", "loras/estilo.safetensors"]
        );
        assert_eq!(
            found.models[0].root,
            fx.home.join("ComfyUI/models/checkpoints/sdxl.safetensors")
        );
    }

    #[test]
    fn whisper_files_and_core_ml_packages_are_recognised() {
        let fx = Fixture::new();
        let big = vec![1u8; 2 * 1024 * 1024];
        fx.write(".cache/whisper/small.pt", &big);
        fx.write(".cache/whisper/ggml-base.bin", &big);
        fx.write(
            ".cache/whisper/ggml-base-encoder.mlmodelc/weights/weight.bin",
            &big,
        );
        fx.write(".cache/whisper/ggml-base-encoder.mlmodelc/model.mil", b"x");
        let ctx = fx.ctx();
        let found = scan_flat(
            RuntimeKind::Whisper,
            "whisper/",
            &fx.home.join(".cache/whisper"),
            2,
            &roots(&fx),
            &ctx,
        );
        let format = |name: &str| {
            found
                .models
                .iter()
                .find(|m| m.name == name)
                .unwrap()
                .detected
                .format
        };
        assert_eq!(format("whisper/small.pt"), ModelFormat::PyTorch);
        assert_eq!(format("whisper/ggml-base.bin"), ModelFormat::Ggml);
        assert_eq!(
            format("whisper/ggml-base-encoder.mlmodelc"),
            ModelFormat::CoreMl
        );
    }
}
