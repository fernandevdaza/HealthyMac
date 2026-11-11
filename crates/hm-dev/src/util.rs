//! Small helpers shared by the scanners and the non-scanner APIs.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use hm_core::exec::{self, CmdOutput};
use hm_core::{
    fsutil, tf, Action, Finding, Recovery, Risk, SafetyPolicy, SafetyViolation, ScanContext,
};

/// A text in Spanish and English for a `static` table, where [`hm_core::t!`] cannot run. [`get`]
/// picks the current language, as `t!` does.
///
/// [`get`]: Text::get
#[derive(Clone, Copy, Debug)]
pub(crate) struct Text {
    es: &'static str,
    en: &'static str,
}

impl Text {
    pub(crate) const fn new(es: &'static str, en: &'static str) -> Self {
        Self { es, en }
    }

    pub(crate) fn get(self) -> &'static str {
        if hm_core::i18n::is_english() {
            self.en
        } else {
            self.es
        }
    }
}

/// Long enough for `git status` on a big repository, short enough not to hold up a scan.
pub(crate) const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Home-relative folders macOS guards with a permission prompt (see rule 6 of the architecture).
const PROMPTING_FOLDERS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mobile Documents",
    "Library/CloudStorage",
];

/// `/usr/bin/git` and `/usr/bin/xcrun` are stubs: with no developer directory, running one pops up
/// the "install the command line developer tools" dialog. `xcode-select -p` is the silent check.
fn developer_tools_present() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        exec::run(
            Path::new("/usr/bin/xcode-select"),
            &["-p"],
            Duration::from_secs(5),
        )
        .map(|output| output.success())
        .unwrap_or(false)
    })
}

/// Finds a tool that is safe to run: never a stub that would raise a dialog mid-scan.
pub(crate) fn tool(ctx: &ScanContext, name: &str) -> Option<PathBuf> {
    let path = ctx.which(name)?;
    if path.starts_with("/usr/bin") && matches!(name, "git" | "xcrun") && !developer_tools_present()
    {
        return None;
    }
    Some(path)
}

/// Runs a read-only tool. `None` when it could not start or did not finish in time; a non-zero exit
/// still returns the output, because several tools use it to mean "nothing found".
pub(crate) fn run_tool(path: &Path, args: &[&str], timeout: Duration) -> Option<CmdOutput> {
    exec::run(path, args, timeout)
        .ok()
        .filter(|output| !output.timed_out)
}

/// Runs a read-only git command against `repo`.
///
/// `--no-optional-locks` stops `git status` refreshing the index, and disabling fsmonitor stops it
/// starting a daemon or running a hook named by the repository's config. A scan must do neither.
pub(crate) fn run_git(git: &Path, repo: &Path, args: &[&str]) -> Option<CmdOutput> {
    let repo = repo.to_str()?;
    let mut full = vec![
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-C",
        repo,
    ];
    full.extend_from_slice(args);
    run_tool(git, &full, GIT_TIMEOUT)
}

/// `true` if touching `path` could raise a permission prompt on the user's screen.
pub(crate) fn may_prompt(ctx: &ScanContext, path: &Path) -> bool {
    if path.starts_with("/Volumes") {
        return true;
    }
    let canonical_home = ctx.home.canonicalize().ok();
    PROMPTING_FOLDERS.iter().any(|relative| {
        path.starts_with(ctx.home.join(relative))
            || canonical_home
                .as_ref()
                .is_some_and(|home| path.starts_with(home.join(relative)))
    })
}

/// Rule 6: whether this path may be read at all right now.
pub(crate) fn may_touch(ctx: &ScanContext, path: &Path) -> bool {
    !may_prompt(ctx, path) || fsutil::has_full_disk_access(&ctx.home)
}

/// Whether a path exists, without ever causing a permission prompt to find out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathState {
    Exists,
    Missing,
    /// Asking would raise a prompt, so the answer is not known.
    Unknown,
}

pub(crate) fn path_state(ctx: &ScanContext, path: &Path) -> PathState {
    if !may_touch(ctx, path) {
        PathState::Unknown
    } else if std::fs::symlink_metadata(path).is_ok() {
        PathState::Exists
    } else {
        PathState::Missing
    }
}

/// `true` if the user asked never to touch this path.
pub(crate) fn is_user_excluded(ctx: &ScanContext, path: &Path) -> bool {
    let policy = SafetyPolicy::new(&ctx.home, &ctx.config.excluded_paths);
    matches!(
        policy.check_removal(path),
        Err(SafetyViolation::UserExcluded(_))
    )
}

/// Parses `1.2.3`, `v1.2.3` or `2026.2` into comparable numbers. `None` if it is not a version.
pub(crate) fn parse_version(text: &str) -> Option<Vec<u64>> {
    let text = text.strip_prefix('v').unwrap_or(text);
    if text.is_empty() {
        return None;
    }
    text.split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

/// The last step before a scanner returns: holds every finding to the project's rules, whatever
/// the scanner did. The cleaner re-checks all of this, but a finding that can never succeed should
/// not reach the user in the first place.
///
/// - Excluded or vanished targets are dropped, and so is anything with nothing left to remove.
/// - A target the safety policy refuses becomes report-only, so the UI never offers a clean that
///   is bound to fail.
/// - Nothing unrecoverable or marked for review stays preselected (rule 4).
pub(crate) fn finalize(ctx: &ScanContext, findings: Vec<Finding>) -> Vec<Finding> {
    let policy = SafetyPolicy::new(&ctx.home, &ctx.config.excluded_paths);
    let mut vetted: Vec<Finding> = findings
        .into_iter()
        .filter_map(|finding| vet(ctx, &policy, finding))
        .collect();
    vetted.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.id.cmp(&b.id)));
    vetted
}

fn vet(ctx: &ScanContext, policy: &SafetyPolicy, mut finding: Finding) -> Option<Finding> {
    let mut refusal = None;
    match &finding.action {
        Action::Trash { paths } => {
            let mut kept = Vec::with_capacity(paths.len());
            for path in paths {
                match policy.check_removal(path) {
                    Ok(()) => kept.push(path.clone()),
                    Err(SafetyViolation::UserExcluded(_) | SafetyViolation::Missing(_)) => {}
                    Err(violation) => refusal = Some(violation.to_string()),
                }
            }
            if kept.is_empty() && refusal.is_none() {
                return None;
            }
            if kept.len() != paths.len() && !kept.is_empty() {
                let info = fsutil::measure_many(&kept, &ctx.cancel);
                finding.size = info.exclusive();
                finding.shared_bytes = info.shared;
                finding.action = Action::Trash { paths: kept };
            }
            if finding.size == 0 && refusal.is_none() {
                return None;
            }
        }
        Action::TrashContents { dir } => {
            // The cleaner validates each child, never the folder, so check what it will check.
            let child = fsutil::children(dir).into_iter().next()?;
            match policy.check_removal(&child) {
                Ok(()) => {}
                Err(SafetyViolation::UserExcluded(_) | SafetyViolation::Missing(_)) => return None,
                Err(violation) => refusal = Some(violation.to_string()),
            }
            if finding.size == 0 && refusal.is_none() {
                return None;
            }
        }
        Action::Command { .. }
        | Action::CloneDedupe { .. }
        | Action::EmptyTrash
        | Action::ReportOnly => {}
    }

    if let Some(reason) = refusal {
        finding.explanation = tf!(
            "{} HealthyMac no puede limpiar esta ruta ({reason}).",
            "{} HealthyMac can't clean this path ({reason}).",
            finding.explanation
        );
        finding.action = Action::ReportOnly;
        finding.size = 0;
    }
    if finding.risk == Risk::Review
        || finding.recovery == Recovery::None
        || finding.action == Action::ReportOnly
    {
        finding.selected_by_default = false;
    }
    Some(finding)
}

/// A plural-aware count: `carpetas(1)` is "1 carpeta".
pub(crate) fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The newest of two optional times.
pub(crate) fn newest(a: Option<SystemTime>, b: Option<SystemTime>) -> Option<SystemTime> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// A path for display, shortened with `~`.
pub(crate) fn tilde(ctx: &ScanContext, path: &Path) -> String {
    let inside_home = path
        .strip_prefix(&ctx.home)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            let home = ctx.home.canonicalize().ok()?;
            path.strip_prefix(home).ok().map(Path::to_path_buf)
        });
    match inside_home {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(parse_version("v20.11.0"), Some(vec![20, 11, 0]));
        assert_eq!(parse_version("2026.2"), Some(vec![2026, 2]));
        assert_eq!(parse_version("system"), None);
        assert_eq!(parse_version(""), None);
        assert!(parse_version("3.9.1") < parse_version("3.13.0"));
    }

    #[test]
    fn prompting_folders_are_detected_and_gated() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ScanContext::with_home(dir.path(), hm_core::Config::default());
        let guarded = dir.path().join("Documents/app/App.xcodeproj");
        assert!(may_prompt(&ctx, &guarded));
        assert!(may_prompt(&ctx, Path::new("/Volumes/USB/app")));
        assert!(!may_prompt(&ctx, &dir.path().join("Coding/app")));
        // A fixture home has no probe folders, so it counts as lacking Full Disk Access.
        assert!(!may_touch(&ctx, &guarded));
        assert_eq!(path_state(&ctx, &guarded), PathState::Unknown);
    }
}
