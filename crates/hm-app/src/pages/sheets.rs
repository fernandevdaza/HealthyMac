//! The modal sheets. A sheet is a column: fixed header, a body that scrolls if the window is short, and
//! a fixed footer holding the buttons, so the way out is always on screen.

use gpui::{
    deferred, div, prelude::*, px, AnimationExt, AnyElement, Context, ElementId, FontWeight, Window,
};
use hm_core::fmt::human_size;
use hm_core::{t, tf, CleanStatus};

use crate::assets::Icon;
use crate::flows::clean::{DoneItem, Sheet};
use crate::state::Shell;
use crate::theme::{self, font, radius, sp};
use crate::ui::charts::progress_bar;
use crate::ui::chrome::{big_number, notice, NoticeKind};
use crate::ui::controls::{icon_button, spinner, toggle, Btn};
use crate::ui::label::path_label;
use crate::ui::motion;
use crate::ui::{clip, col, f, icon, row, TextExt};

pub(super) const SHEET_WIDTH: f32 = 580.0;
/// Rows to lay out in a sheet. Anything past this is summed up in one line instead.
pub(super) const MAX_LISTED: usize = 150;

impl Shell {
    pub fn render_sheet(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let body: AnyElement = match self.sheet.as_ref()? {
            Sheet::Review(review) => self.render_review(review, window, cx),
            Sheet::Cleaning(_) => self.render_cleaning(cx),
            Sheet::Result(result) => self.render_result(result, cx),
            Sheet::Terminate(_) => self.render_terminate(cx),
        };
        let serial = match self.sheet.as_ref()? {
            Sheet::Review(_) => 1,
            Sheet::Cleaning(_) => 2,
            Sheet::Result(_) => 3,
            Sheet::Terminate(_) => 4,
        };
        let max_height = (f(window.viewport_size().height) - 64.0).max(320.0);
        let closing = self.sheet_closing;
        let opened = self.sheet_opened;
        // The scrim fades on its own layer, so the sheet can move with its own timing above it.
        let scrim = div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .bg(theme::scrim())
            .with_animation(
                ElementId::NamedInteger(
                    if closing { "scrim-out" } else { "scrim-in" }.into(),
                    opened,
                ),
                motion::animation(if closing {
                    motion::pace::LEAVE
                } else {
                    motion::pace::ENTER
                }),
                move |this, t| this.opacity(if closing { 1.0 - t } else { t }),
            );
        // Swapping one sheet for another (review, then cleaning, then the result) cross-fades the
        // content instead of replaying the whole entrance.
        // Not while leaving: the exit wraps the sheet in a different animation, which would
        // restart this fade from zero and blank the sheet as it goes.
        let body = div().flex().flex_col().min_h(px(0.)).child(body);
        let body = if closing {
            body.into_any_element()
        } else {
            body.with_animation(
                ElementId::NamedInteger("sheet-body".into(), serial * 1_000 + opened),
                motion::animation(motion::pace::QUICK),
                |this, t| this.opacity(t),
            )
            .into_any_element()
        };
        let sheet = col()
            .id("sheet")
            .occlude()
            .relative()
            .w(px(SHEET_WIDTH))
            .max_h(px(max_height))
            .rounded(px(radius::LG))
            .bg(theme::sheet_bg())
            .border_1()
            .border_color(theme::hairline_strong())
            .shadow(theme::sheet_shadow())
            .overflow_hidden()
            .child(body);
        let sheet = if closing {
            sheet
                .with_animation(
                    ElementId::NamedInteger("sheet-out".into(), opened),
                    motion::animation(motion::pace::LEAVE),
                    |this, t| this.opacity(1.0 - t).top(px(10.0 * t)),
                )
                .into_any_element()
        } else {
            // Fades in and settles 8 px, with no overshoot: the sheet is a step in a task, not a
            // show. The work inside it is where the motion goes.
            sheet
                .with_animation(
                    ElementId::NamedInteger("sheet-in".into(), opened),
                    motion::animation(motion::pace::OPEN),
                    |this, t| this.opacity(t).top(px(8.0 * (1.0 - t))),
                )
                .into_any_element()
        };
        Some(
            deferred(
                div()
                    .id("sheet-layer")
                    .occlude()
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .size_full()
                    .child(scrim)
                    .child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .left(px(0.))
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(sheet),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// The one moment in the app with motion of its own: the number processed glides to each new
    /// total, the bar slides between steps, and finished items scroll up through a short list with
    /// their outcome, like a ticker. Everything else in the app stays still.
    fn render_cleaning(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(Sheet::Cleaning(sheet)) = &self.sheet else {
            return div().into_any_element();
        };
        let total = sheet.total.max(1) as f32;
        let finished = sheet.done.len();
        let step = finished as u64;
        let fraction = finished as f32 / total;
        let fraction_before = finished.saturating_sub(1) as f32 / total;
        let (bytes_before, bytes_now) = (sheet.bytes_before, sheet.bytes_done);
        let simulated = self.mode.is_simulated();

        let number = div().with_animation(
            ElementId::NamedInteger("clean-bytes".into(), step),
            motion::animation(motion::pace::TICK),
            move |_, t| {
                let shown = bytes_before as f64 + (bytes_now - bytes_before) as f64 * t as f64;
                div().child(big_number(
                    human_size(shown.round() as u64),
                    font::DISPLAY,
                    theme::text_1(),
                ))
            },
        );
        let bar = div().w_full().with_animation(
            ElementId::NamedInteger("clean-bar".into(), step),
            motion::animation(motion::pace::TICK),
            move |_, t| {
                progress_bar(
                    fraction_before + (fraction - fraction_before) * t,
                    theme::primary(),
                    6.0,
                )
            },
        );

        col()
            .p(sp(6.))
            .gap(sp(4.))
            .child(
                row()
                    .gap(sp(2.5))
                    .child(spinner("clean-spin", 16.0, theme::text_2()))
                    .child(
                        div()
                            .fs(font::HEADLINE)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if simulated {
                                t!("Simulando…", "Simulating…")
                            } else {
                                t!("Limpiando…", "Cleaning…")
                            }),
                    ),
            )
            .child(
                col()
                    .gap(sp(2.5))
                    .child(row().items_end().gap(sp(2.5)).child(number).when(
                        sheet.expected > 0,
                        |this| {
                            this.child(
                                div()
                                    .pb(px(5.))
                                    .fs(font::CAPTION)
                                    .tabular()
                                    .text_color(theme::text_2())
                                    .child(tf!(
                                        "de {} elegidos",
                                        "of {} selected",
                                        human_size(sheet.expected)
                                    )),
                            )
                        },
                    ))
                    .child(bar)
                    .child(
                        row()
                            .justify_between()
                            .gap(sp(3.))
                            .fs(font::CAPTION)
                            .text_color(theme::text_3())
                            .child(clip().flex_1().child(sheet.current.clone()))
                            .child(div().flex_none().tabular().child(tf!(
                                "{} de {}",
                                "{} of {}",
                                (sheet.index + 1).min(sheet.total),
                                sheet.total
                            ))),
                    ),
            )
            .child(done_ticker(&sheet.done))
            .child(
                row().justify_end().child(
                    Btn::secondary(if sheet.cancelling {
                        t!("Terminando el actual…", "Finishing the current one…")
                    } else {
                        t!("Cancelar", "Cancel")
                    })
                    .disabled(sheet.cancelling)
                    .build(
                        "clean-cancel",
                        cx.listener(|this, _, _, cx| this.cancel_clean(cx)),
                    ),
                ),
            )
            .into_any_element()
    }

    fn render_terminate(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(Sheet::Terminate(sheet)) = &self.sheet else {
            return div().into_any_element();
        };
        let port = &sheet.port;
        let simulated = !self.mode.may_signal_processes();
        col()
            .child(sheet_header(
                &tf!("¿Terminar {}?", "Quit {}?", port.process_name),
                &tf!(
                    "Escucha en el puerto {} · PID {}",
                    "Listening on port {} · PID {}",
                    port.port,
                    port.pid
                ),
                cx,
            ))
            .child(
                col()
                    .px(sp(5.))
                    .pb(sp(4.))
                    .gap(sp(3.5))
                    .when_some(port.cwd.clone().filter(|cwd| cwd.as_os_str() != "/"), |this, cwd| {
                        this.child(path_label(cwd.display().to_string(), font::CAPTION, theme::text_2()))
                    })
                    .child(
                        div()
                            .fs(font::BODY)
                            .text_color(theme::text_2())
                            .child(t!(
                                "Se le pide que termine por las buenas (SIGTERM), para que pueda cerrar y guardar lo que tenga a medias.",
                                "It is asked to quit nicely (SIGTERM), so it can close and save any unfinished work."
                            )),
                    )
                    .child(
                        row()
                            .gap(sp(2.5))
                            .items_start()
                            .child(toggle(
                                "force-kill",
                                sheet.force,
                                theme::rose(),
                                cx.listener(|this, _, _, cx| this.toggle_terminate_force(cx)),
                            ))
                            .child(
                                col()
                                    .flex_1()
                                    .child(div().fs(font::BODY).child(t!("Forzar (SIGKILL)", "Force (SIGKILL)")))
                                    .child(
                                        div()
                                            .fs(font::CAPTION)
                                            .text_color(theme::text_2())
                                            .child(t!(
                                                "Solo si no responde: lo corta en seco y puede perder lo que no haya guardado.",
                                                "Only if it doesn't respond: it stops at once, and unsaved work may be lost."
                                            )),
                                    ),
                            ),
                    )
                    .when(simulated, |this| {
                        this.child(notice(
                            NoticeKind::Info,
                            t!(
                                "SIMULACIÓN: no se va a enviar ninguna señal",
                                "SIMULATION: no signal will be sent"
                            ),
                            None,
                            None,
                        ))
                    })
                    .when_some(sheet.error.clone(), |this, error| {
                        this.child(notice(
                            NoticeKind::Error,
                            t!("No se ha podido terminar", "Couldn't quit the process"),
                            Some(error.into()),
                            None,
                        ))
                    }),
            )
            .child(
                row()
                    .p(sp(5.))
                    .pt(px(0.))
                    .gap(sp(2.))
                    .justify_end()
                    .child(Btn::ghost(t!("Cancelar", "Cancel")).build(
                        "terminate-cancel",
                        cx.listener(|this, _, _, cx| this.close_sheet(cx)),
                    ))
                    .child(
                        Btn::danger(if sheet.force {
                            t!("Forzar cierre", "Force quit")
                        } else {
                            t!("Terminar", "Quit")
                        })
                            .disabled(sheet.in_flight)
                            .build(
                                "terminate-confirm",
                                cx.listener(|this, _, _, cx| this.confirm_terminate(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

pub(super) fn sheet_header(
    title: &str,
    subtitle: &str,
    cx: &mut Context<Shell>,
) -> impl IntoElement {
    row()
        .flex_none()
        .px(sp(5.))
        .pt(sp(4.5))
        .pb(sp(3.))
        .gap(sp(3.))
        .items_start()
        .child(
            col()
                .flex_1()
                .min_w(px(0.))
                .gap(sp(0.5))
                .child(
                    div()
                        .fs(font::HEADLINE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .fs(font::BODY)
                        .text_color(theme::text_2())
                        .child(subtitle.to_string()),
                ),
        )
        .child(icon_button(
            "sheet-close",
            Icon::X,
            theme::text_3(),
            cx.listener(|this, _, _, cx| this.close_sheet(cx)),
        ))
}

const TICKER_ROW: f32 = 28.0;
const TICKER_ROWS: usize = 4;

/// The last few finished items, newest at the bottom. When one arrives the list slides up a row,
/// so the eye follows the work instead of seeing rows jump.
fn done_ticker(done: &[DoneItem]) -> impl IntoElement {
    let shown = done.len().min(TICKER_ROWS + 1);
    let first = done.len() - shown;
    let sliding = done.len() > TICKER_ROWS;
    let rows: Vec<AnyElement> = done[first..]
        .iter()
        .enumerate()
        .map(|(offset, item)| {
            let (which, color) = match item.status {
                CleanStatus::Done | CleanStatus::WouldClean => (Icon::Check, theme::green()),
                CleanStatus::Skipped => (Icon::Minus, theme::text_3()),
                CleanStatus::Partial => (Icon::Alert, theme::amber()),
                CleanStatus::Failed => (Icon::Alert, theme::rose()),
            };
            let row_el = row()
                .flex_none()
                .h(px(TICKER_ROW))
                .gap(sp(2.5))
                .child(icon(which, 14.0, color))
                .child(
                    clip()
                        .flex_1()
                        .fs(font::BODY)
                        .text_color(theme::text_2())
                        .child(item.title.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .fs(font::CAPTION)
                        .tabular()
                        .text_color(theme::text_3())
                        .child(if item.bytes > 0 {
                            human_size(item.bytes)
                        } else {
                            String::new()
                        }),
                );
            // Until the list is full there is nothing to slide, so the newest row fades in.
            if !sliding && first + offset + 1 == done.len() {
                row_el
                    .with_animation(
                        ElementId::NamedInteger("ticker-new".into(), done.len() as u64),
                        motion::animation(motion::pace::QUICK),
                        |this, t| this.opacity(t),
                    )
                    .into_any_element()
            } else {
                row_el.into_any_element()
            }
        })
        .collect();
    let list = col().children(rows);
    let list = if sliding {
        list.with_animation(
            ElementId::NamedInteger("ticker".into(), done.len() as u64),
            motion::animation(motion::pace::TICK),
            |this, t| this.relative().top(px(-TICKER_ROW * t)),
        )
        .into_any_element()
    } else {
        list.into_any_element()
    };
    div()
        .h(px(TICKER_ROW * TICKER_ROWS as f32))
        .overflow_hidden()
        .border_t_1()
        .border_color(theme::hairline())
        .pt(sp(1.))
        .child(list)
}
