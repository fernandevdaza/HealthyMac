//! Running the scanners, with live progress and cancellation.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::SystemTime;

use futures::channel::mpsc;
use futures::StreamExt;
use gpui::Context;
use hm_core::{run_scan, Area, CancelToken, Finding, ScanEvent, Scanner};

use crate::logic::selection::{is_actionable, needs_full_disk_access};
use crate::state::{FindingRow, Heading, ScannerMeta, ScannerStatus, Shell};

pub fn all_scanners() -> Vec<Box<dyn Scanner>> {
    let mut scanners = hm_system::scanners();
    scanners.extend(hm_dev::scanners());
    scanners.extend(hm_ai::scanners());
    scanners
}

/// The scanners this run shows, with their names in the current language.
pub fn scanners_for(mode: &crate::state::RunMode) -> Vec<ScannerMeta> {
    if mode.demo {
        crate::demo::scan::meta(mode.stress)
    } else {
        scanner_meta(&all_scanners())
    }
}

pub fn scanner_meta(scanners: &[Box<dyn Scanner>]) -> Vec<ScannerMeta> {
    scanners
        .iter()
        .map(|scanner| ScannerMeta {
            id: scanner.id(),
            name: scanner.name(),
            description: scanner.description(),
            area: scanner.area(),
        })
        .collect()
}

enum ScanMsg {
    Event(ScanEvent),
    Finished(Vec<Finding>),
}

impl Shell {
    pub fn scanner_name(&self, id: &str) -> String {
        self.scanners
            .iter()
            .find(|meta| meta.id == id)
            .map(|meta| meta.name.to_string())
            .unwrap_or_else(|| id.to_string())
    }

    pub fn start_scan(&mut self, cx: &mut Context<Self>) {
        if self.scan.running {
            return;
        }
        let cancel = CancelToken::new();
        self.scan.running = true;
        self.scan.cancelling = false;
        self.scan.was_cancelled = false;
        self.scan.cancel = Some(cancel.clone());
        self.scan.statuses = self
            .scanners
            .iter()
            .map(|meta| (meta.id, ScannerStatus::Waiting))
            .collect();

        let (tx, mut rx) = mpsc::unbounded::<ScanMsg>();
        if self.mode.demo {
            let stress = self.mode.stress;
            std::thread::spawn(move || {
                crate::demo::scan::run(
                    cancel,
                    stress,
                    |event| {
                        let _ = tx.unbounded_send(ScanMsg::Event(event));
                    },
                    |findings| {
                        let _ = tx.unbounded_send(ScanMsg::Finished(findings));
                    },
                );
            });
        } else {
            let mut ctx = self.fresh_ctx();
            ctx.cancel = cancel;
            std::thread::spawn(move || {
                let scanners = all_scanners();
                let sender = Mutex::new(tx.clone());
                let findings = run_scan(&scanners, &ctx, &move |event| {
                    if let Ok(sender) = sender.lock() {
                        let _ = sender.unbounded_send(ScanMsg::Event(event));
                    }
                });
                let _ = tx.unbounded_send(ScanMsg::Finished(findings));
            });
        }

        cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                let finished = matches!(message, ScanMsg::Finished(_));
                let alive = this.update(cx, |shell, cx| {
                    shell.on_scan_message(message);
                    cx.notify();
                });
                if alive.is_err() || finished {
                    break;
                }
            }
            // The scan's thread died without finishing. Leave no spinner behind.
            this.update(cx, |shell, cx| {
                if shell.scan.running {
                    shell.scan.running = false;
                    shell.scan.finished_once = true;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.scan.cancel {
            cancel.cancel();
            self.scan.cancelling = true;
            cx.notify();
        }
    }

    fn on_scan_message(&mut self, message: ScanMsg) {
        match message {
            ScanMsg::Event(ScanEvent::Started { scanner_id }) => {
                self.scan
                    .statuses
                    .insert(scanner_id, ScannerStatus::Running);
            }
            ScanMsg::Event(ScanEvent::Finished {
                scanner_id,
                findings,
                ..
            }) => {
                let bytes = findings
                    .iter()
                    .filter(|finding| is_actionable(finding))
                    .map(|finding| finding.size)
                    .sum();
                self.scan.statuses.insert(
                    scanner_id,
                    ScannerStatus::Done {
                        count: findings.len(),
                        bytes,
                    },
                );
            }
            ScanMsg::Event(ScanEvent::Failed { scanner_id, error }) => {
                self.scan
                    .statuses
                    .insert(scanner_id, ScannerStatus::Failed(error));
            }
            ScanMsg::Finished(findings) => {
                self.scan.was_cancelled = self.scan.cancelling;
                self.scan.running = false;
                self.scan.cancelling = false;
                self.scan.cancel = None;
                self.scan.finished_once = true;
                self.scan.finished_at = Some(SystemTime::now());
                self.findings = findings;
                self.rebuild_finding_views();
                self.refresh_health();
            }
        }
    }

    /// Rebuilds the display rows of every area from `self.findings`.
    pub fn rebuild_finding_views(&mut self) {
        for area in Area::ALL {
            let rows = build_rows(&self.findings, area);
            let view = self.views.entry(area).or_default();
            view.list.reset(rows.len());
            view.rows = rows;
        }
    }
}

/// Sorts an area's findings into scanner sections and groups, largest first.
pub fn build_rows(findings: &[Finding], area: Area) -> Vec<FindingRow> {
    let mut by_scanner: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, finding) in findings.iter().enumerate() {
        if finding.area == area && !needs_full_disk_access(finding) {
            by_scanner
                .entry(&finding.scanner_id)
                .or_default()
                .push(index);
        }
    }
    let total_of = |members: &[usize]| -> u64 {
        members
            .iter()
            .filter(|&&index| is_actionable(&findings[index]))
            .map(|&index| findings[index].size)
            .fold(0u64, u64::saturating_add)
    };
    let mut scanners: Vec<(&str, Vec<usize>)> = by_scanner.into_iter().collect();
    scanners.sort_by(|a, b| total_of(&b.1).cmp(&total_of(&a.1)).then(a.0.cmp(b.0)));

    let mut rows = Vec::new();
    for (scanner_id, members) in scanners {
        let total = total_of(&members);
        let mut sorted = members;
        sorted.sort_by(|&a, &b| findings[b].size.cmp(&findings[a].size).then(a.cmp(&b)));
        if let [index] = sorted[..] {
            rows.push(FindingRow::Item {
                index,
                heading: Some(Heading::Scanner),
            });
            rows.push(FindingRow::Gap);
            continue;
        }
        rows.push(FindingRow::Scanner {
            scanner_id: scanner_id.to_string(),
            members: Rc::new(sorted.clone()),
            total,
        });

        let mut ungrouped = Vec::new();
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for index in sorted {
            match &findings[index].group {
                None => ungrouped.push(index),
                Some(name) => match groups.iter_mut().find(|(group, _)| group == name) {
                    Some((_, list)) => list.push(index),
                    None => groups.push((name.clone(), vec![index])),
                },
            }
        }
        rows.extend(ungrouped.into_iter().map(|index| FindingRow::Item {
            index,
            heading: None,
        }));
        groups.sort_by_key(|group| std::cmp::Reverse(total_of(&group.1)));
        for (name, list) in groups {
            if let [index] = list[..] {
                rows.push(FindingRow::Item {
                    index,
                    heading: Some(Heading::Group(name)),
                });
                continue;
            }
            let total = total_of(&list);
            rows.push(FindingRow::Group {
                title: name,
                members: Rc::new(list.clone()),
                total,
            });
            rows.extend(list.into_iter().map(|index| FindingRow::Item {
                index,
                heading: None,
            }));
        }
        rows.push(FindingRow::Gap);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(scanner: &str, key: &str, size: u64, group: Option<&str>) -> Finding {
        let mut finding = Finding::new(scanner, Area::System, key, key).size(size);
        finding.action = hm_core::Action::Trash {
            paths: vec![format!("/h/{key}").into()],
        };
        finding.group = group.map(str::to_string);
        finding
    }

    fn shape(findings: &[Finding], rows: &[FindingRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                FindingRow::Scanner { scanner_id, .. } => format!("S:{scanner_id}"),
                FindingRow::Group { title, .. } => format!("G:{title}"),
                FindingRow::Item { index, heading } => {
                    let title = &findings[*index].title;
                    match heading {
                        None => format!("I:{title}"),
                        Some(Heading::Scanner) => format!("I:{title}^S"),
                        Some(Heading::Group(group)) => format!("I:{title}^{group}"),
                    }
                }
                FindingRow::Gap => "-".to_string(),
            })
            .collect()
    }

    #[test]
    fn rows_are_grouped_and_largest_first() {
        let findings = vec![
            finding("small", "a", 1, None),
            finding("small", "e", 2, None),
            finding("big", "b", 10, Some("G1")),
            finding("big", "f", 5, Some("G1")),
            finding("big", "c", 100, None),
            finding("big", "d", 50, Some("G2")),
            finding("big", "g", 40, Some("G2")),
        ];
        let rows = build_rows(&findings, Area::System);
        assert_eq!(
            shape(&findings, &rows),
            [
                "S:big", "I:c", "G:G2", "I:d", "I:g", "G:G1", "I:b", "I:f", "-", "S:small", "I:e",
                "I:a", "-"
            ]
        );
    }

    #[test]
    fn a_lone_item_stands_in_for_its_heading() {
        let findings = vec![
            finding("alone", "a", 1, None),
            finding("big", "b", 10, Some("G1")),
            finding("big", "c", 100, None),
        ];
        let rows = build_rows(&findings, Area::System);
        assert_eq!(
            shape(&findings, &rows),
            ["S:big", "I:c", "I:b^G1", "-", "I:a^S", "-"]
        );
    }

    #[test]
    fn missing_access_notes_are_left_out_of_the_list() {
        let mut note = Finding::new("large", Area::System, "sin_acceso", "Falta acceso")
            .tag(hm_core::TAG_NEEDS_FULL_DISK_ACCESS);
        note.action = hm_core::Action::ReportOnly;
        let findings = vec![finding("caches", "a", 1, None), note];
        let rows = build_rows(&findings, Area::System);
        assert_eq!(shape(&findings, &rows), ["I:a^S", "-"]);
    }

    #[test]
    fn other_areas_are_left_out() {
        let findings = vec![finding("s", "a", 1, None)];
        assert!(build_rows(&findings, Area::Files).is_empty());
    }
}
