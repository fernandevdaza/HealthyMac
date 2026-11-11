//! Every local model on the machine, across runtimes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use hm_core::fsutil::{clone_id, measure, measure_many, CancelToken};
use hm_core::{t, tf, Action, Area, Finding, Recovery, Risk, ScanContext};

use crate::duplicates::{self, MIN_DUPLICATE_BYTES};
use crate::files::{name_of, relative, unique_bytes, walk, FileRec};
use crate::fit;
use crate::locate::{Detail, Roots};
use crate::metadata::{is_gguf, is_projector, is_weight};
use crate::runtime::{self, Ports};
use crate::stores::{folders, huggingface, newest, ollama, Leftover, RawModel, StoreScan};
use crate::types::{
    LocalModel, MemoryBudget, ModelFormat, ModelInventory, RuntimeKind, RuntimeStatus, UsageSignal,
};
use crate::usage::UsageRecords;

const REMOVAL_SCANNER_ID: &str = "ai.model_removal";

/// A model, and the real files behind it.
pub(crate) type Entry = (LocalModel, Vec<FileRec>);

/// Everything a scan learned, before any hashing.
#[derive(Debug)]
pub(crate) struct Snapshot {
    pub roots: Roots,
    pub budget: MemoryBudget,
    /// Largest first.
    pub entries: Vec<Entry>,
    pub leftovers: Vec<Leftover>,
    pub stale_locks: Vec<PathBuf>,
    pub dataset_repos: Vec<PathBuf>,
    pub runtimes: Vec<RuntimeStatus>,
    pub total_bytes: u64,
}

/// A path can be reached more than once, as when oMLX is pointed at the LM Studio folder. Whoever
/// sees it first owns it.
struct Visited(HashSet<PathBuf>);

impl Visited {
    fn first_time(&mut self, path: &Path) -> bool {
        self.0
            .insert(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
    }
}

fn has_hub_layout(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| name_of(&entry.path()).starts_with("models--"))
        })
        .unwrap_or(false)
}

fn collect(roots: &Roots, ctx: &ScanContext, usage: &UsageRecords) -> StoreScan {
    let mut scan = StoreScan::default();
    let mut visited = Visited(HashSet::new());

    for root in &roots.ollama {
        if let Some(real) = roots
            .follow_root(root)
            .filter(|real| visited.first_time(real))
        {
            scan.absorb(ollama::scan(&real, ctx, usage));
        }
    }

    if let Some(hub) = roots
        .follow_root(&roots.hub)
        .filter(|hub| visited.first_time(hub))
    {
        scan.absorb(huggingface::scan_hub(
            &hub,
            RuntimeKind::HuggingFace,
            roots,
            ctx,
            usage,
        ));
    }

    for root in &roots.lm_models {
        if let Some(real) = roots
            .follow_root(root)
            .filter(|real| visited.first_time(real))
        {
            scan.absorb(folders::scan_lm_studio(&real, roots, ctx, usage));
        }
    }
    scan.absorb(folders::scan_lm_temp(roots, ctx));

    // oMLX also serves the Hugging Face cache, but its models are counted as Hugging Face's, above.
    for dir in &roots.omlx.model_dirs {
        if let Some(real) = roots
            .follow_root(dir)
            .filter(|real| visited.first_time(real))
        {
            if has_hub_layout(&real) {
                scan.absorb(huggingface::scan_hub(
                    &real,
                    RuntimeKind::OMlx,
                    roots,
                    ctx,
                    usage,
                ));
            } else {
                scan.absorb(folders::scan_omlx(&real, roots, ctx, usage));
            }
        }
    }

    // Current llama.cpp keeps its downloads in the Hugging Face layout. With LLAMA_CACHE unset,
    // that's the Hugging Face cache itself. Older versions left flat files here.
    if let Some(cache) = roots
        .follow_root(&roots.llama_cache)
        .filter(|cache| visited.first_time(cache))
    {
        if has_hub_layout(&cache) {
            scan.absorb(huggingface::scan_hub(
                &cache,
                RuntimeKind::LlamaCpp,
                roots,
                ctx,
                usage,
            ));
        }
        scan.absorb(folders::scan_flat(
            RuntimeKind::LlamaCpp,
            "",
            &cache,
            1,
            roots,
            ctx,
        ));
    }

    for dir in &roots.whisper {
        if let Some(real) = roots
            .follow_root(dir)
            .filter(|real| visited.first_time(real))
        {
            scan.absorb(folders::scan_flat(
                RuntimeKind::Whisper,
                "whisper/",
                &real,
                2,
                roots,
                ctx,
            ));
        }
    }

    let checkpoints = roots.torch_home.join("hub/checkpoints");
    if let Some(real) = roots
        .follow_root(&checkpoints)
        .filter(|real| visited.first_time(real))
    {
        scan.absorb(folders::scan_flat(
            RuntimeKind::TorchHub,
            "",
            &real,
            2,
            roots,
            ctx,
        ));
    }

    if let Some(real) = roots
        .follow_root(&roots.sentence_transformers)
        .filter(|real| visited.first_time(real))
    {
        for model in subdirectories(&real) {
            folders::scan_model_folder(
                RuntimeKind::SentenceTransformers,
                &name_of(&model),
                &model,
                false,
                roots,
                ctx,
                &mut scan,
            );
        }
    }

    if let Some(real) = roots
        .follow_root(&roots.gpt4all)
        .filter(|real| visited.first_time(real))
    {
        scan.absorb(folders::scan_flat(
            RuntimeKind::Gpt4All,
            "",
            &real,
            1,
            roots,
            ctx,
        ));
    }

    for root in &roots.jan {
        for sub in ["llamacpp/models", "mlx/models", "models"] {
            let models = root.join(sub);
            if let Some(real) = roots
                .follow_root(&models)
                .filter(|real| visited.first_time(real))
            {
                for model in subdirectories(&real) {
                    folders::scan_model_folder(
                        RuntimeKind::Jan,
                        &name_of(&model),
                        &model,
                        false,
                        roots,
                        ctx,
                        &mut scan,
                    );
                }
            }
        }
    }

    // These are only usual locations, and not something the user set, so a refusal is silent.
    for (runtime, list) in [
        (RuntimeKind::ComfyUi, &roots.comfy),
        (RuntimeKind::Automatic1111, &roots.a1111),
    ] {
        for root in list {
            let models = root.join("models");
            if !roots.allowed_quietly(&models) {
                continue;
            }
            if let Some(real) = roots
                .follow_root(&models)
                .filter(|real| visited.first_time(real))
            {
                scan.absorb(folders::scan_flat(runtime, "", &real, 6, roots, ctx));
            }
        }
    }

    if let Some(models) = &roots.draw_things {
        if roots.allowed_quietly(models) {
            if let Some(real) = roots
                .follow_root(models)
                .filter(|real| visited.first_time(real))
            {
                let mut found =
                    folders::scan_flat(RuntimeKind::DrawThings, "", &real, 1, roots, ctx);
                for model in &mut found.models {
                    // Its `.ckpt` files are its own format, in spite of the name.
                    model.detected.format = ModelFormat::Unknown;
                }
                scan.absorb(found);
            }
        } else if runtime::app_path(ctx, "Draw Things.app").is_some() {
            roots.note_skipped(models);
        }
    }
    if !roots.full_disk_access && runtime::app_path(ctx, "ComfyUI.app").is_some() {
        roots.note_skipped(&ctx.home.join("Documents/ComfyUI"));
    }

    collect_stray_gguf(roots, ctx, &mut visited, &mut scan);
    scan
}

/// Plenty of tools nobody has heard of leave large models in `~/.cache`: the machine this was built
/// on had 66 GB in one. GGUF is the one format that's unmistakably a model by its name. Bounded, and
/// the big caches that can't hold one aren't entered.
fn collect_stray_gguf(
    roots: &Roots,
    ctx: &ScanContext,
    visited: &mut Visited,
    scan: &mut StoreScan,
) {
    const NOT_ENTERED: [&str; 8] = [
        "huggingface",
        "whisper",
        "whisper.cpp",
        "torch",
        "lm-studio",
        "uv",
        "pip",
        "pnpm",
    ];
    let Some(cache) = roots.follow_root(&roots.cache) else {
        return;
    };
    let mut folders: Vec<PathBuf> = Vec::new();
    walk(&cache, 4, ctx, &mut |path, meta| {
        let name = name_of(path);
        if name.starts_with('.') {
            return false;
        }
        if meta.is_dir() {
            return !(path.parent() == Some(cache.as_path())
                && NOT_ENTERED.contains(&name.as_str()));
        }
        if meta.is_file() && is_gguf(&name) {
            folders.extend(path.parent().map(Path::to_path_buf));
        }
        false
    });
    folders.sort();
    folders.dedup();
    for folder in folders {
        if visited.first_time(&folder) {
            let name = relative(&folder, &cache);
            folders::scan_model_folder(RuntimeKind::Other, &name, &folder, false, roots, ctx, scan);
        }
    }
}

fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            !name_of(path).starts_with('.')
                && fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
        })
        .collect();
    found.sort();
    found
}

/// Turns raw models into what the UI sees. Shared bytes are figured out here, where all of them are
/// known, and only among models of one runtime, as documented on the field.
fn finish_models(raw: Vec<RawModel>, budget: &MemoryBudget) -> Vec<Entry> {
    let mut uses: HashMap<(RuntimeKind, (u64, u64)), u32> = HashMap::new();
    for model in &raw {
        let mut seen = HashSet::new();
        for file in &model.files {
            if seen.insert(file.key()) {
                *uses.entry((model.runtime, file.key())).or_default() += 1;
            }
        }
    }

    let mut used_ids: HashSet<String> = HashSet::new();
    let mut entries: Vec<Entry> = raw
        .into_iter()
        .map(|model| {
            let size = unique_bytes(&model.files);
            let mut seen = HashSet::new();
            let shared_bytes = model
                .files
                .iter()
                .filter(|file| seen.insert(file.key()))
                .filter(|file| {
                    uses.get(&(model.runtime, file.key()))
                        .is_some_and(|count| *count > 1)
                })
                .map(|file| file.bytes)
                .sum();
            // Logical size, since that's what a loader maps, however the file is stored.
            let logical: u64 = model.files.iter().map(|file| file.len).sum();

            let base_id = format!("{}:{}", model.runtime.key(), model.name);
            let mut id = base_id.clone();
            let mut suffix = 2;
            while !used_ids.insert(id.clone()) {
                id = format!("{base_id}#{suffix}");
                suffix += 1;
            }

            let (last_used, last_used_source) = match model.used {
                Some(used) => (Some(used), UsageSignal::RuntimeRecord),
                None => {
                    let downloaded = newest_weight_time(&model.files);
                    (
                        downloaded,
                        if downloaded.is_some() {
                            UsageSignal::Modified
                        } else {
                            UsageSignal::None
                        },
                    )
                }
            };

            let local = LocalModel {
                id,
                name: model.name,
                runtime: model.runtime,
                format: model.detected.format,
                quantization: model.detected.quantization,
                architecture: model.detected.architecture,
                parameter_count: model.detected.parameter_count,
                context_length: model.detected.context_length,
                size,
                shared_bytes,
                root: model.root,
                files: model.files.iter().map(|file| file.path.clone()).collect(),
                last_used,
                last_used_source,
                loaded: false,
                fit: fit::assess(logical, budget),
                incomplete: model.incomplete,
            };
            (local, model.files)
        })
        .collect();
    entries.sort_by(|(a, _), (b, _)| b.size.cmp(&a.size).then(a.id.cmp(&b.id)));
    entries
}

/// When the weights last changed, which is to say when they were downloaded.
pub(crate) fn newest_weight_time(files: &[FileRec]) -> Option<SystemTime> {
    let weights = newest(
        files
            .iter()
            .filter(|file| is_weight(&file.logical, file.len))
            .map(|file| file.modified),
    );
    weights.or_else(|| newest(files.iter().map(|file| file.modified)))
}

/// Matches what running runtimes report against the inventory. A model in memory is in use now,
/// whatever any record says.
fn apply_loaded(
    entries: &mut [Entry],
    runtimes: &[RuntimeStatus],
    usage: &UsageRecords,
    now: SystemTime,
) {
    for status in runtimes {
        for loaded in &status.loaded {
            let wanted = loaded.name.to_lowercase();
            let lm_id = usage
                .lm_identifiers
                .get(&wanted)
                .map(|id| id.to_lowercase());
            for (model, _) in entries.iter_mut() {
                let name = model.name.to_lowercase();
                let matches = match status.kind {
                    RuntimeKind::LmStudio => {
                        model.runtime == RuntimeKind::LmStudio
                            && (lm_id.as_deref() == Some(name.as_str()) || wanted == name)
                    }
                    RuntimeKind::OMlx => {
                        (model.runtime == RuntimeKind::OMlx && wanted == name)
                            || (model.runtime == RuntimeKind::HuggingFace
                                && wanted == name.replacen('/', "--", 1))
                    }
                    kind => model.runtime == kind && wanted == name,
                };
                if matches {
                    model.loaded = true;
                    model.last_used = Some(now);
                    model.last_used_source = UsageSignal::LoadedNow;
                }
            }
        }
    }
}

/// Disk really in use: hard links and whole clone families count once, across everything.
fn total_bytes(entries: &[Entry], leftovers: &[Leftover]) -> u64 {
    let mut inodes = HashSet::new();
    let mut clones = HashSet::new();
    let mut total = 0;
    let records = entries.iter().flat_map(|(_, files)| files.iter()).chain(
        leftovers
            .iter()
            .flat_map(|leftover| leftover.records.iter()),
    );
    for record in records {
        if !inodes.insert(record.key()) {
            continue;
        }
        // Small files aren't worth the system call. Nobody clones them in practice.
        if record.len >= MIN_DUPLICATE_BYTES {
            if let Some(id) = clone_id(&record.path) {
                if !clones.insert((record.dev, id)) {
                    continue;
                }
            }
        }
        total += record.bytes;
    }
    total
}

fn fill_storage(runtimes: &mut Vec<RuntimeStatus>, entries: &[Entry], leftovers: &[Leftover]) {
    let mut by_runtime: HashMap<RuntimeKind, (HashSet<(u64, u64)>, u64)> = HashMap::new();
    let records = entries
        .iter()
        .flat_map(|(model, files)| files.iter().map(move |file| (model.runtime, file)))
        .chain(leftovers.iter().flat_map(|leftover| {
            leftover
                .records
                .iter()
                .map(move |file| (leftover.runtime, file))
        }));
    for (runtime, record) in records {
        let (seen, total) = by_runtime.entry(runtime).or_default();
        if seen.insert(record.key()) {
            *total += record.bytes;
        }
    }
    for (kind, (_, total)) in by_runtime {
        match runtimes.iter_mut().find(|row| row.kind == kind) {
            Some(row) => row.storage_bytes = total,
            None => runtimes.push(RuntimeStatus {
                kind,
                installed: false,
                running: false,
                endpoint: None,
                version: None,
                loaded: Vec::new(),
                storage_path: entries
                    .iter()
                    .find(|(model, _)| model.runtime == kind)
                    .map(|(model, _)| model.root.clone()),
                storage_bytes: total,
                loaded_count: None,
                loaded_memory_bytes: None,
            }),
        }
    }
}

impl Snapshot {
    pub fn build(ctx: &ScanContext, budget: MemoryBudget) -> Self {
        Self::build_with(ctx, budget, None)
    }

    pub fn build_with(ctx: &ScanContext, budget: MemoryBudget, ports: Option<Ports>) -> Self {
        let roots = Roots::resolve(ctx, Detail::Full);
        let usage = UsageRecords::load(&roots);
        let found = collect(&roots, ctx, &usage);
        let mut entries = finish_models(found.models, &budget);
        let ports = ports.unwrap_or_else(|| Ports::resolve(&roots));
        let mut runtimes = runtime::status_with(ctx, &roots, &ports);
        apply_loaded(&mut entries, &runtimes, &usage, ctx.now);
        fill_storage(&mut runtimes, &entries, &found.leftovers);
        let total_bytes = total_bytes(&entries, &found.leftovers);
        Self {
            roots,
            budget,
            entries,
            leftovers: found.leftovers,
            stale_locks: found.stale_locks,
            dataset_repos: found.dataset_repos,
            runtimes,
            total_bytes,
        }
    }

    pub fn into_inventory(self, ctx: &ScanContext) -> ModelInventory {
        let report = duplicates::find(ctx, &self.entries);
        ModelInventory {
            reclaimable_bytes: report.groups.iter().map(|group| group.reclaimable).sum(),
            duplicates: report.groups,
            hashed_files: report.hashed_files,
            hashed_bytes: report.hashed_bytes,
            skipped_roots: self.roots.skipped(),
            models: self.entries.into_iter().map(|(model, _)| model).collect(),
            runtimes: self.runtimes,
            total_bytes: self.total_bytes,
            memory: self.budget,
        }
    }
}

/// Builds the full inventory. Slow on first run, because finding duplicates may need hashing: call
/// it from a background thread.
pub fn scan(ctx: &ScanContext) -> Result<ModelInventory> {
    Ok(Snapshot::build(ctx, fit::current_budget()).into_inventory(ctx))
}

/// Processes that hold a runtime's models open. Removing a model under them isn't a disaster, since
/// an open file stays readable, but the app would be left confused.
pub(crate) fn runtime_processes(runtime: RuntimeKind) -> &'static [&'static str] {
    match runtime {
        RuntimeKind::LmStudio => &["LM Studio"],
        RuntimeKind::OMlx => &["oMLX", "omlx-cli", "omlx-server"],
        RuntimeKind::Jan => &["Jan"],
        RuntimeKind::Gpt4All => &["GPT4All", "gpt4all"],
        RuntimeKind::DrawThings => &["Draw Things"],
        RuntimeKind::ComfyUi => &["ComfyUI"],
        _ => &[],
    }
}

/// How removing a model works. Shared with scanners that offer it.
#[derive(Debug)]
pub(crate) struct RemovalPlan {
    pub action: Action,
    pub size: u64,
    pub items: u64,
    pub explanation: String,
}

/// What `ollama rm` is handed comes from a path on disk, so it mustn't be able to look like a flag.
/// No shell is involved, which rules out anything worse than that.
fn is_safe_ollama_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/'))
}

/// A folder with several quantizations holds more than this model. Then it's just this model's own
/// GGUFs that go. Files linked in from elsewhere are left: they belong to someone else.
fn own_variant_files(model: &LocalModel) -> Option<Vec<PathBuf>> {
    // A linked folder belongs to whatever it points at. All that goes is the link.
    let is_real_folder = fs::symlink_metadata(&model.root).is_ok_and(|meta| meta.is_dir());
    if !is_real_folder {
        return None;
    }
    let is_own_weights = |path: &&PathBuf| {
        let name = name_of(path);
        is_gguf(&name) && !is_projector(&name) && path.starts_with(&model.root)
    };
    let own: Vec<PathBuf> = model.files.iter().filter(is_own_weights).cloned().collect();
    if own.is_empty() {
        return None;
    }
    let mut stack = vec![model.root.clone()];
    let mut others = false;
    while let Some(dir) = stack.pop() {
        // Not knowing what else is in here is a reason to take less, not more.
        let Ok(entries) = fs::read_dir(&dir) else {
            others = true;
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = name_of(&path);
            if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                stack.push(path);
            } else if is_gguf(&name) && !is_projector(&name) && !own.contains(&path) {
                others = true;
            }
        }
    }
    others.then_some(own)
}

pub(crate) fn removal_plan(model: &LocalModel) -> RemovalPlan {
    let cancel = CancelToken::new();
    match model.runtime {
        RuntimeKind::Ollama => {
            if !is_safe_ollama_name(&model.name) {
                return RemovalPlan {
                    action: Action::ReportOnly,
                    size: 0,
                    items: 1,
                    explanation: t!(
                        "Su nombre no es válido para `ollama rm`. Elimínalo desde Ollama.",
                        "Its name isn't valid for `ollama rm`. Remove it from Ollama."
                    )
                    .to_string(),
                };
            }
            RemovalPlan {
                action: Action::Command {
                    program: "ollama".to_string(),
                    args: vec!["rm".to_string(), model.name.clone()],
                },
                size: model.size.saturating_sub(model.shared_bytes),
                items: 1,
                explanation: t!(
                    "Se elimina con `ollama rm`, que respeta las capas compartidas con otros \
                     modelos: solo se libera lo que usa este en exclusiva. Ollama tiene que \
                     estar abierto. Se puede volver a descargar con `ollama pull`.",
                    "It's removed with `ollama rm`, which keeps the layers shared with other \
                     models: only what this one uses alone is freed. Ollama has to be open. You \
                     can download it again with `ollama pull`."
                )
                .to_string(),
            }
        }
        RuntimeKind::HuggingFace | RuntimeKind::LlamaCpp | RuntimeKind::OMlx
            if name_of(&model.root).starts_with("models--") =>
        {
            let (paths, size) = huggingface::repo_removal(&model.root);
            RemovalPlan {
                items: model.files.len() as u64,
                action: Action::Trash { paths },
                size,
                explanation: t!(
                    "Va a la Papelera la carpeta del modelo en la caché de Hugging Face, junto \
                     con los datos que no comparte con otros repositorios. Se vuelve a \
                     descargar la próxima vez que algo lo pida.",
                    "The model's folder in the Hugging Face cache goes to the Trash, along with \
                     the data it doesn't share with other repositories. It's downloaded again the \
                     next time something asks for it."
                )
                .to_string(),
            }
        }
        runtime => {
            let paths = own_variant_files(model).unwrap_or_else(|| vec![model.root.clone()]);
            RemovalPlan {
                items: model.files.len() as u64,
                size: measure_many(&paths, &cancel).bytes,
                action: Action::Trash { paths },
                explanation: if runtime == RuntimeKind::Other {
                    t!(
                        "Lo guardó otro programa y HealthyMac no sabe de cuál, ni si es una descarga o algo que \
                         costó tiempo generar. Va a la Papelera: compruébalo antes de vaciarla.",
                        "Another app saved it, and HealthyMac doesn't know which one, or whether it's a \
                         download or something that took time to generate. It goes to the Trash: check it \
                         before you empty the Trash."
                    )
                    .to_string()
                } else {
                    tf!(
                        "Va a la Papelera. Para usarlo otra vez habrá que volver a descargarlo desde {}.",
                        "It goes to the Trash. To use it again, you'll have to download it from {} again.",
                        runtime.label()
                    )
                },
            }
        }
    }
}

/// `Finding::last_used` is shown as when something was last used, so a download date must not end
/// up there.
pub(crate) fn honest_last_used(model: &LocalModel) -> Option<SystemTime> {
    match model.last_used_source {
        UsageSignal::LoadedNow | UsageSignal::RuntimeRecord => model.last_used,
        UsageSignal::Modified | UsageSignal::None => None,
    }
}

/// How a model comes back. A model from an unknown tool may not be something anyone can download.
pub(crate) fn removal_risk(runtime: RuntimeKind) -> (Risk, Recovery) {
    if runtime == RuntimeKind::Other {
        (Risk::Review, Recovery::None)
    } else {
        (Risk::Moderate, Recovery::Redownload)
    }
}

/// Builds the finding that removes `model` when handed to the cleaner, using the runtime's own
/// mechanism where it has one (`ollama rm`, so that shared layers survive). Never selected by
/// default. Reads the disk to work out what removal would really free, so call it when needed.
pub fn model_removal_finding(model: &LocalModel) -> Finding {
    let plan = removal_plan(model);
    let (risk, recovery) = removal_risk(model.runtime);
    Finding::new(REMOVAL_SCANNER_ID, Area::LocalAi, &model.id, &model.name)
        .group(model.runtime.label())
        .subtitle(model.root.display().to_string())
        .explanation(plan.explanation)
        .size(plan.size)
        .items(plan.items)
        .last_used(honest_last_used(model))
        .risk(risk)
        .recovery(recovery)
        .selected(false)
        .action(plan.action)
        .blocked_by(runtime_processes(model.runtime))
}

/// What a tree holds, for findings about caches.
pub(crate) fn measure_path(path: &Path, ctx: &ScanContext) -> hm_core::SizeInfo {
    measure(path, &ctx.cancel)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::http::tests::closed_port;
    use crate::test_support::{sha, Fixture};
    use crate::types::{ModelFit, ModelFormat};
    use hm_core::fsutil;
    use hm_core::{CleanOptions, CleanStatus, Cleaner};
    use std::os::unix::fs::MetadataExt;
    use std::time::Duration;

    pub(crate) const BIG_BUDGET: MemoryBudget = MemoryBudget {
        total_ram: 1 << 50,
        gpu_limit: 1 << 49,
    };

    fn no_runtimes() -> Ports {
        Ports {
            ollama: None,
            lm_studio: closed_port(),
            omlx: closed_port(),
            llama_cpp: closed_port(),
        }
    }

    fn inventory(fx: &Fixture) -> ModelInventory {
        let ctx = fx.ctx();
        Snapshot::build_with(&ctx, BIG_BUDGET, Some(no_runtimes())).into_inventory(&ctx)
    }

    fn model<'a>(inventory: &'a ModelInventory, name: &str) -> &'a LocalModel {
        inventory
            .models
            .iter()
            .find(|model| model.name == name)
            .unwrap_or_else(|| panic!("{name} not in {:?}", inventory.models))
    }

    fn weights(seed: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|at| seed.wrapping_add((at % 251) as u8).wrapping_mul(3))
            .collect()
    }

    #[test]
    fn an_empty_home_is_an_empty_inventory() {
        let fx = Fixture::new();
        let inventory = inventory(&fx);
        assert!(inventory.models.is_empty());
        assert!(inventory.duplicates.is_empty());
        assert_eq!(inventory.total_bytes, 0);
    }

    #[test]
    fn ollama_layers_are_shared_between_tags_and_counted_once_in_the_total() {
        let fx = Fixture::new();
        let base = weights(1, 40_000);
        let (only_a, only_b) = (weights(2, 20_000), weights(3, 8_000));
        fx.ollama_model(
            "registry.ollama.ai/library/m/a",
            &[("model", &base), ("params", &only_a)],
        );
        fx.ollama_model(
            "registry.ollama.ai/library/m/b",
            &[("model", &base), ("params", &only_b)],
        );
        let blob_bytes = |bytes: &[u8]| {
            fs::metadata(
                fx.home
                    .join(".ollama/models/blobs")
                    .join(format!("sha256-{}", sha(bytes))),
            )
            .unwrap()
            .blocks()
                * 512
        };
        let inventory = inventory(&fx);
        let a = model(&inventory, "m:a");
        let b = model(&inventory, "m:b");

        let config_bytes = {
            let config = br#"{"model_format":"gguf","model_family":"llama","model_type":"7.2B","file_type":"Q4_0"}"#;
            blob_bytes(config)
        };
        assert_eq!(
            a.size,
            blob_bytes(&base) + blob_bytes(&only_a) + config_bytes
        );
        assert_eq!(
            b.size,
            blob_bytes(&base) + blob_bytes(&only_b) + config_bytes
        );
        assert_eq!(a.shared_bytes, blob_bytes(&base) + config_bytes);
        assert_eq!(
            inventory.total_bytes,
            blob_bytes(&base) + blob_bytes(&only_a) + blob_bytes(&only_b) + config_bytes,
            "each blob once"
        );
        assert_eq!(
            inventory
                .runtimes
                .iter()
                .find(|r| r.kind == RuntimeKind::Ollama)
                .unwrap()
                .storage_bytes,
            inventory.total_bytes
        );
    }

    #[test]
    fn hugging_face_symlinks_are_not_double_counted() {
        let fx = Fixture::new();
        let repo = fx.hf_repo("org/m");
        let content = weights(4, 100_000);
        let etag = sha(&content);
        let blob = repo.blob(&etag, &content);
        repo.snapshot(
            "aaa",
            &[
                ("model.safetensors", &etag),
                ("a/b/otro.safetensors", &etag),
            ],
        );
        repo.main("aaa");
        let inventory = inventory(&fx);
        let found = model(&inventory, "org/m");
        assert_eq!(found.size, fs::metadata(&blob).unwrap().blocks() * 512);
        assert_eq!(found.files, vec![blob]);
        assert_eq!(found.shared_bytes, 0);
        assert_eq!(inventory.total_bytes, found.size);
    }

    /// The killer feature: one set of weights held by an Ollama blob, a Hugging Face blob in the
    /// shared store whose name isn't its digest, an LM Studio file and an oMLX file.
    fn four_copies(fx: &Fixture, content: &[u8]) -> Vec<PathBuf> {
        let etag = sha(content);
        fx.ollama_model(
            "registry.ollama.ai/library/qwen/latest",
            &[("model", content)],
        );
        let ollama = fx
            .home
            .join(".ollama/models/blobs")
            .join(format!("sha256-{etag}"));

        let repo = fx.hf_repo("org/qwen-GGUF");
        let payload = repo.store_blob(&etag, &"7".repeat(64), content);
        repo.snapshot("aaa", &[("qwen.gguf", &etag)]);
        repo.main("aaa");

        let lm = fx.write(".lmstudio/models/org/qwen-GGUF/qwen.gguf", content);
        let omlx = fx.write(".omlx/models/qwen/qwen.gguf", content);
        vec![ollama, payload, lm, omlx]
    }

    #[test]
    fn a_duplicate_across_runtimes_is_one_correct_group() {
        let fx = Fixture::new();
        let content = weights(5, 300_000);
        let paths = four_copies(&fx, &content);
        let inventory = inventory(&fx);

        assert_eq!(inventory.duplicates.len(), 1);
        let group = &inventory.duplicates[0];
        assert_eq!(
            group.sha256,
            sha(&content),
            "the content's digest, not the store's Xet name"
        );
        assert_eq!(group.file_size, content.len() as u64);
        let mut found = group.paths.clone();
        found.sort();
        let mut expected = paths.clone();
        expected.sort();
        assert_eq!(found, expected);
        // HF and LM Studio both call it org/qwen-GGUF.
        assert_eq!(group.model_names, ["org/qwen-GGUF", "qwen", "qwen:latest"]);
        assert_eq!(
            group.runtimes,
            [
                RuntimeKind::Ollama,
                RuntimeKind::LmStudio,
                RuntimeKind::HuggingFace,
                RuntimeKind::OMlx
            ]
        );

        let allocated = fs::metadata(&paths[0]).unwrap().blocks() * 512;
        assert_eq!(group.reclaimable, allocated * 3);
        assert_eq!(inventory.reclaimable_bytes, group.reclaimable);
        // Only the two without a digest in their name needed hashing.
        assert_eq!(inventory.hashed_files, 2);
        assert_eq!(group.finding_id, format!("ai.duplicates:{}", group.sha256));
        // A content-addressed copy is preferred as the original.
        let keep = group.keep.as_ref().unwrap();
        assert!(*keep == paths[0] || *keep == paths[1]);
        assert_eq!(group.paths[0], *keep);
    }

    #[test]
    fn known_digests_mean_no_file_is_read_at_all() {
        let fx = Fixture::new();
        let content = weights(6, 300_000);
        let etag = sha(&content);
        fx.ollama_model(
            "registry.ollama.ai/library/m/latest",
            &[("model", &content)],
        );
        let repo = fx.hf_repo("org/m");
        repo.blob(&etag, &content);
        repo.snapshot("aaa", &[("m.gguf", &etag)]);
        repo.main("aaa");
        let inventory = inventory(&fx);
        assert_eq!(inventory.duplicates.len(), 1);
        assert_eq!(inventory.hashed_files, 0);
    }

    #[test]
    fn same_size_different_bytes_are_not_duplicates_and_never_get_fully_hashed() {
        let fx = Fixture::new();
        // Like the layers of one model: exactly the same size and different content.
        for index in 0..6u8 {
            fx.write(
                &format!(".lmstudio/models/org/roto/parts/layer.{index}.mlx.npz"),
                &weights(index, 300_000),
            );
        }
        fx.write(".omlx/models/otro/layer.mlx.npz", &weights(100, 300_000));
        let inventory = inventory(&fx);
        assert!(inventory.duplicates.is_empty());
        assert_eq!(inventory.hashed_files, 0, "the samples were enough");
    }

    #[test]
    fn identical_samples_with_different_middles_are_caught_by_the_full_hash() {
        let fx = Fixture::new();
        let mut a = vec![0u8; 1024 * 1024];
        let mut b = a.clone();
        // Outside the three sampled windows.
        a[200_000] = 1;
        b[200_000] = 2;
        fx.write(".lmstudio/models/org/a/m.gguf", &a);
        fx.write(".lmstudio/models/org/b/m.gguf", &b);
        let inventory = inventory(&fx);
        assert_eq!(inventory.hashed_files, 2);
        assert!(inventory.duplicates.is_empty());
    }

    #[test]
    fn small_files_are_not_worth_a_group() {
        let fx = Fixture::new();
        fx.write(".lmstudio/models/org/a/m.gguf", &[9u8; 100]);
        fx.write(".lmstudio/models/org/b/m.gguf", &[9u8; 100]);
        assert!(inventory(&fx).duplicates.is_empty());
    }

    #[test]
    fn hard_links_are_one_copy() {
        let fx = Fixture::new();
        let content = weights(7, 300_000);
        let first = fx.write(".lmstudio/models/org/a/m.gguf", &content);
        let second = fx.home.join(".lmstudio/models/org/b/m.gguf");
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        fs::hard_link(&first, &second).unwrap();
        let inventory = inventory(&fx);
        assert!(inventory.duplicates.is_empty());
        assert_eq!(
            inventory.total_bytes,
            fs::metadata(&first).unwrap().blocks() * 512
        );
    }

    /// Real clones of a file, as the cleaner would make them.
    fn clone_of(source: &Path, destination: &Path) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let source = CString::new(source.as_os_str().as_bytes()).unwrap();
        let target = CString::new(destination.as_os_str().as_bytes()).unwrap();
        let result = unsafe { libc::clonefile(source.as_ptr(), target.as_ptr(), 0) };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    }

    #[test]
    fn already_cloned_copies_report_zero_reclaimable_and_need_no_hashing() {
        let fx = Fixture::new();
        let content = weights(8, 300_000);
        let first = fx.write(".lmstudio/models/org/a/m.gguf", &content);
        clone_of(&first, &fx.home.join(".lmstudio/models/org/b/m.gguf"));

        let ctx = fx.ctx();
        let snap = Snapshot::build_with(&ctx, BIG_BUDGET, Some(no_runtimes()));
        let report = duplicates::find(&ctx, &snap.entries);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].reclaimable, 0);
        assert_eq!(report.groups[0].sha256, "");
        assert_eq!(report.hashed_files, 0);
        assert!(report.findings.is_empty(), "nothing to do means no finding");
        assert_eq!(
            snap.total_bytes,
            fs::metadata(&first).unwrap().blocks() * 512,
            "disk used once"
        );
    }

    #[test]
    fn only_the_uncloned_copy_counts_when_some_already_share_blocks() {
        let fx = Fixture::new();
        let content = weights(9, 300_000);
        let original = fx.write(".lmstudio/models/org/a/m.gguf", &content);
        clone_of(&original, &fx.home.join(".lmstudio/models/org/b/m.gguf"));
        let separate = fx.write(".lmstudio/models/org/c/m.gguf", &content);

        let ctx = fx.ctx();
        let snap = Snapshot::build_with(&ctx, BIG_BUDGET, Some(no_runtimes()));
        let report = duplicates::find(&ctx, &snap.entries);
        let group = &report.groups[0];
        assert_eq!(group.paths.len(), 3);
        assert_eq!(
            group.reclaimable,
            fs::metadata(&separate).unwrap().blocks() * 512
        );
        // The two that already share are kept. It's the third that becomes a clone.
        let Action::CloneDedupe { keep, duplicates } = &report.findings[0].action else {
            panic!("expected a clone action");
        };
        assert_ne!(*keep, separate);
        assert_eq!(*duplicates, vec![separate]);
    }

    #[test]
    fn a_clone_edited_afterwards_is_not_taken_for_a_copy() {
        let fx = Fixture::new();
        let content = weights(40, 300_000);
        let original = fx.write(".lmstudio/models/org/a/m.gguf", &content);
        let edited = fx.home.join(".lmstudio/models/org/b/m.gguf");
        clone_of(&original, &edited);
        // One byte changed in place, outside the sampled windows. Copy-on-write splits off just that
        // block, so the two still share nearly all of their blocks. Are they still "the same"?
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = fs::File::options().write(true).open(&edited).unwrap();
            file.seek(SeekFrom::Start(90_000)).unwrap();
            file.write_all(&[content[90_000] ^ 0xff]).unwrap();
        }
        assert_ne!(fs::read(&original).unwrap(), fs::read(&edited).unwrap());
        assert_ne!(
            fsutil::clone_id(&original),
            fsutil::clone_id(&edited),
            "macOS gives it a new id"
        );

        let inventory = inventory(&fx);
        assert!(
            inventory.duplicates.is_empty(),
            "{:?}",
            inventory.duplicates
        );
        assert_eq!(
            inventory.hashed_files, 2,
            "the sample can't tell and the hash must"
        );
    }

    #[test]
    fn private_size_is_what_the_reclaimable_figure_rests_on() {
        let fx = Fixture::new();
        let content = weights(41, 300_000);
        let original = fx.write("a/m.bin", &content);
        let allocated = fs::metadata(&original).unwrap().blocks() * 512;
        assert_eq!(
            fsutil::private_size(&original),
            Some(allocated),
            "an ordinary file owns its blocks"
        );
        let clone = fx.home.join("b/m.bin");
        clone_of(&original, &clone);
        assert_eq!(
            fsutil::private_size(&original),
            Some(0),
            "once cloned, removing one frees nothing"
        );
        assert_eq!(fsutil::private_size(&clone), Some(0));
    }

    #[test]
    fn a_whole_family_of_clones_frees_its_blocks_only_once() {
        let fx = Fixture::new();
        let content = weights(10, 300_000);
        // Three alone-standing families: {a, a2, a3} is largest, so {b, b2} gets replaced.
        let a = fx.write(".lmstudio/models/org/a/m.gguf", &content);
        clone_of(&a, &fx.home.join(".lmstudio/models/org/a2/m.gguf"));
        clone_of(&a, &fx.home.join(".lmstudio/models/org/a3/m.gguf"));
        let b = fx.write(".lmstudio/models/org/b/m.gguf", &content);
        let b2 = fx.home.join(".lmstudio/models/org/b2/m.gguf");
        clone_of(&b, &b2);

        let ctx = fx.ctx();
        let snap = Snapshot::build_with(&ctx, BIG_BUDGET, Some(no_runtimes()));
        let report = duplicates::find(&ctx, &snap.entries);
        let group = &report.groups[0];
        assert_eq!(group.reclaimable, fs::metadata(&b).unwrap().blocks() * 512);
        let Action::CloneDedupe { duplicates, .. } = &report.findings[0].action else {
            panic!("expected a clone action");
        };
        assert_eq!(*duplicates, vec![b, b2]);
    }

    #[test]
    fn paths_the_policy_refuses_are_never_offered() {
        let fx = Fixture::new();
        let content = weights(11, 300_000);
        let a = fx.write(".lmstudio/models/org/a/m.gguf", &content);
        let excluded = fx.write(".lmstudio/models/org/b/m.gguf", &content);
        let mut ctx = fx.ctx();
        ctx.config.excluded_paths = vec![excluded.parent().unwrap().to_path_buf()];
        let snap = Snapshot::build_with(&ctx, BIG_BUDGET, Some(no_runtimes()));
        let report = duplicates::find(&ctx, &snap.entries);
        let group = &report.groups[0];
        assert_eq!(
            group.reclaimable, 0,
            "the excluded one can't be replaced nor be the original"
        );
        assert_eq!(group.keep.as_ref(), Some(&a));
        assert!(report.findings.is_empty());
    }

    #[test]
    fn the_deduplication_finding_really_works_through_the_cleaner() {
        let fx = Fixture::new();
        let content = weights(12, 400_000);
        let paths = four_copies(&fx, &content);
        // Make a resolved snapshot link observable: this is how HF reaches the store file.
        let through_snapshot = fx
            .home
            .join(".cache/huggingface/hub/models--org--qwen-GGUF/snapshots/aaa/qwen.gguf");

        let ctx = fx.ctx();
        let snap = Snapshot::build_with(&ctx, BIG_BUDGET, Some(no_runtimes()));
        let report = duplicates::find(&ctx, &snap.entries);
        let finding = report.findings.first().unwrap().clone();
        assert_eq!(finding.risk, Risk::Safe);
        assert!(finding.selected_by_default);
        let predicted = finding.size;

        let cleaner = Cleaner::new(&fx.home, &[], CleanOptions::default());
        let outcome = cleaner
            .clean(std::slice::from_ref(&finding), &CancelToken::new(), &|_| {})
            .outcomes
            .remove(0);
        assert_eq!(outcome.status, CleanStatus::Done, "{:?}", outcome.errors);
        assert_eq!(outcome.freed_bytes, predicted);

        for path in &paths {
            assert_eq!(fs::read(path).unwrap(), content, "{}", path.display());
            assert!(!fs::symlink_metadata(path).unwrap().file_type().is_symlink());
        }
        assert_eq!(
            fs::read(&through_snapshot).unwrap(),
            content,
            "the snapshot link still resolves"
        );

        let ids: HashSet<_> = paths
            .iter()
            .map(|path| fsutil::clone_id(path).unwrap())
            .collect();
        assert_eq!(ids.len(), 1, "all of them share blocks now");

        // The blob stays read-only, as Hugging Face expects, and everything is still found.
        assert!(fs::metadata(&paths[1]).unwrap().permissions().readonly());
        let rescanned = inventory(&fx);
        assert_eq!(rescanned.models.len(), 4);
        assert_eq!(rescanned.duplicates[0].reclaimable, 0);
        assert_eq!(rescanned.reclaimable_bytes, 0);
        assert_eq!(
            rescanned.total_bytes,
            snap.total_bytes - predicted,
            "what was promised is what the disk gained"
        );

        // A second run has nothing left to do.
        let again = cleaner
            .clean(&[finding], &CancelToken::new(), &|_| {})
            .outcomes
            .remove(0);
        assert_eq!(again.status, CleanStatus::Skipped);
    }

    #[test]
    fn every_supported_store_is_found_where_it_really_lives() {
        let fx = Fixture::new();
        let big = |seed| weights(seed, 2 * 1024 * 1024);
        let gguf = {
            let mut builder = crate::gguf::tests::Builder::default();
            builder
                .string("general.architecture", "llama")
                .u32("general.file_type", 15);
            builder.build(3)
        };
        fx.ollama_model("registry.ollama.ai/library/m/latest", &[("model", &big(1))]);
        fx.write(".lmstudio/models/org/gguf/m.gguf", &gguf);
        fx.write(".cache/lm-studio/models/org/legado/m.gguf", &gguf);
        let repo = fx.hf_repo("mlx-community/whisper-large-v3");
        repo.blob(&sha(&big(2)), &big(2));
        repo.snapshot("aaa", &[("weights.npz", &sha(&big(2)))]);
        repo.main("aaa");
        fx.write(".omlx/models/oq/config.json", b"{}");
        fx.write(".omlx/models/oq/model.safetensors", &big(3));
        fx.write(".cache/whisper/small.pt", &big(4));
        fx.write(".cache/whisper/ggml-base.bin", &big(5));
        fx.write("whisper.cpp/models/ggml-tiny.bin", &big(6));
        fx.write("whisper.cpp/models/for-tests-ggml-tiny.bin", b"pequeno");
        fx.write("Library/Caches/llama.cpp/org_repo_m.gguf", &big(7));
        fx.write("Library/Caches/llama.cpp/org_repo_m.gguf.etag", b"x");
        fx.write(
            "Library/Application Support/Jan/data/llamacpp/models/qwen/model.gguf",
            &gguf,
        );
        fx.write(
            "Library/Application Support/Jan/data/llamacpp/models/qwen/model.yml",
            b"x",
        );
        fx.write("jan/models/legado/model.gguf", &gguf);
        fx.write(
            "Library/Application Support/nomic.ai/GPT4All/mistral.gguf",
            &big(8),
        );
        fx.write(
            "Library/Application Support/nomic.ai/GPT4All/localdocs_v2.db",
            &big(9),
        );
        fx.write("ComfyUI/models/checkpoints/sd.safetensors", &big(10));
        fx.write("comfy/ComfyUI/models/vae/v.safetensors", &big(11));
        fx.write(
            "stable-diffusion-webui/models/Stable-diffusion/s.safetensors",
            &big(12),
        );
        fx.write(".cache/torch/hub/checkpoints/resnet.pth", &big(13));
        fx.write(
            ".cache/torch/sentence_transformers/all-mpnet/pytorch_model.bin",
            &big(14),
        );

        let inventory = inventory(&fx);
        let found: HashSet<(RuntimeKind, &str)> = inventory
            .models
            .iter()
            .map(|m| (m.runtime, m.name.as_str()))
            .collect();
        let expected: HashSet<(RuntimeKind, &str)> = [
            (RuntimeKind::Ollama, "m:latest"),
            (RuntimeKind::LmStudio, "org/gguf"),
            (RuntimeKind::LmStudio, "org/legado"),
            (RuntimeKind::HuggingFace, "mlx-community/whisper-large-v3"),
            (RuntimeKind::OMlx, "oq"),
            (RuntimeKind::Whisper, "whisper/small.pt"),
            (RuntimeKind::Whisper, "whisper/ggml-base.bin"),
            (RuntimeKind::Whisper, "whisper/ggml-tiny.bin"),
            (RuntimeKind::LlamaCpp, "org_repo_m.gguf"),
            (RuntimeKind::Jan, "qwen"),
            (RuntimeKind::Jan, "legado"),
            (RuntimeKind::Gpt4All, "mistral.gguf"),
            (RuntimeKind::ComfyUi, "checkpoints/sd.safetensors"),
            (RuntimeKind::ComfyUi, "vae/v.safetensors"),
            (RuntimeKind::Automatic1111, "Stable-diffusion/s.safetensors"),
            (RuntimeKind::TorchHub, "resnet.pth"),
            (RuntimeKind::SentenceTransformers, "all-mpnet"),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            found.difference(&expected).collect::<Vec<_>>(),
            Vec::<&(RuntimeKind, &str)>::new()
        );
        assert_eq!(
            expected.difference(&found).collect::<Vec<_>>(),
            Vec::<&(RuntimeKind, &str)>::new()
        );
        assert_eq!(
            model(&inventory, "mlx-community/whisper-large-v3").format,
            ModelFormat::Mlx
        );
        for row in &inventory.runtimes {
            if inventory.models.iter().any(|m| m.runtime == row.kind) {
                assert!(row.storage_bytes > 0, "{:?}", row.kind);
            }
        }
    }

    #[test]
    fn a_llama_cache_in_the_hugging_face_layout_is_read_as_one() {
        let fx = Fixture::new();
        let hub = fx.home.join("Library/Caches/llama.cpp");
        let content = weights(50, 8000);
        fx.write(
            &format!(
                "Library/Caches/llama.cpp/models--org--m/blobs/{}",
                sha(&content)
            ),
            &content,
        );
        let snapshot = hub.join("models--org--m/snapshots/aaa");
        fs::create_dir_all(&snapshot).unwrap();
        std::os::unix::fs::symlink(
            format!("../../blobs/{}", sha(&content)),
            snapshot.join("m.gguf"),
        )
        .unwrap();
        fx.write("Library/Caches/llama.cpp/models--org--m/refs/main", b"aaa");
        let inventory = inventory(&fx);
        let found = model(&inventory, "org/m");
        assert_eq!(found.runtime, RuntimeKind::LlamaCpp);
        assert_eq!(found.format, ModelFormat::Gguf);
        assert_eq!(found.id, "llama_cpp:org/m");
    }

    #[test]
    fn hashes_are_cached_and_reused_until_the_file_changes() {
        let fx = Fixture::new();
        let content = weights(13, 300_000);
        let a = fx.write(".lmstudio/models/org/a/m.gguf", &content);
        fx.write(".lmstudio/models/org/b/m.gguf", &content);

        assert_eq!(inventory(&fx).hashed_files, 2);
        assert!(crate::hashcache::cache_path(&fx.home).is_file());
        assert_eq!(inventory(&fx).hashed_files, 0, "served from the cache");

        fx.set_age(&a, 1);
        assert_eq!(inventory(&fx).hashed_files, 1, "only what changed");
    }

    #[test]
    fn only_the_hash_cache_is_written_by_a_scan() {
        fn tree(path: &Path, out: &mut Vec<(PathBuf, u64, i128)>) {
            let meta = fs::symlink_metadata(path).unwrap();
            let mtime = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
            out.push((path.to_path_buf(), meta.len(), mtime));
            if meta.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    tree(&entry.unwrap().path(), out);
                }
            }
        }
        let fx = Fixture::new();
        four_copies(&fx, &weights(14, 300_000));
        let mut before = Vec::new();
        tree(&fx.home, &mut before);
        inventory(&fx);
        let mut after = Vec::new();
        tree(&fx.home, &mut after);
        let caches = fx.home.join("Library");
        let outside = |entries: Vec<(PathBuf, u64, i128)>| {
            let mut kept: Vec<_> = entries
                .into_iter()
                .filter(|(path, ..)| !path.starts_with(&caches))
                .collect();
            kept.sort();
            // The home's own mtime changes when `Library` is created.
            kept.retain(|(path, ..)| *path != fx.home);
            kept
        };
        assert_eq!(outside(before), outside(after));
    }

    #[test]
    fn prompting_folders_are_not_read_without_full_disk_access_and_are_with_it() {
        let fx = Fixture::new();
        let big = weights(15, 2 * 1024 * 1024);
        fx.write(
            "Documents/ComfyUI/models/checkpoints/sdxl.safetensors",
            &big,
        );
        fx.write(
            "Library/Containers/com.liuliu.draw-things/Data/Documents/Models/sd.ckpt",
            &big,
        );
        fx.write("Applications/Draw Things.app/Contents/Info.plist", b"");
        fx.write("ComfyUI/models/checkpoints/en-casa.safetensors", &big);

        let gated = inventory(&fx);
        let names: Vec<_> = gated.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["checkpoints/en-casa.safetensors"]);
        assert_eq!(
            gated.skipped_roots,
            vec![fx
                .home
                .join("Library/Containers/com.liuliu.draw-things/Data/Documents/Models")]
        );

        // hm_core's probe: the folder exists and can be read.
        fs::create_dir_all(fx.home.join("Library/Safari")).unwrap();
        let open = inventory(&fx);
        assert_eq!(open.models.len(), 3, "{:?}", open.models);
        assert!(open.skipped_roots.is_empty());
        let draw = open
            .models
            .iter()
            .find(|m| m.runtime == RuntimeKind::DrawThings)
            .unwrap();
        assert_eq!(draw.format, ModelFormat::Unknown);
    }

    #[test]
    fn a_configured_root_in_a_prompting_place_is_reported_and_untouched() {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/settings.json",
            format!(
                r#"{{"downloadsFolder":"{}"}}"#,
                fx.home.join("Downloads/modelos").display()
            )
            .as_bytes(),
        );
        fx.write("Downloads/modelos/org/m/m.gguf", b"GGUF");
        let inventory = inventory(&fx);
        assert!(inventory.models.is_empty());
        assert_eq!(
            inventory.skipped_roots,
            vec![fx.home.join("Downloads/modelos")]
        );
    }

    #[test]
    fn omlx_pointed_at_other_stores_does_not_count_them_twice() {
        let fx = Fixture::new();
        let content = weights(16, 200_000);
        let repo = fx.hf_repo("org/m");
        let etag = sha(&content);
        repo.blob(&etag, &content);
        repo.snapshot("aaa", &[("config.json", &etag)]);
        repo.main("aaa");
        fx.write(".lmstudio/models/pub/lm/config.json", b"{}");
        fx.write(
            ".lmstudio/models/pub/lm/model.safetensors",
            &weights(17, 200_000),
        );
        fx.write(
            ".omlx/settings.json",
            format!(
                r#"{{"model":{{"model_dirs":["{}","{}"]}},"huggingface":{{"hf_cache_enabled":true}}}}"#,
                fx.home.join(".cache/huggingface/hub").display(),
                fx.home.join(".lmstudio/models").display()
            )
            .as_bytes(),
        );
        let inventory = inventory(&fx);
        let names: HashSet<_> = inventory.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(inventory.models.len(), 2, "{names:?}");
        assert!(inventory
            .models
            .iter()
            .all(|m| m.runtime != RuntimeKind::OMlx));
    }

    #[test]
    fn omlx_records_also_date_the_hugging_face_models_it_serves() {
        let fx = Fixture::new();
        let repo = fx.hf_repo("org/m");
        let content = weights(18, 5000);
        let etag = sha(&content);
        repo.blob(&etag, &content);
        repo.snapshot("aaa", &[("m.safetensors", &etag)]);
        repo.main("aaa");
        let database = fx.home.join(".omlx/usage.sqlite3");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        let created = hm_core::exec::run(
            Path::new("/usr/bin/sqlite3"),
            &[
                database.to_str().unwrap(),
                "CREATE TABLE model_usage_hourly (timestamp_hour INTEGER, model_id TEXT, \
                 PRIMARY KEY (timestamp_hour, model_id)) WITHOUT ROWID; \
                 INSERT INTO model_usage_hourly VALUES (1700000000,'org--m');",
            ],
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(created.success());
        let inventory = inventory(&fx);
        let found = model(&inventory, "org/m");
        assert_eq!(found.last_used_source, UsageSignal::RuntimeRecord);
        assert_eq!(
            found.last_used,
            Some(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
    }

    #[test]
    fn modification_time_is_labelled_as_what_it_is() {
        let fx = Fixture::new();
        let path = fx.write(".lmstudio/models/org/m/m.gguf", b"GGUF");
        fx.set_age(&path, 10);
        let inventory = inventory(&fx);
        let found = model(&inventory, "org/m");
        assert_eq!(found.last_used_source, UsageSignal::Modified);
        assert_eq!(
            found.last_used,
            fs::metadata(&path).unwrap().modified().ok()
        );
    }

    #[test]
    fn loaded_models_are_marked_through_each_runtimes_naming() {
        let fx = Fixture::new();
        fx.ollama_model(
            "registry.ollama.ai/library/llama3/latest",
            &[("model", b"x")],
        );
        fx.write(
            ".lmstudio/models/lmstudio-community/Q-MLX-4bit/config.json",
            b"{}",
        );
        fx.write(
            ".lmstudio/models/lmstudio-community/Q-MLX-4bit/m.safetensors",
            &weights(19, 3000),
        );
        fx.write(".omlx/models/Q-oQ4/config.json", b"{}");
        fx.write(".omlx/models/Q-oQ4/m.safetensors", &weights(20, 3000));
        let repo = fx.hf_repo("org/hf");
        let etag = sha(b"hf");
        repo.blob(&etag, b"hf");
        repo.snapshot("aaa", &[("m.safetensors", &etag)]);
        repo.main("aaa");

        let ctx = fx.ctx();
        let roots = Roots::resolve(&ctx, Detail::Full);
        let usage = UsageRecords {
            lm_identifiers: HashMap::from([(
                "q-mlx".to_string(),
                "lmstudio-community/Q-MLX-4bit".to_string(),
            )]),
            ..UsageRecords::default()
        };
        let found = collect(&roots, &ctx, &usage);
        let mut entries = finish_models(found.models, &BIG_BUDGET);
        let loaded_row = |kind, names: &[&str]| RuntimeStatus {
            kind,
            installed: true,
            running: true,
            endpoint: None,
            version: None,
            loaded: names
                .iter()
                .map(|name| crate::types::LoadedModel {
                    name: name.to_string(),
                    memory_bytes: None,
                    gpu_memory_bytes: None,
                })
                .collect(),
            storage_path: None,
            storage_bytes: 0,
            loaded_count: None,
            loaded_memory_bytes: None,
        };
        let rows = vec![
            loaded_row(RuntimeKind::Ollama, &["llama3:latest"]),
            loaded_row(RuntimeKind::LmStudio, &["q-mlx"]),
            loaded_row(RuntimeKind::OMlx, &["Q-oQ4", "org--hf"]),
        ];
        apply_loaded(&mut entries, &rows, &usage, ctx.now);
        assert_eq!(entries.len(), 4);
        for (entry, _) in &entries {
            assert!(entry.loaded, "{}", entry.name);
            assert_eq!(entry.last_used_source, UsageSignal::LoadedNow);
            assert_eq!(entry.last_used, Some(ctx.now));
        }
    }

    #[test]
    fn fit_is_judged_on_the_logical_size() {
        let fx = Fixture::new();
        let path = fx.write(".lmstudio/models/org/enorme/m.gguf", b"GGUF");
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(1 << 40)
            .unwrap();
        let ctx = fx.ctx();
        let budget = MemoryBudget {
            total_ram: 48 << 30,
            gpu_limit: 36 << 30,
        };
        let snap = Snapshot::build_with(&ctx, budget, Some(no_runtimes()));
        assert_eq!(snap.entries[0].0.fit, ModelFit::TooLarge);
        assert!(snap.entries[0].0.size < 1 << 30, "sparse on disk");
    }

    #[test]
    fn broken_files_everywhere_never_fail_the_scan() {
        let fx = Fixture::new();
        fx.write(
            ".ollama/models/manifests/registry.ollama.ai/library/a/latest",
            b"\xff\xfe nope",
        );
        fx.write(
            ".ollama/models/manifests/registry.ollama.ai/library/b/latest",
            b"{}",
        );
        fx.write(".ollama/models/manifests/suelto", b"[]");
        fx.write(".ollama/models/blobs/no-es-un-blob", b"x");
        fx.write(".cache/huggingface/hub/models--/x", b"x");
        fx.write(
            ".cache/huggingface/hub/models--a--b",
            b"un archivo, no una carpeta",
        );
        fx.write(".cache/huggingface/hub/models--c--d/refs/main", b"\xff\xfe");
        fx.write(
            ".cache/huggingface/hub/blobs/.huggingface-shared-blobs",
            b"2\n",
        );
        std::os::unix::fs::symlink(
            "/no/existe",
            fx.home.join(".cache/huggingface/hub/models--e--f"),
        )
        .unwrap();
        fx.write(".lmstudio/models/org/m/m.gguf", b"GGUF esto no es");
        fx.write(".lmstudio/settings.json", b"\xff");
        fx.write(".lmstudio/.internal/model-data.json", b"{");
        std::os::unix::fs::symlink(
            fx.home.join(".lmstudio/models/bucle"),
            fx.home.join(".lmstudio/models/bucle"),
        )
        .unwrap();
        fx.write(".omlx/settings.json", b"[");
        fx.write(".omlx/usage.sqlite3", b"no es sqlite");
        std::os::unix::fs::symlink("/no/existe", fx.home.join(".omlx/models")).unwrap();
        let inventory = inventory(&fx);
        assert_eq!(inventory.models.len(), 1);
        assert_eq!(inventory.models[0].format, ModelFormat::Gguf);
    }

    #[test]
    fn a_cancelled_scan_returns_what_it_has_without_hanging() {
        let fx = Fixture::new();
        four_copies(&fx, &weights(21, 300_000));
        let ctx = fx.ctx();
        ctx.cancel.cancel();
        let snap = Snapshot::build_with(&ctx, BIG_BUDGET, Some(no_runtimes()));
        let report = duplicates::find(&ctx, &snap.entries);
        assert_eq!(report.hashed_files, 0);
    }

    #[test]
    fn removing_an_ollama_model_goes_through_ollama_and_counts_only_its_own_layers() {
        let fx = Fixture::new();
        let shared = weights(22, 60_000);
        let own = weights(23, 30_000);
        fx.ollama_model(
            "registry.ollama.ai/library/a/latest",
            &[("model", &shared), ("params", &own)],
        );
        fx.ollama_model("registry.ollama.ai/library/b/latest", &[("model", &shared)]);
        let inventory = inventory(&fx);
        let found = model(&inventory, "a:latest");
        let finding = model_removal_finding(found);
        assert_eq!(
            finding.action,
            Action::Command {
                program: "ollama".to_string(),
                args: vec!["rm".to_string(), "a:latest".to_string()],
            }
        );
        assert_eq!(finding.size, found.size - found.shared_bytes);
        assert!(finding.size > 0 && finding.size < found.size);
        assert_eq!(finding.risk, Risk::Moderate);
        assert_eq!(finding.recovery, Recovery::Redownload);
        assert!(!finding.selected_by_default);
    }

    #[test]
    fn an_ollama_name_that_looks_like_a_flag_is_refused() {
        let fx = Fixture::new();
        fx.ollama_model("registry.ollama.ai/library/m/latest", &[("model", b"x")]);
        let mut found = inventory(&fx).models.remove(0);
        found.name = "--force:latest".to_string();
        assert_eq!(model_removal_finding(&found).action, Action::ReportOnly);
        found.name = "a b:latest".to_string();
        assert_eq!(model_removal_finding(&found).action, Action::ReportOnly);
    }

    #[test]
    fn removing_a_hugging_face_model_takes_its_store_payloads_too() {
        let fx = Fixture::new();
        let repo = fx.hf_repo("org/m");
        let content = weights(24, 80_000);
        let etag = sha(&content);
        let payload = repo.store_blob(&etag, &"6".repeat(64), &content);
        repo.snapshot("aaa", &[("m.safetensors", &etag)]);
        repo.main("aaa");
        let inventory = inventory(&fx);
        let finding = model_removal_finding(model(&inventory, "org/m"));
        let Action::Trash { paths } = &finding.action else {
            panic!("expected trash");
        };
        assert!(paths.contains(&repo.dir().to_path_buf()));
        assert!(paths.contains(&payload));
        assert!(finding.size >= fs::metadata(&payload).unwrap().blocks() * 512);

        // Every target must pass the policy for the cleaner to act.
        let policy = hm_core::SafetyPolicy::new(&fx.home, &[]);
        for path in paths {
            assert_eq!(policy.check_removal(path), Ok(()), "{}", path.display());
        }
        let outcome = Cleaner::new(
            &fx.home,
            &[],
            CleanOptions {
                dry_run: true,
                ..CleanOptions::default()
            },
        )
        .clean(std::slice::from_ref(&finding), &CancelToken::new(), &|_| {})
        .outcomes
        .remove(0);
        assert_eq!(
            outcome.status,
            CleanStatus::WouldClean,
            "{:?}",
            outcome.errors
        );
        assert!(payload.exists());
    }

    #[test]
    fn removing_from_lm_studio_takes_the_folder_or_only_the_right_quantization() {
        let fx = Fixture::new();
        fx.write(".lmstudio/models/org/one/m.gguf", b"GGUF");
        fx.write(".lmstudio/models/org/one/notas.txt", b"hola");
        fx.write(".lmstudio/models/org/two/a.gguf", b"GGUF a");
        fx.write(".lmstudio/models/org/two/b.gguf", b"GGUF b");
        let projector = fx.write(".lmstudio/models/org/two/mmproj-f16.gguf", b"GGUF p");
        let inventory = inventory(&fx);

        let one = model_removal_finding(model(&inventory, "org/one"));
        assert_eq!(
            one.action,
            Action::Trash {
                paths: vec![fx.home.join(".lmstudio/models/org/one")]
            }
        );
        assert!(one.blocked_by.contains(&"LM Studio".to_string()));

        let variant = model_removal_finding(model(&inventory, "org/two/a.gguf"));
        let Action::Trash { paths } = &variant.action else {
            panic!("expected trash");
        };
        assert_eq!(
            *paths,
            vec![fx.home.join(".lmstudio/models/org/two/a.gguf")]
        );
        assert!(!paths.contains(&projector));
    }

    #[test]
    fn removal_through_a_link_does_not_reach_the_file_it_points_at() {
        let fx = Fixture::new();
        let blob = fx.write(
            ".cache/huggingface/hub/models--o--n/blobs/abc",
            &weights(25, 8000),
        );
        let link = fx.home.join(".lmstudio/models/org/enlace/a.gguf");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&blob, &link).unwrap();
        fx.write(".lmstudio/models/org/enlace/b.gguf", b"GGUF b");
        let inventory = inventory(&fx);
        let linked = model_removal_finding(model(&inventory, "org/enlace/a.gguf"));
        let Action::Trash { paths } = &linked.action else {
            panic!("expected trash");
        };
        assert!(!paths.contains(&blob));
    }

    #[test]
    fn removing_a_linked_model_folder_takes_only_the_link() {
        let fx = Fixture::new();
        let real = fx.write("elsewhere/m/m.gguf", &weights(28, 8000));
        fx.write("elsewhere/m/otra.gguf", &weights(29, 8000));
        let link = fx.home.join(".lmstudio/models/org/enlazado");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(real.parent().unwrap(), &link).unwrap();
        let inventory = inventory(&fx);
        let finding = model_removal_finding(model(&inventory, "org/enlazado/m.gguf"));
        assert_eq!(finding.action, Action::Trash { paths: vec![link] });
        assert!(
            finding.size < 8000,
            "removing a link frees next to nothing: {}",
            finding.size
        );
    }

    #[test]
    fn a_folder_that_cannot_be_fully_read_never_widens_a_removal() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new();
        let mine = fx.write(".lmstudio/models/org/m/a.gguf", b"GGUF a");
        fx.write(".lmstudio/models/org/m/b.gguf", b"GGUF b");
        let inventory = inventory(&fx);
        let found = model(&inventory, "org/m/a.gguf").clone();

        let locked = fx.home.join(".lmstudio/models/org/m/cerrada");
        fs::create_dir_all(&locked).unwrap();
        fs::remove_file(fx.home.join(".lmstudio/models/org/m/b.gguf")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let finding = model_removal_finding(&found);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(finding.action, Action::Trash { paths: vec![mine] });
    }

    #[test]
    fn a_download_date_never_appears_as_when_a_model_was_last_used() {
        let fx = Fixture::new();
        let path = fx.write(".lmstudio/models/org/m/m.gguf", b"GGUF");
        fx.set_age(&path, 100);
        let inventory = inventory(&fx);
        let found = model(&inventory, "org/m");
        assert!(found.last_used.is_some());
        assert_eq!(model_removal_finding(found).last_used, None);
    }

    #[test]
    fn stray_ggufs_in_the_cache_folder_are_found_like_the_real_66_gb_one() {
        let fx = Fixture::new();
        fx.write(".cache/acme-mlx/Q2_0/m-00001-of-00002.gguf", b"GGUF uno");
        fx.write(".cache/acme-mlx/Q2_0/m-00002-of-00002.gguf", b"GGUF dos");
        fx.write(".cache/acme-mlx/tokenizer/tokenizer.json", b"{}");
        fx.write(".cache/otra/m.safetensors", b"no se puede saber que es");
        fx.write(".cache/uv/no-entrar/m.gguf", b"GGUF");
        let inventory = inventory(&fx);
        assert_eq!(inventory.models.len(), 1, "{:?}", inventory.models);
        let found = &inventory.models[0];
        assert_eq!(found.name, "acme-mlx/Q2_0");
        assert_eq!(found.runtime, RuntimeKind::Other);
        assert_eq!(found.files.len(), 2);

        let finding = model_removal_finding(found);
        assert_eq!(
            finding.risk,
            Risk::Review,
            "nobody knows how this comes back"
        );
        assert_eq!(finding.recovery, Recovery::None);
        assert!(!finding.selected_by_default);
    }

    #[test]
    fn known_stores_inside_the_cache_folder_are_not_counted_again() {
        let fx = Fixture::new();
        fx.write(".cache/whisper/ggml.gguf", &weights(30, 2 * 1024 * 1024));
        fx.write(".cache/lm-studio/models/org/m/m.gguf", &weights(31, 8000));
        let names: HashSet<_> = inventory(&fx)
            .models
            .into_iter()
            .map(|m| (m.runtime, m.name))
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names
            .iter()
            .all(|(runtime, _)| *runtime != RuntimeKind::Other));
    }

    #[test]
    fn flat_models_are_removed_one_file_at_a_time() {
        let fx = Fixture::new();
        let big = weights(26, 2 * 1024 * 1024);
        let file = fx.write(".cache/whisper/small.pt", &big);
        fx.write(".cache/whisper/tiny.pt", &weights(27, 2 * 1024 * 1024));
        let inventory = inventory(&fx);
        let finding = model_removal_finding(model(&inventory, "whisper/small.pt"));
        assert_eq!(finding.action, Action::Trash { paths: vec![file] });
    }
}
