//! Things drawn by hand: the health ring, sparklines, and stacked and progress bars.

use std::f32::consts::TAU;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    canvas, div, ease_out_quint, linear_color_stop, linear_gradient, point, prelude::*, px,
    relative, Animation, AnimationExt, Background, Div, ElementId, FontWeight, Hsla, PathBuilder,
    Pixels, Point, SharedString, Window,
};

use super::{col, f, row, TextExt};
use crate::theme;

// ---- Ring --------------------------------------------------------------------------------------

/// An arc of constant thickness, as a filled polygon. Angles in turns, clockwise from 12 o'clock.
fn paint_arc(
    window: &mut Window,
    center: Point<Pixels>,
    radius: f32,
    thickness: f32,
    from: f32,
    to: f32,
    color: impl Into<Background>,
) {
    let span = to - from;
    if span <= 0.0 {
        return;
    }
    let steps = ((span * 180.0).ceil() as usize).max(1);
    let outer = radius + thickness / 2.0;
    let inner = radius - thickness / 2.0;
    let at = |r: f32, t: f32| {
        let angle = (from + span * t - 0.25) * TAU;
        point(
            center.x + px(r * angle.cos()),
            center.y + px(r * angle.sin()),
        )
    };
    let mut points = Vec::with_capacity((steps + 1) * 2);
    for step in 0..=steps {
        points.push(at(outer, step as f32 / steps as f32));
    }
    for step in (0..=steps).rev() {
        points.push(at(inner, step as f32 / steps as f32));
    }
    let mut builder = PathBuilder::fill();
    builder.add_polygon(&points, true);
    if let Ok(path) = builder.build() {
        window.paint_path(path, color.into());
    }
}

fn paint_disc(window: &mut Window, center: Point<Pixels>, radius: f32, color: Hsla) {
    let points: Vec<_> = (0..28)
        .map(|step| {
            let angle = step as f32 / 28.0 * TAU;
            point(
                center.x + px(radius * angle.cos()),
                center.y + px(radius * angle.sin()),
            )
        })
        .collect();
    let mut builder = PathBuilder::fill();
    builder.add_polygon(&points, true);
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// A ring whose colour sweeps from one stop to the other, which a linear gradient cannot do, so the
/// arc is drawn as many short segments.
pub fn paint_ring(
    window: &mut Window,
    center: Point<Pixels>,
    radius: f32,
    thickness: f32,
    fraction: f32,
    stops: (Hsla, Hsla),
) {
    paint_arc(
        window,
        center,
        radius,
        thickness,
        0.0,
        1.0,
        theme::surface_2(),
    );
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return;
    }
    // A full score is a closed ring: no caps, nothing to meet.
    let full = fraction >= 0.999;
    // Near the top the two round caps would touch and pinch the ring where it starts. Keep a clean
    // gap of a cap's width plus a little air between them instead.
    let circumference = TAU * radius.max(1.0);
    let fraction = if full {
        1.0
    } else {
        fraction.min(1.0 - (thickness + 4.0) / circumference)
    };
    let segments = ((fraction * 96.0).ceil() as usize).max(1);
    let step = fraction / segments as f32;
    for index in 0..segments {
        let from = index as f32 * step;
        // Each segment overlaps the next slightly, to hide the seam.
        let to = (from + step + 0.002).min(fraction);
        let color = theme::mix(stops.0, stops.1, index as f32 / segments.max(2) as f32);
        paint_arc(window, center, radius, thickness, from, to, color);
    }
    if full {
        return;
    }
    // Round caps.
    let cap = |turns: f32| {
        let angle = (turns - 0.25) * TAU;
        point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        )
    };
    paint_disc(window, cap(0.0), thickness / 2.0, stops.0);
    paint_disc(
        window,
        cap(fraction),
        thickness / 2.0,
        theme::mix(stops.0, stops.1, 1.0),
    );
}

/// The ring's colour for a score, as a state: fine, worth a look, or a problem. The same colour at
/// both ends, because the ring reports a state and is not decoration.
pub fn score_stops(score: f32) -> (Hsla, Hsla) {
    // The same bands as the labels in hm_system::health: Buena and up, Regular, Atención.
    let color = if score >= 75.0 {
        theme::green()
    } else if score >= 50.0 {
        theme::amber()
    } else {
        theme::rose()
    };
    (color, color)
}

/// The animated health ring with the score and its label in the middle.
pub fn health_ring(
    key: u64,
    from: f32,
    to: f32,
    label: SharedString,
    diameter: f32,
) -> impl IntoElement {
    let thickness = (diameter * 0.078).round();
    let stops = score_stops(to);
    div()
        .relative()
        .flex_none()
        .size(px(diameter))
        .with_animation(
            ElementId::NamedInteger("health-ring".into(), key),
            Animation::new(Duration::from_millis(1100)).with_easing(ease_out_quint()),
            move |this, t| {
                let value = from + (to - from) * t;
                let label = label.clone();
                this.child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let center = bounds.center();
                            let radius = (f(bounds.size.width) - thickness) / 2.0;
                            paint_ring(window, center, radius, thickness, value / 100.0, stops);
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .child(
                    col()
                        .absolute()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_size(px((diameter * 0.32).round()))
                                .line_height(px((diameter * 0.36).round()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .tabular()
                                .text_color(theme::text_1())
                                .child(format!("{}", value.round() as i32)),
                        )
                        .child(
                            div()
                                .fs(theme::font::BODY)
                                .text_color(theme::text_2())
                                .child(label),
                        ),
                )
            },
        )
}

// ---- Sparkline ---------------------------------------------------------------------------------

/// A line with a soft fill below it. Values are mapped so that `max` touches the top.
pub fn sparkline(values: Rc<Vec<f32>>, max: f32, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            if values.len() < 2 || max <= 0.0 {
                return;
            }
            let width = f(bounds.size.width);
            let height = f(bounds.size.height);
            let step = width / (values.len() - 1) as f32;
            let map = |index: usize, value: f32| {
                let y = height - (value / max).clamp(0.0, 1.0) * (height - 3.0) - 1.5;
                point(
                    bounds.origin.x + px(index as f32 * step),
                    bounds.origin.y + px(y),
                )
            };
            let mut area = PathBuilder::fill();
            area.move_to(point(bounds.origin.x, bounds.origin.y + bounds.size.height));
            for (index, value) in values.iter().enumerate() {
                area.line_to(map(index, *value));
            }
            area.line_to(point(
                bounds.origin.x + bounds.size.width,
                bounds.origin.y + bounds.size.height,
            ));
            area.close();
            if let Ok(path) = area.build() {
                window.paint_path(
                    path,
                    linear_gradient(
                        180.,
                        linear_color_stop(color.alpha(0.28), 0.),
                        linear_color_stop(color.alpha(0.0), 1.),
                    ),
                );
            }
            let mut line = PathBuilder::stroke(px(1.5));
            for (index, value) in values.iter().enumerate() {
                let at = map(index, *value);
                if index == 0 {
                    line.move_to(at);
                } else {
                    line.line_to(at);
                }
            }
            if let Ok(path) = line.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size_full()
}

// ---- Bars --------------------------------------------------------------------------------------

/// A horizontal bar split into proportional segments that always fills its row.
///
/// Segments share the row through flex-grow rather than percentage widths: a scrolling sheet lays out
/// before its width is definite, and percentages then collapse to nothing.
pub fn stack_bar(segments: &[(f32, Background)], height: f32) -> Div {
    let visible: Vec<_> = segments
        .iter()
        .filter(|(fraction, _)| *fraction > 0.0)
        .collect();
    let corner = px(height / 2.0);
    let count = visible.len();
    row()
        .w_full()
        .h(px(height))
        .gap(px(2.))
        .children(
            visible
                .into_iter()
                .enumerate()
                .map(|(index, (fraction, color))| {
                    let mut segment = div()
                        .h_full()
                        .min_w(px(2.))
                        .bg(*color)
                        .when(index == 0, |this| this.rounded_l(corner))
                        .when(index + 1 == count, |this| this.rounded_r(corner));
                    let style = segment.style();
                    // Scaled up because flex-grow leaves space unfilled whenever the factors in play add up
                    // to less than 1, which is what happens once a tiny segment freezes at its minimum.
                    style.flex_grow = Some(*fraction * 100.0);
                    style.flex_shrink = Some(1.0);
                    style.flex_basis = Some(px(0.).into());
                    segment
                }),
        )
}

/// A determinate bar.
pub fn progress_bar(fraction: f32, color: Hsla, height: f32) -> Div {
    let fraction = fraction.clamp(0.0, 1.0);
    let corner = px(height / 2.0);
    div()
        .w_full()
        .h(px(height))
        .rounded(corner)
        .bg(theme::surface_2())
        .when(fraction > 0.0, |this| {
            this.child(
                div()
                    .h_full()
                    .w(relative(fraction))
                    .min_w(px(height))
                    .rounded(corner)
                    .bg(color),
            )
        })
}

/// An indeterminate bar, for when there is no honest way to say how far along something is.
pub fn indeterminate_bar(id: impl Into<ElementId>, color: Hsla, height: f32) -> Div {
    let corner = px(height / 2.0);
    div()
        .relative()
        .w_full()
        .h(px(height))
        .rounded(corner)
        .bg(theme::surface_2())
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .top(px(0.))
                .h_full()
                .w(relative(0.3))
                .rounded(corner)
                .bg(color)
                .with_animation(
                    id,
                    Animation::new(Duration::from_millis(1400))
                        .repeat()
                        .with_easing(gpui::ease_in_out),
                    |this, t| this.left(relative(-0.3 + 1.3 * t)),
                ),
        )
}

/// A vertical usage bar, for per-core CPU.
pub fn meter(fraction: f32, color: Hsla, height: f32) -> Div {
    let fraction = fraction.clamp(0.0, 1.0);
    div()
        .flex()
        .flex_col()
        .justify_end()
        .h(px(height))
        .flex_1()
        .min_w(px(3.))
        .rounded(px(2.))
        .bg(theme::surface_2())
        .child(
            div()
                .w_full()
                .h(relative(fraction))
                .rounded(px(2.))
                .bg(color),
        )
}
