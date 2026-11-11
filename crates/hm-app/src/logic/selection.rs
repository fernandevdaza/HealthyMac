//! What the user has chosen to clean, and what that adds up to.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use hm_core::fmt::human_size;
use hm_core::{tf, Action, Area, Finding, Recovery, RemovalMode, Risk};

/// A finding the cleaner can act on. Report-only findings are shown, but can never be selected.
pub fn is_actionable(finding: &Finding) -> bool {
    finding.action != Action::ReportOnly
}

/// A scanner's note that it skipped something for lack of Full Disk Access. The area pages gather
/// these into one notice with a way to grant it, instead of listing them as findings.
pub fn needs_full_disk_access(finding: &Finding) -> bool {
    !is_actionable(finding)
        && finding
            .tags
            .iter()
            .any(|tag| tag == hm_core::TAG_NEEDS_FULL_DISK_ACCESS)
}

/// Selection for a set of findings. Defaults come only from `selected_by_default`; what the user
/// changes is kept as an override, keyed by the finding's stable id, so it survives a new scan.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    overrides: HashMap<String, bool>,
}

impl Selection {
    pub fn is_selected(&self, finding: &Finding) -> bool {
        if !is_actionable(finding) {
            return false;
        }
        self.overrides
            .get(&finding.id)
            .copied()
            .unwrap_or(finding.selected_by_default)
    }

    pub fn set(&mut self, finding: &Finding, selected: bool) {
        if is_actionable(finding) {
            self.overrides.insert(finding.id.clone(), selected);
        }
    }

    pub fn toggle(&mut self, finding: &Finding) {
        let selected = self.is_selected(finding);
        self.set(finding, !selected);
    }

    /// Sets every actionable finding in `findings` at once.
    pub fn set_all<'a>(&mut self, findings: impl IntoIterator<Item = &'a Finding>, selected: bool) {
        for finding in findings {
            self.set(finding, selected);
        }
    }

    pub fn state_of<'a>(&self, findings: impl IntoIterator<Item = &'a Finding>) -> GroupState {
        let (mut on, mut off) = (false, false);
        for finding in findings.into_iter().filter(|f| is_actionable(f)) {
            if self.is_selected(finding) {
                on = true;
            } else {
                off = true;
            }
        }
        match (on, off) {
            (true, true) => GroupState::Mixed,
            (true, false) => GroupState::All,
            (false, true) => GroupState::None,
            (false, false) => GroupState::Unavailable,
        }
    }

    pub fn summarize<'a>(&self, findings: impl IntoIterator<Item = &'a Finding>) -> Summary {
        let mut summary = Summary::default();
        for finding in findings {
            if self.is_selected(finding) {
                summary.add(finding);
            }
        }
        summary
    }
}

/// The state a "select all" checkbox shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupState {
    All,
    Mixed,
    None,
    /// Nothing in the group can be selected.
    Unavailable,
}

/// Item count and bytes, overall and broken down.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub count: usize,
    /// Bytes that cleaning releases for certain.
    pub bytes: u64,
    /// Bytes the same findings also occupy but share with something else, through APFS clones or
    /// hard links. They stay on disk, though removing every clone would free more than the sum.
    pub shared_bytes: u64,
    /// Index with [`risk_index`].
    pub by_risk: [RiskTotal; 3],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RiskTotal {
    pub count: usize,
    pub bytes: u64,
}

pub fn risk_index(risk: Risk) -> usize {
    match risk {
        Risk::Safe => 0,
        Risk::Moderate => 1,
        Risk::Review => 2,
    }
}

impl Summary {
    pub fn add(&mut self, finding: &Finding) {
        self.count += 1;
        self.bytes = self.bytes.saturating_add(finding.size);
        self.shared_bytes = self.shared_bytes.saturating_add(finding.shared_bytes);
        let total = &mut self.by_risk[risk_index(finding.risk)];
        total.count += 1;
        total.bytes = total.bytes.saturating_add(finding.size);
    }

    pub fn of<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> Self {
        let mut summary = Self::default();
        for finding in findings {
            summary.add(finding);
        }
        summary
    }

    /// `bytes` is only a floor when any of it is shared: removing the last clone frees those blocks
    /// too, and that cannot be known ahead of time.
    pub fn is_lower_bound(&self) -> bool {
        self.shared_bytes > 0
    }

    /// The total to show, saying "al menos" when it is only a floor.
    pub fn total_label(&self) -> String {
        if self.is_lower_bound() {
            tf!("al menos {}", "at least {}", human_size(self.bytes))
        } else {
            human_size(self.bytes)
        }
    }

    pub fn needs_acknowledgement(&self) -> bool {
        self.by_risk[risk_index(Risk::Review)].count > 0
    }
}

/// Reclaimable bytes per area, counting only what could actually be cleaned.
pub fn reclaimable_by_area(findings: &[Finding]) -> HashMap<Area, u64> {
    let mut totals = HashMap::new();
    for finding in findings.iter().filter(|f| is_actionable(f)) {
        let total = totals.entry(finding.area).or_insert(0u64);
        *total = total.saturating_add(finding.size);
    }
    totals
}

/// Where a finding's bytes end up. Mirrors what [`hm_core::Cleaner`] does, so the review sheet can
/// tell the truth before anything happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Destination {
    /// Moved to the Trash: recoverable, but no space is freed until it is emptied.
    Trash,
    /// Deleted at once, because it regenerates by itself and the user opted in.
    Deleted,
    /// The owning tool removes it.
    Tool,
    /// Replaced by an APFS clone. Nothing is lost.
    Clone,
    /// Permanently empties the Trash.
    EmptyTrash,
    Nothing,
}

pub fn destination(finding: &Finding, mode: RemovalMode) -> Destination {
    match finding.action {
        Action::Trash { .. } | Action::TrashContents { .. } => {
            let permanent = mode == RemovalMode::DeleteRegenerable
                && finding.risk == Risk::Safe
                && matches!(finding.recovery, Recovery::Automatic | Recovery::Rebuild);
            if permanent {
                Destination::Deleted
            } else {
                Destination::Trash
            }
        }
        Action::Command { .. } => Destination::Tool,
        Action::CloneDedupe { .. } => Destination::Clone,
        Action::EmptyTrash => Destination::EmptyTrash,
        Action::ReportOnly => Destination::Nothing,
    }
}

/// Whether everything a finding touches lies inside `home`. A run against a test home still has the
/// real machine around it: Docker, `brew` and /Applications are not part of the fixture, so anything
/// that would reach them must be left alone.
pub fn stays_inside(finding: &Finding, home: &Path) -> bool {
    match &finding.action {
        Action::Trash { paths } => paths.iter().all(|path| path.starts_with(home)),
        Action::TrashContents { dir } => dir.starts_with(home),
        Action::CloneDedupe { keep, duplicates } => {
            keep.starts_with(home) && duplicates.iter().all(|path| path.starts_with(home))
        }
        // The cleaner empties `home/.Trash`, and nothing else.
        Action::EmptyTrash | Action::ReportOnly => true,
        // A command acts on the machine, whatever home it was given.
        Action::Command { .. } => false,
    }
}

/// Ids in `ids` that are gone from `findings`, used to drop what has already been cleaned.
pub fn retain_unclean(findings: &mut Vec<Finding>, cleaned: &HashSet<String>) {
    findings.retain(|finding| !cleaned.contains(&finding.id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use hm_core::Area;

    fn finding(key: &str, size: u64, risk: Risk, selected: bool) -> Finding {
        Finding::new("t", Area::System, key, key)
            .size(size)
            .risk(risk)
            .selected(selected)
            .trash(format!("/h/{key}"))
    }

    fn report_only(key: &str, size: u64) -> Finding {
        Finding::new("t", Area::System, key, key).size(size)
    }

    #[test]
    fn defaults_come_from_the_finding() {
        let selection = Selection::default();
        assert!(selection.is_selected(&finding("a", 1, Risk::Safe, true)));
        assert!(!selection.is_selected(&finding("b", 1, Risk::Review, false)));
    }

    #[test]
    fn report_only_can_never_be_selected() {
        let mut selection = Selection::default();
        let info = report_only("r", 500);
        selection.set(&info, true);
        selection.toggle(&info);
        assert!(!selection.is_selected(&info));
        assert_eq!(selection.summarize([&info]).count, 0);
    }

    #[test]
    fn totals_add_up_by_risk() {
        let findings = vec![
            finding("a", 100, Risk::Safe, true),
            finding("b", 50, Risk::Moderate, true),
            finding("c", 7, Risk::Review, false),
            finding("d", 3, Risk::Safe, false),
            report_only("e", 999),
        ];
        let mut selection = Selection::default();
        selection.set(&findings[2], true);
        let summary = selection.summarize(&findings);
        assert_eq!(summary.count, 3);
        assert_eq!(summary.bytes, 157);
        assert_eq!(summary.by_risk[risk_index(Risk::Safe)].bytes, 100);
        assert_eq!(summary.by_risk[risk_index(Risk::Moderate)].count, 1);
        assert_eq!(summary.by_risk[risk_index(Risk::Review)].bytes, 7);
        assert!(summary.needs_acknowledgement());
    }

    #[test]
    fn no_review_items_means_no_acknowledgement() {
        let findings = vec![finding("a", 1, Risk::Safe, true)];
        assert!(!Selection::default()
            .summarize(&findings)
            .needs_acknowledgement());
    }

    #[test]
    fn totals_saturate_rather_than_overflow() {
        let findings = vec![
            finding("a", u64::MAX, Risk::Safe, true),
            finding("b", u64::MAX, Risk::Safe, true),
        ];
        assert_eq!(Selection::default().summarize(&findings).bytes, u64::MAX);
    }

    #[test]
    fn overrides_survive_a_rescan_with_the_same_ids() {
        let mut selection = Selection::default();
        selection.set(&finding("a", 1, Risk::Safe, true), false);
        let rescanned = finding("a", 2, Risk::Safe, true);
        assert!(!selection.is_selected(&rescanned));
    }

    #[test]
    fn group_state() {
        let findings = vec![
            finding("a", 1, Risk::Safe, true),
            finding("b", 1, Risk::Safe, false),
            report_only("c", 1),
        ];
        let mut selection = Selection::default();
        assert_eq!(selection.state_of(&findings), GroupState::Mixed);
        selection.set_all(&findings, true);
        assert_eq!(selection.state_of(&findings), GroupState::All);
        selection.set_all(&findings, false);
        assert_eq!(selection.state_of(&findings), GroupState::None);
        assert_eq!(
            selection.state_of(&[report_only("x", 1)]),
            GroupState::Unavailable
        );
    }

    #[test]
    fn reclaimable_ignores_report_only() {
        let findings = vec![finding("a", 10, Risk::Safe, false), report_only("b", 90)];
        assert_eq!(reclaimable_by_area(&findings)[&Area::System], 10);
    }

    #[test]
    fn permanent_deletion_only_for_safe_and_regenerable() {
        let cache = finding("a", 1, Risk::Safe, true).recovery(Recovery::Automatic);
        assert_eq!(destination(&cache, RemovalMode::Trash), Destination::Trash);
        assert_eq!(
            destination(&cache, RemovalMode::DeleteRegenerable),
            Destination::Deleted
        );

        let download = finding("b", 1, Risk::Safe, true).recovery(Recovery::Redownload);
        assert_eq!(
            destination(&download, RemovalMode::DeleteRegenerable),
            Destination::Trash
        );

        let risky = finding("c", 1, Risk::Moderate, true).recovery(Recovery::Automatic);
        assert_eq!(
            destination(&risky, RemovalMode::DeleteRegenerable),
            Destination::Trash
        );
    }

    #[test]
    fn shared_bytes_make_the_total_a_lower_bound() {
        let exclusive = finding("a", 1_000_000_000, Risk::Safe, true);
        let mut venv = finding("b", 45_000_000, Risk::Moderate, true);
        venv.shared_bytes = 916_000_000;

        let plain = Selection::default().summarize([&exclusive]);
        assert!(!plain.is_lower_bound());
        assert_eq!(plain.total_label(), "1,0 GB");

        let shared = Selection::default().summarize([&exclusive, &venv]);
        assert_eq!(
            shared.bytes, 1_045_000_000,
            "shared bytes are not added to the total"
        );
        assert_eq!(shared.shared_bytes, 916_000_000);
        assert!(shared.is_lower_bound());
        assert_eq!(shared.total_label(), "al menos 1,0 GB");
    }

    #[test]
    fn a_test_home_never_reaches_the_real_machine() {
        let home = Path::new("/tmp/fixture");
        let inside =
            Finding::new("t", Area::System, "a", "a").trash("/tmp/fixture/Library/Caches/x");
        let outside = Finding::new("t", Area::Apps, "b", "b").trash("/Applications/Real.app");
        let sibling = Finding::new("t", Area::Files, "c", "c").trash("/tmp/fixture-other/x");
        let command = Finding::new("t", Area::Developer, "d", "d").action(Action::Command {
            program: "docker".to_string(),
            args: vec!["system".to_string(), "prune".to_string()],
        });
        let mixed = Finding::new("t", Area::Apps, "e", "e").action(Action::Trash {
            paths: vec!["/tmp/fixture/ok".into(), "/Users/real/thing".into()],
        });
        assert!(stays_inside(&inside, home));
        assert!(!stays_inside(&outside, home));
        assert!(
            !stays_inside(&sibling, home),
            "a shared prefix is not the same folder"
        );
        assert!(!stays_inside(&command, home));
        assert!(!stays_inside(&mixed, home));
    }
}
