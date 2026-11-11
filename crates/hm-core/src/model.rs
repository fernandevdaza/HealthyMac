//! The data model shared by scanners, the cleaner, the CLI and the UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::fsutil::SizeInfo;

/// Top-level grouping shown in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    System,
    Developer,
    LocalAi,
    Apps,
    Files,
}

impl Area {
    pub const ALL: [Area; 5] = [
        Area::System,
        Area::Developer,
        Area::LocalAi,
        Area::Apps,
        Area::Files,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Area::System => crate::t!("Sistema", "System"),
            Area::Developer => "Developer",
            Area::LocalAi => crate::t!("IA local", "Local AI"),
            Area::Apps => "Apps",
            Area::Files => crate::t!("Archivos", "Files"),
        }
    }
}

/// What the user stands to lose if this finding is cleaned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Regenerated without the user noticing (caches, logs).
    Safe,
    /// Nothing is lost, but it costs time or bandwidth to get back.
    Moderate,
    /// May contain something the user cares about. Never selected by default.
    Review,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Safe => crate::t!("Seguro", "Safe"),
            Risk::Moderate => crate::t!("Moderado", "Moderate"),
            Risk::Review => crate::t!("Revisar", "Review"),
        }
    }
}

/// How the removed thing comes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    /// The owning tool recreates it on demand.
    Automatic,
    /// Comes back after rebuilding a project.
    Rebuild,
    /// Comes back after downloading again.
    Redownload,
    /// Only recoverable from the Trash.
    None,
    /// Nothing goes away at all, as when duplicates become APFS clones of each other.
    NothingLost,
}

impl Recovery {
    pub fn label(self) -> &'static str {
        match self {
            Recovery::Automatic => crate::t!("Se regenera solo", "Regenerates on its own"),
            Recovery::Rebuild => {
                crate::t!("Se regenera al compilar", "Regenerates on the next build")
            }
            Recovery::Redownload => crate::t!(
                "Hay que volver a descargarlo",
                "Needs to be downloaded again"
            ),
            Recovery::None => crate::t!(
                "Solo recuperable desde la Papelera",
                "Only recoverable from the Trash"
            ),
            Recovery::NothingLost => crate::t!("No se pierde nada", "Nothing is lost"),
        }
    }
}

/// What the cleaner does when the user approves a finding.
///
/// Scanners only *describe* the action. [`crate::clean::Cleaner`] executes it after re-validating
/// it against [`crate::safety::SafetyPolicy`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Remove these exact paths.
    Trash { paths: Vec<PathBuf> },
    /// Remove everything inside `dir`, keeping `dir` itself. Children are listed at clean time and
    /// each one is validated individually.
    TrashContents { dir: PathBuf },
    /// Run a tool's own cleanup command. Never through a shell. The program must be on the
    /// allow-list in [`crate::safety::SafetyPolicy::check_command`].
    Command { program: String, args: Vec<String> },
    /// Replace each duplicate with an APFS clone of `keep`. Nothing is lost: every path stays
    /// valid and only the duplicated blocks are released.
    CloneDedupe {
        keep: PathBuf,
        duplicates: Vec<PathBuf>,
    },
    /// Permanently delete everything in the user's Trash, whatever the removal mode. This is the
    /// step that actually frees the space from earlier cleans, so it always needs an explicit
    /// choice: findings using it must be `Risk::Review` and not selected by default.
    EmptyTrash,
    /// Informational. The UI shows it but the cleaner never touches it.
    ReportOnly,
}

/// One thing a scanner found that the user could act on.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    /// Stable across scans: `"{scanner_id}:{key}"`. Used to remember selections.
    pub id: String,
    pub scanner_id: String,
    pub area: Area,
    /// Optional sub-heading inside the scanner's section, e.g. a project name.
    pub group: Option<String>,
    /// Short name, in the current language.
    pub title: String,
    /// Usually the path.
    pub subtitle: Option<String>,
    /// One or two sentences, in the current language: what this is and what happens if it goes
    /// away.
    pub explanation: String,
    /// On-disk bytes that cleaning this would really release.
    pub size: u64,
    /// Bytes it also occupies but shares with something else through APFS clones or hard links, so
    /// they stay on disk after a clean. A `.venv` built by uv is often mostly this.
    #[serde(default)]
    pub shared_bytes: u64,
    pub item_count: u64,
    pub last_used: Option<SystemTime>,
    pub risk: Risk,
    pub recovery: Recovery,
    pub selected_by_default: bool,
    pub action: Action,
    /// Process names (exact match) that must not be running when this is cleaned, e.g. `"Xcode"`.
    pub blocked_by: Vec<String>,
    pub tags: Vec<String>,
}

/// Tag on the report-only finding a scanner emits when it skipped something for lack of Full Disk
/// Access. Every scanner uses the same one, so the UI can gather them into a single call to action.
pub const TAG_NEEDS_FULL_DISK_ACCESS: &str = "requiere_acceso_total";

impl Finding {
    /// Starts a finding with conservative defaults: `Review` risk, unrecoverable, not selected and
    /// report-only. Scanners opt in to anything more permissive explicitly.
    pub fn new(
        scanner_id: &str,
        area: Area,
        key: impl AsRef<str>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: format!("{scanner_id}:{}", key.as_ref()),
            scanner_id: scanner_id.to_string(),
            area,
            group: None,
            title: title.into(),
            subtitle: None,
            explanation: String::new(),
            size: 0,
            shared_bytes: 0,
            item_count: 0,
            last_used: None,
            risk: Risk::Review,
            recovery: Recovery::None,
            selected_by_default: false,
            action: Action::ReportOnly,
            blocked_by: Vec::new(),
            tags: Vec::new(),
        }
    }

    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn explanation(mut self, explanation: impl Into<String>) -> Self {
        self.explanation = explanation.into();
        self
    }

    pub fn size(mut self, size: u64) -> Self {
        self.size = size;
        self
    }

    pub fn shared_bytes(mut self, shared_bytes: u64) -> Self {
        self.shared_bytes = shared_bytes;
        self
    }

    pub fn items(mut self, item_count: u64) -> Self {
        self.item_count = item_count;
        self
    }

    pub fn last_used(mut self, last_used: Option<SystemTime>) -> Self {
        self.last_used = last_used;
        self
    }

    /// Sets size, item count and last-used from a measurement. The size is what removal would
    /// release, not what the tree occupies.
    pub fn measured(mut self, info: &SizeInfo) -> Self {
        self.size = info.exclusive();
        self.shared_bytes = info.shared;
        self.item_count = info.files;
        self.last_used = info.newest_modified;
        self
    }

    pub fn risk(mut self, risk: Risk) -> Self {
        self.risk = risk;
        self
    }

    pub fn recovery(mut self, recovery: Recovery) -> Self {
        self.recovery = recovery;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected_by_default = selected;
        self
    }

    pub fn action(mut self, action: Action) -> Self {
        self.action = action;
        self
    }

    /// Shorthand for an [`Action::Trash`] of one path, using it as the subtitle.
    pub fn trash(mut self, path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        self.subtitle = Some(path.display().to_string());
        self.action = Action::Trash {
            paths: vec![path.to_path_buf()],
        };
        self
    }

    pub fn blocked_by(mut self, processes: &[&str]) -> Self {
        self.blocked_by = processes.iter().map(|p| p.to_string()).collect();
        self
    }

    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Paths this finding would remove. Empty for commands, clones and report-only findings.
    pub fn target_paths(&self) -> Vec<&Path> {
        match &self.action {
            Action::Trash { paths } => paths.iter().map(PathBuf::as_path).collect(),
            Action::TrashContents { dir } => vec![dir.as_path()],
            _ => Vec::new(),
        }
    }
}

/// Removes double counting between scanners.
///
/// - Same target path twice: the non-[`Area::System`] finding wins, because a specialised scanner
///   explains it better than the generic cache sweep.
/// - A target nested inside another finding's target: the inner one is dropped, since cleaning the
///   outer one already covers it and its bytes are already counted.
pub fn dedupe_findings(findings: Vec<Finding>) -> Vec<Finding> {
    let mut best_for_path: HashMap<PathBuf, usize> = HashMap::new();
    let mut dropped = vec![false; findings.len()];

    for (index, finding) in findings.iter().enumerate() {
        for path in finding.target_paths() {
            match best_for_path.get(path) {
                None => {
                    best_for_path.insert(path.to_path_buf(), index);
                }
                Some(&other) => {
                    let keep_new =
                        findings[other].area == Area::System && finding.area != Area::System;
                    let (loser, winner) = if keep_new {
                        (other, index)
                    } else {
                        (index, other)
                    };
                    dropped[loser] = true;
                    best_for_path.insert(path.to_path_buf(), winner);
                }
            }
        }
    }

    for (index, finding) in findings.iter().enumerate() {
        if dropped[index] {
            continue;
        }
        let targets = finding.target_paths();
        let nested = !targets.is_empty()
            && targets.iter().all(|path| {
                path.ancestors().skip(1).any(|ancestor| {
                    best_for_path
                        .get(ancestor)
                        .is_some_and(|&outer| outer != index && !dropped[outer])
                })
            });
        if nested {
            dropped[index] = true;
        }
    }

    findings
        .into_iter()
        .zip(dropped)
        .filter_map(|(finding, dropped)| (!dropped).then_some(finding))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(key: &str, area: Area, path: &str) -> Finding {
        Finding::new("t", area, key, key).trash(path)
    }

    #[test]
    fn exact_duplicate_prefers_specialised_scanner() {
        let out = dedupe_findings(vec![
            finding("generic", Area::System, "/h/Library/Caches/Homebrew"),
            finding("brew", Area::Developer, "/h/Library/Caches/Homebrew"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].area, Area::Developer);
    }

    #[test]
    fn nested_target_is_dropped() {
        let out = dedupe_findings(vec![
            finding("outer", Area::Developer, "/h/p/target"),
            finding("inner", Area::Developer, "/h/p/target/debug"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "outer");
    }

    #[test]
    fn unrelated_targets_are_kept() {
        let out = dedupe_findings(vec![
            finding("a", Area::System, "/h/a"),
            finding("b", Area::System, "/h/ab"),
        ]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn new_finding_is_conservative() {
        let f = Finding::new("s", Area::Files, "k", "t");
        assert_eq!(f.risk, Risk::Review);
        assert!(!f.selected_by_default);
        assert_eq!(f.action, Action::ReportOnly);
    }
}
