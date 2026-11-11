//! The dashboard's storage list, a row per area under the disk bar, and every scanner's progress
//! while one runs.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString};
use hm_core::fmt::human_size;
use hm_core::{t, tf, Area};

use super::resumen::section_title_row;
use crate::assets::Icon;
use crate::logic::selection::is_actionable;
use crate::state::{AppsTab, Page, ScannerStatus, Shell};
use crate::theme::{self, font, radius, sp};
use crate::ui::controls::{dot, spinner};
use crate::ui::{clip, col, icon, row, TextExt};

/// The areas in the order they appear on the dashboard.
pub const AREAS: [Area; 5] = [
    Area::System,
    Area::Developer,
    Area::LocalAi,
    Area::Apps,
    Area::Files,
];

impl Shell {
    /// The disk's breakdown as a list, the way System Settings > Storage shows it: a row per area,
    /// keyed to the bar above by its colour, then what else is in use and what is free. It replaces
    /// both the bar's legend and the old tiles, which showed the same numbers twice.
    ///
    /// `rest` is the "everything else" label and size, plus free space, when the disk is known.
    pub(super) fn storage_list(
        &self,
        rest: Option<(&'static str, u64, u64)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = AREAS
            .iter()
            .map(|&area| self.storage_row(area, cx).into_any_element())
            .collect();
        if let Some((label, other, free)) = rest {
            rows.push(plain_row(theme::surface_4(), label, human_size(other)));
            rows.push(plain_row(
                theme::surface_2(),
                t!("Libre", "Free"),
                human_size(free),
            ));
        }
        let count = rows.len();
        col()
            .w_full()
            .rounded(px(radius::MD))
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .children(rows.into_iter().enumerate().map(|(index, row_el)| {
                div()
                    .when(index + 1 < count, |this| {
                        this.border_b_1().border_color(theme::hairline())
                    })
                    .child(row_el)
            }))
    }

    fn storage_row(&self, area: Area, cx: &mut Context<Self>) -> impl IntoElement {
        let scanned = self.scan.finished_once;
        let members: Vec<_> = self
            .findings
            .iter()
            .filter(|f| f.area == area && is_actionable(f))
            .collect();
        let total: u64 = members.iter().map(|f| f.size).fold(0, u64::saturating_add);
        let selected = self.selection.summarize(members.iter().copied());
        row()
            .id(SharedString::from(format!("storage-{area:?}")))
            .h(px(54.))
            .px(sp(3.5))
            .gap(sp(3.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_1()))
            .active(|style| style.bg(theme::surface_2()))
            .on_click(cx.listener(move |this, _, _, cx| {
                if area == Area::Apps {
                    this.apps.tab = AppsTab::Leftovers;
                    this.navigate(Page::Apps, cx);
                } else {
                    this.navigate(Page::Area(area), cx);
                }
            }))
            .child(dot(theme::area_color(area), 9.0))
            .child(
                col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .fs(font::BODY)
                            .font_weight(FontWeight::MEDIUM)
                            .child(area.label()),
                    )
                    .child(
                        clip()
                            .fs(font::CAPTION)
                            .text_color(theme::text_3())
                            .child(tile_blurb(area)),
                    ),
            )
            .when(scanned, |this| {
                this.child(
                    col()
                        .flex_none()
                        .items_end()
                        .child(
                            div()
                                .fs(font::BODY)
                                .font_weight(FontWeight::SEMIBOLD)
                                .tabular()
                                .text_color(if total == 0 {
                                    theme::text_3()
                                } else {
                                    theme::text_1()
                                })
                                .child(if total == 0 {
                                    t!("Nada", "Nothing").to_string()
                                } else {
                                    human_size(total)
                                }),
                        )
                        .child(
                            div()
                                .fs(font::MICRO)
                                .tabular()
                                .text_color(theme::text_3())
                                .child(if total == 0 {
                                    t!("Todo limpio", "All clean").to_string()
                                } else if selected.count > 0 {
                                    tf!("{} elegidos", "{} selected", selected.total_label())
                                } else {
                                    t!("Sin elegir", "None selected").to_string()
                                }),
                        ),
                )
            })
            .child(icon(Icon::ChevronRight, 13.0, theme::text_3()))
    }

    /// Every scanner and what it is doing right now, by area.
    pub fn render_live_scan(&self) -> impl IntoElement {
        let mut scanners = self.scanners.clone();
        scanners.sort_by_key(|meta| AREAS.iter().position(|area| *area == meta.area));
        let (done, total) = self.scan.progress();
        col()
            .gap(sp(3.))
            .child(section_title_row(if self.scan.running {
                t!("Revisando tu Mac", "Checking your Mac")
            } else {
                t!(
                    "Lo que dio tiempo a revisar",
                    "What it checked before stopping"
                )
            }))
            .when(total > 0 && done == total && self.scan.running, |this| {
                this.child(
                    div()
                        .fs(font::CAPTION)
                        .text_color(theme::text_2())
                        .child(t!("Atando cabos…", "Putting it all together…")),
                )
            })
            .child(
                div()
                    .grid()
                    .grid_cols(3)
                    .w_full()
                    .gap_x(sp(5.))
                    .gap_y(sp(0.5))
                    .children(scanners.into_iter().map(|meta| {
                        let status = self
                            .scan
                            .statuses
                            .get(meta.id)
                            .cloned()
                            .unwrap_or(ScannerStatus::Waiting);
                        scanner_line(meta.id, meta.name, status)
                    })),
            )
    }
}

fn tile_blurb(area: Area) -> &'static str {
    match area {
        Area::System => t!(
            "Cachés, registros y restos del sistema",
            "Caches, logs and system leftovers"
        ),
        Area::Developer => t!(
            "Compilaciones, dependencias y Docker",
            "Builds, dependencies and Docker"
        ),
        Area::LocalAi => t!(
            "Modelos repetidos y cachés de IA",
            "Duplicate models and AI caches"
        ),
        Area::Apps => t!(
            "Restos de apps que ya no tienes",
            "Leftovers from apps you removed"
        ),
        Area::Files => t!(
            "Archivos grandes y descargas viejas",
            "Large files and old downloads"
        ),
    }
}

fn scanner_line(id: &'static str, name: &'static str, status: ScannerStatus) -> impl IntoElement {
    let status_icon: AnyElement = match &status {
        ScannerStatus::Waiting => dot(theme::surface_4(), 6.0).mx(sp(1.)).into_any_element(),
        ScannerStatus::Running => spinner(
            SharedString::from(format!("spin-{id}")),
            14.0,
            theme::text_2(),
        )
        .into_any_element(),
        ScannerStatus::Done { .. } => icon(Icon::Check, 14.0, theme::text_2()).into_any_element(),
        ScannerStatus::Failed(_) => icon(Icon::Alert, 14.0, theme::amber()).into_any_element(),
    };
    let trailing = match &status {
        ScannerStatus::Done { bytes, .. } if *bytes > 0 => human_size(*bytes),
        ScannerStatus::Done { .. } => t!("Nada", "Nothing").to_string(),
        ScannerStatus::Failed(_) => t!("Falló", "Failed").to_string(),
        _ => String::new(),
    };
    let waiting = matches!(status, ScannerStatus::Waiting);
    row()
        .min_w(px(0.))
        .h(px(28.))
        .gap(sp(2.))
        .child(row().w(px(16.)).justify_center().child(status_icon))
        .child(
            clip()
                .flex_1()
                .fs(font::CAPTION)
                .text_color(if waiting {
                    theme::text_3()
                } else {
                    theme::text_1()
                })
                .child(name),
        )
        .child(
            div()
                .flex_none()
                .fs(font::CAPTION)
                .tabular()
                .text_color(theme::text_2())
                .child(trailing),
        )
}

/// A row with nothing behind it to open: what else is in use, and free space.
fn plain_row(color: gpui::Hsla, label: &'static str, value: String) -> AnyElement {
    row()
        .h(px(40.))
        .px(sp(3.5))
        .gap(sp(3.))
        .child(
            div()
                .flex_none()
                .size(px(9.))
                .rounded_full()
                .bg(color)
                .border_1()
                .border_color(theme::hairline_strong()),
        )
        .child(
            div()
                .flex_1()
                .fs(font::BODY)
                .text_color(theme::text_2())
                .child(label),
        )
        .child(
            div()
                .flex_none()
                .mr(px(25.))
                .fs(font::BODY)
                .tabular()
                .text_color(theme::text_2())
                .child(value),
        )
        .into_any_element()
}
