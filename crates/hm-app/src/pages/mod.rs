//! The root view and one module per page.

mod apps;
mod area;
mod disk;
mod disk_map;
mod models;
mod models_budget;
mod monitor;
mod ports;
mod projects;
mod resumen;
mod resumen_tiles;
mod settings;
mod sheet_result;
mod sheet_review;
mod sheets;
mod sidebar;
mod startup;

use gpui::{
    div, ease_in_out, prelude::*, px, AnimationExt, AnyElement, Context, Div, ElementId,
    FontWeight, KeyDownEvent, MouseButton, Render, Stateful, Window,
};

use crate::state::{Page, ScrollPane, Shell};
use crate::theme::{self, font, radius, sp};
use crate::ui::motion;
use crate::ui::scrollbar::{scrollbar, ScrollSource};
use crate::ui::{f, TextExt};

/// How long a toast stays up. `show_toast` removes it after the same time.
pub const TOAST_MS: u64 = 4_000;

/// Opens the Full Disk Access list in System Settings.
pub const FULL_DISK_ACCESS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.render_page(window, cx);
        let sheet = self.render_sheet(window, cx);
        let toast = self.render_toast();
        div()
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(theme::window_bg())
            .text_color(theme::text_1())
            .fs(font::BODY)
            .track_focus(&self.focus)
            .key_context("Shell")
            .on_key_down(cx.listener(Self::on_key))
            .on_action(
                cx.listener(|this, _: &crate::OpenSettings, _, cx| {
                    this.navigate(Page::Ajustes, cx)
                }),
            )
            .on_action(|_: &crate::Minimize, window, _| window.minimize_window())
            .on_action(|_: &crate::Zoom, window, _| window.zoom_window())
            .on_action(|_: &crate::CloseWindow, window, _| window.remove_window())
            .child(self.render_sidebar(cx))
            .child(
                // The page sits flush against the sidebar, as in Finder or System Settings, rather
                // than floating as a card: this is a Mac app, not a web dashboard.
                div()
                    .relative()
                    .flex_1()
                    .min_w(px(0.))
                    .bg(theme::panel_bg())
                    .border_l_1()
                    .border_color(theme::hairline())
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .flex()
                            .flex_col()
                            // Switching pages is routine, so it only softens the swap: a short fade,
                            // no movement, the same as before the redesign.
                            .with_animation(
                                ElementId::NamedInteger("page".into(), self.page.serial()),
                                motion::animation_eased(160, ease_in_out),
                                |this, t| this.opacity(0.2 + 0.8 * t),
                            )
                            .map_element(|this| this.child(page)),
                    )
                    .child(self.drag_strip("panel-drag", 28.0, cx)),
            )
            .children(sheet)
            .children(toast)
    }
}

impl Shell {
    fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            Page::Resumen => self.render_resumen(window, cx),
            Page::Area(area) => self.render_area(area, window, cx),
            Page::Modelos => self.render_models(window, cx),
            Page::Proyectos => self.render_projects(window, cx),
            Page::Puertos => self.render_ports(window, cx),
            Page::Apps => self.render_apps(window, cx),
            Page::Arranque => self.render_startup(window, cx),
            Page::Mapa => self.render_disk(window, cx),
            Page::Monitor => self.render_monitor(window, cx),
            Page::Ajustes => self.render_settings(window, cx),
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" && self.sheet.is_some() {
            self.close_sheet(cx);
            return;
        }
        if keystroke.modifiers.platform && self.sheet.is_none() {
            let page = match keystroke.key.as_str() {
                "1" => Some(Page::Resumen),
                "2" => Some(Page::Area(hm_core::Area::System)),
                "3" => Some(Page::Area(hm_core::Area::Developer)),
                "4" => Some(Page::Area(hm_core::Area::LocalAi)),
                "5" => Some(Page::Area(hm_core::Area::Files)),
                "6" => Some(Page::Modelos),
                "7" => Some(Page::Proyectos),
                "8" => Some(Page::Monitor),
                "9" => Some(Page::Mapa),
                _ => None,
            };
            if let Some(page) = page {
                self.navigate(page, cx);
            }
        }
    }

    /// An invisible strip that moves the window, standing in for the titlebar we hid.
    pub fn drag_strip(
        &self,
        id: &'static str,
        height: f32,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .h(px(height))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_pressed = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_pressed = false),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.titlebar_pressed {
                    this.titlebar_pressed = false;
                    window.start_window_move();
                }
            }))
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
    }

    fn render_toast(&self) -> Option<AnyElement> {
        let toast = self.toast.as_ref()?;
        Some(
            div()
                .absolute()
                .bottom(px(24.))
                .left(px(0.))
                .right(px(0.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w(px(520.))
                        .px(sp(4.))
                        .py(sp(2.5))
                        .rounded(px(radius::LG))
                        .bg(theme::sheet_bg())
                        .border_1()
                        .border_color(theme::hairline_strong())
                        .shadow(theme::sheet_shadow())
                        .fs(font::BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .child(toast.message.clone())
                        // One animation for the toast's whole life: it rises in, waits, and fades out
                        // just before the timer in `show_toast` removes it.
                        .with_animation(
                            ElementId::NamedInteger("toast".into(), toast.serial),
                            motion::linear(TOAST_MS),
                            |this, t| {
                                if motion::reduced() {
                                    return this;
                                }
                                let ms = t * TOAST_MS as f32;
                                let arrive = motion::ease_out((ms / 260.0).min(1.0));
                                let leave = ((TOAST_MS as f32 - ms) / 220.0).clamp(0.0, 1.0);
                                this.opacity(arrive.min(leave))
                                    .mb(px(10.0 * (1.0 - arrive)))
                            },
                        ),
                )
                .into_any_element(),
        )
    }
}

/// A scrolling page body with our scrollbar on top.
pub fn scroll_pane(id: &'static str, pane: &ScrollPane, content: impl IntoElement) -> Div {
    div()
        .relative()
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .child(
            div()
                .id(id)
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&pane.handle)
                .child(content),
        )
        .child(scrollbar(
            ScrollSource::Handle(pane.handle.clone()),
            pane.bar.clone(),
        ))
}

/// A list row's outer box. Rows inside `list` and `uniform_list` shrink to fit unless told otherwise,
/// and this keeps their hover highlight bleeding slightly past the page gutter.
pub fn gutter_row() -> Div {
    div().w_full().px(px(theme::GUTTER - 8.0))
}

/// Whether the content area is wide enough for optional columns.
pub fn content_width(window: &Window) -> f32 {
    f(window.viewport_size().width) - theme::SIDEBAR_WIDTH - 8.0
}

/// Thousands separator for the current language: "1.204" in Spanish, "1,204" in English.
pub fn fmt_count(value: u64) -> String {
    let separator = if hm_core::i18n::is_english() {
        ','
    } else {
        '.'
    };
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(separator);
        }
        out.push(digit);
    }
    out
}

/// Choosing between singular and plural, the honest way. Callers pass both forms through `t!`.
pub fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", fmt_count(count as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_use_spanish_separators() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_204), "1.204");
        assert_eq!(fmt_count(12_345_678), "12.345.678");
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "hallazgo", "hallazgos"), "1 hallazgo");
        assert_eq!(plural(0, "hallazgo", "hallazgos"), "0 hallazgos");
        assert_eq!(plural(2_000, "hallazgo", "hallazgos"), "2.000 hallazgos");
    }
}
