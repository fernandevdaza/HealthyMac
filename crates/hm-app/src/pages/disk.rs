//! The disk map: a real squarified treemap you can walk into.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, AnyElement, Context, ElementId, FontWeight, Window};
use hm_core::fmt::{human_age, human_size};
use hm_core::{t, tf};
use hm_system::types::DirNode;

use super::disk_map::treemap_canvas;
use crate::assets::Icon;
use crate::state::Shell;
use crate::theme::{self, font, radius, sp, GUTTER};
use crate::ui::charts::indeterminate_bar;
use crate::ui::chrome::{big_number, eyebrow, notice, page_header, NoticeKind};
use crate::ui::controls::{spinner, Btn, BtnSize};
use crate::ui::label::path_label;
use crate::ui::{clip, col, icon, row, TextExt};

pub(super) fn node_at<'a>(root: &'a DirNode, trail: &[usize]) -> Option<&'a DirNode> {
    let mut node = root;
    for &index in trail {
        node = node.children.get(index)?;
    }
    Some(node)
}

impl Shell {
    pub fn render_disk(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut actions: Vec<AnyElement> = Vec::new();
        if self.disk.loading {
            actions.push(
                Btn::secondary(t!("Cancelar", "Cancel"))
                    .icon(Icon::Stop)
                    .build(
                        "disk-cancel",
                        cx.listener(|this, _, _, cx| this.cancel_disk_scan(cx)),
                    )
                    .into_any_element(),
            );
        } else {
            actions.push(
                Btn::ghost(t!("Elegir carpeta…", "Choose folder…"))
                    .icon(Icon::Folder)
                    .disabled(self.mode.demo)
                    .build(
                        "disk-choose",
                        cx.listener(|this, _, _, cx| this.choose_disk_root(cx)),
                    )
                    .into_any_element(),
            );
            actions.push(
                Btn::secondary(t!("Volver a medir", "Measure again"))
                    .icon(Icon::Refresh)
                    .build(
                        "disk-rescan",
                        cx.listener(|this, _, _, cx| this.load_disk(cx)),
                    )
                    .into_any_element(),
            );
        }
        let header = page_header(
            t!("Mapa de disco", "Disk Map"),
            t!(
                "Cada rectángulo ocupa lo que ocupa de verdad. Haz clic para entrar en una carpeta.",
                "Every rectangle is drawn to scale. Click a folder to go into it."
            ),
            actions,
        );

        let body: AnyElement = if self.disk.root.is_none() {
            if self.disk.loading {
                self.render_disk_loading().into_any_element()
            } else if let Some(error) = self.disk.error.clone() {
                let retry = Btn::secondary(t!("Reintentar", "Try again"))
                    .size(BtnSize::Sm)
                    .build(
                        "disk-retry",
                        cx.listener(|this, _, _, cx| this.load_disk(cx)),
                    )
                    .into_any_element();
                div()
                    .px(px(GUTTER))
                    .child(notice(
                        NoticeKind::Error,
                        t!(
                            "No hemos podido medir esa carpeta",
                            "Couldn't measure that folder"
                        ),
                        Some(error.into()),
                        Some(retry),
                    ))
                    .into_any_element()
            } else {
                crate::ui::chrome::empty_state(
                    Icon::Treemap,
                    theme::text_2(),
                    t!("Has cancelado la medición", "Measuring cancelled"),
                    t!(
                        "Vuelve a medir cuando quieras, o elige una carpeta concreta para ir más rápido.",
                        "Measure again whenever you like, or choose a specific folder to go faster."
                    ),
                    Some(
                        Btn::primary(t!("Medir de nuevo", "Measure again"))
                            .icon(Icon::Treemap)
                            .build("disk-start", cx.listener(|this, _, _, cx| this.load_disk(cx)))
                            .into_any_element(),
                    ),
                )
                .into_any_element()
            }
        } else {
            self.render_disk_map(cx).into_any_element()
        };
        col()
            .size_full()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_disk_loading(&self) -> impl IntoElement {
        let path = self
            .disk
            .root_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let elapsed = self.disk.started.map_or(0, |at| at.elapsed().as_secs());
        col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(sp(3.5))
            .child(
                col()
                    .w(px(360.))
                    .gap(sp(2.5))
                    .child(
                        row()
                            .gap(sp(2.))
                            .child(spinner("disk-spin", 14.0, theme::text_2()))
                            .child(
                                div()
                                    .fs(font::SUBHEAD)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(t!("Midiendo tu disco", "Measuring your disk")),
                            ),
                    )
                    .child(indeterminate_bar("disk-progress", theme::primary(), 6.0))
                    .child(path_label(path, font::CAPTION, theme::text_2()))
                    .child(div().fs(font::CAPTION).text_color(theme::text_3()).child(
                        if elapsed > 8 {
                            t!(
                                "Una carpeta grande tarda lo suyo. Solo lee: no cambia nada.",
                                "A big folder takes a while. Read only: nothing is changed."
                            )
                        } else {
                            t!(
                                "Solo lee: no cambia nada.",
                                "Read only: nothing is changed."
                            )
                        },
                    )),
            )
    }

    fn choose_disk_root(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t!("Medir esta carpeta", "Measure this folder").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = receiver.await {
                if let Some(path) = paths.pop() {
                    this.update(cx, |shell, cx| shell.scan_disk_root(path, cx))
                        .ok();
                }
            }
        })
        .detach();
    }

    fn render_disk_map(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.disk.root.as_ref() else {
            return div().into_any_element();
        };
        let trail = self.disk.trail.clone();
        let Some(current) = node_at(root, &trail) else {
            self.disk.trail.clear();
            cx.notify();
            return div().into_any_element();
        };

        // Breadcrumb: every ancestor is a way back.
        let mut crumbs: Vec<(String, usize)> = vec![(display_name(root), 0)];
        let mut walker = root;
        for (depth, &index) in trail.iter().enumerate() {
            if let Some(child) = walker.children.get(index) {
                crumbs.push((child.name.clone(), depth + 1));
                walker = child;
            }
        }
        let breadcrumb = row()
            .flex_1()
            .min_w(px(0.))
            .gap(sp(0.5))
            .overflow_hidden()
            .children(crumbs.iter().enumerate().map(|(position, (name, depth))| {
                let depth = *depth;
                let last = position + 1 == crumbs.len();
                row()
                    .flex_none()
                    .gap(sp(0.5))
                    .when(position > 0, |this| {
                        this.child(icon(Icon::ChevronRight, 11.0, theme::text_3()))
                    })
                    .child(
                        div()
                            .id(ElementId::NamedInteger("crumb".into(), depth as u64))
                            .px(sp(1.5))
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .rounded(px(radius::SM))
                            .fs(font::CAPTION)
                            .whitespace_nowrap()
                            .text_color(if last {
                                theme::text_1()
                            } else {
                                theme::text_2()
                            })
                            .when(last, |this| this.font_weight(FontWeight::MEDIUM))
                            .when(!last, |this| {
                                this.cursor_pointer()
                                    .hover(|style| {
                                        style.bg(theme::surface_2()).text_color(theme::text_1())
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.disk.trail.truncate(depth);
                                        this.disk.hover = None;
                                        cx.notify();
                                    }))
                            })
                            .child(name.clone()),
                    )
            }));

        let current_size = current.size;
        let current_path = current.path.clone();
        let partial = root.partial;
        let info = self.render_disk_info(current);
        let can_go_up = !trail.is_empty();

        let entity = cx.entity().downgrade();
        let hover = self.disk.hover.clone();
        let map_root = shallow_copy(current, 2);

        col()
            .flex_1()
            .min_h(px(0.))
            .px(px(GUTTER))
            .pb(px(GUTTER - 12.0))
            .gap(sp(2.5))
            .child(
                row()
                    .flex_none()
                    .gap(sp(2.5))
                    .child(big_number(
                        human_size(current_size),
                        font::HEADLINE + 2.0,
                        theme::text_1(),
                    ))
                    .child(breadcrumb)
                    .when(can_go_up, |this| {
                        this.child(
                            Btn::ghost(t!("Subir", "Up"))
                                .size(BtnSize::Sm)
                                .icon(Icon::ArrowUp)
                                .build(
                                    "disk-up",
                                    cx.listener(|this, _, _, cx| {
                                        this.disk.trail.pop();
                                        this.disk.hover = None;
                                        cx.notify();
                                    }),
                                ),
                        )
                    })
                    .when(!self.mode.demo, |this| {
                        this.child(Btn::ghost(t!("Mostrar en Finder", "Show in Finder")).size(BtnSize::Sm).build(
                            "disk-reveal",
                            cx.listener(move |_, _, _, cx| cx.reveal_path(&current_path)),
                        ))
                    }),
            )
            .when(partial, |this| {
                this.child(notice(
                    NoticeKind::Info,
                    t!(
                        "Algunas carpetas no se han podido leer, así que el total es un mínimo",
                        "Some folders couldn't be read, so the total is a minimum"
                    ),
                    Some(
                        t!(
                            "Con Acceso total al disco el mapa ve todo. Puedes darlo en Ajustes.",
                            "With Full Disk Access the map sees everything. You can grant it in Settings."
                        )
                        .into(),
                    ),
                    None,
                ))
            })
            .child(treemap_canvas(map_root, hover, trail, entity).into_any_element())
            .child(info)
            .into_any_element()
    }

    fn render_disk_info(&self, current: &DirNode) -> impl IntoElement {
        let hovered = self
            .disk
            .hover
            .as_ref()
            .and_then(|hover| node_at(current, &hover.path));
        let now = std::time::SystemTime::now();
        let node = hovered.unwrap_or(current);
        let share = if current.size > 0 {
            node.size as f64 / current.size as f64 * 100.0
        } else {
            0.0
        };
        let share = tf!("{share:.1} %", "{share:.1}%");
        // Spanish writes decimals with a comma.
        let share = if hm_core::i18n::is_english() {
            share
        } else {
            share.replace('.', ",")
        };
        row()
            .flex_none()
            .h(px(44.))
            .gap(sp(3.5))
            .child(
                col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        clip()
                            .fs(font::BODY)
                            .font_weight(FontWeight::MEDIUM)
                            .child(node.name.clone()),
                    )
                    .child(path_label(
                        node.path.display().to_string(),
                        font::MICRO,
                        theme::text_3(),
                    )),
            )
            .child(
                row()
                    .flex_none()
                    .gap(sp(4.))
                    .when(node.file_count > 0, |this| {
                        this.child(stat(
                            t!("Archivos", "Files"),
                            super::fmt_count(node.file_count),
                        ))
                    })
                    .when_some(node.last_modified, |this, when| {
                        this.child(stat(t!("Cambiado", "Modified"), human_age(when, now)))
                    })
                    .child(stat(t!("Del total", "Of total"), share))
                    .child(stat(t!("Tamaño", "Size"), human_size(node.size))),
            )
    }
}

fn stat(label: &'static str, value: String) -> impl IntoElement {
    col().items_end().child(eyebrow(label)).child(
        div()
            .fs(font::BODY)
            .tabular()
            .font_weight(FontWeight::MEDIUM)
            .child(value),
    )
}

/// A copy of just the levels the map draws. The whole tree can be huge, and this happens per frame.
fn shallow_copy(node: &DirNode, depth: usize) -> DirNode {
    DirNode {
        name: node.name.clone(),
        path: node.path.clone(),
        size: node.size,
        is_dir: node.is_dir,
        file_count: node.file_count,
        last_modified: node.last_modified,
        children: if depth == 0 {
            Vec::new()
        } else {
            node.children
                .iter()
                .map(|child| shallow_copy(child, depth - 1))
                .collect()
        },
        other_size: node.other_size,
        partial: node.partial,
    }
}

fn display_name(node: &DirNode) -> String {
    if node.name.is_empty() {
        node.path.display().to_string()
    } else {
        node.name.clone()
    }
}

impl Shell {
    /// Walks into a tile. A folder scanned too shallowly gets measured on the spot.
    pub(super) fn drill_into(
        &mut self,
        trail: Vec<usize>,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.disk.root.as_ref() else {
            return;
        };
        let mut full = trail;
        let mut target = None;
        for index in path {
            full.push(index);
            let Some(node) = node_at(root, &full) else {
                return;
            };
            if !node.is_dir {
                full.pop();
                break;
            }
            target = Some(node);
        }
        let Some(node) = target else {
            return;
        };
        if node.children.is_empty() {
            if node.size > 50_000_000 && !self.mode.demo {
                let path = node.path.clone();
                self.deepen(full, path, cx);
            }
            return;
        }
        self.disk.trail = full;
        self.disk.hover = None;
        cx.notify();
    }

    fn deepen(&mut self, at: Vec<usize>, path: PathBuf, cx: &mut Context<Self>) {
        if self.disk.deepening.is_some() {
            return;
        }
        self.disk.deepening = Some(path.clone());
        let cancel = hm_core::CancelToken::new();
        let home = self.ctx.home.clone();
        Shell::run_bg(
            cx,
            move || hm_system::space::scan_tree_in_home(&path, &home, 4, 10_000_000, &cancel),
            move |shell, result, cx| {
                shell.disk.deepening = None;
                match result {
                    Ok(tree) => {
                        if let Some(root) = shell.disk.root.as_mut() {
                            let mut node = root;
                            for &index in &at {
                                let Some(next) = node.children.get_mut(index) else {
                                    return;
                                };
                                node = next;
                            }
                            node.children = tree.children;
                            node.other_size = tree.other_size;
                            if !node.children.is_empty() {
                                shell.disk.trail = at;
                                shell.disk.hover = None;
                            }
                        }
                    }
                    Err(error) => shell.show_toast(
                        tf!(
                            "No se pudo medir esa carpeta: {error}",
                            "Couldn't measure that folder: {error}"
                        ),
                        cx,
                    ),
                }
            },
        );
    }
}
