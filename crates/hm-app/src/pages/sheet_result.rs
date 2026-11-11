//! The result of a clean, honest about the difference between freed and merely moved to the Trash.

use gpui::{div, prelude::*, px, AnyElement, Context, ElementId, Hsla, IntoElement};
use hm_core::{t, tf, CleanStatus};

use super::plural;
use super::sheets::{sheet_header, MAX_LISTED, SHEET_WIDTH};
use crate::assets::Icon;
use crate::flows::clean::ResultSheet;
use crate::logic::errors::{blocked_by_app_management, friendly_error, APP_MANAGEMENT_URL};
use crate::state::Shell;
use crate::theme::{self, font, radius, sp};
use crate::ui::chrome::{eyebrow, notice, NoticeKind};
use crate::ui::controls::{pill, Btn, BtnSize};
use crate::ui::motion;
use crate::ui::scrollbar::{scrollbar, ScrollSource};
use crate::ui::{clip, col, row, TextExt};

impl Shell {
    pub(super) fn render_result(&self, result: &ResultSheet, cx: &mut Context<Self>) -> AnyElement {
        let freed = result.report.freed_bytes();
        let trashed = result.report.trashed_bytes();
        let estimated = result.report.has_estimates();
        let was_cut_short = |outcome: &hm_core::CleanOutcome| {
            outcome
                .errors
                .iter()
                .any(|error| is_cancelled_marker(error))
        };
        let failures = result
            .report
            .outcomes
            .iter()
            .filter(|outcome| {
                matches!(outcome.status, CleanStatus::Failed | CleanStatus::Partial)
                    && !was_cut_short(outcome)
            })
            .count();
        let done = result
            .report
            .outcomes
            .iter()
            .filter(|outcome| matches!(outcome.status, CleanStatus::Done | CleanStatus::WouldClean))
            .count();
        let skipped = result
            .report
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == CleanStatus::Skipped || was_cut_short(outcome))
            .count();
        let simulated = result.simulated;

        let title: &str = if simulated && result.cancelled {
            t!("Simulación cancelada", "Simulation cancelled")
        } else if simulated {
            t!("Simulación terminada", "Simulation finished")
        } else if failures > 0 {
            t!("Hecho, con algún problema", "Done, with some problems")
        } else if result.cancelled {
            t!("Limpieza cancelada", "Cleaning cancelled")
        } else if done == 0 {
            t!("No se ha limpiado nada", "Nothing was cleaned")
        } else {
            t!("Hecho", "Done")
        };
        let subtitle: String = if simulated && result.cancelled {
            t!(
                "Esto es lo que habría pasado hasta ahí. No se ha tocado nada.",
                "This is what would have happened up to that point. Nothing was touched."
            )
            .to_string()
        } else if simulated {
            t!(
                "Esto es lo que habría pasado. No se ha tocado nada.",
                "This is what would have happened. Nothing was touched."
            )
            .to_string()
        } else if result.cancelled {
            tf!(
                "Se ha parado a mitad: {} de {} estaban hechos.",
                "Stopped partway: {} of {} were done.",
                result.report.outcomes.len(),
                result.attempted
            )
        } else {
            tf!(
                "{} de {}.",
                "{} of {} done.",
                plural(
                    done,
                    t!("elemento listo", "item"),
                    t!("elementos listos", "items")
                ),
                result.attempted
            )
        };

        let needs_app_management = result.report.outcomes.iter().any(|outcome| {
            outcome
                .errors
                .iter()
                .any(|error| blocked_by_app_management(&outcome.finding_id, error))
        });
        let pane = result.scroll.clone();
        let hidden = result.details.len().saturating_sub(MAX_LISTED);

        let figures = col()
                    .w_full()
                    .px(sp(5.))
                    .pb(sp(4.))
                    .gap(sp(3.5))
                    .when(freed > 0 || trashed > 0, |this| {
                        // A figure at zero only earns its place when it is the point: nothing freed
                        // yet is worth saying, but "0 B in the Trash" after a dedupe is just noise.
                        this.child(
                            row()
                                .w(px(SHEET_WIDTH - 42.0))
                                .gap(sp(2.5))
                                .items_start()
                                .child(result_figure(
                                    match (simulated, estimated) {
                                        (true, true) => t!("Se habría liberado, aprox.", "Would be freed, approx."),
                                        (true, false) => t!("Se habría liberado", "Would be freed"),
                                        (false, true) => t!("Liberado ahora, aprox.", "Freed now, approx."),
                                        (false, false) => t!("Liberado ahora", "Freed now"),
                                    },
                                    freed,
                                    if freed > 0 { theme::green() } else { theme::text_3() },
                                    match (simulated, estimated) {
                                        (_, true) => t!(
                                            "Parte es lo que estimó el escaneo: herramientas como Docker o pnpm pueden liberar menos.",
                                            "Part of it is the scan's estimate: tools like Docker or pnpm may free less."
                                        ),
                                        (true, false) => t!("Espacio libre al momento.", "Freed right away."),
                                        (false, false) => t!("Ya es espacio libre en tu disco.", "Already free on your disk."),
                                    },
                                ))
                                .when(trashed > 0, |this| {
                                    this.child(result_figure(
                                        if simulated {
                                            t!("Habría ido a la Papelera", "Would have gone to the Trash")
                                        } else {
                                            t!("En la Papelera", "In the Trash")
                                        },
                                        trashed,
                                        theme::amber(),
                                        t!(
                                            "Aún ocupa espacio, y se puede recuperar. No queda libre hasta que vacíes la Papelera.",
                                            "Still takes up space, and you can get it back. It isn't freed until you empty the Trash."
                                        ),
                                    ))
                                }),
                        )
                    })
                    .when(needs_app_management, |this| {
                        this.child(notice(
                            NoticeKind::Warning,
                            t!("macOS protege esta app", "macOS protects this app"),
                            Some(
                                t!(
                                    "Para poder desinstalarla, concede a HealthyMac el permiso «Gestión de apps» en Ajustes del Sistema, dentro de Privacidad y seguridad, y vuelve a intentarlo.",
                                    "To uninstall it, give HealthyMac the “App Management” permission in System Settings, under Privacy & Security, then try again."
                                )
                                .into(),
                            ),
                            Some(
                                Btn::secondary(t!("Abrir Ajustes", "Open Settings"))
                                    .size(BtnSize::Sm)
                                    .icon(Icon::External)
                                    .build(
                                        "open-app-management",
                                        cx.listener(|_, _, _, cx| cx.open_url(APP_MANAGEMENT_URL)),
                                    )
                                    .into_any_element(),
                            ),
                        ))
                    })
                    .when(failures > 0 || skipped > 0, |this| {
                        this.child(
                            div()
                                .fs(font::CAPTION)
                                .text_color(theme::text_2())
                                .child(match (failures, skipped) {
                                    (0, 1) => t!("1 elemento se ha quedado sin tocar:", "1 item was left untouched:").to_string(),
                                    (0, others) => tf!(
                                        "{others} elementos se han quedado sin tocar:",
                                        "{others} items were left untouched:"
                                    ),
                                    (1, 0) => t!("1 elemento no ha ido bien:", "1 item had a problem:").to_string(),
                                    (bad, 0) => tf!("{bad} elementos no han ido bien:", "{bad} items had a problem:"),
                                    (1, 1) => t!(
                                        "1 elemento no ha ido bien y otro se ha quedado sin tocar:",
                                        "1 item had a problem and another was left untouched:"
                                    )
                                    .to_string(),
                                    (1, others) => tf!(
                                        "1 elemento no ha ido bien y otros {others} se han quedado sin tocar:",
                                        "1 item had a problem and {others} more were left untouched:"
                                    ),
                                    (bad, 1) => tf!(
                                        "{bad} elementos no han ido bien y otro se ha quedado sin tocar:",
                                        "{bad} items had a problem and another was left untouched:"
                                    ),
                                    (bad, others) => tf!(
                                        "{bad} elementos no han ido bien y otros {others} se han quedado sin tocar:",
                                        "{bad} items had a problem and {others} more were left untouched:"
                                    ),
                                }),
                        )
                    });

        let details =
            col()
                .w_full()
                .children(result.details.iter().take(MAX_LISTED).enumerate().map(
                    |(index, outcome)| {
                        let title = result
                            .titles
                            .get(&outcome.finding_id)
                            .cloned()
                            .unwrap_or_else(|| outcome.finding_id.clone());
                        outcome_row(index, title, outcome)
                    },
                ))
                .when(hidden > 0, |this| {
                    this.child(
                        div()
                            .px(sp(5.))
                            .py(sp(2.5))
                            .fs(font::CAPTION)
                            .text_color(theme::text_2())
                            .child(tf!(
                                "Y {} más.",
                                "{} not shown.",
                                plural(hidden, t!("elemento", "item"), t!("elementos", "items"))
                            )),
                    )
                });

        col()
            .relative()
            .flex_shrink()
            .min_h(px(0.))
            .child(sheet_header(title, &subtitle, cx))
            .child(
                div()
                    .id("result-body")
                    .flex_shrink()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&pane.handle)
                    .child(figures)
                    .child(details),
            )
            .child(scrollbar(
                ScrollSource::Handle(pane.handle.clone()),
                pane.bar.clone(),
            ))
            .child(
                row()
                    .flex_none()
                    .p(sp(5.))
                    .pt(sp(3.5))
                    .gap(sp(2.))
                    .justify_end()
                    .border_t_1()
                    .border_color(theme::hairline())
                    .when(trashed > 0, |this| {
                        this.child(
                            Btn::secondary(t!("Vaciar la Papelera…", "Empty Trash…"))
                                .icon(Icon::Trash)
                                .build(
                                    "empty-trash",
                                    cx.listener(move |this, _, _, cx| {
                                        this.review_empty_trash(Some(trashed), cx)
                                    }),
                                ),
                        )
                    })
                    .child(Btn::primary(t!("Listo", "Done")).build(
                        "result-done",
                        cx.listener(|this, _, _, cx| this.close_sheet(cx)),
                    )),
            )
            .into_any_element()
    }
}

fn result_figure(
    label: &'static str,
    bytes: u64,
    color: Hsla,
    detail: &'static str,
) -> impl IntoElement {
    col()
        .flex_1()
        .min_w(px(0.))
        .gap(sp(0.5))
        .p(sp(3.5))
        .rounded(px(radius::LG))
        .bg(theme::surface_1())
        .border_1()
        .border_color(theme::hairline())
        .child(eyebrow(label))
        // The result is read once, so its numbers may count up once as it appears.
        .child(motion::counting_size(
            format!("result-{label}"),
            0,
            bytes,
            font::TITLE,
            color,
        ))
        .child(
            div()
                .mt(sp(0.5))
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(detail),
        )
}

/// The error the cleaner records for an item it stopped at because the user cancelled. The engine
/// writes it in the language of the moment, so both are recognised.
fn is_cancelled_marker(error: &str) -> bool {
    matches!(error, "cancelado" | "cancelled" | "canceled")
}

/// The cleaner's note for an item skipped because its app is open, in either language.
fn is_open_app_note(message: &str) -> bool {
    message.ends_with("está abierto") || message.ends_with("is open")
}

fn outcome_row(index: usize, title: String, outcome: &hm_core::CleanOutcome) -> AnyElement {
    let cut_short = outcome
        .errors
        .iter()
        .any(|error| is_cancelled_marker(error));
    let (label, color) = match outcome.status {
        _ if cut_short => (t!("Cancelado", "Cancelled"), theme::text_3()),
        CleanStatus::Failed => (t!("Falló", "Failed"), theme::rose()),
        CleanStatus::Partial => (t!("A medias", "Partial"), theme::amber()),
        _ => (t!("Omitido", "Skipped"), theme::text_3()),
    };
    let reason = outcome
        .errors
        .first()
        .filter(|_| !cut_short)
        .map(|error| {
            if blocked_by_app_management(&outcome.finding_id, error) {
                t!(
                    "Falta el permiso «Gestión de apps».",
                    "Missing the “App Management” permission."
                )
                .to_string()
            } else {
                friendly_error(error)
            }
        })
        .or_else(|| {
            outcome.message.as_ref().map(|message| {
                if is_open_app_note(message) {
                    tf!(
                        "{message}. Ciérralo y vuelve a intentarlo.",
                        "{message}. Quit it and try again."
                    )
                } else {
                    message.clone()
                }
            })
        })
        .unwrap_or_else(|| {
            if cut_short {
                t!(
                    "Lo paraste antes de llegar a este.",
                    "You stopped it before it got to this one."
                )
                .to_string()
            } else {
                String::new()
            }
        });
    row()
        .id(ElementId::NamedInteger("outcome".into(), index as u64))
        .h(px(46.))
        .px(sp(5.))
        .gap(sp(2.5))
        .child(
            col()
                .flex_1()
                .min_w(px(0.))
                .child(clip().fs(font::CAPTION).child(title))
                .child(
                    clip()
                        .fs(font::MICRO)
                        .text_color(theme::text_3())
                        .child(reason),
                ),
        )
        .child(pill(label, color))
        .into_any_element()
}
