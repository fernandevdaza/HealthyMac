//! Buttons, checkboxes, pills and the other small interactive pieces.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    div, percentage, prelude::*, px, svg, Animation, AnimationExt, App, ClickEvent, Div, ElementId,
    FontWeight, Hsla, SharedString, Stateful, Transformation, Window,
};

use super::{icon, row, TextExt};
use crate::assets::Icon;
use crate::logic::selection::GroupState;
use crate::theme::{self, font, radius, sp};

// ---- Buttons -----------------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum BtnKind {
    /// Filled with the text colour. One per screen.
    Primary,
    Secondary,
    Ghost,
    Danger,
    /// Filled red: confirming something that cannot be taken back without care.
    Destructive,
    /// An action repeated on every row of a list. Quiet until the pointer is over its row, so a
    /// column of identical buttons does not shout. The row carries `.group(ROW_GROUP)`.
    Row,
    /// The same, for actions that remove or stop something: it turns red as its row is hovered.
    RowDanger,
}

/// The group name a list row gives itself so its `Row` and `RowDanger` buttons can wake up.
pub const ROW_GROUP: &str = "list-row";

#[derive(Clone, Copy, PartialEq)]
pub enum BtnSize {
    Sm,
    Md,
    Lg,
}

pub struct Btn {
    label: SharedString,
    kind: BtnKind,
    size: BtnSize,
    icon: Option<Icon>,
    disabled: bool,
}

impl Btn {
    pub fn new(label: impl Into<SharedString>, kind: BtnKind) -> Self {
        Self {
            label: label.into(),
            kind,
            size: BtnSize::Md,
            icon: None,
            disabled: false,
        }
    }

    pub fn primary(label: impl Into<SharedString>) -> Self {
        Self::new(label, BtnKind::Primary)
    }

    pub fn secondary(label: impl Into<SharedString>) -> Self {
        Self::new(label, BtnKind::Secondary)
    }

    pub fn ghost(label: impl Into<SharedString>) -> Self {
        Self::new(label, BtnKind::Ghost)
    }

    pub fn danger(label: impl Into<SharedString>) -> Self {
        Self::new(label, BtnKind::Danger)
    }

    pub fn destructive(label: impl Into<SharedString>) -> Self {
        Self::new(label, BtnKind::Destructive)
    }

    /// See [`BtnKind::Row`].
    pub fn row_action(label: impl Into<SharedString>) -> Self {
        Self::new(label, BtnKind::Row).size(BtnSize::Sm)
    }

    /// See [`BtnKind::RowDanger`].
    pub fn row_danger(label: impl Into<SharedString>) -> Self {
        Self::new(label, BtnKind::RowDanger).size(BtnSize::Sm)
    }

    pub fn size(mut self, size: BtnSize) -> Self {
        self.size = size;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Builds the element. A disabled button ignores `on_click`, whatever the caller attaches.
    pub fn build(
        self,
        id: impl Into<ElementId>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let (height, text, pad, icon_size) = match self.size {
            BtnSize::Sm => (26.0, font::CAPTION, 10.0, 14.0),
            BtnSize::Md => (32.0, font::BODY, 14.0, 15.0),
            BtnSize::Lg => (40.0, font::BODY + 1.0, 20.0, 16.0),
        };
        let corner = radius::SM;
        let disabled = self.disabled;
        let foreground = match self.kind {
            _ if disabled => theme::text_3(),
            BtnKind::Primary => theme::on_primary(),
            BtnKind::Secondary => theme::text_1(),
            BtnKind::Ghost => theme::text_2(),
            BtnKind::Danger => theme::rose(),
            BtnKind::Destructive => gpui::white(),
            BtnKind::Row | BtnKind::RowDanger => theme::text_3(),
        };
        let base = row()
            .id(id)
            .flex_none()
            .h(px(height))
            .px(px(pad))
            .gap(px(if self.size == BtnSize::Lg { 8.0 } else { 6.0 }))
            .justify_center()
            .rounded(px(corner))
            .whitespace_nowrap()
            .fs(text)
            .font_weight(FontWeight::MEDIUM)
            .text_color(foreground)
            .when_some(self.icon, |this, which| {
                this.child(icon(which, icon_size, foreground))
            })
            .child(self.label);

        if disabled {
            return base.bg(theme::surface_2());
        }
        let styled = match self.kind {
            BtnKind::Primary => base
                .bg(theme::primary())
                .font_weight(FontWeight::SEMIBOLD)
                .hover(|style| style.bg(theme::primary_hover())),
            BtnKind::Secondary => base
                .border_1()
                .border_color(theme::hairline_strong())
                .hover(|style| style.bg(theme::surface_2())),
            BtnKind::Ghost => {
                base.hover(|style| style.bg(theme::surface_2()).text_color(theme::text_1()))
            }
            BtnKind::Danger => base
                .bg(theme::rose().alpha(0.12))
                .hover(|style| style.bg(theme::rose().alpha(0.2))),
            BtnKind::Destructive => base
                .bg(theme::rose())
                .font_weight(FontWeight::SEMIBOLD)
                .hover(|style| style.bg(theme::rose().opacity(0.85))),
            BtnKind::Row => base
                .group_hover(ROW_GROUP, |style| {
                    style.bg(theme::surface_2()).text_color(theme::text_1())
                })
                .hover(|style| style.bg(theme::surface_3()).text_color(theme::text_1())),
            BtnKind::RowDanger => base
                .group_hover(ROW_GROUP, |style| {
                    style.bg(theme::rose().alpha(0.1)).text_color(theme::rose())
                })
                .hover(|style| style.bg(theme::rose().alpha(0.2)).text_color(theme::rose())),
        };
        styled
            .cursor_pointer()
            .active(|style| style.opacity(0.78))
            .on_click(on_click)
    }
}

/// A small square button holding just an icon.
pub fn icon_button(
    id: impl Into<ElementId>,
    which: Icon,
    color: Hsla,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    row()
        .id(id)
        .flex_none()
        .size(px(26.))
        .justify_center()
        .rounded(px(radius::SM))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_3()))
        .active(|style| style.opacity(0.7))
        .child(icon(which, 15.0, color))
        .on_click(on_click)
}

// ---- Checkbox ----------------------------------------------------------------------------------

/// The hit area is larger than the box, because small targets are annoying. A ticked box is filled
/// with the text colour: what is selected is a state, not an area's colour.
pub fn checkbox(
    id: impl Into<ElementId>,
    state: GroupState,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let accent = theme::primary();
    let filled = matches!(state, GroupState::All | GroupState::Mixed);
    let glyph = match state {
        GroupState::All => Some(Icon::Check),
        GroupState::Mixed => Some(Icon::Minus),
        _ => None,
    };
    row()
        .id(id)
        .group("checkbox")
        .flex_none()
        .size(px(24.))
        .justify_center()
        .cursor_pointer()
        .on_click(on_click)
        .child(
            row()
                .size(px(16.))
                .justify_center()
                .rounded(px(4.))
                .when(filled, |this| this.bg(accent))
                .when(!filled, |this| {
                    this.border_1()
                        .border_color(theme::hairline_strong())
                        .bg(theme::surface_1())
                        .group_hover("checkbox", |style| {
                            style.border_color(accent.alpha(0.6)).bg(theme::surface_2())
                        })
                })
                .when_some(glyph, |this, which| {
                    this.child(icon(which, 13.0, theme::on_primary()))
                }),
        )
}

// ---- Pills -------------------------------------------------------------------------------------

/// A quiet tinted label.
pub fn pill(text: impl Into<SharedString>, color: Hsla) -> Div {
    row()
        .flex_none()
        .h(px(20.))
        .px(sp(2.))
        .rounded(px(radius::PILL))
        .bg(color.alpha(0.14))
        .text_color(color)
        .fs(font::MICRO)
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .child(text.into())
}

/// A neutral label for facts that carry no judgement.
pub fn tag(text: impl Into<SharedString>) -> Div {
    row()
        .flex_none()
        .h(px(20.))
        .px(sp(2.))
        .rounded(px(radius::PILL))
        .bg(theme::surface_2())
        .text_color(theme::text_2())
        .fs(font::MICRO)
        .whitespace_nowrap()
        .child(text.into())
}

pub fn dot(color: Hsla, size: f32) -> Div {
    div().flex_none().size(px(size)).rounded_full().bg(color)
}

/// A dot for things happening right now. It does not pulse on purpose: a looping animation
/// repaints the whole window every frame, which is a poor trade for a status light.
pub fn live_dot(color: Hsla) -> Div {
    dot(color, 7.0)
}

// ---- Spinner -----------------------------------------------------------------------------------

pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    svg()
        .path(Icon::Spinner.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
        .with_animation(
            id,
            Animation::new(Duration::from_millis(900)).repeat(),
            |this, value| this.with_transformation(Transformation::rotate(percentage(value))),
        )
}

// ---- Segmented control and friends -------------------------------------------------------------

pub fn segmented(
    id: impl Into<SharedString>,
    labels: &[SharedString],
    selected: usize,
    on_change: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Div {
    let id: SharedString = id.into();
    let on_change = Rc::new(on_change);
    let mut container = row()
        .flex_none()
        .h(px(28.))
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(radius::SM + 1.0))
        .bg(theme::surface_2());
    for (index, label) in labels.iter().enumerate() {
        let is_selected = index == selected;
        let on_change = on_change.clone();
        container = container.child(
            row()
                .id(ElementId::NamedInteger(id.clone(), index as u64))
                .h_full()
                .px(sp(2.5))
                .justify_center()
                .rounded(px(radius::SM - 1.0))
                .fs(font::CAPTION)
                .whitespace_nowrap()
                .when(is_selected, |this| {
                    this.bg(theme::raised())
                        .text_color(theme::text_1())
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!is_selected, |this| {
                    this.text_color(theme::text_2())
                        .cursor_pointer()
                        .hover(|style| style.text_color(theme::text_1()))
                })
                .on_click(move |_, window, cx| on_change(index, window, cx))
                .child(label.clone()),
        );
    }
    container
}

/// `accent` is the state the switch turns on: green for something wanted, red for something risky.
pub fn toggle(
    id: impl Into<ElementId>,
    on: bool,
    accent: Hsla,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .w(px(34.))
        .h(px(20.))
        .p(sp(0.5))
        .rounded(px(10.))
        .cursor_pointer()
        .bg(if on { accent } else { theme::surface_4() })
        .when(on, |this| this.justify_end())
        .on_click(on_click)
        .child(div().size(px(16.)).rounded_full().bg(gpui::white()))
}

pub fn stepper(
    id: impl Into<SharedString>,
    value: impl Into<SharedString>,
    on_less: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_more: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let id: SharedString = id.into();
    row()
        .flex_none()
        .h(px(28.))
        .rounded(px(radius::SM))
        .bg(theme::surface_2())
        .child(icon_button(
            ElementId::NamedInteger(id.clone(), 0),
            Icon::Minus,
            theme::text_2(),
            on_less,
        ))
        .child(
            row()
                .px(sp(2.))
                .min_w(px(72.))
                .justify_center()
                .fs(font::CAPTION)
                .tabular()
                .text_color(theme::text_1())
                .child(value.into()),
        )
        .child(icon_button(
            ElementId::NamedInteger(id, 1),
            Icon::Plus,
            theme::text_2(),
            on_more,
        ))
}
