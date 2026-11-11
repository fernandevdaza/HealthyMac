//! The clean flow: review, then progress, then an honest result. Everything that removes anything
//! goes through here and through `hm_core::Cleaner`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::channel::mpsc;
use futures::StreamExt;
use gpui::Context;
use hm_core::audit::AuditLog;
use hm_core::{
    t, tf, Action, Area, CancelToken, CleanEvent, CleanOptions, CleanOutcome, CleanReport,
    CleanStatus, Cleaner, Finding, Recovery, Risk,
};
use hm_dev::types::ListeningPort;
use hm_system::types::AppInfo;

use crate::logic::selection::{is_actionable, stays_inside, Summary};
use crate::state::{ScrollPane, Shell};
use crate::ui::motion;

// Only one sheet exists at a time, inside an `Option`, so boxing would gain nothing.
#[allow(clippy::large_enum_variant)]
pub enum Sheet {
    Review(ReviewSheet),
    Cleaning(CleaningSheet),
    Result(ResultSheet),
    Terminate(TerminateSheet),
}

pub struct ReviewSheet {
    pub title: String,
    pub confirm_label: String,
    pub items: Vec<Finding>,
    pub summary: Summary,
    pub acknowledged: bool,
    /// Processes from `blocked_by` that are open right now. `None` while checking.
    pub running: Option<HashSet<String>>,
    pub uninstall: Option<UninstallOptions>,
    /// What the user must acknowledge, when the usual wording does not fit.
    pub acknowledgement: Option<String>,
    pub scroll: ScrollPane,
    pub serial: u64,
}

pub struct UninstallOptions {
    pub app: AppInfo,
    pub include_leftovers: bool,
}

pub struct CleaningSheet {
    pub titles: HashMap<String, String>,
    pub total: usize,
    pub index: usize,
    pub current: String,
    pub bytes_done: u64,
    /// What `bytes_done` was before the last item finished, so the number can glide to the new one.
    pub bytes_before: u64,
    /// What the selection adds up to, for "of N".
    pub expected: u64,
    /// Finished items, oldest first, for the running list.
    pub done: Vec<DoneItem>,
    pub cancel: CancelToken,
    pub cancelling: bool,
}

/// One finished item in the cleaning sheet's running list.
pub struct DoneItem {
    pub title: String,
    pub status: CleanStatus,
    pub bytes: u64,
}

pub struct ResultSheet {
    pub report: CleanReport,
    pub titles: HashMap<String, String>,
    pub attempted: usize,
    pub simulated: bool,
    pub cancelled: bool,
    pub scroll: ScrollPane,
    /// Rows of the details list, in display order.
    pub details: Vec<CleanOutcome>,
}

pub struct TerminateSheet {
    pub port: ListeningPort,
    pub force: bool,
    pub error: Option<String>,
    pub in_flight: bool,
}

enum CleanMsg {
    Event(CleanEvent),
    Finished(CleanReport),
}

impl Shell {
    /// Opens the review sheet. Whatever the caller passes, only what can actually be cleaned is kept.
    pub fn open_review(
        &mut self,
        title: impl Into<String>,
        confirm_label: impl Into<String>,
        items: Vec<Finding>,
        cx: &mut Context<Self>,
    ) {
        self.open_review_with(title, confirm_label, items, None, cx);
    }

    fn open_review_with(
        &mut self,
        title: impl Into<String>,
        confirm_label: impl Into<String>,
        items: Vec<Finding>,
        uninstall: Option<UninstallOptions>,
        cx: &mut Context<Self>,
    ) {
        let mut items: Vec<Finding> = items.into_iter().filter(is_actionable).collect();
        // What deserves a look goes first, however long the list.
        items.sort_by(|a, b| b.risk.cmp(&a.risk).then(b.size.cmp(&a.size)));
        if items.is_empty() {
            self.show_toast(
                t!(
                    "No hay nada que limpiar en lo que has elegido.",
                    "There's nothing to clean in what you chose."
                ),
                cx,
            );
            return;
        }
        let summary = Summary::of(&items);
        let processes: Vec<String> = items
            .iter()
            .flat_map(|finding| finding.blocked_by.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        self.toast_serial += 1;
        let serial = self.toast_serial;
        self.open_sheet(Sheet::Review(ReviewSheet {
            title: title.into(),
            confirm_label: confirm_label.into(),
            items,
            summary,
            acknowledged: false,
            running: None,
            uninstall,
            acknowledgement: None,
            scroll: ScrollPane::default(),
            serial,
        }));
        let demo = self.mode.demo;
        Shell::run_bg(
            cx,
            move || {
                Ok(processes
                    .into_iter()
                    .filter(|process| {
                        if demo {
                            process == "Xcode"
                        } else {
                            hm_core::exec::is_process_running(process)
                        }
                    })
                    .collect::<HashSet<String>>())
            },
            move |shell, result, _cx| {
                if let Some(Sheet::Review(review)) = &mut shell.sheet {
                    if review.serial == serial {
                        review.running = Some(result.unwrap_or_default());
                    }
                }
            },
        );
        cx.notify();
    }

    /// Reviews the current selection, optionally limited to one area.
    pub fn review_selection(&mut self, area: Option<Area>, cx: &mut Context<Self>) {
        let items: Vec<Finding> = self
            .findings
            .iter()
            .filter(|finding| area.is_none_or(|area| finding.area == area))
            .filter(|finding| self.selection.is_selected(finding))
            .cloned()
            .collect();
        self.open_review(
            t!("Revisa antes de limpiar", "Review before cleaning"),
            t!("Limpiar", "Clean"),
            items,
            cx,
        );
    }

    pub fn review_uninstall(&mut self, app: AppInfo, cx: &mut Context<Self>) {
        if app.is_system || app.uninstall_blocked.is_some() {
            let reason = app.uninstall_blocked.clone().unwrap_or_else(|| {
                t!(
                    "Las apps del sistema no se pueden desinstalar.",
                    "System apps can't be uninstalled."
                )
                .to_string()
            });
            self.show_toast(reason, cx);
            return;
        }
        let include_leftovers = true;
        let finding = self.uninstall_finding(&app, include_leftovers);
        let title = tf!("Desinstalar {}", "Uninstall {}", app.name);
        self.open_review_with(
            title,
            t!("Desinstalar", "Uninstall"),
            vec![finding],
            Some(UninstallOptions {
                app,
                include_leftovers,
            }),
            cx,
        );
    }

    fn uninstall_finding(&self, app: &AppInfo, include_leftovers: bool) -> Finding {
        if self.mode.demo {
            crate::demo::system::uninstall_finding(app, include_leftovers)
        } else {
            hm_system::apps::uninstall_finding(app, include_leftovers)
        }
    }

    pub fn toggle_uninstall_leftovers(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::Review(review)) = &self.sheet else {
            return;
        };
        let Some(options) = &review.uninstall else {
            return;
        };
        let include = !options.include_leftovers;
        let finding = self.uninstall_finding(&options.app, include);
        if let Some(Sheet::Review(review)) = &mut self.sheet {
            review.summary = Summary::of([&finding]);
            review.items = vec![finding];
            review.acknowledged = false;
            if let Some(options) = &mut review.uninstall {
                options.include_leftovers = include;
            }
        }
        cx.notify();
    }

    pub fn toggle_acknowledged(&mut self, cx: &mut Context<Self>) {
        if let Some(Sheet::Review(review)) = &mut self.sheet {
            review.acknowledged = !review.acknowledged;
            cx.notify();
        }
    }

    pub fn close_sheet(&mut self, cx: &mut Context<Self>) {
        // A clean in progress must be cancelled, not dismissed.
        if matches!(self.sheet, Some(Sheet::Cleaning(_))) {
            return;
        }
        self.dismiss_sheet(cx);
    }

    /// Shows `sheet`. Replacing an open one swaps its content in place; opening from nothing plays
    /// the full entrance.
    pub fn open_sheet(&mut self, sheet: Sheet) {
        if self.sheet.is_none() || self.sheet_closing {
            self.sheet_opened += 1;
        }
        self.sheet_closing = false;
        self.sheet = Some(sheet);
    }

    /// Plays the sheet out, then removes it.
    pub fn dismiss_sheet(&mut self, cx: &mut Context<Self>) {
        if self.sheet.is_none() || self.sheet_closing {
            return;
        }
        if motion::reduced() {
            self.sheet = None;
            cx.notify();
            return;
        }
        self.sheet_closing = true;
        let opened = self.sheet_opened;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(motion::pace::LEAVE + 20))
                .await;
            this.update(cx, |shell, cx| {
                if shell.sheet_closing && shell.sheet_opened == opened {
                    shell.sheet = None;
                    shell.sheet_closing = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Runs what the review sheet lists. Refuses if a required acknowledgement is missing.
    pub fn confirm_review(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::Review(review)) = &self.sheet else {
            return;
        };
        if review.summary.needs_acknowledgement() && !review.acknowledged {
            return;
        }
        let (items, outside) = self.split_for_test_home(review.items.clone());
        let titles: HashMap<String, String> = review
            .items
            .iter()
            .map(|finding| (finding.id.clone(), finding.title.clone()))
            .collect();
        let attempted = items.len();
        // What a test home must not touch still gets reported, so that the result adds up.
        let left_alone: Vec<CleanOutcome> = outside
            .iter()
            .map(|finding| CleanOutcome {
                finding_id: finding.id.clone(),
                status: CleanStatus::Skipped,
                freed_bytes: 0,
                trashed_bytes: 0,
                freed_is_estimate: false,
                message: Some(
                    t!("Queda fuera de la home de prueba", "Outside the test home").to_string(),
                ),
                errors: Vec::new(),
            })
            .collect();

        let cancel = CancelToken::new();
        self.open_sheet(Sheet::Cleaning(CleaningSheet {
            titles: titles.clone(),
            total: attempted,
            index: 0,
            current: items.first().map(|f| f.title.clone()).unwrap_or_default(),
            bytes_done: 0,
            bytes_before: 0,
            expected: items.iter().map(|f| f.size).fold(0, u64::saturating_add),
            done: Vec::new(),
            cancel: cancel.clone(),
            cancelling: false,
        }));

        let skipped_up_front = left_alone.len();
        let (tx, mut rx) = mpsc::unbounded::<CleanMsg>();
        if self.mode.demo {
            // A demo has no business near the real cleaner, so it is not even constructed.
            std::thread::spawn(move || {
                let report = simulate_clean(&items, &cancel, &|event| {
                    let _ = tx.unbounded_send(CleanMsg::Event(event));
                });
                let _ = tx.unbounded_send(CleanMsg::Finished(report));
            });
        } else {
            let options = CleanOptions {
                mode: self.ctx.config.removal_mode,
                dry_run: self.mode.dry_run,
            };
            let home = self.ctx.home.clone();
            let excluded = self.ctx.config.excluded_paths.clone();
            std::thread::spawn(move || {
                let audit = AuditLog::new(AuditLog::default_path(&home));
                let cleaner = Cleaner::new(home, &excluded, options).with_audit(audit);
                let sender = std::sync::Mutex::new(tx.clone());
                let report = cleaner.clean(&items, &cancel, &move |event| {
                    if let Ok(sender) = sender.lock() {
                        let _ = sender.unbounded_send(CleanMsg::Event(event));
                    }
                });
                let _ = tx.unbounded_send(CleanMsg::Finished(report));
            });
        }

        cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                match message {
                    CleanMsg::Event(event) => {
                        let alive = this.update(cx, |shell, cx| {
                            shell.on_clean_event(event);
                            cx.notify();
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                    CleanMsg::Finished(mut report) => {
                        report.outcomes.extend(left_alone);
                        this.update(cx, |shell, cx| {
                            shell.on_clean_finished(
                                report,
                                titles,
                                attempted + skipped_up_front,
                                cx,
                            );
                            cx.notify();
                        })
                        .ok();
                        return;
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Against a test home, anything that reaches outside it is set aside. Otherwise nothing is.
    fn split_for_test_home(&self, items: Vec<Finding>) -> (Vec<Finding>, Vec<Finding>) {
        match &self.mode.home_override {
            Some(home) => items
                .into_iter()
                .partition(|finding| stays_inside(finding, home)),
            None => (items, Vec::new()),
        }
    }

    pub fn cancel_clean(&mut self, cx: &mut Context<Self>) {
        if let Some(Sheet::Cleaning(sheet)) = &mut self.sheet {
            sheet.cancel.cancel();
            sheet.cancelling = true;
            cx.notify();
        }
    }

    fn on_clean_event(&mut self, event: CleanEvent) {
        let Some(Sheet::Cleaning(sheet)) = &mut self.sheet else {
            return;
        };
        match event {
            CleanEvent::Started {
                finding_id,
                index,
                total,
            } => {
                sheet.index = index;
                sheet.total = total;
                if let Some(title) = sheet.titles.get(&finding_id) {
                    sheet.current = title.clone();
                }
            }
            CleanEvent::Finished(outcome) => {
                let bytes = outcome.freed_bytes + outcome.trashed_bytes;
                sheet.bytes_before = sheet.bytes_done;
                sheet.bytes_done += bytes;
                let title = sheet
                    .titles
                    .get(&outcome.finding_id)
                    .cloned()
                    .unwrap_or_else(|| outcome.finding_id.clone());
                sheet.done.push(DoneItem {
                    title,
                    status: outcome.status,
                    bytes,
                });
            }
        }
    }

    fn on_clean_finished(
        &mut self,
        report: CleanReport,
        titles: HashMap<String, String>,
        attempted: usize,
        cx: &mut Context<Self>,
    ) {
        let simulated = self.mode.is_simulated();
        if !simulated {
            let cleaned: HashSet<String> = report
                .outcomes
                .iter()
                .filter(|outcome| outcome.status == CleanStatus::Done)
                .map(|outcome| outcome.finding_id.clone())
                .collect();
            crate::logic::selection::retain_unclean(&mut self.findings, &cleaned);
            self.rebuild_finding_views();
            self.refresh_health();
            self.invalidate_after_clean(cx);
        }
        let mut details: Vec<CleanOutcome> = report
            .outcomes
            .iter()
            .filter(|outcome| {
                !matches!(outcome.status, CleanStatus::Done | CleanStatus::WouldClean)
            })
            .cloned()
            .collect();
        details.sort_by_key(|outcome| match outcome.status {
            CleanStatus::Failed => 0,
            CleanStatus::Partial => 1,
            _ => 2,
        });
        let was_cancelling =
            matches!(&self.sheet, Some(Sheet::Cleaning(sheet)) if sheet.cancelling);
        let cancelled = was_cancelling || report.outcomes.len() < attempted;
        self.open_sheet(Sheet::Result(ResultSheet {
            report,
            titles,
            attempted,
            simulated,
            cancelled,
            scroll: ScrollPane::default(),
            details,
        }));
    }

    /// What was just removed may be listed on other pages, so those need a fresh look.
    fn invalidate_after_clean(&mut self, cx: &mut Context<Self>) {
        self.models.inventory.at = None;
        self.projects.projects.at = None;
        self.apps.apps.at = None;
        self.on_page_enter(cx);
    }

    /// Emptying the Trash is its own explicit step, reviewed like any other clean.
    pub fn review_empty_trash(&mut self, known_bytes: Option<u64>, cx: &mut Context<Self>) {
        let home = self.ctx.home.clone();
        let demo = self.mode.demo;
        Shell::run_bg(
            cx,
            move || {
                if demo {
                    return Ok(known_bytes.unwrap_or(0));
                }
                let cancel = CancelToken::new();
                Ok(hm_core::measure(&home.join(".Trash"), &cancel).bytes)
            },
            |shell, result, cx| {
                let bytes = result.unwrap_or(0);
                let finding = Finding::new(
                    "ui.empty_trash",
                    Area::System,
                    "trash",
                    t!("Vaciar la Papelera", "Empty Trash"),
                )
                .subtitle(t!(
                    "Todo lo que hay ahora en la Papelera",
                    "Everything in the Trash right now"
                ))
                .explanation(t!(
                    "Borra para siempre todo lo que hay en la Papelera, también lo que no pusiste \
                         ahí con HealthyMac. Es el paso que libera el espacio de verdad.",
                    "Permanently deletes everything in the Trash, including what you didn't put \
                         there with HealthyMac. This is the step that actually frees the space."
                ))
                .size(bytes)
                .risk(Risk::Review)
                .recovery(Recovery::None)
                .action(Action::EmptyTrash);
                shell.open_review(
                    t!("Vaciar la Papelera", "Empty Trash"),
                    t!("Vaciar la Papelera", "Empty Trash"),
                    vec![finding],
                    cx,
                );
                if let Some(Sheet::Review(review)) = &mut shell.sheet {
                    review.acknowledgement = Some(
                        t!(
                            "Entiendo que vaciar la Papelera es definitivo: lo que hay dentro no se puede recuperar.",
                            "I understand that emptying the Trash is permanent: what's inside can't be recovered."
                        )
                        .to_string(),
                    );
                }
            },
        );
    }

    // ---- Terminate a process, from the Ports page ------------------------------------------------

    pub fn open_terminate(&mut self, port: ListeningPort, cx: &mut Context<Self>) {
        self.open_sheet(Sheet::Terminate(TerminateSheet {
            port,
            force: false,
            error: None,
            in_flight: false,
        }));
        cx.notify();
    }

    pub fn toggle_terminate_force(&mut self, cx: &mut Context<Self>) {
        if let Some(Sheet::Terminate(sheet)) = &mut self.sheet {
            sheet.force = !sheet.force;
            cx.notify();
        }
    }

    pub fn confirm_terminate(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::Terminate(sheet)) = &mut self.sheet else {
            return;
        };
        let pid = sheet.port.pid;
        let force = sheet.force;
        let name = sheet.port.process_name.clone();
        if !self.mode.may_signal_processes() {
            self.dismiss_sheet(cx);
            self.show_toast(
                tf!(
                    "Simulación: no se ha enviado ninguna señal a {name}.",
                    "Simulation: no signal was sent to {name}."
                ),
                cx,
            );
            return;
        }
        sheet.in_flight = true;
        Shell::run_bg(
            cx,
            move || hm_dev::ports::terminate(pid, force),
            move |shell, result, cx| match result {
                Ok(()) => {
                    shell.dismiss_sheet(cx);
                    shell.show_toast(
                        tf!(
                            "{name} ha recibido la señal.",
                            "{name} received the signal."
                        ),
                        cx,
                    );
                    shell.load_ports(cx);
                }
                Err(error) => {
                    if let Some(Sheet::Terminate(sheet)) = &mut shell.sheet {
                        sheet.in_flight = false;
                        sheet.error = Some(error);
                    }
                }
            },
        );
        cx.notify();
    }
}

/// What the demo shows instead of a clean: no cleaner, no filesystem, only outcomes.
fn simulate_clean(
    items: &[Finding],
    cancel: &CancelToken,
    on_event: &dyn Fn(CleanEvent),
) -> CleanReport {
    let mut report = CleanReport::default();
    let pause = Duration::from_millis((1400 / items.len().max(1) as u64).clamp(25, 300));
    for (index, finding) in items.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        on_event(CleanEvent::Started {
            finding_id: finding.id.clone(),
            index,
            total: items.len(),
        });
        std::thread::sleep(pause);
        let mut outcome = CleanOutcome {
            finding_id: finding.id.clone(),
            status: CleanStatus::WouldClean,
            freed_bytes: 0,
            trashed_bytes: 0,
            freed_is_estimate: matches!(finding.action, Action::Command { .. }),
            message: None,
            errors: Vec::new(),
        };
        // By id, not title: the title is in whichever language the demo was built in.
        if finding.id == "apps.uninstall:Final Cut Pro" {
            outcome.status = CleanStatus::Failed;
            outcome
                .errors
                .push("/Applications/Final Cut Pro.app: Operation not permitted".to_string());
        } else if finding.blocked_by.iter().any(|process| process == "Xcode") {
            outcome.status = CleanStatus::Skipped;
            outcome.message = Some(t!("Xcode está abierto", "Xcode is open").to_string());
        } else {
            match finding.action {
                Action::Command { .. } | Action::CloneDedupe { .. } | Action::EmptyTrash => {
                    outcome.freed_bytes = finding.size
                }
                _ => outcome.trashed_bytes = finding.size,
            }
        }
        on_event(CleanEvent::Finished(outcome.clone()));
        report.outcomes.push(outcome);
    }
    report
}
