//! The sidebar: the window's own background, running under the traffic lights.

use std::sync::{Arc, LazyLock};

use gpui::{
    div, img, prelude::*, px, AnyElement, Context, FontWeight, Image, ImageFormat, SharedString,
};
use hm_core::fmt::human_size;
use hm_core::{t, Area};

use crate::assets::Icon;
use crate::logic::selection::reclaimable_by_area;
use crate::state::{AppsTab, Page, Shell};
use crate::theme::{self, font, radius, sp, SIDEBAR_WIDTH};
use crate::ui::{col, icon, row, TextExt};

/// The app's own icon, the brand mark, rather than a stand-in drawn in code. One per theme, so it
/// never sinks into a dark sidebar or weighs on a light one.
static LOGO_LIGHT: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../../assets/AppIcon-light.png").to_vec(),
    ))
});
static LOGO_DARK: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../../assets/AppIcon-dark.png").to_vec(),
    ))
});

impl Shell {
    pub fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let totals = reclaimable_by_area(&self.findings);
        let size_of = |area: Area| {
            totals
                .get(&area)
                .copied()
                .filter(|&bytes| bytes > 0)
                .map(human_size)
        };

        col()
            .relative()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .px(sp(2.5))
            .pb(sp(3.))
            .child(self.drag_strip("sidebar-drag", 46.0, cx))
            .child(
                // Clears the traffic lights, then the wordmark.
                row()
                    .mt(sp(12.5))
                    .mb(sp(3.))
                    .px(sp(2.))
                    .gap(sp(2.))
                    .child(
                        img(if theme::is_dark() {
                            LOGO_DARK.clone()
                        } else {
                            LOGO_LIGHT.clone()
                        })
                        .flex_none()
                        .size(px(24.)),
                    )
                    .child(
                        div()
                            .fs(font::SUBHEAD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("HealthyMac"),
                    ),
            )
            .child(self.nav_item(
                Page::Resumen,
                Icon::Gauge,
                t!("Resumen", "Overview"),
                None,
                cx,
            ))
            .child(group_label(t!("Limpieza", "Cleanup")))
            .child(self.nav_item(
                Page::Area(Area::System),
                Icon::Sparkle,
                t!("Sistema", "System"),
                size_of(Area::System),
                cx,
            ))
            .child(self.nav_item(
                Page::Area(Area::Developer),
                Icon::Code,
                "Developer",
                size_of(Area::Developer),
                cx,
            ))
            .child(self.nav_item(
                Page::Area(Area::LocalAi),
                Icon::Nodes,
                t!("IA local", "Local AI"),
                size_of(Area::LocalAi),
                cx,
            ))
            .child(self.nav_item(
                Page::Area(Area::Files),
                Icon::File,
                t!("Archivos", "Files"),
                size_of(Area::Files),
                cx,
            ))
            .child(group_label(t!("Desarrollo e IA", "Development and AI")))
            .child(self.nav_item(Page::Modelos, Icon::Cube, t!("Modelos", "Models"), None, cx))
            .child(self.nav_item(
                Page::Proyectos,
                Icon::Folder,
                t!("Proyectos", "Projects"),
                None,
                cx,
            ))
            .child(self.nav_item(Page::Puertos, Icon::Plug, t!("Puertos", "Ports"), None, cx))
            .child(group_label(t!("Tu Mac", "Your Mac")))
            .child(self.nav_item(Page::Apps, Icon::Grid, "Apps", size_of(Area::Apps), cx))
            .child(self.nav_item(
                Page::Arranque,
                Icon::Power,
                t!("Arranque", "Startup"),
                None,
                cx,
            ))
            .child(self.nav_item(
                Page::Mapa,
                Icon::Treemap,
                t!("Mapa de disco", "Disk Map"),
                None,
                cx,
            ))
            .child(self.nav_item(Page::Monitor, Icon::Activity, "Monitor", None, cx))
            .child(div().flex_1().min_h(px(8.)))
            .when_some(self.mode_badges(), |this, badges| this.child(badges))
            .child(self.nav_item(
                Page::Ajustes,
                Icon::Sliders,
                t!("Ajustes", "Settings"),
                None,
                cx,
            ))
    }

    fn nav_item(
        &self,
        page: Page,
        which: Icon,
        label: &'static str,
        trailing: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.page == page;
        row()
            .id(SharedString::from(format!("nav-{}", page.serial())))
            .h(px(30.))
            .px(sp(2.))
            .gap(px(9.))
            .rounded(px(radius::SM))
            .cursor_pointer()
            .when(selected, |this| this.bg(theme::surface_3()))
            .when(!selected, |this| {
                this.hover(|style| style.bg(theme::surface_1()))
                    .active(|style| style.bg(theme::surface_2()))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if page == Page::Apps {
                    this.apps.tab = AppsTab::Installed;
                }
                this.navigate(page, cx);
            }))
            .child(icon(
                which,
                16.0,
                if selected {
                    theme::text_1()
                } else {
                    theme::text_2()
                },
            ))
            .child(
                div()
                    .flex_1()
                    .fs(font::BODY)
                    .whitespace_nowrap()
                    .when(selected, |this| {
                        this.text_color(theme::text_1())
                            .font_weight(FontWeight::MEDIUM)
                    })
                    .when(!selected, |this| this.text_color(theme::text_2()))
                    .child(label),
            )
            .children(trailing.map(|text| {
                div()
                    .fs(font::MICRO)
                    .tabular()
                    .text_color(theme::text_3())
                    .whitespace_nowrap()
                    .child(text)
            }))
    }

    /// Permanently visible while nothing real can happen, so a screenshot can never mislead.
    fn mode_badges(&self) -> Option<AnyElement> {
        let lines = self.mode_lines();
        (!lines.is_empty()).then(|| {
            col()
                .mb(sp(2.))
                .gap(sp(1.5))
                .px(sp(2.5))
                .py(sp(2.))
                .rounded(px(radius::MD))
                .bg(theme::surface_2())
                .children(lines)
                .into_any_element()
        })
    }

    fn mode_lines(&self) -> Vec<AnyElement> {
        let mut badges = Vec::new();
        if self.mode.demo {
            badges.push(badge(
                Icon::Layers,
                "Demo",
                t!("datos inventados", "sample data"),
            ));
        }
        if self.mode.dry_run || self.mode.demo {
            badges.push(badge(
                Icon::Flask,
                t!("Simulación", "Simulation"),
                t!("no se borra nada", "nothing is deleted"),
            ));
        }
        if let (Some(home), false) = (&self.mode.home_override, self.mode.demo) {
            badges.push(badge(
                Icon::Folder,
                t!("Home de prueba", "Test home"),
                &crate::logic::truncate::truncate_middle(&home.display().to_string(), 22),
            ));
        }
        badges
    }
}

fn group_label(text: &'static str) -> impl IntoElement {
    div()
        .mt(sp(3.5))
        .mb(sp(1.))
        .px(sp(2.))
        .fs(font::MICRO)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_3())
        .child(text)
}

/// One quiet line per mode. Together they sit in a single box at the foot of the sidebar.
fn badge(which: Icon, title: &str, detail: &str) -> AnyElement {
    row()
        .gap(sp(2.))
        .child(icon(which, 13.0, theme::text_2()))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .fs(font::MICRO)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_color(theme::text_3())
                .child(
                    div()
                        .flex()
                        .gap(sp(1.))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text_1())
                                .child(title.to_string()),
                        )
                        .child(detail.to_string()),
                ),
        )
        .into_any_element()
}
