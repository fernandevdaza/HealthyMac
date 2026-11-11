//! Logs: `~/Library/Logs`, its crash reports, and `/Library/Logs` when we can read it.

use std::path::{Path, PathBuf};

use anyhow::Result;
use hm_core::fmt::human_size;
use hm_core::{t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner, SizeInfo};
use rayon::prelude::*;

use super::policy;
use crate::gate::{self, name_of, DirListing};

const SCANNER_ID: &str = "sys.logs";
/// Log folders hm-dev owns. `dedupe_findings` drops a target nested inside another, so offering the
/// parent here would hide hm-dev's more specific findings.
const SPECIALISED_LOGS: &[&str] = &["CoreSimulator", "Google", "JetBrains"];

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// At or above this, a folder of logs gets its own finding.
    pub own_finding: u64,
    /// Below this, logs we cannot touch are not worth a line in the UI.
    pub report: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            own_finding: 50_000_000,
            report: 50_000_000,
        }
    }
}

pub(crate) struct Logs;

impl Scanner for Logs {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Registros", "Logs")
    }
    fn description(&self) -> &'static str {
        t!(
            "Registros de apps y del sistema, e informes de fallos antiguos.",
            "App and system logs, and old crash reports."
        )
    }
    fn area(&self) -> Area {
        Area::System
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        Ok(scan_logs(
            ctx,
            Path::new("/Library/Logs"),
            Limits::default(),
        ))
    }
}

pub(crate) fn scan_logs(ctx: &ScanContext, system_logs: &Path, limits: Limits) -> Vec<Finding> {
    let mut findings = user_logs(ctx, limits.own_finding);
    findings.extend(crash_reports(
        ctx,
        &ctx.home_path("Library/Logs/DiagnosticReports"),
        "fallos",
        t!(
            "Informes de fallos de tus apps",
            "Crash reports from your apps"
        ),
    ));
    findings.extend(system_log_findings(ctx, system_logs, limits.report));
    findings
}

struct Entry {
    path: PathBuf,
    info: SizeInfo,
}

fn measure_children(ctx: &ScanContext, dir: &Path, skip: &[&str]) -> Vec<Entry> {
    let DirListing::Entries(paths) = gate::list_dir(dir) else {
        return Vec::new();
    };
    paths
        .par_iter()
        .filter_map(|path| {
            let name = name_of(path);
            let skipped = name.starts_with('.')
                || skip.contains(&name.as_str())
                || policy::is_apple(&name)
                || gate::is_symlink(path)
                || gate::is_excluded(ctx, path)
                || ctx.is_cancelled();
            if skipped {
                return None;
            }
            let info = ctx.measure(path);
            (info.bytes > 0).then(|| Entry {
                path: path.clone(),
                info,
            })
        })
        .collect()
}

fn user_logs(ctx: &ScanContext, own_finding_min: u64) -> Vec<Finding> {
    let root = ctx.home_path("Library/Logs");
    // `HealthyMac` holds our own audit trail of what was cleaned. `DiagnosticReports` gets its own
    // finding below, because unlike logs those reports never come back.
    let mut skip = vec!["HealthyMac", "DiagnosticReports"];
    skip.extend_from_slice(SPECIALISED_LOGS);
    let entries = measure_children(ctx, &root, &skip);
    let (large, small): (Vec<Entry>, Vec<Entry>) = entries
        .into_iter()
        .partition(|entry| entry.info.exclusive() >= own_finding_min);

    let mut findings: Vec<Finding> = large
        .iter()
        .map(|entry| {
            let name = name_of(&entry.path);
            Finding::new(
                SCANNER_ID,
                Area::System,
                &name,
                tf!("Registros de {name}", "{name} logs"),
            )
            .trash(&entry.path)
            .measured(&entry.info)
            .explanation(t!(
                "Registros de actividad. Solo sirven para diagnosticar problemas y la app \
                     escribe unos nuevos cuando los necesita.",
                "Activity logs. They are only useful for diagnosing problems, and the app writes \
                 new ones when it needs them."
            ))
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .selected(true)
        })
        .collect();

    if !small.is_empty() {
        let info = sum_info(&small);
        let paths = small.iter().map(|entry| entry.path.clone()).collect();
        findings.push(
            Finding::new(
                SCANNER_ID,
                Area::System,
                "usuario",
                t!("Registros de apps", "App logs"),
            )
            .subtitle(root.display().to_string())
            .measured(&info)
            .explanation(t!(
                "Registros de actividad de tus apps. Solo sirven para diagnosticar problemas y \
                 cada app escribe unos nuevos cuando los necesita.",
                "Activity logs from your apps. They are only useful for diagnosing problems, and \
                 each app writes new ones when it needs them."
            ))
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .selected(true)
            .action(Action::Trash { paths }),
        );
    }
    findings
}

fn sum_info(entries: &[Entry]) -> SizeInfo {
    let mut total = SizeInfo::default();
    for entry in entries {
        total.bytes += entry.info.bytes;
        total.shared += entry.info.shared;
        total.files += entry.info.files;
        total.newest_modified = total.newest_modified.max(entry.info.newest_modified);
        total.unreadable += entry.info.unreadable;
    }
    total
}

/// Crash reports are the one thing in here that does not regenerate, so rule 4 keeps them
/// unselected. Children are listed at clean time, keeping the folder macOS expects to exist.
fn crash_reports(ctx: &ScanContext, dir: &Path, key: &str, title: &str) -> Option<Finding> {
    if gate::is_excluded(ctx, dir) {
        return None;
    }
    let info = ctx.measure(dir);
    if info.files == 0 {
        return None;
    }
    Some(
        Finding::new(SCANNER_ID, Area::System, key, title)
            .subtitle(dir.display().to_string())
            .measured(&info)
            .explanation(t!(
                "Informes que macOS guarda cuando una app se cierra de golpe. No hacen falta para \
                 nada salvo para investigar ese fallo, y una vez borrados no vuelven, así que no \
                 van preseleccionados.",
                "Reports macOS saves when an app quits unexpectedly. They are only useful for \
                 looking into that crash, and once deleted they do not come back, so they are not \
                 preselected."
            ))
            .risk(Risk::Moderate)
            .recovery(Recovery::None)
            .action(Action::TrashContents {
                dir: dir.to_path_buf(),
            }),
    )
}

fn system_log_findings(ctx: &ScanContext, root: &Path, report_min: u64) -> Vec<Finding> {
    let DirListing::Entries(_) = gate::list_dir(root) else {
        return Vec::new();
    };
    let reports = root.join("DiagnosticReports");
    let mut findings = Vec::new();
    let mut skip = vec!["DiagnosticReports"];
    if gate::is_writable(&reports) {
        findings.extend(crash_reports(
            ctx,
            &reports,
            "fallos_sistema",
            t!("Informes de fallos del sistema", "System crash reports"),
        ));
    } else {
        skip.clear();
    }

    let entries = measure_children(ctx, root, &skip);
    // Trashing needs write access to the parent, and to a folder itself to move it elsewhere.
    let parent_writable = gate::is_writable(root);
    let (removable, locked): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|entry| {
        parent_writable && (entry.path.is_file() || gate::is_writable(&entry.path))
    });

    if !removable.is_empty() {
        let info = sum_info(&removable);
        let paths = removable.iter().map(|entry| entry.path.clone()).collect();
        findings.push(
            Finding::new(
                SCANNER_ID,
                Area::System,
                "sistema",
                t!("Registros del sistema", "System logs"),
            )
            .subtitle(root.display().to_string())
            .measured(&info)
            .explanation(t!(
                "Registros de servicios instalados para todos los usuarios. Solo sirven para \
                 diagnosticar problemas y se escriben de nuevo cuando hacen falta.",
                "Logs from services installed for all users. They are only useful for diagnosing \
                 problems and are written again when needed."
            ))
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .selected(true)
            .action(Action::Trash { paths }),
        );
    }

    let locked_bytes: u64 = locked.iter().map(|entry| entry.info.bytes).sum();
    if locked_bytes >= report_min {
        findings.push(
            Finding::new(
                SCANNER_ID,
                Area::System,
                "sistema_sin_permiso",
                tf!(
                    "Registros del sistema: {}",
                    "System logs: {}",
                    human_size(locked_bytes)
                ),
            )
            .subtitle(root.display().to_string())
            .explanation(t!(
                "Pertenecen al administrador del sistema y HealthyMac nunca pide privilegios, así \
                 que solo se muestran. macOS rota la mayoría por su cuenta.",
                "They belong to the system administrator and HealthyMac never asks for \
                 privileges, so they are only shown. macOS rotates most of them on its own."
            )),
        );
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{find, targets, Fixture, LockedDir};

    fn scan(home: &Fixture, system: &Path) -> Vec<Finding> {
        let limits = Limits {
            own_finding: 100_000,
            report: 1_000_000,
        };
        scan_logs(&home.ctx, system, limits)
    }

    #[test]
    fn missing_folders_yield_nothing() {
        let home = Fixture::new();
        assert!(scan(&home, &home.path("no-system-logs")).is_empty());
    }

    #[test]
    fn our_own_audit_log_is_never_offered() {
        let home = Fixture::new();
        home.write("Library/Logs/HealthyMac/history.jsonl", 200_000);
        home.write("Library/Logs/Other/a.log", 1_000);
        let findings = scan(&home, &home.path("none"));
        for finding in &findings {
            assert!(
                targets(finding)
                    .iter()
                    .all(|path| !path.ends_with("HealthyMac")),
                "{}",
                finding.id
            );
        }
        let aggregate = find(&findings, "usuario").expect("aggregate");
        assert_eq!(targets(aggregate), [home.path("Library/Logs/Other")]);
    }

    #[test]
    fn large_logs_get_their_own_finding_and_are_preselected() {
        let home = Fixture::new();
        let path = home.write("Library/Logs/Chatty/huge.log", 200_000);
        let findings = scan(&home, &home.path("none"));
        let finding = find(&findings, "Chatty").expect("finding");
        assert_eq!(finding.risk, Risk::Safe);
        assert!(finding.selected_by_default);
        assert_eq!(targets(finding), [path.parent().expect("parent")]);
    }

    #[test]
    fn crash_reports_keep_the_folder_and_are_not_preselected() {
        let home = Fixture::new();
        home.write("Library/Logs/DiagnosticReports/App.ips", 10_000);
        let findings = scan(&home, &home.path("none"));
        let finding = find(&findings, "fallos").expect("finding");
        assert!(!finding.selected_by_default);
        assert_eq!(finding.recovery, Recovery::None);
        assert!(matches!(finding.action, Action::TrashContents { .. }));
    }

    #[test]
    fn empty_crash_reports_folder_is_silent() {
        let home = Fixture::new();
        home.mkdir("Library/Logs/DiagnosticReports");
        assert!(find(&scan(&home, &home.path("none")), "fallos").is_none());
    }

    #[test]
    fn writable_system_logs_can_be_cleaned() {
        let home = Fixture::new();
        home.write("system/Vendor/a.log", 2_000);
        home.write("system/DiagnosticReports/b.ips", 2_000);
        let findings = scan(&home, &home.path("system"));
        assert!(find(&findings, "sistema").is_some());
        assert!(find(&findings, "fallos_sistema").is_some());
        assert!(find(&findings, "sistema_sin_permiso").is_none());
    }

    #[test]
    fn unreadable_system_logs_yield_nothing() {
        let home = Fixture::new();
        home.write("system/Vendor/a.log", 2_000);
        let system = home.path("system");
        let _guard = LockedDir::new(&system);
        assert!(scan(&home, &system).is_empty());
    }

    #[test]
    fn read_only_system_logs_are_only_reported() {
        use std::os::unix::fs::PermissionsExt;
        let home = Fixture::new();
        home.write("system/Vendor/a.log", 2_000_000);
        let system = home.path("system");
        std::fs::set_permissions(&system, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let findings = scan(&home, &system);
        std::fs::set_permissions(&system, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let finding = find(&findings, "sistema_sin_permiso").expect("report");
        assert_eq!(finding.action, Action::ReportOnly);
        assert_eq!(finding.size, 0);
        assert!(find(&findings, "sistema").is_none());
    }

    #[test]
    fn the_aggregate_counts_only_what_it_would_free() {
        let home = Fixture::new();
        let original = home.write("elsewhere/a.log", 300_000);
        let clone = home.path("Library/Logs/Tool/a.log");
        std::fs::create_dir_all(clone.parent().expect("parent")).expect("mkdir");
        std::fs::copy(&original, &clone).expect("copy");
        if hm_core::fsutil::clone_id(&original).is_none()
            || hm_core::fsutil::clone_id(&original) != hm_core::fsutil::clone_id(&clone)
        {
            return;
        }
        let findings = scan(&home, &home.path("none"));
        let aggregate = find(&findings, "usuario").expect("aggregate");
        assert!(aggregate.size < 100_000, "{}", aggregate.size);
        assert!(aggregate.shared_bytes >= 300_000);
    }

    #[test]
    fn folders_hm_dev_owns_are_left_to_it() {
        let home = Fixture::new();
        for name in SPECIALISED_LOGS {
            home.write(&format!("Library/Logs/{name}/a.log"), 200_000);
        }
        assert!(scan(&home, &home.path("none")).is_empty());
    }

    #[test]
    fn apple_log_folders_are_left_alone() {
        let home = Fixture::new();
        home.write("Library/Logs/com.apple.AMPLibraryAgent/a.log", 200_000);
        assert!(scan(&home, &home.path("none")).is_empty());
    }
}
