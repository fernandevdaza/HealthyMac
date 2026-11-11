//! What starts by itself. Read-only for now: seeing it is half the battle, and Finder does the rest.

use gpui::{div, prelude::*, px, uniform_list, AnyElement, Context, ElementId, FontWeight, Window};

const KIND_WIDTH: f32 = 128.0;
const ACTION_WIDTH: f32 = 132.0;
use hm_core::{t, tf};
use hm_system::types::{StartupItem, StartupKind};

use super::{gutter_row, plural};
use crate::assets::Icon;
use crate::state::Shell;
use crate::theme::{self, font, radius, sp, GUTTER};
use crate::ui::chrome::{empty_state, notice, page_header, NoticeKind};
use crate::ui::controls::{pill, tag, Btn, ROW_GROUP};
use crate::ui::label::path_label;
use crate::ui::scrollbar::{scrollbar, ScrollSource};
use crate::ui::{clip, col, row, TextExt};

fn kind_label(kind: StartupKind) -> &'static str {
    match kind {
        StartupKind::UserAgent => t!("Tu usuario", "Your user"),
        StartupKind::GlobalAgent => t!("Todos los usuarios", "All users"),
        StartupKind::GlobalDaemon => t!("Sistema", "System"),
    }
}

impl Shell {
    pub fn render_startup(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let items = self.startup.items.data.as_deref();
        let count = items.map_or(0, |items| items.len());
        let orphans = items.map_or(0, |items| items.iter().filter(|item| item.orphaned).count());
        let subtitle = if items.is_none() {
            t!(
                "Lo que se pone en marcha sin que se lo pidas.",
                "What starts up without you asking."
            )
            .to_string()
        } else if orphans == 1 {
            tf!(
                "{}. 1 huérfano apunta a un programa que ya no existe.",
                "{}. 1 orphaned item points to a program that no longer exists.",
                plural(count, t!("elemento", "item"), t!("elementos", "items")),
            )
        } else if orphans > 1 {
            tf!(
                "{}. {} apuntan a programas que ya no existen.",
                "{}. {} point to programs that no longer exist.",
                plural(count, t!("elemento", "item"), t!("elementos", "items")),
                plural(
                    orphans,
                    t!("huérfano", "orphaned item"),
                    t!("huérfanos", "orphaned items")
                )
            )
        } else {
            tf!(
                "{}. Solo lectura: de momento aquí se mira, no se toca.",
                "{}. Read-only for now: you can look, not touch.",
                plural(count, t!("elemento", "item"), t!("elementos", "items"))
            )
        };
        let header = page_header(t!("Arranque", "Startup"), subtitle, Vec::new());

        let body: AnyElement = if let (None, Some(error)) =
            (&self.startup.items.data, &self.startup.items.error)
        {
            div()
                .px(px(GUTTER))
                .child(notice(
                    NoticeKind::Error,
                    t!(
                        "No hemos podido leer lo que arranca",
                        "We couldn't read what starts up"
                    ),
                    Some(error.clone().into()),
                    None,
                ))
                .into_any_element()
        } else if items.is_none() {
            empty_state(
                Icon::Power,
                theme::text_2(),
                t!("Mirando qué arranca", "Checking what starts up"),
                t!("Un segundo.", "One second."),
                None,
            )
            .into_any_element()
        } else if count == 0 {
            empty_state(
                Icon::Power,
                theme::green(),
                t!("Nada arranca por su cuenta", "Nothing starts on its own"),
                t!(
                    "No hay agentes ni daemons de terceros. Tu Mac arranca ligero.",
                    "No third-party agents or daemons. Your Mac starts up light."
                ),
                None,
            )
            .into_any_element()
        } else {
            let handle = self.startup.scroll.clone();
            let bar = self.startup.bar.clone();
            col()
                .flex_1()
                .min_h(px(0.))
                .child(
                    row()
                        .flex_none()
                        .h(px(26.))
                        .mx(px(GUTTER))
                        .gap(sp(3.))
                        .border_b_1()
                        .border_color(theme::hairline())
                        .fs(font::MICRO)
                        .text_color(theme::text_3())
                        .child(div().flex_1().min_w(px(0.)).child(t!("Elemento", "Item")))
                        .child(div().w(px(KIND_WIDTH)).child(t!("Para quién", "Runs for")))
                        .child(div().w(px(ACTION_WIDTH))),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.))
                        .child(
                            uniform_list(
                                "startup-list",
                                count,
                                cx.processor(
                                    |this: &mut Shell,
                                     range: std::ops::Range<usize>,
                                     _window,
                                     _cx| {
                                        range
                                            .filter_map(|index| {
                                                let item =
                                                    this.startup.items.data.as_ref()?.get(index)?;
                                                Some(render_item(index, item))
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .track_scroll(handle.clone())
                            .size_full(),
                        )
                        .child(scrollbar(ScrollSource::Uniform(handle), bar)),
                )
                .into_any_element()
        };
        col()
            .size_full()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

fn render_item(index: usize, item: &StartupItem) -> AnyElement {
    let program = item
        .program
        .clone()
        .unwrap_or_else(|| item.plist_path.display().to_string());
    // The plist is what starts it, and what someone would delete by hand, so that is what Finder
    // shows. For an orphan it is the only thing left to see.
    let plist = item.plist_path.clone();
    gutter_row()
        .child(
            row()
                .id(ElementId::NamedInteger("startup".into(), index as u64))
                .group(ROW_GROUP)
                .h(px(52.))
                .w_full()
                .px(sp(2.))
                .gap(sp(3.))
                .rounded(px(radius::MD))
                .hover(|style| style.bg(theme::surface_1()))
                .child(
                    col()
                        .flex_1()
                        .min_w(px(0.))
                        .child(
                            row()
                                .w_full()
                                .gap(sp(1.5))
                                .child(
                                    clip()
                                        .flex_shrink()
                                        .min_w(px(0.))
                                        .fs(font::BODY)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if item.disabled {
                                            theme::text_3()
                                        } else {
                                            theme::text_1()
                                        })
                                        .child(item.label.clone()),
                                )
                                .when(item.malformed, |this| {
                                    this.child(pill(
                                        t!("No se puede leer", "Unreadable"),
                                        theme::amber(),
                                    ))
                                })
                                .when(item.orphaned, |this| {
                                    this.child(pill(t!("Huérfano", "Orphaned"), theme::rose()))
                                })
                                .when(item.disabled, |this| {
                                    this.child(tag(t!("Desactivado", "Disabled")))
                                })
                                .when(item.keep_alive, |this| {
                                    this.child(tag(t!("Siempre activo", "Always on")))
                                })
                                .when(item.run_at_load && !item.keep_alive, |this| {
                                    this.child(tag(t!("Al iniciar", "At startup")))
                                }),
                        )
                        .child(path_label(
                            program,
                            font::MICRO,
                            if item.orphaned {
                                theme::rose().alpha(0.8)
                            } else {
                                theme::text_3()
                            },
                        )),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(KIND_WIDTH))
                        .fs(font::CAPTION)
                        .text_color(theme::text_3())
                        .child(kind_label(item.kind)),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(ACTION_WIDTH))
                        .flex()
                        .justify_end()
                        .child(
                            Btn::row_action(t!("Mostrar en Finder", "Show in Finder")).build(
                                ElementId::NamedInteger("startup-reveal".into(), index as u64),
                                move |_, _, cx| cx.reveal_path(&plist),
                            ),
                        ),
                ),
        )
        .into_any_element()
}
