//! `~/Library/Caches`: one finding per large cache, the small ones folded into one.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use hm_core::{
    fsutil, t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner, SizeInfo,
};
use rayon::prelude::*;

use super::policy;
use crate::gate::{self, name_of, DirListing};
use crate::inventory::{CacheOwner, InstalledApps, RunningProcesses};

const SCANNER_ID: &str = "sys.caches";
/// One vendor, several unrelated products, so each child is its own cache. `Google` holds Chrome,
/// which must be closed first, and Android Studio, which hm-dev owns. Offering the parent would
/// mislabel it and, since `dedupe_findings` drops nested targets, swallow hm-dev's finding.
const VENDOR_FOLDERS: &[&str] = &["Google"];

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// At or above this, a cache gets its own finding.
    pub own_finding: u64,
    /// At or above this, a cache is not preselected: regenerating it costs real time or data.
    pub big: u64,
    /// A cache whose app is gone must not join the aggregate, because `apps.leftovers` may claim it
    /// and `dedupe_findings` drops a whole finding over one shared path. Below this it is not worth
    /// a finding of its own.
    pub unowned_min: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            own_finding: 50_000_000,
            big: 1_000_000_000,
            unowned_min: 1_000_000,
        }
    }
}

pub(crate) struct UserCaches;

impl Scanner for UserCaches {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Cachés de usuario", "User caches")
    }
    fn description(&self) -> &'static str {
        t!(
            "Archivos temporales que las apps vuelven a generar cuando los necesitan.",
            "Temporary files that apps create again when they need them."
        )
    }
    fn area(&self) -> Area {
        Area::System
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let apps = InstalledApps::discover(ctx);
        let running = RunningProcesses::current();
        Ok(scan_caches(ctx, &apps, &running, Limits::default()))
    }
}

/// Caches this scanner leaves completely alone.
fn is_ignored(name: &str) -> bool {
    if name.starts_with('.')
        || policy::is_specialised_cache(name)
        || name.starts_with("Google/Android")
    {
        return true;
    }
    if policy::is_allowed_apple_cache(name) {
        return false;
    }
    // Not even measured: HealthyMac won't offer to clean these, so their size would answer nothing.
    policy::is_apple(name) || policy::is_sensitive(name)
}

struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    info: SizeInfo,
}

pub(crate) fn scan_caches(
    ctx: &ScanContext,
    apps: &InstalledApps,
    running: &RunningProcesses,
    limits: Limits,
) -> Vec<Finding> {
    let root = ctx.home_path("Library/Caches");
    let DirListing::Entries(paths) = gate::list_dir(&root) else {
        return Vec::new();
    };
    let mut named: Vec<(PathBuf, String)> = Vec::new();
    for path in paths {
        let name = name_of(&path);
        if VENDOR_FOLDERS.contains(&name.as_str()) && fsutil::is_real_dir(&path) {
            for child in fsutil::children(&path) {
                named.push((child.clone(), format!("{name}/{}", name_of(&child))));
            }
        } else {
            named.push((path, name));
        }
    }
    let entries: Vec<Entry> = named
        .par_iter()
        .filter_map(|(path, name)| measure_entry(ctx, path, name))
        .collect();

    let mut findings = Vec::new();
    let mut small: Vec<&Entry> = Vec::new();

    for entry in &entries {
        let owner = apps.owner_of(&entry.name);
        if entry.info.exclusive() >= limits.own_finding {
            findings.push(own_finding(entry, owner.as_ref(), limits));
            continue;
        }
        // Loose small files are not worth a sweep, and we cannot say whose they are. A folder made
        // entirely of clones of something else would free nothing.
        if !entry.is_dir || entry.info.exclusive() == 0 {
            continue;
        }
        let unowned_app = policy::is_reverse_dns(&entry.name) && !apps.covers(&entry.name);
        if unowned_app {
            if entry.info.exclusive() >= limits.unowned_min {
                findings.push(own_finding(entry, None, limits));
            }
            continue;
        }
        // Folding in a cache whose app is open would block the whole aggregate at clean time.
        let owner_running = owner
            .as_ref()
            .is_some_and(|owner| running.names.contains(&owner.process));
        if !owner_running {
            small.push(entry);
        }
    }

    if let Some(finding) = aggregate_finding(&root, &small, apps) {
        findings.push(finding);
    }
    findings
}

fn measure_entry(ctx: &ScanContext, path: &Path, name: &str) -> Option<Entry> {
    if ctx.is_cancelled() || gate::is_symlink(path) || gate::is_excluded(ctx, path) {
        return None;
    }
    if is_ignored(name) {
        return None;
    }
    let info = ctx.measure(path);
    (info.bytes > 0).then(|| Entry {
        path: path.to_path_buf(),
        name: name.to_string(),
        is_dir: fsutil::is_real_dir(path),
        info,
    })
}

fn own_finding(entry: &Entry, owner: Option<&CacheOwner>, limits: Limits) -> Finding {
    let display = owner.map_or(entry.name.as_str(), |owner| owner.display.as_str());
    let mut explanation = tf!(
        "Archivos temporales de {display}. Se regeneran solos cuando hacen falta; la primera vez \
         puede ir algo más lento.",
        "Temporary files from {display}. They regenerate on their own when needed; the first time \
         may be a little slower."
    );
    if entry.name.eq_ignore_ascii_case("com.apple.Safari") {
        explanation = t!(
            "Caché de red de Safari: lo mismo que borra «Vaciar cachés» en su menú \
             Desarrollo. Las páginas tardarán algo más en cargar la primera vez.",
            "Safari's network cache: the same thing “Empty Caches” clears in its Develop menu. \
             Pages will take a little longer to load the first time."
        )
        .to_string();
    }
    if owner.is_some() {
        explanation.push_str(&tf!(
            " Hay que cerrar {display} antes de limpiarla.",
            " Quit {display} before cleaning it."
        ));
    }
    let big = entry.info.exclusive() >= limits.big;
    let mut finding = Finding::new(
        SCANNER_ID,
        Area::System,
        &entry.name,
        tf!("Caché de {display}", "{display} cache"),
    )
    .trash(&entry.path)
    .measured(&entry.info);
    if big {
        explanation.push_str(t!(
            " Ocupa mucho, así que volver a generarla puede costar tiempo o datos. Por eso no va \
             preseleccionada.",
            " It is large, so rebuilding it can cost time or data. That is why it is not \
             preselected."
        ));
        finding = finding.risk(Risk::Moderate).recovery(Recovery::Redownload);
    } else {
        finding = finding
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .selected(true);
    }
    if entry.info.unreadable > 0 {
        explanation.push_str(t!(
            " Una parte no se pudo leer, así que puede ocupar más.",
            " Part of it could not be read, so it may take up more space."
        ));
    }
    if let Some(owner) = owner {
        finding = finding.blocked_by(&[owner.process.as_str()]);
    }
    finding.explanation(explanation)
}

fn aggregate_finding(root: &Path, small: &[&Entry], apps: &InstalledApps) -> Option<Finding> {
    if small.is_empty() {
        return None;
    }
    let mut blocked: Vec<String> = small
        .iter()
        .filter_map(|entry| apps.owner_of(&entry.name))
        .map(|owner| owner.process)
        .collect();
    blocked.sort();
    blocked.dedup();

    let paths: Vec<PathBuf> = small.iter().map(|entry| entry.path.clone()).collect();
    let bytes = small.iter().map(|entry| entry.info.exclusive()).sum();
    let shared = small.iter().map(|entry| entry.info.shared).sum();
    let files = small.iter().map(|entry| entry.info.files).sum();
    let newest: Option<SystemTime> = small
        .iter()
        .filter_map(|entry| entry.info.newest_modified)
        .max();

    let mut finding = Finding::new(
        SCANNER_ID,
        Area::System,
        "pequenas",
        tf!(
            "Cachés pequeñas de {} apps",
            "Small caches from {} apps",
            small.len()
        ),
    )
    .subtitle(root.display().to_string())
    .explanation(t!(
        "Cachés que ocupan poco por separado. Cada app las regenera cuando las necesita. Las de \
         apps abiertas ahora mismo se dejan para el próximo escaneo.",
        "Caches that are small on their own. Each app regenerates them when it needs them. Those \
         of apps that are open right now are left for the next scan."
    ))
    .size(bytes)
    .shared_bytes(shared)
    .items(files)
    .last_used(newest)
    .risk(Risk::Safe)
    .recovery(Recovery::Automatic)
    .selected(true)
    .action(Action::Trash { paths });
    finding.blocked_by = blocked;
    Some(finding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{find, targets, Fixture, LockedDir};

    const SMALL_LIMITS: Limits = Limits {
        own_finding: 100_000,
        big: 1_000_000,
        unowned_min: 20_000,
    };

    fn scan(home: &Fixture, apps: &InstalledApps) -> Vec<Finding> {
        scan_caches(&home.ctx, apps, &RunningProcesses::default(), SMALL_LIMITS)
    }

    fn apps_with(home: &Fixture, id: &str, process: &str) -> InstalledApps {
        home.app(&format!("Applications/{process}.app"), id, process);
        InstalledApps::from_roots(&[home.path("Applications")])
    }

    #[test]
    fn missing_or_unreadable_caches_folder_yields_nothing() {
        let home = Fixture::new();
        assert!(scan(&home, &InstalledApps::default()).is_empty());
        let caches = home.mkdir("Library/Caches");
        let _guard = LockedDir::new(&caches);
        assert!(scan(&home, &InstalledApps::default()).is_empty());
    }

    #[test]
    fn large_cache_gets_its_own_finding_and_blocks_on_its_app() {
        let home = Fixture::new();
        let apps = apps_with(&home, "com.example.browser", "Browser");
        let path = home.write("Library/Caches/com.example.browser/data.bin", 200_000);
        let findings = scan(&home, &apps);
        let finding = find(&findings, "com.example.browser").expect("finding");
        assert_eq!(finding.risk, Risk::Safe);
        assert!(finding.selected_by_default);
        assert_eq!(finding.blocked_by, ["Browser"]);
        assert_eq!(targets(finding), [path.parent().expect("parent")]);
        assert!(finding.size >= 200_000);
    }

    #[test]
    fn very_large_cache_is_moderate_and_not_preselected() {
        let home = Fixture::new();
        home.write("Library/Caches/SomeTool/data.bin", 1_100_000);
        let findings = scan(&home, &InstalledApps::default());
        let finding = find(&findings, "SomeTool").expect("finding");
        assert_eq!(finding.risk, Risk::Moderate);
        assert_eq!(finding.recovery, Recovery::Redownload);
        assert!(!finding.selected_by_default);
    }

    #[test]
    fn small_caches_fold_into_one_finding() {
        let home = Fixture::new();
        let apps = apps_with(&home, "com.example.one", "One");
        home.write("Library/Caches/com.example.one/a", 10_000);
        home.write("Library/Caches/SomeTool/b", 10_000);
        let findings = scan(&home, &apps);
        assert_eq!(findings.len(), 1);
        let finding = find(&findings, "pequenas").expect("aggregate");
        assert_eq!(targets(finding).len(), 2);
        assert_eq!(finding.blocked_by, ["One"]);
        assert!(finding.selected_by_default);
        assert_eq!(finding.item_count, 2);
    }

    #[test]
    fn aggregate_leaves_out_caches_of_running_apps() {
        let home = Fixture::new();
        let apps = apps_with(&home, "com.example.one", "One");
        home.write("Library/Caches/com.example.one/a", 10_000);
        home.write("Library/Caches/SomeTool/b", 10_000);
        let mut running = RunningProcesses::default();
        running.names.insert("One".to_string());
        let findings = scan_caches(&home.ctx, &apps, &running, SMALL_LIMITS);
        let finding = find(&findings, "pequenas").expect("aggregate");
        assert_eq!(targets(finding), [home.path("Library/Caches/SomeTool")]);
        assert!(finding.blocked_by.is_empty());
    }

    #[test]
    fn cache_of_an_uninstalled_app_never_joins_the_aggregate() {
        let home = Fixture::new();
        home.write("Library/Caches/com.gone.app/a", 30_000);
        home.write("Library/Caches/com.gone.tiny/a", 100);
        home.write("Library/Caches/SomeTool/b", 10_000);
        let findings = scan(&home, &InstalledApps::default());
        assert!(find(&findings, "com.gone.app").is_some());
        assert!(find(&findings, "com.gone.tiny").is_none());
        let aggregate = find(&findings, "pequenas").expect("aggregate");
        assert_eq!(targets(aggregate), [home.path("Library/Caches/SomeTool")]);
    }

    #[test]
    fn specialised_caches_are_left_to_their_scanners() {
        let home = Fixture::new();
        for name in [
            "Homebrew",
            "pip",
            "ms-playwright",
            "electron",
            "JetBrains",
            "llama.cpp",
            "huggingface",
            "HealthyMac",
            "pypoetry",
            "com.apple.dt.Xcode",
        ] {
            home.write(&format!("Library/Caches/{name}/data"), 200_000);
        }
        assert!(scan(&home, &InstalledApps::default()).is_empty());
    }

    #[test]
    fn apple_and_sensitive_caches_are_left_completely_alone() {
        let home = Fixture::new();
        for name in [
            "com.apple.bird",
            "com.apple.Music",
            "com.apple.iTunes",
            "com.apple.TV",
            "CloudKit",
            "com.apple.nsurlsessiond",
            "OneDrive",
            "com.1password.app",
        ] {
            home.write(&format!("Library/Caches/{name}/data"), 200_000);
        }
        assert!(scan(&home, &InstalledApps::default()).is_empty());
    }

    #[test]
    fn safari_is_the_one_apple_cache_offered_and_needs_safari_closed() {
        let home = Fixture::new();
        home.write("Library/Caches/com.apple.Safari/data", 200_000);
        let findings = scan(&home, &InstalledApps::default());
        let finding = find(&findings, "com.apple.Safari").expect("finding");
        assert_eq!(finding.blocked_by, ["Safari"]);
        assert!(find(&findings, "protegidas").is_none());
    }

    #[test]
    fn symlinks_and_user_exclusions_are_skipped() {
        let mut home = Fixture::new();
        let outside = home.write("elsewhere/big", 200_000);
        home.mkdir("Library/Caches");
        std::os::unix::fs::symlink(
            outside.parent().expect("parent"),
            home.path("Library/Caches/link"),
        )
        .expect("symlink");
        home.write("Library/Caches/Keep/data", 200_000);
        home.ctx.config.excluded_paths = vec![home.path("Library/Caches/Keep")];
        assert!(scan(&home, &InstalledApps::default()).is_empty());
    }

    #[test]
    fn unreadable_cache_inside_says_size_is_a_lower_bound() {
        let home = Fixture::new();
        home.write("Library/Caches/SomeTool/data", 200_000);
        let locked = home.mkdir("Library/Caches/SomeTool/private");
        let _guard = LockedDir::new(&locked);
        let findings = scan(&home, &InstalledApps::default());
        let finding = find(&findings, "SomeTool").expect("finding");
        assert!(finding.explanation.contains("no se pudo leer"));
    }

    #[test]
    fn google_is_split_by_product_and_android_studio_is_left_to_hm_dev() {
        let home = Fixture::new();
        let chrome = home.write("Library/Caches/Google/Chrome/Default/data", 200_000);
        home.write("Library/Caches/Google/AndroidStudio2026.1.2/index", 200_000);
        let findings = scan(&home, &InstalledApps::default());
        assert_eq!(findings.len(), 1);
        let finding = find(&findings, "Google/Chrome").expect("finding");
        assert_eq!(finding.title, "Caché de Google Chrome");
        assert_eq!(finding.blocked_by, ["Google Chrome"]);
        let expected = chrome.ancestors().nth(2).expect("ancestor").to_path_buf();
        assert_eq!(targets(finding), [expected]);
    }

    /// What pnpm, uv and bun leave behind: it looks big, but removing it would free next to nothing.
    #[test]
    fn a_cache_of_clones_is_sized_by_what_it_would_free() {
        let home = Fixture::new();
        let original = home.write("elsewhere/data.bin", 300_000);
        let clone = home.path("Library/Caches/SomeTool/data.bin");
        std::fs::create_dir_all(clone.parent().expect("parent")).expect("mkdir");
        std::fs::copy(&original, &clone).expect("copy");
        if fsutil::clone_id(&original).is_none()
            || fsutil::clone_id(&original) != fsutil::clone_id(&clone)
        {
            return;
        }
        // Not on its own and not in the aggregate: there is nothing to free.
        let findings = scan(&home, &InstalledApps::default());
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn aggregate_sizes_are_what_it_would_free() {
        let home = Fixture::new();
        home.write("Library/Caches/ToolA/a", 10_000);
        home.write("Library/Caches/ToolB/b", 10_000);
        let findings = scan(&home, &InstalledApps::default());
        let aggregate = find(&findings, "pequenas").expect("aggregate");
        assert!(aggregate.size >= 20_000);
        assert_eq!(aggregate.shared_bytes, 0);
    }

    #[test]
    fn loose_small_files_are_ignored() {
        let home = Fixture::new();
        home.write("Library/Caches/INSTALLATION_ID", 100);
        home.write("Library/Caches/.DS_Store", 100);
        assert!(scan(&home, &InstalledApps::default()).is_empty());
    }
}
