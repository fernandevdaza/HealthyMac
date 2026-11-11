//! The findings of one area: grouped, selectable and explained.

use gpui::{
    div, list, prelude::*, px, AnimationExt, AnyElement, Context, ElementId, FontWeight,
    SharedString, Window,
};
use hm_core::fmt::{human_age, human_size};
use hm_core::{t, tf, Area, Finding};

use super::{content_width, plural, FULL_DISK_ACCESS_URL};
use crate::assets::Icon;
use crate::logic::selection::{is_actionable, needs_full_disk_access, GroupState};
use crate::state::{FindingRow, Heading, Shell};
use crate::theme::{self, font, radius, sp, GUTTER};
use crate::ui::chrome::{big_number, empty_state, notice, page_header, NoticeKind};
use crate::ui::controls::{checkbox, pill, tag, Btn, BtnSize};
use crate::ui::motion;
use crate::ui::scrollbar::{scrollbar, ScrollSource};
use crate::ui::{clip, clip_middle, col, icon, row, TextExt};

pub fn area_blurb(area: Area) -> &'static str {
    match area {
        Area::System => t!(
            "Cachés, registros y restos que macOS y tus apps van dejando atrás.",
            "Caches, logs and leftovers that macOS and your apps leave behind."
        ),
        Area::Developer => t!(
            "Lo que tus herramientas acumulan: casi todo se regenera al compilar.",
            "What your tools pile up. Nearly all of it is rebuilt when you compile."
        ),
        Area::LocalAi => t!(
            "Cachés de modelos, duplicados y restos de experimentos.",
            "Model caches, duplicates and leftovers from experiments."
        ),
        Area::Apps => t!(
            "Restos de apps que ya no están en tu Mac.",
            "Leftovers from apps that are no longer on your Mac."
        ),
        Area::Files => t!(
            "Archivos grandes y descargas viejas. Aquí mandas tú: mira antes de borrar.",
            "Large files and old downloads. You decide here: look before you delete."
        ),
    }
}

impl Shell {
    pub fn render_area(
        &mut self,
        area: Area,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let actions = vec![self.rescan_button(cx)];
        let header = page_header(area.label(), area_blurb(area), actions);
        col()
            .size_full()
            .child(header)
            .child(self.render_findings_body(area, window, cx))
            .into_any_element()
    }

    pub fn rescan_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.scan.running {
            Btn::secondary(if self.scan.cancelling {
                t!("Cancelando…", "Canceling…")
            } else {
                t!("Cancelar escaneo", "Cancel scan")
            })
            .icon(Icon::Stop)
            .disabled(self.scan.cancelling)
            .build("rescan", cx.listener(|this, _, _, cx| this.cancel_scan(cx)))
            .into_any_element()
        } else {
            Btn::secondary(if self.scan.finished_once {
                t!("Escanear de nuevo", "Scan again")
            } else {
                t!("Escanear", "Scan")
            })
            .icon(if self.scan.finished_once {
                Icon::Refresh
            } else {
                Icon::Scan
            })
            .build("rescan", cx.listener(|this, _, _, cx| this.start_scan(cx)))
            .into_any_element()
        }
    }

    /// The list, or the right placeholder. Shared with the Leftovers tab on the Apps page.
    pub fn render_findings_body(
        &mut self,
        area: Area,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = theme::text_2();
        let view = self.views.entry(area).or_default();
        if view.rows.is_empty() {
            if self.scan.running {
                return div()
                    .id("live-scan")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(GUTTER))
                    .child(self.render_live_scan())
                    .into_any_element();
            }
            let (title, message) = if self.scan.finished_once {
                (
                    t!("Por aquí todo limpio", "All clean here"),
                    t!(
                        "El último escaneo no ha encontrado nada en esta sección. Buenas noticias.",
                        "The last scan found nothing in this section. Good news."
                    ),
                )
            } else {
                (
                    t!("Todavía no hemos mirado", "We haven't looked yet"),
                    t!(
                        "Escanea y te contaremos qué hay, cuánto ocupa y qué pasa si lo quitas. No se toca nada hasta que tú lo digas.",
                        "Scan and we'll tell you what's here, how much space it takes and what happens if you remove it. Nothing is touched until you say so."
                    ),
                )
            };
            let action = (!self.scan.finished_once).then(|| {
                Btn::primary(t!("Escanear", "Scan"))
                    .size(BtnSize::Lg)
                    .icon(Icon::Scan)
                    .build(
                        "area-scan",
                        cx.listener(|this, _, _, cx| this.start_scan(cx)),
                    )
                    .into_any_element()
            });
            let which = if self.scan.finished_once {
                Icon::Shield
            } else {
                Icon::Scan
            };
            let empty = empty_state(which, accent, title, message, action);
            return match self.access_notice(area, cx) {
                Some(access) => col()
                    .size_full()
                    .child(div().flex_none().px(px(GUTTER)).child(access))
                    .child(empty)
                    .into_any_element(),
                None => empty.into_any_element(),
            };
        }

        let list_state = view.list.clone();
        let bar = view.bar.clone();
        let members: Vec<&Finding> = self.findings.iter().filter(|f| f.area == area).collect();
        let summary = self.selection.summarize(members.iter().copied());
        let available = crate::logic::selection::Summary::of(
            members.iter().copied().filter(|f| is_actionable(f)),
        );
        let total = available.bytes;
        let actionable = available.count;
        let state = self.selection.state_of(members.iter().copied());
        let access = self.access_notice(area, cx);

        col()
            .flex_1()
            .min_h(px(0.))
            .child(
                row()
                    .flex_none()
                    .px(px(GUTTER))
                    .pb(sp(3.5))
                    .gap(sp(3.5))
                    .items_end()
                    .child(big_number(
                        human_size(total),
                        font::DISPLAY,
                        theme::text_1(),
                    ))
                    .child(
                        col().flex_1().min_w(px(0.)).pb(px(5.)).child(
                            clip()
                                .fs(font::CAPTION)
                                .text_color(theme::text_2())
                                .child(tf!(
                                    "{}se pueden recuperar, en {}",
                                    "{}can be freed up across {}",
                                    if available.is_lower_bound() {
                                        t!("como mínimo ", "or more ")
                                    } else {
                                        ""
                                    },
                                    plural(
                                        actionable,
                                        t!("hallazgo", "item"),
                                        t!("hallazgos", "items")
                                    )
                                )),
                        ),
                    )
                    .child(
                        div().pb(sp(1.)).child(
                            Btn::ghost(if state == GroupState::All {
                                t!("Quitar todo", "Deselect all")
                            } else {
                                t!("Elegir todo", "Select all")
                            })
                            .size(BtnSize::Sm)
                            .disabled(state == GroupState::Unavailable)
                            .build(
                                "select-all-area",
                                cx.listener(move |this, _, _, cx| {
                                    this.toggle_all_in_area(area, cx)
                                }),
                            ),
                        ),
                    ),
            )
            .when_some(access, |this, access| {
                this.child(div().flex_none().px(px(GUTTER)).pb(sp(3.)).child(access))
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .border_t_1()
                    .border_color(theme::hairline())
                    .child(
                        list(
                            list_state.clone(),
                            cx.processor(move |this: &mut Shell, index: usize, window, cx| {
                                this.render_finding_row(area, index, window, cx)
                            }),
                        )
                        .size_full(),
                    )
                    .child(scrollbar(ScrollSource::List(list_state), bar)),
            )
            .when(summary.count > 0, |this| {
                this.child(self.render_selection_bar(Some(area), summary, cx))
            })
            .into_any_element()
    }

    /// One notice for every part of this area a scanner skipped for lack of Full Disk Access, with
    /// the way to grant it. `None` when nothing was skipped.
    fn access_notice(&self, area: Area, cx: &mut Context<Self>) -> Option<AnyElement> {
        let skipped: Vec<&Finding> = self
            .findings
            .iter()
            .filter(|f| f.area == area && needs_full_disk_access(f))
            .collect();
        let detail = match skipped.as_slice() {
            [] => return None,
            [only] if !only.explanation.is_empty() => only.explanation.clone(),
            _ => {
                let mut names: Vec<String> = skipped
                    .iter()
                    .map(|f| self.scanner_name(&f.scanner_id))
                    .collect();
                names.dedup();
                tf!(
                    "Sin ese permiso, parte de {} se queda fuera del escaneo. Se concede en Ajustes del Sistema y se puede retirar cuando quieras.",
                    "Without that permission, part of {} is left out of the scan. You grant it in System Settings and can take it back at any time.",
                    join_names(&names)
                )
            }
        };
        let open = Btn::secondary(t!("Abrir Ajustes", "Open Settings"))
            .size(BtnSize::Sm)
            .icon(Icon::External)
            .build(
                "area-fda-open",
                cx.listener(|_, _, _, cx| cx.open_url(FULL_DISK_ACCESS_URL)),
            );
        Some(
            notice(
                NoticeKind::Info,
                t!(
                    "Falta el Acceso total al disco",
                    "Full Disk Access is missing"
                ),
                Some(detail.into()),
                Some(open.into_any_element()),
            )
            .into_any_element(),
        )
    }

    fn toggle_all_in_area(&mut self, area: Area, cx: &mut Context<Self>) {
        let members: Vec<&Finding> = self.findings.iter().filter(|f| f.area == area).collect();
        let select = self.selection.state_of(members.iter().copied()) != GroupState::All;
        self.selection.set_all(members.iter().copied(), select);
        cx.notify();
    }

    fn toggle_members(&mut self, members: &[usize], cx: &mut Context<Self>) {
        let select = self
            .selection
            .state_of(members.iter().map(|&index| &self.findings[index]))
            != GroupState::All;
        for &index in members {
            self.selection.set(&self.findings[index], select);
        }
        cx.notify();
    }

    fn render_finding_row(
        &mut self,
        area: Area,
        row_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row_data) = self
            .views
            .get(&area)
            .and_then(|view| view.rows.get(row_index))
            .cloned()
        else {
            return div().into_any_element();
        };
        match row_data {
            FindingRow::Scanner {
                scanner_id,
                members,
                total,
            } => {
                let meta = self
                    .scanners
                    .iter()
                    .find(|meta| meta.id == scanner_id)
                    .cloned();
                let name = meta
                    .as_ref()
                    .map(|m| m.name.to_string())
                    .unwrap_or(scanner_id.clone());
                let description = meta.map(|m| m.description.to_string()).unwrap_or_default();
                let state = self
                    .selection
                    .state_of(members.iter().map(|&index| &self.findings[index]));
                let group_members = members.clone();
                row()
                    .w_full()
                    .pt(sp(4.))
                    .pb(sp(1.5))
                    .px(px(GUTTER - 4.0))
                    .gap(sp(2.))
                    .child(if state == GroupState::Unavailable {
                        div().size(px(24.)).flex_none().into_any_element()
                    } else {
                        checkbox(
                            ElementId::NamedInteger("scanner-check".into(), row_index as u64),
                            state,
                            cx.listener(move |this, _, _, cx| {
                                this.toggle_members(&group_members, cx)
                            }),
                        )
                        .into_any_element()
                    })
                    .child(
                        col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                clip()
                                    .fs(font::BODY)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_1())
                                    .child(name),
                            )
                            .child(
                                clip()
                                    .fs(font::CAPTION)
                                    .text_color(theme::text_3())
                                    .child(description),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .mr(px(31.))
                            .fs(font::BODY)
                            .font_weight(FontWeight::MEDIUM)
                            .tabular()
                            .text_color(theme::text_2())
                            .child(if total > 0 {
                                human_size(total)
                            } else {
                                String::new()
                            }),
                    )
                    .into_any_element()
            }
            FindingRow::Group {
                title,
                members,
                total,
            } => {
                let state = self
                    .selection
                    .state_of(members.iter().map(|&index| &self.findings[index]));
                let group_members = members.clone();
                row()
                    .w_full()
                    .pt(sp(2.))
                    .pb(sp(0.5))
                    .pl(px(GUTTER + 12.0))
                    .pr(px(GUTTER - 4.0))
                    .gap(sp(2.))
                    .child(if state == GroupState::Unavailable {
                        div().size(px(24.)).flex_none().into_any_element()
                    } else {
                        checkbox(
                            ElementId::NamedInteger("group-check".into(), row_index as u64),
                            state,
                            cx.listener(move |this, _, _, cx| {
                                this.toggle_members(&group_members, cx)
                            }),
                        )
                        .into_any_element()
                    })
                    .child(
                        clip()
                            .flex_1()
                            .fs(font::CAPTION)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_2())
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .mr(px(31.))
                            .fs(font::CAPTION)
                            .tabular()
                            .text_color(theme::text_3())
                            .child(human_size(total)),
                    )
                    .into_any_element()
            }
            FindingRow::Item { index, heading } => {
                self.render_finding_item(area, row_index, index, heading, window, cx)
            }
            FindingRow::Gap => div().h(px(8.)).into_any_element(),
        }
    }

    fn render_finding_item(
        &mut self,
        area: Area,
        row_index: usize,
        index: usize,
        heading: Option<Heading>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let finding = &self.findings[index];
        let stands_for_scanner = heading == Some(Heading::Scanner);
        let nested = finding.group.is_some() && !stands_for_scanner;
        // A lone item takes its group's name, or keeps its own and says which scanner found it.
        let title: SharedString = match &heading {
            Some(Heading::Group(name)) => name.clone().into(),
            _ => finding.title.clone().into(),
        };
        let context = if stands_for_scanner {
            let name = self.scanner_name(&finding.scanner_id);
            let (lower_name, lower_title) = (name.to_lowercase(), finding.title.to_lowercase());
            let repeats = lower_name.contains(&lower_title) || lower_title.contains(&lower_name);
            (!repeats).then_some(name)
        } else {
            None
        };
        let view = &self.views[&area];
        let expanded = view.expanded.contains(&finding.id);
        let actionable = is_actionable(finding);
        let selected = self.selection.is_selected(finding);
        let id = finding.id.clone();
        let wide = content_width(window) > 860.0;
        let age = finding
            .last_used
            .map(|when| human_age(when, std::time::SystemTime::now()));
        let subtitle: SharedString = finding.subtitle.clone().unwrap_or_default().into();
        let shared = (finding.shared_bytes > 0).then(|| {
            tf!(
                "+{} compartidos",
                "+{} shared",
                human_size(finding.shared_bytes)
            )
        });
        let toggle_id = id.clone();
        let risk_color = theme::risk_color(finding.risk);

        col()
            .w_full()
            .when(stands_for_scanner, |this| this.pt(sp(2.)))
            .pl(px(if nested { GUTTER + 12.0 } else { GUTTER - 4.0 }))
            .pr(px(GUTTER - 4.0))
            .child(
                col()
                    .w_full()
                    .rounded(px(radius::MD))
                    .when(expanded, |this| this.bg(theme::surface_1()))
                    .child(
                        row()
                            .w_full()
                            .min_h(px(46.))
                            .gap(sp(2.))
                            .child(if actionable {
                                checkbox(
                                    ElementId::NamedInteger("item-check".into(), row_index as u64),
                                    if selected {
                                        GroupState::All
                                    } else {
                                        GroupState::None
                                    },
                                    cx.listener(move |this, _, _, cx| {
                                        if let Some(finding) = this.findings.get(index) {
                                            this.selection.toggle(finding);
                                            cx.notify();
                                        }
                                    }),
                                )
                                .into_any_element()
                            } else {
                                row()
                                    .size(px(24.))
                                    .flex_none()
                                    .justify_center()
                                    .child(icon(Icon::Info, 14.0, theme::text_3()))
                                    .into_any_element()
                            })
                            .child(
                                row()
                                    .id(ElementId::NamedInteger(
                                        "item-row".into(),
                                        row_index as u64,
                                    ))
                                    .flex_1()
                                    .min_w(px(0.))
                                    .min_h(px(46.))
                                    .gap(sp(3.))
                                    .pr(sp(1.5))
                                    .rounded(px(radius::MD))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_1()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_expanded(area, &toggle_id, row_index, cx)
                                    }))
                                    .child(
                                        col()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .child(
                                                clip()
                                                    .fs(font::BODY)
                                                    .when(stands_for_scanner, |this| {
                                                        this.font_weight(FontWeight::SEMIBOLD)
                                                    })
                                                    .text_color(theme::text_1())
                                                    .child(title),
                                            )
                                            .when(
                                                !subtitle.is_empty()
                                                    || shared.is_some()
                                                    || context.is_some(),
                                                |this| {
                                                    this.child(
                                                        row()
                                                            .w_full()
                                                            .gap(sp(2.))
                                                            .when_some(context, |this, context| {
                                                                this.child(
                                                                    div()
                                                                        .flex_none()
                                                                        .fs(font::MICRO)
                                                                        .text_color(theme::text_2())
                                                                        .child(context),
                                                                )
                                                            })
                                                            .when(!subtitle.is_empty(), |this| {
                                                                this.child(
                                                                    clip_middle()
                                                                        .flex_1()
                                                                        .fs(font::MICRO)
                                                                        .text_color(theme::text_3())
                                                                        .child(subtitle.clone()),
                                                                )
                                                            })
                                                            .when_some(
                                                                shared.clone(),
                                                                |this, shared| {
                                                                    this.child(
                                                                        div()
                                                                            .flex_none()
                                                                            .fs(font::MICRO)
                                                                            .tabular()
                                                                            .text_color(
                                                                                theme::text_3(),
                                                                            )
                                                                            .child(shared),
                                                                    )
                                                                },
                                                            ),
                                                    )
                                                },
                                            ),
                                    )
                                    .when_some(age.filter(|_| wide), |this, age| {
                                        this.child(
                                            div()
                                                .flex_none()
                                                .w(px(92.))
                                                .text_right()
                                                .fs(font::CAPTION)
                                                .text_color(theme::text_3())
                                                .whitespace_nowrap()
                                                .child(age),
                                        )
                                    })
                                    .child(div().flex_none().w(px(72.)).flex().justify_end().child(
                                        if actionable {
                                            pill(finding.risk.label(), risk_color)
                                                .into_any_element()
                                        } else {
                                            tag(t!("Aviso", "Note")).into_any_element()
                                        },
                                    ))
                                    .child(
                                        div()
                                            .flex_none()
                                            .w(px(72.))
                                            .text_right()
                                            .fs(font::BODY)
                                            .font_weight(FontWeight::MEDIUM)
                                            .tabular()
                                            .text_color(if actionable {
                                                theme::text_1()
                                            } else {
                                                theme::text_3()
                                            })
                                            .child(if finding.size > 0 {
                                                human_size(finding.size)
                                            } else {
                                                "—".to_string()
                                            }),
                                    )
                                    .child(icon(
                                        if expanded {
                                            Icon::ChevronDown
                                        } else {
                                            Icon::ChevronRight
                                        },
                                        13.0,
                                        theme::text_3(),
                                    )),
                            ),
                    )
                    .when(expanded, |this| {
                        this.child(div().child(finding_details(finding)).with_animation(
                            ElementId::NamedInteger("details".into(), row_index as u64),
                            motion::animation(motion::pace::QUICK),
                            |this, t| this.opacity(t),
                        ))
                    }),
            )
            .into_any_element()
    }

    fn toggle_expanded(&mut self, area: Area, id: &str, row_index: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.views.get_mut(&area) {
            if !view.expanded.remove(id) {
                view.expanded.insert(id.to_string());
            }
            // Only this row changed height.
            view.list.splice(row_index..row_index + 1, 1);
        }
        cx.notify();
    }
}

/// The expanded part of a row: what it is, what happens if it goes, and how it comes back.
fn finding_details(finding: &Finding) -> impl IntoElement {
    let explanation = if finding.explanation.is_empty() {
        t!(
            "Sin más detalles sobre este elemento.",
            "No more details about this item."
        )
        .to_string()
    } else {
        finding.explanation.clone()
    };
    let items = (finding.item_count > 0).then(|| {
        plural(
            finding.item_count as usize,
            t!("archivo", "file"),
            t!("archivos", "files"),
        )
    });
    col()
        .pl(sp(8.))
        .pr(sp(8.))
        .pb(sp(3.))
        .gap(sp(2.))
        .child(
            div()
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(explanation),
        )
        .when(finding.shared_bytes > 0, |this| {
            this.child(
                div()
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(tf!(
                        "Limpiarlo libera {}. Otros {} están compartidos con otra carpeta, como la caché de uv o el almacén de pnpm, así que esos bloques siguen en el disco.",
                        "Cleaning it frees {}. Another {} is shared with another folder, such as the uv cache or the pnpm store, so those blocks stay on disk.",
                        human_size(finding.size),
                        human_size(finding.shared_bytes)
                    )),
            )
        })
        .child(
            row()
                .flex_wrap()
                .gap(sp(1.5))
                .when(is_actionable(finding), |this| {
                    this.child(tag(finding.recovery.label()))
                })
                .when_some(items, |this, items| this.child(tag(items)))
                .when(!finding.blocked_by.is_empty(), |this| {
                    this.child(pill(
                        tf!(
                            "No se limpia con {} abierto",
                            "Can't clean while {} is open",
                            finding.blocked_by.join(t!(" o ", " or "))
                        ),
                        theme::amber(),
                    ))
                }),
        )
}

/// "A", "A y B", "A, B y C".
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => tf!("{} y {last}", "{} and {last}", rest.join(", ")),
    }
}
