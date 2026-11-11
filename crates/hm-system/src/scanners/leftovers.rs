//! What uninstalled apps left behind.
//!
//! The one scanner where a false positive destroys data, since `Application Support` can hold
//! everything an app ever saved. Each check below only ever moves a candidate towards "keep":
//!
//! 1. The name must be a real bundle id, and not Apple's, a shared SDK's or anything sensitive.
//! 2. Nothing on this Mac may vouch for its vendor: apps, helpers, plug-ins, launch agents, running
//!    processes. If that list could not be read in full, nothing is called a leftover at all.
//! 3. A preferences file alone is not enough. Java apps and command-line tools write `<id>.plist`
//!    with no matching bundle: on the Mac this was developed on, three of the five leftovers that
//!    were only a plist were wrong (ZAP writing as `org.parosproxy.paros`, the Android emulator, and
//!    part of macOS), and each would have freed about 4 KB. All risk, no reward.
//! 4. Nothing may have written there in the last 30 days.
//! 5. Spotlight, asked about the vendor, must positively answer that nothing is there. If it
//!    stalls or is switched off, the answer is "keep".

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::Result;
use hm_core::fmt::{age_days, human_age};
use hm_core::{
    fsutil, t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner, SizeInfo,
};
use rayon::prelude::*;

use super::policy;
use crate::gate::{self, name_of};
use crate::inventory::{AppLocator, InstalledApps, Spotlight};

const SCANNER_ID: &str = "apps.leftovers";
const MIN_AGE_DAYS: u64 = 30;

/// What a leftover is, as told to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    ApplicationSupport,
    Cache,
    Preferences,
    SavedState,
    HttpStorage,
    WebKit,
}

impl Kind {
    /// As it reads in the middle of a sentence.
    fn label(self) -> &'static str {
        match self {
            Kind::ApplicationSupport => t!("soporte de aplicación", "application support"),
            Kind::Cache => t!("caché", "cache"),
            Kind::Preferences => t!("preferencias", "preferences"),
            Kind::SavedState => t!("estado guardado", "saved state"),
            Kind::HttpStorage => t!("almacenamiento http", "HTTP storage"),
            Kind::WebKit => t!("datos de webkit", "WebKit data"),
        }
    }
}

struct Location {
    relative: &'static str,
    kind: Kind,
    /// Removed from the name to get the bundle id. Empty means the name is the id.
    suffix: &'static str,
    /// Files rather than folders: preference plists and cookie stores.
    files: bool,
}

const LOCATIONS: &[Location] = &[
    Location {
        relative: "Library/Application Support",
        kind: Kind::ApplicationSupport,
        suffix: "",
        files: false,
    },
    Location {
        relative: "Library/Caches",
        kind: Kind::Cache,
        suffix: "",
        files: false,
    },
    Location {
        relative: "Library/Preferences",
        kind: Kind::Preferences,
        suffix: ".plist",
        files: true,
    },
    Location {
        relative: "Library/Saved Application State",
        kind: Kind::SavedState,
        suffix: ".savedState",
        files: false,
    },
    Location {
        relative: "Library/HTTPStorages",
        kind: Kind::HttpStorage,
        suffix: "",
        files: false,
    },
    Location {
        relative: "Library/HTTPStorages",
        kind: Kind::HttpStorage,
        suffix: ".binarycookies",
        files: true,
    },
    Location {
        relative: "Library/WebKit",
        kind: Kind::WebKit,
        suffix: "",
        files: false,
    },
];

pub(crate) struct AppLeftovers;

impl Scanner for AppLeftovers {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Restos de apps", "App leftovers")
    }
    fn description(&self) -> &'static str {
        t!(
            "Datos que dejaron apps que ya no están instaladas.",
            "Data left behind by apps that are no longer installed."
        )
    }
    fn area(&self) -> Area {
        Area::Apps
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let apps = InstalledApps::discover(ctx);
        let locator = Spotlight::new(ctx, apps.spotlight_canary.clone());
        Ok(scan_leftovers(ctx, &apps, &locator))
    }
}

#[derive(Debug, Default)]
struct Candidate {
    paths: Vec<(PathBuf, Kind)>,
}

pub(crate) fn scan_leftovers(
    ctx: &ScanContext,
    apps: &InstalledApps,
    locator: &dyn AppLocator,
) -> Vec<Finding> {
    let candidates = collect_candidates(ctx, apps);
    if candidates.is_empty() {
        return Vec::new();
    }
    if !apps.complete {
        return vec![cannot_confirm(t!(
            "No se pudo leer la lista completa de apps instaladas, y con una lista incompleta una \
             app que sigue instalada parecería desinstalada. Por seguridad no se marca ningún \
             resto.",
            "The full list of installed apps could not be read, and with an incomplete list an \
             app that is still installed would look uninstalled. To be safe, nothing is marked \
             as a leftover."
        ))];
    }
    if !locator.is_healthy() {
        return vec![cannot_confirm(t!(
            "Spotlight no está respondiendo, y hace falta para comprobar que una app no esté \
             instalada en otra ubicación. Por seguridad no se marca ningún resto hasta que vuelva \
             a funcionar.",
            "Spotlight is not responding, and it is needed to check that an app is not installed \
             somewhere else. To be safe, nothing is marked as a leftover until it works again."
        ))];
    }
    let candidates: Vec<(String, Candidate)> = candidates.into_iter().collect();
    let mut findings: Vec<Finding> = candidates
        .par_iter()
        .filter_map(|(id, candidate)| candidate_finding(ctx, locator, id, candidate))
        .collect();
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.size));
    findings
}

fn cannot_confirm(explanation: &str) -> Finding {
    Finding::new(
        SCANNER_ID,
        Area::Apps,
        "sin_confirmar",
        t!(
            "No se pudieron comprobar los restos de apps",
            "App leftovers could not be checked"
        ),
    )
    .explanation(explanation)
}

/// Every bundle-id-named item in the known locations that passes the name and vendor checks,
/// grouped by the shortest id so `com.foo.app` and `com.foo.app.ShipIt` read as one app.
fn collect_candidates(ctx: &ScanContext, apps: &InstalledApps) -> BTreeMap<String, Candidate> {
    let access = gate::has_access(ctx);
    let mut grouped: BTreeMap<String, Candidate> = BTreeMap::new();
    for location in LOCATIONS {
        for path in fsutil::children(&ctx.home_path(location.relative)) {
            let name = name_of(&path);
            let Some(id) = name.strip_suffix(location.suffix) else {
                continue;
            };
            if !is_candidate_name(id)
                || apps.covers(id)
                || gate::is_excluded(ctx, &path)
                || (!access && gate::is_inside_guarded(&ctx.home, &path))
            {
                continue;
            }
            let right_type = if location.files {
                path.is_file() && !gate::is_symlink(&path)
            } else {
                fsutil::is_real_dir(&path)
            };
            if right_type {
                grouped
                    .entry(id.to_string())
                    .or_default()
                    .paths
                    .push((path, location.kind));
            }
        }
    }

    let mut merged: BTreeMap<String, Candidate> = BTreeMap::new();
    for (id, candidate) in grouped {
        // Sorted order puts `com.foo.app` straight before anything nested under it.
        let parent = merged
            .keys()
            .next_back()
            .filter(|last| {
                id.to_lowercase()
                    .starts_with(&format!("{}.", last.to_lowercase()))
            })
            .cloned();
        match parent {
            Some(parent) => {
                if let Some(entry) = merged.get_mut(&parent) {
                    entry.paths.extend(candidate.paths);
                }
            }
            None => {
                merged.insert(id, candidate);
            }
        }
    }
    merged
}

fn is_candidate_name(id: &str) -> bool {
    policy::is_reverse_dns(id)
        && !policy::is_apple(id)
        && !policy::is_sensitive(id)
        && !policy::is_shared_namespace(id)
}

fn candidate_finding(
    ctx: &ScanContext,
    locator: &dyn AppLocator,
    id: &str,
    candidate: &Candidate,
) -> Option<Finding> {
    if ctx.is_cancelled()
        || candidate
            .paths
            .iter()
            .all(|(_, kind)| *kind == Kind::Preferences)
    {
        return None;
    }
    let mut total = SizeInfo::default();
    let mut kinds: Vec<Kind> = Vec::new();
    for (path, kind) in &candidate.paths {
        let info = ctx.measure(path);
        // If part of it cannot be read, we cannot tell when it was last used.
        if info.unreadable > 0 {
            return None;
        }
        total.bytes += info.bytes;
        total.shared += info.shared;
        total.files += info.files;
        total.newest_modified = total.newest_modified.max(info.newest_modified);
        if !kinds.contains(kind) {
            kinds.push(*kind);
        }
    }
    let newest: SystemTime = total.newest_modified?;
    if total.files == 0 || total.exclusive() == 0 {
        return None;
    }
    // A date in the future reads as recent. Either way, leave it.
    if age_days(newest, ctx.now).is_none_or(|days| days < MIN_AGE_DAYS) {
        return None;
    }
    // Last, because it starts a process: only the candidates that survived get here.
    if locator.knows(id) {
        return None;
    }
    let paths: Vec<PathBuf> = candidate
        .paths
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    let subtitle = if paths.len() == 1 {
        paths[0].display().to_string()
    } else {
        tf!("{} ubicaciones", "{} locations", paths.len())
    };
    Some(
        Finding::new(
            SCANNER_ID,
            Area::Apps,
            id,
            tf!("Restos de {id}", "Leftovers of {id}"),
        )
        .subtitle(subtitle)
        .measured(&total)
        .explanation(tf!(
            "Ninguna app instalada usa este identificador y nada lo ha tocado desde {}. Contiene: \
             {}. Puede incluir ajustes o datos que querrías conservar si piensas volver a \
             instalarla, así que revísalo antes.",
            "No installed app uses this identifier, and it was last changed {}. It holds: {}. It may include settings or data you'd want to keep if you plan to install \
             the app again, so check it first.",
            human_age(newest, ctx.now),
            kinds
                .iter()
                .map(|kind| kind.label())
                .collect::<Vec<_>>()
                .join(", ")
        ))
            .risk(Risk::Review)
            .recovery(Recovery::None)
            .selected(false)
            .action(Action::Trash { paths }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{find, targets, Fixture, LockedDir};
    use std::time::Duration;

    struct FakeLocator {
        healthy: bool,
        knows: bool,
    }

    impl AppLocator for FakeLocator {
        fn is_healthy(&self) -> bool {
            self.healthy
        }
        fn knows(&self, _: &str) -> bool {
            self.knows
        }
    }

    const ABSENT: FakeLocator = FakeLocator {
        healthy: true,
        knows: false,
    };

    /// A clock far enough ahead that everything in the fixture looks long abandoned.
    fn aged_home() -> Fixture {
        let mut home = Fixture::new();
        home.ctx.now = SystemTime::now() + Duration::from_secs(60 * 86_400);
        home
    }

    fn complete_index() -> InstalledApps {
        InstalledApps::from_roots(&[])
    }

    #[test]
    fn nothing_to_look_at_is_silent() {
        let home = aged_home();
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn groups_every_location_of_an_uninstalled_app() {
        let home = aged_home();
        let support = home.write("Library/Application Support/com.gone.app/data.db", 1_000);
        let cache = home.write("Library/Caches/com.gone.app/x", 1_000);
        let prefs = home.write("Library/Preferences/com.gone.app.plist", 1_000);
        let state = home.write(
            "Library/Saved Application State/com.gone.app.savedState/w",
            1_000,
        );
        let http = home.write("Library/HTTPStorages/com.gone.app/h", 1_000);
        let cookies = home.write("Library/HTTPStorages/com.gone.app.binarycookies", 1_000);
        let webkit = home.write("Library/WebKit/com.gone.app/w", 1_000);

        let findings = scan_leftovers(&home.ctx, &complete_index(), &ABSENT);
        assert_eq!(findings.len(), 1);
        let finding = find(&findings, "com.gone.app").expect("finding");
        assert_eq!(finding.risk, Risk::Review);
        assert!(!finding.selected_by_default);
        let removed = targets(finding);
        assert_eq!(removed.len(), 7);
        for expected in [
            support.parent().expect("parent").to_path_buf(),
            cache.parent().expect("parent").to_path_buf(),
            prefs,
            state.parent().expect("parent").to_path_buf(),
            http.parent().expect("parent").to_path_buf(),
            cookies,
            webkit.parent().expect("parent").to_path_buf(),
        ] {
            assert!(removed.contains(&expected), "{expected:?}");
        }
    }

    /// Opera's profile is named after its bundle id, so it looks just like a leftover.
    #[test]
    fn a_guarded_folder_is_not_measured_without_access() {
        let home = aged_home();
        home.write(
            "Library/Application Support/com.operasoftware.Opera/Default/Cookies",
            1_000,
        );
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
        home.grant_full_disk_access();
        assert_eq!(
            scan_leftovers(&home.ctx, &complete_index(), &ABSENT).len(),
            1
        );
    }

    #[test]
    fn nested_ids_merge_into_the_parent_app() {
        let home = aged_home();
        home.write("Library/Caches/com.gone.app/x", 1_000);
        home.write("Library/Caches/com.gone.app.ShipIt/x", 1_000);
        let findings = scan_leftovers(&home.ctx, &complete_index(), &ABSENT);
        assert_eq!(findings.len(), 1);
        assert_eq!(targets(&findings[0]).len(), 2);
    }

    #[test]
    fn an_installed_app_or_its_vendor_is_not_a_leftover() {
        let home = aged_home();
        home.app(
            "Applications/Chrome.app",
            "com.google.Chrome",
            "Google Chrome",
        );
        let apps = InstalledApps::from_roots(&[home.path("Applications")]);
        home.write("Library/Application Support/com.google.Chrome/x", 1_000);
        home.write("Library/Application Support/com.google.Earth/x", 1_000);
        assert!(scan_leftovers(&home.ctx, &apps, &ABSENT).is_empty());
    }

    #[test]
    fn apple_shared_and_sensitive_names_are_never_leftovers() {
        let home = aged_home();
        for name in [
            "com.apple.Notes",
            "group.com.apple.notes",
            "com.crashlytics.data",
            "org.sparkle-project.Sparkle",
            "com.1password.safari",
            "com.getdropbox.dropbox",
            "com.adobe.Lightroom",
            "dev.healthymac.app",
            "org.cups.PrintingPrefs",
            "com.android.Emulator",
            "org.graalvm.polyglot",
        ] {
            home.write(&format!("Library/Application Support/{name}/x"), 1_000);
        }
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn plain_names_are_never_matched() {
        let home = aged_home();
        for name in ["Slack", "Google", "notion.id", "My.Old.Stuff"] {
            home.write(&format!("Library/Application Support/{name}/x"), 1_000);
        }
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn recent_activity_means_still_in_use() {
        let mut home = aged_home();
        home.write("Library/Application Support/com.gone.app/x", 1_000);
        home.ctx.now = SystemTime::now() + Duration::from_secs(5 * 86_400);
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn recent_activity_anywhere_keeps_the_whole_app() {
        let home = aged_home();
        home.write("Library/Application Support/com.gone.app/old", 1_000);
        // A clock 60 days ahead, then one location "written" 60 days in the future: it is newer than
        // anything the age check could call abandoned.
        let recent = home.write("Library/Caches/com.gone.app/new", 1_000);
        let future = SystemTime::now() + Duration::from_secs(59 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&recent)
            .expect("open")
            .set_modified(future)
            .expect("set mtime");
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn spotlight_knowing_the_vendor_keeps_it() {
        let home = aged_home();
        home.write("Library/Application Support/com.elsewhere.app/x", 1_000);
        let locator = FakeLocator {
            healthy: true,
            knows: true,
        };
        assert!(scan_leftovers(&home.ctx, &complete_index(), &locator).is_empty());
    }

    #[test]
    fn unhealthy_spotlight_flags_nothing_and_says_so() {
        let home = aged_home();
        home.write("Library/Application Support/com.gone.app/x", 1_000);
        let locator = FakeLocator {
            healthy: false,
            knows: false,
        };
        let findings = scan_leftovers(&home.ctx, &complete_index(), &locator);
        assert_eq!(findings.len(), 1);
        let finding = find(&findings, "sin_confirmar").expect("report");
        assert_eq!(finding.action, Action::ReportOnly);
        assert!(finding.target_paths().is_empty());
    }

    #[test]
    fn an_incomplete_app_list_flags_nothing_and_says_so() {
        let home = aged_home();
        home.write("Library/Application Support/com.gone.app/x", 1_000);
        home.write_text("Applications/Broken.app/Contents/Info.plist", "not a plist");
        let apps = InstalledApps::from_roots(&[home.path("Applications")]);
        assert!(!apps.complete);
        let findings = scan_leftovers(&home.ctx, &apps, &ABSENT);
        assert_eq!(findings.len(), 1);
        assert!(find(&findings, "sin_confirmar").is_some());
    }

    #[test]
    fn unreadable_subtree_cannot_be_called_abandoned() {
        let home = aged_home();
        home.write("Library/Application Support/com.gone.app/x", 1_000);
        let locked = home.mkdir("Library/Application Support/com.gone.app/private");
        let _guard = LockedDir::new(&locked);
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn size_is_what_removal_would_free_across_locations() {
        let home = aged_home();
        let original = home.write("elsewhere/data.bin", 300_000);
        let clone = home.path("Library/Application Support/com.gone.app/data.bin");
        std::fs::create_dir_all(clone.parent().expect("parent")).expect("mkdir");
        std::fs::copy(&original, &clone).expect("copy");
        home.write("Library/WebKit/com.gone.app/w", 1_000);
        if fsutil::clone_id(&original).is_none()
            || fsutil::clone_id(&original) != fsutil::clone_id(&clone)
        {
            return;
        }
        let findings = scan_leftovers(&home.ctx, &complete_index(), &ABSENT);
        let finding = find(&findings, "com.gone.app").expect("finding");
        assert!(finding.size < 100_000, "{}", finding.size);
        assert!(finding.shared_bytes >= 300_000);
    }

    #[test]
    fn empty_folders_are_not_worth_a_finding() {
        let home = aged_home();
        home.mkdir("Library/Application Support/com.gone.app");
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn user_exclusions_and_symlinks_are_respected() {
        let mut home = aged_home();
        home.write("Library/Application Support/com.keep.app/x", 1_000);
        home.ctx.config.excluded_paths =
            vec![home.path("Library/Application Support/com.keep.app")];
        let target = home.mkdir("elsewhere");
        std::os::unix::fs::symlink(
            &target,
            home.path("Library/Application Support/com.link.app"),
        )
        .expect("symlink");
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn missing_library_folders_are_fine() {
        let home = aged_home();
        home.write("Library/WebKit/com.gone.app/x", 1_000);
        let findings = scan_leftovers(&home.ctx, &complete_index(), &ABSENT);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].item_count, 1);
    }

    /// What ZAP and the Android emulator look like: a plist under an id no bundle claims.
    #[test]
    fn a_preferences_file_alone_is_not_evidence() {
        let home = aged_home();
        home.write("Library/Preferences/org.parosproxy.paros.plist", 1_000);
        assert!(scan_leftovers(&home.ctx, &complete_index(), &ABSENT).is_empty());
    }

    #[test]
    fn a_preferences_file_goes_along_when_there_is_other_data() {
        let home = aged_home();
        let prefs = home.write("Library/Preferences/com.gone.app.plist", 1_000);
        home.write("Library/WebKit/com.gone.app/x", 1_000);
        let findings = scan_leftovers(&home.ctx, &complete_index(), &ABSENT);
        assert_eq!(findings.len(), 1);
        assert!(targets(&findings[0]).contains(&prefs));
    }
}
