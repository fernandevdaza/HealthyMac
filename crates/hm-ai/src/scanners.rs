//! Junk scanners for the Local AI area.
//!
//! All of them describe a single scan of the disk, shared by way of [`Shared`]: `run_scan` calls
//! them in parallel, and walking every model store once per scanner would be silly. Identical
//! `ctx.home` and `ctx.now` mean the same scan.
//!
//! Scanners only describe. The one thing written is the hash cache, which rule 2 allows.
//!
//! **What's selected by default.** Only what comes back by itself, or loses nothing: deduplication,
//! regenerated caches, stale locks. Anything needing a download to get back isn't preselected, even
//! when it's clearly unused, per rule 4 and because models are large and slow to get back. Partial
//! downloads and orphans are one tick away and go to the Trash.
//!
//! **What's called stale.** Only models with a record of use from their own runtime, and none in 90
//! days. A model with no such record isn't flagged, however old its files. See the `usage` module.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::Result;
use hm_core::fmt::{human_age, human_size};
use hm_core::fsutil;
use hm_core::{
    t, tf, Action, Area, Finding, Recovery, Risk, SafetyPolicy, SafetyViolation, ScanContext,
    Scanner,
};

use crate::duplicates;
use crate::files::older_than;
use crate::fit;
use crate::inventory::{
    honest_last_used, measure_path, newest_weight_time, removal_plan, removal_risk,
    runtime_processes, Entry, Snapshot,
};
use crate::stores::huggingface::repo_removal;
use crate::stores::{Leftover, LeftoverKind, ABANDONED_MIN_DAYS};
use crate::types::{LocalModel, MemoryBudget, ModelFit, RuntimeKind, UsageSignal};

pub(crate) const STALE_DAYS: u64 = 90;
/// Below this a cache isn't worth a line in the list.
const MIN_CACHE_BYTES: u64 = 1024 * 1024;

#[derive(Default)]
struct Shared {
    current: Mutex<Option<(PathBuf, SystemTime, Arc<Snapshot>)>>,
    budget: Option<MemoryBudget>,
}

impl Shared {
    /// Holds the lock while building, so scanners running side by side wait for one walk. That's
    /// only sound because building doesn't use rayon: `run_scan` calls this on rayon workers, and one
    /// holding this lock could otherwise pick up another scanner's job while waiting for its own
    /// tasks, and then block forever on the lock it already holds. Keep rayon out of `Snapshot::build`.
    fn snapshot(&self, ctx: &ScanContext) -> Arc<Snapshot> {
        let mut current = match self.current.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some((home, now, snapshot)) = &*current {
            if *home == ctx.home && *now == ctx.now {
                return snapshot.clone();
            }
        }
        let budget = self.budget.clone().unwrap_or_else(fit::current_budget);
        let snapshot = Arc::new(Snapshot::build(ctx, budget));
        *current = Some((ctx.home.clone(), ctx.now, snapshot.clone()));
        snapshot
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Duplicates,
    Abandoned,
    Orphans,
    Stale,
    TooLarge,
    Broken,
    Caches,
    Protected,
}

impl Kind {
    /// In the language current when asked, not when the scanner was made.
    fn name(self) -> &'static str {
        match self {
            Kind::Duplicates => t!("Modelos duplicados", "Duplicate models"),
            Kind::Abandoned => t!("Descargas abandonadas", "Abandoned downloads"),
            Kind::Orphans => t!("Datos huérfanos de modelos", "Orphaned model data"),
            Kind::Stale => t!("Modelos sin usar", "Unused models"),
            Kind::TooLarge => t!(
                "Modelos que no caben en este Mac",
                "Models too big for this Mac"
            ),
            Kind::Broken => t!("Modelos incompletos", "Incomplete models"),
            Kind::Caches => t!("Cachés de IA", "AI caches"),
            Kind::Protected => t!(
                "Modelos en carpetas protegidas",
                "Models in protected folders"
            ),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Kind::Duplicates => t!(
                "Los mismos pesos guardados por varios programas. Se convierten en clones APFS sin romper ninguno.",
                "The same weights stored by several apps. They're turned into APFS clones without breaking any of them."
            ),
            Kind::Abandoned => t!(
                "Descargas de modelos a medias que llevan al menos una semana sin avanzar.",
                "Unfinished model downloads that haven't made progress in at least a week."
            ),
            Kind::Orphans => t!(
                "Capas y revisiones antiguas que ningún modelo instalado usa ya.",
                "Layers and old revisions that no installed model uses anymore."
            ),
            Kind::Stale => t!(
                "Modelos que su propio programa registra como sin cargar desde hace más de 90 días.",
                "Models their own app records as not loaded in more than 90 days."
            ),
            Kind::TooLarge => t!(
                "Modelos que necesitan más memoria de la que tiene este equipo.",
                "Models that need more memory than this Mac has."
            ),
            Kind::Broken => t!(
                "Modelos a los que les faltan archivos, de modo que su programa no puede cargarlos.",
                "Models with missing files, so their app can't load them."
            ),
            Kind::Caches => t!(
                "Datos intermedios y conjuntos de datos que las herramientas de IA guardan en disco.",
                "Intermediate data and datasets that AI tools keep on disk."
            ),
            Kind::Protected => t!(
                "Carpetas de modelos que solo se pueden revisar con Acceso total al disco.",
                "Model folders that can only be checked with Full Disk Access."
            ),
        }
    }
}

struct AiScanner {
    id: &'static str,
    kind: Kind,
    shared: Arc<Shared>,
}

impl Scanner for AiScanner {
    fn id(&self) -> &'static str {
        self.id
    }

    fn name(&self) -> &'static str {
        self.kind.name()
    }

    fn description(&self) -> &'static str {
        self.kind.description()
    }

    fn area(&self) -> Area {
        Area::LocalAi
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let snapshot = self.shared.snapshot(ctx);
        Ok(match self.kind {
            Kind::Duplicates => duplicates::find(ctx, &snapshot.entries).findings,
            Kind::Abandoned => abandoned(self.id, ctx, &snapshot),
            Kind::Orphans => orphans(self.id, ctx, &snapshot),
            Kind::Stale => stale(self.id, ctx, &snapshot),
            Kind::TooLarge => too_large(self.id, ctx, &snapshot),
            Kind::Broken => broken(self.id, ctx, &snapshot),
            Kind::Caches => caches(self.id, ctx, &snapshot),
            Kind::Protected => protected(self.id, &snapshot),
        })
    }
}

/// Every scanner this crate provides.
pub fn scanners() -> Vec<Box<dyn Scanner>> {
    scanners_sharing(Arc::new(Shared::default()))
}

fn scanners_sharing(shared: Arc<Shared>) -> Vec<Box<dyn Scanner>> {
    const LIST: [(&str, Kind); 8] = [
        (duplicates::SCANNER_ID, Kind::Duplicates),
        ("ai.abandoned_downloads", Kind::Abandoned),
        ("ai.orphans", Kind::Orphans),
        ("ai.stale_models", Kind::Stale),
        ("ai.too_large", Kind::TooLarge),
        ("ai.broken_models", Kind::Broken),
        ("ai.caches", Kind::Caches),
        ("ai.protected_storage", Kind::Protected),
    ];
    LIST.into_iter()
        .map(|(id, kind)| {
            Box::new(AiScanner {
                id,
                kind,
                shared: shared.clone(),
            }) as Box<dyn Scanner>
        })
        .collect()
}

/// The cleaner refuses anything the policy refuses, such as models on an external disk. There's no
/// point in offering that as an action, but the bytes are still worth knowing about.
fn guard(finding: Finding, ctx: &ScanContext) -> Finding {
    let targets: Vec<PathBuf> = match &finding.action {
        Action::Trash { paths } => paths.clone(),
        Action::TrashContents { dir } => vec![dir.clone()],
        _ => return finding,
    };
    guard_paths(finding, &targets, ctx)
}

/// `targets` are what the finding really touches. A tool's own command names no path for the cleaner
/// to check, so a scanner offering `ollama rm` has to pass the blobs that would go.
fn guard_paths(finding: Finding, targets: &[PathBuf], ctx: &ScanContext) -> Finding {
    let policy = SafetyPolicy::new(&ctx.home, &ctx.config.excluded_paths);
    let refused = targets.iter().any(|path| {
        !matches!(
            policy.check_removal(path),
            Ok(()) | Err(SafetyViolation::Missing(_))
        )
    });
    if !refused {
        return finding;
    }
    let explanation = tf!(
        "{} Está en una ubicación que HealthyMac no toca, así que tendrás que eliminarlo a mano.",
        "{} It's in a location HealthyMac doesn't touch, so you'll have to delete it yourself.",
        finding.explanation
    );
    finding
        .explanation(explanation)
        .action(Action::ReportOnly)
        .selected(false)
}

fn trash_finding(id: &str, key: &str, title: String, paths: Vec<PathBuf>) -> Finding {
    Finding::new(id, Area::LocalAi, key, title).action(Action::Trash { paths })
}

fn age_text(time: Option<SystemTime>, now: SystemTime) -> String {
    time.map(|time| human_age(time, now))
        .unwrap_or_else(|| t!("hace tiempo", "a while ago").to_string())
}

fn abandoned(id: &str, ctx: &ScanContext, snapshot: &Snapshot) -> Vec<Finding> {
    let mut findings: Vec<Finding> = snapshot
        .leftovers
        .iter()
        .filter(|leftover| leftover.kind == LeftoverKind::Partial)
        .map(|leftover| {
            let finding = trash_finding(
                id,
                &leftover.key,
                tf!(
                    "Descarga sin terminar: {}",
                    "Unfinished download: {}",
                    leftover.label
                ),
                leftover.paths.clone(),
            )
            .group(leftover.runtime.label())
            .subtitle(subtitle(&leftover.paths))
            .explanation(tf!(
                "Descarga de {} que no avanza desde {}. No sirve para nada hasta que se termine, y si la \
                 quitas, volver a descargar ese modelo empezará de cero.",
                "This {} download last made progress {}. It's no use until it finishes, and if you \
                 remove it, downloading that model again starts from scratch.",
                leftover.runtime.label(),
                age_text(leftover.newest, ctx.now),
            ))
            .size(leftover.bytes())
            .items(leftover.records.len() as u64)
            .last_used(leftover.newest)
            .risk(Risk::Moderate)
            .recovery(Recovery::Redownload)
            .selected(false);
            guard(finding, ctx)
        })
        .collect();

    if !snapshot.stale_locks.is_empty() {
        let measured = fsutil::measure_many(&snapshot.stale_locks, &ctx.cancel);
        let finding = trash_finding(
            id,
            "hf-locks",
            t!(
                "Bloqueos antiguos de Hugging Face",
                "Old Hugging Face lock files"
            )
            .to_string(),
            snapshot.stale_locks.clone(),
        )
        .group(RuntimeKind::HuggingFace.label())
        .subtitle(subtitle(&snapshot.stale_locks))
        .explanation(t!(
            "Archivos de bloqueo de descargas que terminaron hace tiempo. Están vacíos, nadie los está \
             usando y Hugging Face los crea de nuevo cuando le hacen falta.",
            "Lock files from downloads that finished long ago. They're empty, nothing is using them, \
             and Hugging Face creates them again when it needs them."
        ))
        .size(measured.bytes)
        .items(measured.files)
        .risk(Risk::Safe)
        .recovery(Recovery::Automatic)
        .selected(true);
        findings.push(guard(finding, ctx));
    }
    findings
}

fn subtitle(paths: &[PathBuf]) -> String {
    match paths {
        [only] => only.display().to_string(),
        [first, rest @ ..] => {
            let parent = first.parent().unwrap_or(first);
            tf!(
                "{} y {} más",
                "{} and {} more",
                parent.display(),
                rest.len()
            )
        }
        [] => String::new(),
    }
}

fn orphan_text(leftover: &Leftover) -> (String, String) {
    match leftover.kind {
        LeftoverKind::OllamaOrphan => (
            t!("Capas huérfanas de Ollama", "Orphaned Ollama layers").to_string(),
            t!(
                "Ningún modelo instalado las usa: no aparecen en ningún manifiesto. Ollama suele borrarlas al \
                 arrancar, salvo con OLLAMA_NOPRUNE o si llevaba tiempo sin abrirse.",
                "No installed model uses them: they aren't in any manifest. Ollama usually deletes them when \
                 it starts, unless OLLAMA_NOPRUNE is set or it hadn't been opened in a while."
            )
            .to_string(),
        ),
        LeftoverKind::HfUnreferenced => (
            tf!(
                "Datos sin usar de {}",
                "Unused data from {}",
                leftover.label
            ),
            t!(
                "Archivos descargados de este repositorio a los que no apunta ninguna revisión. Nada los \
                 encontrará ya: si algo los necesitara, se volverían a descargar.",
                "Files downloaded from this repository that no revision points to. Nothing will find them \
                 anymore: if something needed them, they would be downloaded again."
            )
            .to_string(),
        ),
        LeftoverKind::HfOldRevision => (
            tf!(
                "Revisiones antiguas de {}",
                "Old revisions of {}",
                leftover.label
            ),
            t!(
                "Versiones anteriores de este modelo, sustituidas por una más nueva. El modelo actual sigue \
                 intacto. Solo harían falta a un programa fijado a esa versión exacta, y se volverían a \
                 descargar.",
                "Earlier versions of this model, replaced by a newer one. The current model stays intact. \
                 Only an app pinned to that exact version would need them, and they would be downloaded \
                 again."
            )
            .to_string(),
        ),
        LeftoverKind::HfStoreOrphan => (
            t!(
                "Datos compartidos sin usar de Hugging Face",
                "Unused shared Hugging Face data"
            )
            .to_string(),
            t!(
                "Archivos del almacén compartido de la caché a los que no enlaza ningún modelo ni conjunto de \
                 datos. Quedan así cuando se borra un repositorio a mano.",
                "Files in the cache's shared store that no model or dataset links to. They're left behind \
                 when a repository is deleted by hand."
            )
            .to_string(),
        ),
        LeftoverKind::Partial => (leftover.label.clone(), String::new()),
    }
}

fn orphans(id: &str, ctx: &ScanContext, snapshot: &Snapshot) -> Vec<Finding> {
    snapshot
        .leftovers
        .iter()
        .filter(|leftover| leftover.kind != LeftoverKind::Partial)
        .map(|leftover| {
            let (title, explanation) = orphan_text(leftover);
            let kind_key = match leftover.kind {
                LeftoverKind::HfOldRevision => "revisions",
                LeftoverKind::HfUnreferenced => "unreferenced",
                _ => "blobs",
            };
            let blocked: &[&str] = if leftover.runtime == RuntimeKind::Ollama {
                &["ollama", "Ollama"]
            } else {
                &[]
            };
            let finding = trash_finding(
                id,
                format!("{kind_key}:{}", leftover.key).as_str(),
                title,
                leftover.paths.clone(),
            )
            .group(leftover.runtime.label())
            .subtitle(subtitle(&leftover.paths))
            .explanation(explanation)
            .size(leftover.bytes())
            .items(leftover.records.len() as u64)
            .last_used(leftover.newest)
            .risk(Risk::Moderate)
            .recovery(Recovery::Redownload)
            .selected(false)
            .blocked_by(blocked);
            guard(finding, ctx)
        })
        .collect()
}

fn model_finding(
    id: &str,
    ctx: &ScanContext,
    model: &LocalModel,
    explanation: String,
    risk: Risk,
) -> Finding {
    let plan = removal_plan(model);
    let (least_risk, recovery) = removal_risk(model.runtime);
    let finding = Finding::new(id, Area::LocalAi, &model.id, &model.name)
        .group(model.runtime.label())
        .subtitle(model.root.display().to_string())
        .explanation(format!("{explanation} {}", plan.explanation))
        .size(plan.size)
        .items(plan.items)
        .last_used(honest_last_used(model))
        .risk(risk.max(least_risk))
        .recovery(recovery)
        .selected(false)
        .action(plan.action)
        .blocked_by(runtime_processes(model.runtime));
    if matches!(finding.action, Action::Command { .. }) {
        guard_paths(finding, &model.files, ctx)
    } else {
        guard(finding, ctx)
    }
}

/// A record is required. The files must be old as well, so a model downloaded again last week after
/// a long time unused isn't called forgotten.
fn is_stale((model, files): &Entry, now: SystemTime) -> bool {
    model.last_used_source == UsageSignal::RuntimeRecord
        && !model.loaded
        && !model.incomplete
        && model.fit != ModelFit::TooLarge
        && older_than(model.last_used, now, STALE_DAYS)
        && older_than(newest_weight_time(files), now, STALE_DAYS)
}

fn stale(id: &str, ctx: &ScanContext, snapshot: &Snapshot) -> Vec<Finding> {
    snapshot
        .entries
        .iter()
        .filter(|entry| is_stale(entry, ctx.now))
        .map(|(model, _)| {
            let explanation = tf!(
                "{} registra su último uso {}.",
                "{} last recorded using it {}.",
                usage_source(model),
                age_text(model.last_used, ctx.now)
            );
            model_finding(id, ctx, model, explanation, Risk::Moderate)
        })
        .collect()
}

/// Who actually wrote the record: oMLX is what dates Hugging Face models.
fn usage_source(model: &LocalModel) -> &'static str {
    match model.runtime {
        RuntimeKind::HuggingFace => RuntimeKind::OMlx.label(),
        runtime => runtime.label(),
    }
}

fn too_large(id: &str, ctx: &ScanContext, snapshot: &Snapshot) -> Vec<Finding> {
    snapshot
        .entries
        .iter()
        .filter(|(model, _)| model.fit == ModelFit::TooLarge && !model.incomplete && !model.loaded)
        .map(|(model, files)| {
            let logical: u64 = files.iter().map(|file| file.len).sum();
            let explanation = tf!(
                "Necesita unos {} de memoria y este Mac tiene {}, así que no se puede cargar aquí.",
                "It needs about {} of memory and this Mac has {}, so it can't be loaded here.",
                human_size(fit::estimated_memory(logical)),
                human_size(snapshot.budget.total_ram),
            );
            model_finding(id, ctx, model, explanation, Risk::Moderate)
        })
        .collect()
}

fn broken(id: &str, ctx: &ScanContext, snapshot: &Snapshot) -> Vec<Finding> {
    snapshot
        .entries
        .iter()
        // A download in progress is incomplete too, and is nobody's problem yet.
        .filter(|(model, files)| {
            model.incomplete && !model.loaded && older_than(newest_weight_time(files), ctx.now, ABANDONED_MIN_DAYS)
        })
        .map(|(model, _)| {
            let explanation = tf!(
                "A esta copia le faltan archivos, de modo que {} no puede cargarla. Puede ser una descarga o \
                 una conversión interrumpida.",
                "This copy is missing files, so {} can't load it. It may be an interrupted download or \
                 conversion.",
                model.runtime.label()
            );
            model_finding(id, ctx, model, explanation, Risk::Review)
        })
        .collect()
}

fn dir_finding(
    id: &str,
    key: &str,
    ctx: &ScanContext,
    dir: &Path,
    title: &str,
    explanation: &str,
) -> Option<Finding> {
    let info = measure_path(dir, ctx);
    (info.bytes >= MIN_CACHE_BYTES).then(|| {
        Finding::new(id, Area::LocalAi, key, title)
            .subtitle(dir.display().to_string())
            .explanation(explanation)
            .size(info.exclusive())
            .shared_bytes(info.shared)
            .items(info.files)
            .last_used(info.newest_modified)
            .action(Action::TrashContents {
                dir: dir.to_path_buf(),
            })
    })
}

/// oMLX's on-disk prefix cache grows into tens of gigabytes, far beyond most of what it keeps. It
/// rebuilds as prompts come in, so all it costs is speed the first time. `response-state` is left
/// out: it's the state of stored API responses, and that doesn't come back.
fn omlx_cache(id: &str, ctx: &ScanContext, snapshot: &Snapshot) -> Option<Finding> {
    let cache = &snapshot.roots.omlx.ssd_cache;
    if !snapshot.roots.allowed_quietly(cache) {
        return None;
    }
    let paths: Vec<PathBuf> = fsutil::children(cache)
        .into_iter()
        .filter(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            name != "response-state" && !name.starts_with('.')
        })
        .collect();
    if paths.is_empty() {
        return None;
    }
    let info = fsutil::measure_many(&paths, &ctx.cancel);
    (info.bytes >= MIN_CACHE_BYTES).then(|| {
        let title = t!("Caché en disco de oMLX", "oMLX disk cache").to_string();
        let finding = trash_finding(id, "omlx-ssd", title, paths)
            .group(RuntimeKind::OMlx.label())
            .subtitle(cache.display().to_string())
            .explanation(t!(
                "Contextos ya procesados que oMLX guarda para responder antes a conversaciones repetidas. \
                 Se regeneran solos: lo único que notarás es que los primeros mensajes tardan más. No \
                 se toca mientras oMLX esté abierto.",
                "Already processed contexts that oMLX keeps to answer repeated conversations faster. \
                 They're rebuilt automatically: all you'll notice is that the first messages take longer. \
                 Left alone while oMLX is open."
            ))
            .size(info.exclusive())
            .shared_bytes(info.shared)
            .items(info.files)
            .last_used(info.newest_modified)
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .selected(true)
            .blocked_by(runtime_processes(RuntimeKind::OMlx));
        guard(finding, ctx)
    })
}

fn caches(id: &str, ctx: &ScanContext, snapshot: &Snapshot) -> Vec<Finding> {
    let roots = &snapshot.roots;
    let mut findings = Vec::new();
    findings.extend(omlx_cache(id, ctx, snapshot));

    if roots.allowed_quietly(&roots.datasets_cache) {
        findings.extend(
            dir_finding(
                id,
                "hf-datasets",
                ctx,
                &roots.datasets_cache,
                t!(
                    "Caché de conjuntos de datos de Hugging Face",
                    "Hugging Face datasets cache"
                ),
                t!(
                    "Conjuntos de datos ya preparados para usar. Muchos se pueden volver a generar, pero algunos \
                     tardan horas o dejaron de estar disponibles, así que conviene revisarlo.",
                    "Datasets already prepared for use. Many can be generated again, but some take hours or \
                     are no longer available, so it's worth a look first."
                ),
            )
            .map(|finding| {
                guard(
                    finding
                        .group(RuntimeKind::HuggingFace.label())
                        .risk(Risk::Review)
                        .recovery(Recovery::Redownload)
                        .selected(false),
                    ctx,
                )
            }),
        );
    }

    if !snapshot.dataset_repos.is_empty() {
        let mut paths = Vec::new();
        let mut bytes = 0;
        for repo in &snapshot.dataset_repos {
            let (repo_paths, repo_bytes) = repo_removal(repo);
            paths.extend(repo_paths);
            bytes += repo_bytes;
        }
        if bytes >= MIN_CACHE_BYTES {
            let count = snapshot.dataset_repos.len();
            let title = t!("Conjuntos de datos descargados", "Downloaded datasets").to_string();
            let explanation = if count == 1 {
                tf!(
                    "{count} conjuntos de datos en la caché de modelos de Hugging Face. Se pueden volver a \
                     descargar, salvo si el original ya no está publicado.",
                    "1 dataset in the Hugging Face model cache. It can be downloaded again, unless the \
                     original is no longer published."
                )
            } else {
                tf!(
                    "{count} conjuntos de datos en la caché de modelos de Hugging Face. Se pueden volver a \
                     descargar, salvo si el original ya no está publicado.",
                    "{count} datasets in the Hugging Face model cache. They can be downloaded again, unless \
                     the original is no longer published."
                )
            };
            let finding = trash_finding(id, "hf-dataset-repos", title, paths)
                .group(RuntimeKind::HuggingFace.label())
                .subtitle(roots.hub.display().to_string())
                .explanation(explanation)
                .size(bytes)
                .items(count as u64)
                .risk(Risk::Review)
                .recovery(Recovery::Redownload)
                .selected(false);
            findings.push(guard(finding, ctx));
        }
    }

    if roots.allowed_quietly(&roots.xet_cache) {
        findings.extend(
            dir_finding(
                id,
                "hf-xet",
                ctx,
                &roots.xet_cache,
                t!(
                    "Caché de transferencias de Hugging Face",
                    "Hugging Face transfer cache"
                ),
                t!(
                    "Fragmentos que Hugging Face guarda para acelerar las descargas. Se regeneran solos.",
                    "Chunks Hugging Face keeps to speed up downloads. They're rebuilt automatically."
                ),
            )
            .map(|finding| {
                guard(
                    finding
                        .group(RuntimeKind::HuggingFace.label())
                        .risk(Risk::Safe)
                        .recovery(Recovery::Automatic)
                        .selected(true),
                    ctx,
                )
            }),
        );
    }

    let torch_hub = roots.torch_home.join("hub");
    if roots.allowed_quietly(&torch_hub) {
        findings.extend(
            dir_finding(
                id,
                "torch-hub",
                ctx,
                &torch_hub,
                t!("Caché de PyTorch Hub", "PyTorch Hub cache"),
                t!(
                    "Pesos y código que PyTorch descarga cuando un programa los pide. Se vuelven a descargar la \
                     próxima vez.",
                    "Weights and code PyTorch downloads when an app asks for them. They're downloaded again \
                     the next time."
                ),
            )
            .map(|finding| {
                guard(
                    finding
                        .group(RuntimeKind::TorchHub.label())
                        .risk(Risk::Moderate)
                        .recovery(Recovery::Redownload)
                        .selected(false),
                    ctx,
                )
            }),
        );
    }
    findings
}

/// Rule 6: one report-only finding saying what granting access would bring.
fn protected(id: &str, snapshot: &Snapshot) -> Vec<Finding> {
    let skipped = snapshot.roots.skipped();
    if skipped.is_empty() {
        return Vec::new();
    }
    let list = skipped
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let title = t!(
        "Hay modelos que no se han podido revisar",
        "Some models could not be checked"
    );
    vec![Finding::new(id, Area::LocalAi, "full-disk-access", title)
        .explanation(tf!(
            "Estas carpetas pueden tener modelos, pero entrar en ellas haría que macOS pidiera permiso en \
             mitad del análisis: {list}. Concede Acceso total al disco a HealthyMac para incluirlas en el \
             inventario y buscar duplicados también ahí.",
            "These folders may hold models, but opening them would make macOS ask for permission in the \
             middle of the scan: {list}. Give HealthyMac Full Disk Access to include them in the inventory \
             and look for duplicates there too."
        ))
        .items(skipped.len() as u64)
        .tag(hm_core::TAG_NEEDS_FULL_DISK_ACCESS)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::tests::BIG_BUDGET;
    use crate::test_support::{sha, Fixture};
    use hm_core::{run_scan, CleanOptions, CleanStatus, Cleaner};
    use std::collections::HashSet;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, UNIX_EPOCH};

    fn testing_scanners(budget: MemoryBudget) -> Vec<Box<dyn Scanner>> {
        scanners_sharing(Arc::new(Shared {
            current: Mutex::new(None),
            budget: Some(budget),
        }))
    }

    fn scan_with(fx: &Fixture, budget: MemoryBudget) -> Vec<Finding> {
        let scanners = testing_scanners(budget);
        run_scan(&scanners, &fx.ctx(), &|event| {
            if let hm_core::ScanEvent::Failed { scanner_id, error } = event {
                panic!("{scanner_id}: {error}");
            }
        })
    }

    fn scan_all(fx: &Fixture) -> Vec<Finding> {
        scan_with(fx, BIG_BUDGET)
    }

    fn from(findings: &[Finding], scanner: &str) -> Vec<Finding> {
        findings
            .iter()
            .filter(|finding| finding.scanner_id == scanner)
            .cloned()
            .collect()
    }

    fn weights(seed: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|at| seed.wrapping_add((at % 251) as u8).wrapping_mul(3))
            .collect()
    }

    /// A little of everything that's flagged, on one fixture.
    fn busy_home(fx: &Fixture) {
        let content = weights(1, 300_000);
        let etag = sha(&content);
        fx.ollama_model(
            "registry.ollama.ai/library/qwen/latest",
            &[("model", &content)],
        );
        fx.ollama_blob(&weights(2, 9000));
        let repo = fx.hf_repo("org/qwen");
        let payload_etag = sha(&weights(3, 8000));
        repo.store_blob(&payload_etag, &"4".repeat(64), &weights(3, 8000));
        repo.blob(&etag, &content);
        repo.blob(&sha(b"sin usar"), &weights(5, 9000));
        repo.blob(
            &format!("{}.incomplete", sha(b"parcial")),
            &weights(6, 9000),
        );
        repo.snapshot(
            "aaa",
            &[("qwen.gguf", &etag), ("otros.safetensors", &payload_etag)],
        );
        repo.main("aaa");
        fx.write(
            ".cache/huggingface/hub/.locks/models--org--qwen/x.lock",
            b"",
        );
        fx.write(
            ".cache/huggingface/datasets/org___d/data.arrow",
            &weights(7, 2 * 1024 * 1024),
        );
        fx.write(
            ".cache/huggingface/xet/chunks/a",
            &weights(8, 2 * 1024 * 1024),
        );
        fx.write(
            ".cache/torch/hub/checkpoints/resnet.pth",
            &weights(9, 2 * 1024 * 1024),
        );
        fx.write(
            ".omlx/cache/a/entry.safetensors",
            &weights(10, 2 * 1024 * 1024),
        );
        fx.write(".omlx/cache/_gdn_sidecars/x", &weights(11, 2 * 1024 * 1024));
        fx.write(
            ".omlx/cache/response-state/keep.json",
            &weights(12, 2 * 1024 * 1024),
        );
        fx.write(
            ".lmstudio/models/org/roto/parts/model.layers.0.mlx.npz",
            &weights(13, 5000),
        );
        fx.write(".lmstudio/models/org/qwen/qwen.gguf", &content);
        fx.age_tree(".ollama", 30);
        fx.age_tree(".cache", 30);
        fx.age_tree(".lmstudio", 30);
    }

    #[test]
    fn every_id_is_namespaced_and_every_finding_obeys_the_rules() {
        let fx = Fixture::new();
        busy_home(&fx);
        let list = scanners();
        assert_eq!(
            list.iter()
                .map(|scanner| scanner.id())
                .collect::<HashSet<_>>()
                .len(),
            list.len()
        );
        for scanner in &list {
            assert!(scanner.id().starts_with("ai."), "{}", scanner.id());
            assert_eq!(scanner.area(), Area::LocalAi);
            assert!(!scanner.name().is_empty() && !scanner.description().is_empty());
        }

        let findings = scan_all(&fx);
        assert!(
            findings.len() >= 8,
            "{:#?}",
            findings.iter().map(|f| &f.id).collect::<Vec<_>>()
        );
        let ids: HashSet<_> = findings.iter().map(|finding| finding.id.clone()).collect();
        assert_eq!(ids.len(), findings.len(), "stable ids must be unique");
        for finding in &findings {
            assert!(finding.id.starts_with("ai."), "{}", finding.id);
            assert_eq!(finding.area, Area::LocalAi);
            assert!(
                !finding.title.is_empty() && !finding.explanation.is_empty(),
                "{}",
                finding.id
            );
            if finding.risk == Risk::Review {
                assert!(!finding.selected_by_default, "rule 4: {}", finding.id);
            }
            if finding.recovery == Recovery::Redownload || finding.recovery == Recovery::None {
                assert!(!finding.selected_by_default, "rule 4: {}", finding.id);
            }
            if finding.selected_by_default {
                assert_eq!(finding.risk, Risk::Safe, "{}", finding.id);
            }
        }
    }

    #[test]
    fn every_action_passes_the_cleaner_in_a_dry_run_and_nothing_moves() {
        let fx = Fixture::new();
        busy_home(&fx);
        let findings = scan_all(&fx);
        let cleaner = Cleaner::new(
            &fx.home,
            &[],
            CleanOptions {
                dry_run: true,
                ..CleanOptions::default()
            },
        );
        let report = cleaner.clean(&findings, &hm_core::CancelToken::new(), &|_| {});
        for outcome in &report.outcomes {
            assert!(
                matches!(
                    outcome.status,
                    CleanStatus::WouldClean | CleanStatus::Skipped
                ),
                "{}: {:?} {:?}",
                outcome.finding_id,
                outcome.status,
                outcome.errors
            );
        }
        assert_eq!(scan_all(&fx).len(), findings.len(), "still all there");
    }

    #[test]
    fn scanners_share_one_walk_per_scan_and_redo_it_for_the_next() {
        let fx = Fixture::new();
        let list = testing_scanners(BIG_BUDGET);
        let ctx = fx.ctx();
        run_scan(&list, &ctx, &|_| {});
        fx.write(".lmstudio/models/org/nuevo/m.gguf", &weights(1, 5000));
        fx.age_tree(".lmstudio", 30);
        let stale_scan = list
            .iter()
            .find(|scanner| scanner.id() == "ai.broken_models")
            .unwrap();
        // The same scan sees the same world, even with something new on disk.
        assert!(stale_scan.scan(&ctx).unwrap().is_empty());

        fx.write(
            ".lmstudio/models/org/roto/parts/x.mlx.npz",
            &weights(2, 5000),
        );
        fx.age_tree(".lmstudio", 30);
        let mut next = ctx.clone();
        next.now += Duration::from_secs(1);
        assert_eq!(stale_scan.scan(&next).unwrap().len(), 1);
    }

    #[test]
    fn deduplication_is_safe_selected_and_runs_for_real() {
        let fx = Fixture::new();
        busy_home(&fx);
        let findings = from(&scan_all(&fx), "ai.duplicates");
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.risk, Risk::Safe);
        assert!(finding.selected_by_default);
        assert!(finding.size > 0);
        assert!(matches!(finding.action, Action::CloneDedupe { .. }));
        assert!(
            finding.title.contains("(LM Studio)") && finding.title.contains("(Ollama)"),
            "{}",
            finding.title
        );
        let report = Cleaner::new(&fx.home, &[], CleanOptions::default()).clean(
            std::slice::from_ref(finding),
            &hm_core::CancelToken::new(),
            &|_| {},
        );
        assert_eq!(
            report.outcomes[0].status,
            CleanStatus::Done,
            "{:?}",
            report.outcomes[0].errors
        );
        assert!(from(&scan_all(&fx), "ai.duplicates").is_empty());
    }

    #[test]
    fn abandoned_downloads_are_offered_unselected_and_locks_selected() {
        let fx = Fixture::new();
        busy_home(&fx);
        let findings = from(&scan_all(&fx), "ai.abandoned_downloads");
        let partial = findings
            .iter()
            .find(|f| f.title.contains("org/qwen"))
            .unwrap();
        assert_eq!(partial.risk, Risk::Moderate);
        assert_eq!(partial.recovery, Recovery::Redownload);
        assert!(!partial.selected_by_default);
        assert!(partial.size >= 9000);

        let locks = findings
            .iter()
            .find(|f| f.id.ends_with("hf-locks"))
            .unwrap();
        assert_eq!(locks.risk, Risk::Safe);
        assert!(locks.selected_by_default);
    }

    #[test]
    fn recent_downloads_are_not_abandoned() {
        let fx = Fixture::new();
        let repo = fx.hf_repo("org/m");
        repo.blob(&format!("{}.incomplete", sha(b"x")), &weights(1, 9000));
        fx.write(".cache/huggingface/hub/.locks/models--org--m/x.lock", b"");
        assert!(from(&scan_all(&fx), "ai.abandoned_downloads").is_empty());
    }

    #[test]
    fn orphans_are_described_per_kind_and_never_selected() {
        let fx = Fixture::new();
        busy_home(&fx);
        let findings = from(&scan_all(&fx), "ai.orphans");
        assert!(findings.iter().any(|f| f.title.contains("Ollama")));
        assert!(findings.iter().any(|f| f.title.contains("org/qwen")));
        for finding in &findings {
            assert_eq!(finding.risk, Risk::Moderate);
            assert!(!finding.selected_by_default);
            assert!(finding.size > 0);
        }
        let ollama = findings
            .iter()
            .find(|f| f.title.contains("Ollama"))
            .unwrap();
        assert!(ollama.blocked_by.contains(&"ollama".to_string()));
    }

    fn model_data(fx: &Fixture, id: &str, when: SystemTime) {
        let ms = when.duration_since(UNIX_EPOCH).unwrap().as_millis();
        fx.write(
            ".lmstudio/.internal/model-data.json",
            format!(r#"{{"json":[["{id}",{{"lastLoadedTimestamp":{ms}}}]],"meta":{{}}}}"#)
                .as_bytes(),
        );
    }

    fn lm_model(fx: &Fixture, name: &str) {
        fx.write(&format!(".lmstudio/models/{name}/config.json"), b"{}");
        fx.write(
            &format!(".lmstudio/models/{name}/model.safetensors"),
            &weights(1, 5000),
        );
    }

    fn days_ago(days: u64) -> SystemTime {
        SystemTime::now() - Duration::from_secs(days * 86_400)
    }

    #[test]
    fn a_model_unused_for_three_months_by_its_own_record_is_stale() {
        let fx = Fixture::new();
        lm_model(&fx, "org/viejo");
        fx.age_tree(".lmstudio/models", 200);
        model_data(&fx, "org/viejo", days_ago(100));
        let findings = from(&scan_all(&fx), "ai.stale_models");
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.risk, Risk::Moderate);
        assert_eq!(finding.recovery, Recovery::Redownload);
        assert!(!finding.selected_by_default);
        assert!(matches!(finding.action, Action::Trash { .. }));
        assert!(finding
            .explanation
            .contains("LM Studio registra su último uso"));
    }

    #[test]
    fn no_record_means_no_claim_however_old_the_files_are() {
        let fx = Fixture::new();
        lm_model(&fx, "org/sin-registro");
        fx.write(".cache/whisper/small.pt", &weights(2, 2 * 1024 * 1024));
        let repo = fx.hf_repo("org/hf");
        let etag = sha(b"pesos");
        repo.blob(&etag, b"pesos");
        repo.snapshot("aaa", &[("m.safetensors", &etag)]);
        repo.main("aaa");
        fx.age_tree(".lmstudio", 2000);
        fx.age_tree(".cache", 2000);
        assert!(from(&scan_all(&fx), "ai.stale_models").is_empty());
    }

    #[test]
    fn recent_use_or_a_recent_download_is_not_stale() {
        let fx = Fixture::new();
        lm_model(&fx, "org/usado");
        lm_model(&fx, "org/redescargado");
        fx.age_tree(".lmstudio/models/org/usado", 200);
        model_data(&fx, "org/usado", days_ago(89));
        assert!(from(&scan_all(&fx), "ai.stale_models").is_empty());
        // Its files are fresh, though its last record is old.
        model_data(&fx, "org/redescargado", days_ago(300));
        assert!(from(&scan_all(&fx), "ai.stale_models").is_empty());
    }

    #[test]
    fn stale_ollama_models_use_ollama_rm() {
        let fx = Fixture::new();
        let content = weights(3, 5000);
        fx.ollama_model(
            "registry.ollama.ai/library/viejo/latest",
            &[("model", &content)],
        );
        fx.age_tree(".ollama", 200);
        fx.write(
            ".ollama/logs/server.log",
            format!(
                "time=2025-01-01T00:00:00Z level=INFO msg=\"starting llama server\" cmd=\"ollama runner --model /x/blobs/sha256-{}\"\n",
                sha(&content)
            )
            .as_bytes(),
        );
        let findings = from(&scan_all(&fx), "ai.stale_models");
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].action,
            Action::Command {
                program: "ollama".to_string(),
                args: vec!["rm".to_string(), "viejo:latest".to_string()],
            }
        );
    }

    #[test]
    fn models_too_large_for_this_machine_are_flagged_once_never_selected() {
        let fx = Fixture::new();
        lm_model(&fx, "org/enorme");
        fx.age_tree(".lmstudio/models", 200);
        model_data(&fx, "org/enorme", days_ago(100));
        let small = MemoryBudget {
            total_ram: 1024,
            gpu_limit: 512,
        };
        let findings = scan_with(&fx, small);
        let large = from(&findings, "ai.too_large");
        assert_eq!(large.len(), 1);
        assert!(!large[0].selected_by_default);
        assert_eq!(large[0].risk, Risk::Moderate);
        assert!(large[0].explanation.contains("no se puede cargar"));
        assert!(
            from(&findings, "ai.stale_models").is_empty(),
            "shown once, not twice"
        );
    }

    #[test]
    fn broken_models_get_a_review_finding_and_are_not_reported_as_anything_else() {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/models/org/roto/parts/model.layers.0.mlx.npz",
            &weights(1, 5000),
        );
        fx.age_tree(".lmstudio/models", 200);
        model_data(&fx, "org/roto", days_ago(100));
        let findings = scan_all(&fx);
        let broken = from(&findings, "ai.broken_models");
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].risk, Risk::Review);
        assert!(!broken[0].selected_by_default);
        assert!(from(&findings, "ai.stale_models").is_empty());
    }

    #[test]
    fn an_incomplete_model_still_being_written_is_not_called_broken() {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/models/org/bajando/m-00001-of-00002.gguf",
            &weights(1, 5000),
        );
        assert!(from(&scan_all(&fx), "ai.broken_models").is_empty());
        fx.age_tree(".lmstudio/models", 8);
        assert_eq!(from(&scan_all(&fx), "ai.broken_models").len(), 1);
    }

    #[test]
    fn a_model_from_an_unknown_tool_stays_at_review_whatever_flagged_it() {
        let fx = Fixture::new();
        fx.write(".cache/herramienta/m.gguf", &weights(1, 5000));
        let small = MemoryBudget {
            total_ram: 1024,
            gpu_limit: 512,
        };
        let large = from(&scan_with(&fx, small), "ai.too_large");
        assert_eq!(large[0].risk, Risk::Review);
        assert_eq!(large[0].recovery, Recovery::None);
        assert!(!large[0].selected_by_default);
        assert_eq!(large[0].last_used, None, "a download date isn't a use");
    }

    #[test]
    fn caches_have_the_right_risks_and_the_omlx_response_state_survives() {
        let fx = Fixture::new();
        busy_home(&fx);
        let findings = from(&scan_all(&fx), "ai.caches");

        let datasets = findings
            .iter()
            .find(|f| f.id.ends_with("hf-datasets"))
            .unwrap();
        assert_eq!(datasets.risk, Risk::Review);
        assert!(!datasets.selected_by_default);

        let torch = findings
            .iter()
            .find(|f| f.id.ends_with("torch-hub"))
            .unwrap();
        assert_eq!(torch.risk, Risk::Moderate);

        let xet = findings.iter().find(|f| f.id.ends_with("hf-xet")).unwrap();
        assert_eq!(xet.risk, Risk::Safe);
        assert!(xet.selected_by_default);

        let omlx = findings
            .iter()
            .find(|f| f.id.ends_with("omlx-ssd"))
            .unwrap();
        assert_eq!(omlx.risk, Risk::Safe);
        assert!(omlx.blocked_by.contains(&"oMLX".to_string()));
        let Action::Trash { paths } = &omlx.action else {
            panic!("expected trash");
        };
        assert_eq!(paths.len(), 2);
        assert!(!paths.iter().any(|path| path.ends_with("response-state")));
        assert!(omlx.size >= 4 * 1024 * 1024 && omlx.size < 5 * 1024 * 1024);
    }

    #[test]
    fn dataset_repositories_are_offered_for_review_with_their_store_payloads() {
        let fx = Fixture::new();
        let dataset = fx.hf_repo_kind("datasets", "org/d");
        let content = weights(4, 2 * 1024 * 1024);
        let etag = sha(&content);
        let payload = dataset.store_blob(&etag, &"5".repeat(64), &content);
        dataset.snapshot("aaa", &[("train.parquet", &etag)]);
        dataset.main("aaa");
        let findings = from(&scan_all(&fx), "ai.caches");
        let repos = findings
            .iter()
            .find(|f| f.id.ends_with("hf-dataset-repos"))
            .unwrap();
        assert_eq!(repos.risk, Risk::Review);
        assert!(repos.size >= 2 * 1024 * 1024);
        let Action::Trash { paths } = &repos.action else {
            panic!("expected trash");
        };
        assert!(paths.contains(&payload));
    }

    #[test]
    fn small_caches_are_not_worth_mentioning() {
        let fx = Fixture::new();
        fx.write(".cache/huggingface/datasets/x", b"poco");
        fx.write(".omlx/cache/a/x", b"poco");
        assert!(from(&scan_all(&fx), "ai.caches").is_empty());
    }

    #[test]
    fn exactly_one_report_only_finding_explains_what_access_would_bring() {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/settings.json",
            format!(
                r#"{{"downloadsFolder":"{}"}}"#,
                fx.home.join("Documents/m").display()
            )
            .as_bytes(),
        );
        fx.write("Applications/Draw Things.app/Contents/Info.plist", b"");
        fx.write("Applications/ComfyUI.app/Contents/Info.plist", b"");
        let findings = from(&scan_all(&fx), "ai.protected_storage");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].action, Action::ReportOnly);
        assert!(!findings[0].selected_by_default);
        assert!(findings[0].explanation.contains("Acceso total al disco"));
        assert_eq!(findings[0].item_count, 3);
    }

    #[test]
    fn nothing_gated_means_nothing_to_say() {
        let fx = Fixture::new();
        assert!(from(&scan_all(&fx), "ai.protected_storage").is_empty());
    }

    #[test]
    fn findings_outside_what_the_policy_allows_become_report_only() {
        let fx = Fixture::new();
        let finding = trash_finding(
            "t",
            "k",
            "t".to_string(),
            vec![fx.write(".ssh/id_rsa", b"k")],
        )
        .explanation("Algo.")
        .selected(true);
        let guarded = guard(finding, &fx.ctx());
        assert_eq!(guarded.action, Action::ReportOnly);
        assert!(!guarded.selected_by_default);
    }

    #[test]
    fn excluded_paths_are_not_offered() {
        let fx = Fixture::new();
        fx.ollama_blob(&weights(1, 9000));
        fx.ollama_model("registry.ollama.ai/library/m/latest", &[("model", b"x")]);
        fx.age_tree(".ollama", 30);
        let mut ctx = fx.ctx();
        ctx.config.excluded_paths = vec![fx.home.join(".ollama")];
        let scanners = testing_scanners(BIG_BUDGET);
        let findings = run_scan(&scanners, &ctx, &|_| {});
        let orphans = from(&findings, "ai.orphans");
        assert_eq!(orphans[0].action, Action::ReportOnly);
    }

    #[test]
    fn an_excluded_ollama_folder_is_not_reached_through_ollama_rm() {
        let fx = Fixture::new();
        let content = weights(3, 5000);
        fx.ollama_model(
            "registry.ollama.ai/library/viejo/latest",
            &[("model", &content)],
        );
        fx.age_tree(".ollama", 200);
        fx.write(
            ".ollama/logs/server.log",
            format!(
                "time=2025-01-01T00:00:00Z level=INFO msg=x cmd=\"--model /x/blobs/sha256-{}\"\n",
                sha(&content)
            )
            .as_bytes(),
        );
        let mut ctx = fx.ctx();
        ctx.config.excluded_paths = vec![fx.home.join(".ollama/models")];
        let scanners = testing_scanners(BIG_BUDGET);
        let stale = from(&run_scan(&scanners, &ctx, &|_| {}), "ai.stale_models");
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].action, Action::ReportOnly);
    }

    #[test]
    fn partial_downloads_of_same_named_models_get_their_own_ids() {
        let fx = Fixture::new();
        fx.write(
            ".lmstudio/models/org/m/downloading_a.gguf.part",
            &weights(1, 5000),
        );
        fx.write(
            ".cache/lm-studio/models/org/m/downloading_a.gguf.part",
            &weights(2, 5000),
        );
        fx.write(
            "ComfyUI/models/checkpoints/a.safetensors.tmp",
            &weights(3, 5000),
        );
        fx.write(
            "stable-diffusion-webui/models/VAE/a.safetensors.tmp",
            &weights(4, 5000),
        );
        fx.age_tree(".lmstudio", 30);
        fx.age_tree(".cache", 30);
        fx.age_tree("ComfyUI", 30);
        fx.age_tree("stable-diffusion-webui", 30);
        let findings = from(&scan_all(&fx), "ai.abandoned_downloads");
        assert_eq!(
            findings.len(),
            4,
            "{:#?}",
            findings.iter().map(|f| &f.id).collect::<Vec<_>>()
        );
        let ids: HashSet<_> = findings.iter().map(|f| &f.id).collect();
        assert_eq!(ids.len(), 4);
        assert!(
            findings.iter().all(|f| !f.title.ends_with(": ")),
            "a title with no name"
        );
    }

    #[test]
    fn real_sizes_match_the_disk() {
        let fx = Fixture::new();
        let blob = fx.ollama_blob(&weights(1, 90_000));
        fx.ollama_model("registry.ollama.ai/library/m/latest", &[("model", b"x")]);
        fx.age_tree(".ollama", 30);
        let orphans = from(&scan_all(&fx), "ai.orphans");
        assert_eq!(orphans[0].size, fs::metadata(&blob).unwrap().blocks() * 512);
    }
}
