//! Design tokens: colour, type scale, spacing and radii. Nothing else defines a colour.
//!
//! The look follows `docs/DESIGN.md`: a quiet, neutral Mac utility where colour reports state and
//! never decorates. Every token has a dark and a light value, and the active one is chosen by
//! [`set_dark`], which follows the system unless the user picked a side in Settings.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{point, px, rgb, BoxShadow, Hsla, Pixels};
use hm_core::{Area, Risk};

pub const SIDEBAR_WIDTH: f32 = 216.0;
pub const MIN_WINDOW_W: f32 = 980.0;
pub const MIN_WINDOW_H: f32 = 640.0;
/// How far from the top the page's content starts, clearing the drag region.
pub const TITLEBAR_H: f32 = 44.0;
/// Page gutter.
pub const GUTTER: f32 = 28.0;

/// 4 pt spacing grid: `sp(3)` is 12 px.
pub fn sp(steps: f32) -> Pixels {
    px(steps * 4.0)
}

// ---- Type scale --------------------------------------------------------------------------------

pub mod font {
    pub const DISPLAY: f32 = 34.0;
    pub const TITLE: f32 = 26.0;
    pub const HEADLINE: f32 = 18.0;
    pub const SUBHEAD: f32 = 15.0;
    pub const BODY: f32 = 13.0;
    pub const CAPTION: f32 = 12.0;
    pub const MICRO: f32 = 11.0;
}

// ---- Radii -------------------------------------------------------------------------------------

/// One scale, used the same way everywhere: controls, then cards and rows, then sheets. Labels are
/// pills.
pub mod radius {
    /// Buttons, inputs, segmented controls, checkboxes.
    pub const SM: f32 = 6.0;
    /// Cards, list rows, notices.
    pub const MD: f32 = 10.0;
    /// Sheets and other large containers.
    pub const LG: f32 = 14.0;
    pub const PILL: f32 = 100.0;
}

// ---- Mode --------------------------------------------------------------------------------------

static DARK: AtomicBool = AtomicBool::new(true);

/// Switches every token at once. The caller redraws.
pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

fn pick(dark: u32, light: u32) -> Hsla {
    hex(if is_dark() { dark } else { light })
}

// ---- Colour ------------------------------------------------------------------------------------

pub fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

pub fn hexa(value: u32, alpha: f32) -> Hsla {
    hex(value).alpha(alpha)
}

/// The foreground at some strength: white on dark, black on light. Surfaces and lines are made of
/// it, so they keep their weight in both modes.
fn ink(dark_alpha: f32, light_alpha: f32) -> Hsla {
    if is_dark() {
        hexa(0xFFFFFF, dark_alpha)
    } else {
        hexa(0x000000, light_alpha)
    }
}

/// The sidebar, which is the window itself and runs under the traffic lights.
pub fn window_bg() -> Hsla {
    pick(0x1C1C1E, 0xF2F2F4)
}
/// The page.
pub fn panel_bg() -> Hsla {
    pick(0x151516, 0xFFFFFF)
}
pub fn sheet_bg() -> Hsla {
    pick(0x232325, 0xFFFFFF)
}
pub fn scrim() -> Hsla {
    if is_dark() {
        hexa(0x000000, 0.55)
    } else {
        hexa(0x000000, 0.22)
    }
}

pub fn surface_1() -> Hsla {
    ink(0.03, 0.025)
}
pub fn surface_2() -> Hsla {
    ink(0.06, 0.045)
}
pub fn surface_3() -> Hsla {
    ink(0.09, 0.07)
}
pub fn surface_4() -> Hsla {
    ink(0.13, 0.10)
}
pub fn hairline() -> Hsla {
    ink(0.08, 0.09)
}
pub fn hairline_strong() -> Hsla {
    ink(0.14, 0.16)
}
/// A segment or tab that is lifted out of its track: a lighter grey on dark, white on light.
pub fn raised() -> Hsla {
    if is_dark() {
        hexa(0xFFFFFF, 0.14)
    } else {
        hex(0xFFFFFF)
    }
}

pub fn text_1() -> Hsla {
    pick(0xEDEDEF, 0x111113)
}
pub fn text_2() -> Hsla {
    pick(0xA3A3A8, 0x5C5C61)
}
/// Metadata. Still passes AA against its background in both modes.
pub fn text_3() -> Hsla {
    pick(0x86868B, 0x76767B)
}

/// The primary button and anything selected: the text colour itself, so nothing competes with it.
pub fn primary() -> Hsla {
    text_1()
}
pub fn primary_hover() -> Hsla {
    pick(0xFFFFFF, 0x2C2C2E)
}
/// Text and glyphs on [`primary`].
pub fn on_primary() -> Hsla {
    panel_bg()
}

// ---- State colours: macOS system colours, with their high-contrast variants on light ----------

pub fn green() -> Hsla {
    pick(0x30D158, 0x248A3D)
}
/// Between fine and a problem, such as a model that only just fits.
pub fn yellow() -> Hsla {
    pick(0xFFD60A, 0x8A6A00)
}
/// On light, a brown amber rather than Apple's reddish orange, so Moderado never reads as Revisar.
pub fn amber() -> Hsla {
    pick(0xFF9F0A, 0xB45309)
}
pub fn rose() -> Hsla {
    pick(0xFF453A, 0xD70015)
}
pub fn blue() -> Hsla {
    pick(0x0A84FF, 0x0040DD)
}

// ---- Categorical colours: charts only ----------------------------------------------------------

pub fn teal() -> Hsla {
    pick(0x5FB3B3, 0x3E8E8E)
}
pub fn violet() -> Hsla {
    pick(0x9C8CD9, 0x7565C2)
}
pub fn lime() -> Hsla {
    pick(0x9CB86A, 0x6F8F3A)
}
pub fn orange() -> Hsla {
    pick(0xD99A5E, 0xB8722F)
}
pub fn slate() -> Hsla {
    pick(0x6C9BD2, 0x4A78B5)
}

/// Hues for the disk map's top-level folders. Muted, like the other chart colours.
pub const TREEMAP_HUES: [f32; 8] = [0.48, 0.70, 0.58, 0.08, 0.97, 0.53, 0.40, 0.78];

/// A disk-map tile. Deeper tiles step towards the foreground, so children read as lit from inside
/// their parent: lighter on dark, darker on light. Labels on them use `text_1`, which stays legible
/// either way because the tiles keep to the background's half of the lightness range.
pub fn treemap_tile(hue: f32, depth: usize, rank: usize, is_dir: bool, hovered: bool) -> Hsla {
    let step = 0.075 * depth as f32 + 0.012 * (rank % 4) as f32;
    let saturation = if is_dir { 0.34 } else { 0.2 };
    let lightness = if is_dark() {
        (0.17 + step + if hovered { 0.08 } else { 0.0 }).min(0.6)
    } else {
        (0.90 - step * 0.8 - if hovered { 0.06 } else { 0.0 }).max(0.55)
    };
    gpui::hsla(hue, saturation, lightness, 1.0)
}

/// The "everything else" tile inside a folder: quieter than any real child.
pub fn treemap_rest(hue: f32) -> Hsla {
    if is_dark() {
        gpui::hsla(hue, 0.14, 0.15, 1.0)
    } else {
        gpui::hsla(hue, 0.10, 0.93, 1.0)
    }
}

/// An area's series colour in charts (the disk bar, its legend). Not for icons, buttons or text.
pub fn area_color(area: Area) -> Hsla {
    match area {
        Area::System => teal(),
        Area::Developer => violet(),
        Area::LocalAi => orange(),
        Area::Apps => lime(),
        Area::Files => slate(),
    }
}

pub fn risk_color(risk: Risk) -> Hsla {
    match risk {
        Risk::Safe => green(),
        Risk::Moderate => amber(),
        Risk::Review => rose(),
    }
}

/// Interpolates in HSL along the shortest way round the hue circle.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    let mut delta = b.h - a.h;
    if delta > 0.5 {
        delta -= 1.0;
    } else if delta < -0.5 {
        delta += 1.0;
    }
    Hsla {
        h: (a.h + delta * t).rem_euclid(1.0),
        s: a.s + (b.s - a.s) * t,
        l: a.l + (b.l - a.l) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

// ---- Elevation ---------------------------------------------------------------------------------

/// Sheets and toasts: the only things that float.
pub fn sheet_shadow() -> Vec<BoxShadow> {
    let (far, near) = if is_dark() { (0.5, 0.3) } else { (0.16, 0.08) };
    vec![
        BoxShadow {
            color: hexa(0x000000, far),
            offset: point(px(0.), px(20.)),
            blur_radius: px(56.),
            spread_radius: px(0.),
        },
        BoxShadow {
            color: hexa(0x000000, near),
            offset: point(px(0.), px(2.)),
            blur_radius: px(8.),
            spread_radius: px(0.),
        },
    ]
}
