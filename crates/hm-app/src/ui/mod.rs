//! Our own component set. No third-party widgets, because the look is the point.

pub mod charts;
pub mod chrome;
pub mod controls;
pub mod label;
pub mod motion;
pub mod scrollbar;

use std::sync::{Arc, LazyLock};

use gpui::{div, prelude::*, px, svg, Div, FontFeatures, Hsla, Pixels, Svg};

use crate::assets::Icon;

pub use label::{clip, clip_middle};

/// Pixels as a plain float. GPUI keeps the inner value private.
pub fn f(value: Pixels) -> f32 {
    value.into()
}

static TABULAR: LazyLock<FontFeatures> =
    LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".to_string(), 1)])));

pub trait TextExt: Styled + Sized {
    /// Digits of equal width, so that columns of numbers line up.
    fn tabular(mut self) -> Self {
        self.text_style()
            .get_or_insert_with(Default::default)
            .font_features = Some(TABULAR.clone());
        self
    }

    /// Font size with a line height that goes with it.
    fn fs(self, size: f32) -> Self {
        let leading = if size >= 24.0 { 1.15 } else { 1.4 };
        self.text_size(px(size))
            .line_height(px((size * leading).round()))
    }
}

impl<T: Styled> TextExt for T {}

pub fn icon(which: Icon, size: f32, color: Hsla) -> Svg {
    svg()
        .path(which.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

pub fn row() -> Div {
    div().flex().flex_row().items_center()
}

pub fn col() -> Div {
    div().flex().flex_col()
}
