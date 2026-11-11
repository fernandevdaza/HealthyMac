//! The memory budget: a ruler of this Mac's memory, with every model standing where its size falls.

use gpui::{
    canvas, div, pattern_slash, point, prelude::*, px, quad, Bounds, Context, Hsla, MouseMoveEvent,
    SharedString, Size, TextRun, Window,
};
use hm_ai::types::ModelInventory;
use hm_core::fmt::{human_memory, human_size};
use hm_core::{t, tf};

use super::models::{fit_color, runtime_loaded_bytes};
use super::plural;
use crate::state::Shell;
use crate::theme::{self, font, sp, GUTTER};
use crate::ui::chrome::{big_number, card, eyebrow, notice, section_title, NoticeKind};
use crate::ui::controls::dot;
use crate::ui::{col, f, row, TextExt};

pub(super) fn render_budget_hero(
    inventory: &ModelInventory,
    hovered: Option<usize>,
    stale_error: Option<String>,
    cx: &mut Context<Shell>,
) -> impl IntoElement {
    let running = inventory
        .runtimes
        .iter()
        .filter(|runtime| runtime.running)
        .count();
    let loaded_bytes: u64 = inventory.runtimes.iter().map(runtime_loaded_bytes).sum();
    let runtime_count = inventory
        .models
        .iter()
        .map(|model| model.runtime)
        .collect::<std::collections::HashSet<_>>()
        .len();
    col()
        .w_full()
        .px(px(GUTTER))
        .pb(sp(2.))
        .gap(sp(4.))
        .when(!inventory.skipped_roots.is_empty(), |this| {
            let folders: Vec<String> = inventory
                .skipped_roots
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            this.child(notice(
                NoticeKind::Info,
                tf!(
                    "{} sin mirar: pedirían permiso a mitad de escaneo",
                    "{} not checked, to avoid a permission prompt mid-scan",
                    plural(
                        folders.len(),
                        t!("carpeta de modelos se ha quedado", "model folder"),
                        t!("carpetas de modelos se han quedado", "model folders")
                    )
                ),
                Some(
                    tf!(
                        "{}. Con Acceso total al disco entran en la cuenta.",
                        "{}. With Full Disk Access they are counted too.",
                        folders.join(", ")
                    )
                    .into(),
                ),
                None,
            ))
        })
        .when_some(stale_error, |this, error| {
            this.child(notice(
                NoticeKind::Warning,
                t!(
                    "No hemos podido actualizar: esto es lo último que sabemos",
                    "Couldn't refresh: showing the last known data"
                ),
                Some(error.into()),
                None,
            ))
        })
        .child(
            row()
                .gap(sp(10.))
                .items_start()
                .child(
                    col()
                        .gap(sp(0.5))
                        .child(eyebrow(t!("En tu disco", "On your disk")))
                        .child(if inventory.models.is_empty() {
                            big_number(t!("Nada", "None"), font::DISPLAY, theme::text_3())
                        } else {
                            big_number(
                                human_size(inventory.total_bytes),
                                font::DISPLAY,
                                theme::text_1(),
                            )
                        })
                        .child(div().fs(font::CAPTION).text_color(theme::text_2()).child(
                            if inventory.models.is_empty() {
                                t!("Ningún modelo todavía", "No models yet").to_string()
                            } else {
                                tf!(
                                    "{} en {}",
                                    "{} in {}",
                                    plural(
                                        inventory.models.len(),
                                        t!("modelo", "model"),
                                        t!("modelos", "models")
                                    ),
                                    plural(runtime_count, "runtime", "runtimes")
                                )
                            },
                        )),
                )
                .when(inventory.reclaimable_bytes > 0, |this| {
                    this.child(
                        col()
                            .gap(sp(0.5))
                            .child(eyebrow(t!("Repetidos", "Duplicates")))
                            .child(big_number(
                                human_size(inventory.reclaimable_bytes),
                                font::DISPLAY,
                                theme::text_1(),
                            ))
                            .child(
                                div()
                                    .fs(font::CAPTION)
                                    .text_color(theme::text_2())
                                    .child(t!(
                                        "se recuperan sin perder nada",
                                        "can be freed without losing anything"
                                    )),
                            ),
                    )
                })
                .child(
                    col()
                        .gap(sp(0.5))
                        .child(eyebrow(t!("Ahora en memoria", "In memory now")))
                        .child(big_number(
                            if loaded_bytes > 0 {
                                human_memory(loaded_bytes)
                            } else {
                                t!("Nada", "None").to_string()
                            },
                            font::DISPLAY,
                            if loaded_bytes > 0 {
                                theme::text_1()
                            } else {
                                theme::text_3()
                            },
                        ))
                        .child(div().fs(font::CAPTION).text_color(theme::text_2()).child(
                            if running > 0 {
                                plural(
                                    running,
                                    t!("runtime activo", "runtime running"),
                                    t!("runtimes activos", "runtimes running"),
                                )
                            } else {
                                t!("Ningún runtime activo", "No runtimes running").to_string()
                            },
                        )),
                ),
        )
        .child(render_budget(inventory, hovered, cx))
}

struct Marker {
    x: f32,
    lane: usize,
    color: Hsla,
    name: SharedString,
    size: u64,
    loaded: bool,
}

const BAR_HEIGHT: f32 = 14.0;
const LANE: f32 = 12.0;

/// The memory budget: a ruler of this Mac's memory, with every model standing where its size falls.
fn render_budget(
    inventory: &ModelInventory,
    hovered: Option<usize>,
    cx: &mut Context<Shell>,
) -> impl IntoElement {
    let budget = inventory.memory.clone();
    let known = budget.total_ram > 0;
    let largest = inventory
        .models
        .iter()
        .map(|model| model.size)
        .max()
        .unwrap_or(0);
    let scale_max = (budget.total_ram as f64 * 1.1)
        .max(largest as f64 * 1.04)
        .max(1.0);
    let models: Vec<(u64, Hsla, SharedString, bool)> = inventory
        .models
        .iter()
        .map(|model| {
            (
                model.size,
                fit_color(model.fit),
                SharedString::from(model.name.clone()),
                model.loaded,
            )
        })
        .collect();
    let entity = cx.entity().downgrade();
    let height = 124.0;

    card()
        .w_full()
        .p(sp(4.))
        .pb(sp(3.))
        .child(
            row()
                .justify_between()
                .mb(sp(2.5))
                .child(section_title(t!("Presupuesto de memoria", "Memory budget")))
                .child(
                    row()
                        .gap(sp(3.))
                        .child(legend_dot(theme::green(), t!("Sobra", "Fits easily")))
                        .child(legend_dot(theme::yellow(), t!("Justo", "Tight fit")))
                        .child(legend_dot(theme::amber(), t!("Excede la GPU", "Exceeds GPU")))
                        .child(legend_dot(theme::rose(), t!("No cabe", "Won't fit"))),
                ),
        )
        .child(if !known {
            div()
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(t!(
                    "No hemos podido leer cuánta memoria tiene este Mac, así que no podemos comparar.",
                    "Couldn't read how much memory this Mac has, so there is nothing to compare against."
                ))
                .into_any_element()
        } else {
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    let width = f(bounds.size.width);
                    let x_of = |bytes: u64| (bytes as f64 / scale_max * width as f64) as f32;
                    let ram_x = x_of(budget.total_ram);
                    let gpu_x = x_of(budget.gpu_limit.min(budget.total_ram));
                    let bar_y = f(bounds.size.height) - 34.0 - BAR_HEIGHT;
                    let origin = bounds.origin;
                    let rect = |x: f32, w: f32| Bounds {
                        origin: point(origin.x + px(x), origin.y + px(bar_y)),
                        size: Size { width: px(w.max(0.0)), height: px(BAR_HEIGHT) },
                    };

                    // Everything past the RAM: no fit at all.
                    window.paint_quad(quad(
                        rect(0.0, width),
                        px(BAR_HEIGHT / 2.0),
                        theme::surface_2(),
                        px(0.),
                        theme::hairline(),
                        Default::default(),
                    ));
                    // Between the GPU limit and the RAM: possible, but slow.
                    window.paint_quad(quad(
                        rect(gpu_x, ram_x - gpu_x),
                        px(2.),
                        pattern_slash(theme::amber().alpha(0.42), 1.5, 5.0),
                        px(0.),
                        theme::hairline(),
                        Default::default(),
                    ));
                    // Up to the GPU limit: where a model runs properly.
                    window.paint_quad(quad(
                        rect(0.0, gpu_x),
                        gpui::Corners {
                            top_left: px(BAR_HEIGHT / 2.0),
                            bottom_left: px(BAR_HEIGHT / 2.0),
                            top_right: px(2.),
                            bottom_right: px(2.),
                        },
                        theme::text_3().alpha(0.5),
                        px(0.),
                        theme::hairline(),
                        Default::default(),
                    ));

                    // Stand each model on the bar. Close neighbours take the next lane up.
                    let mut markers: Vec<Marker> = Vec::with_capacity(models.len());
                    let mut lanes: Vec<f32> = Vec::new();
                    let mut order: Vec<usize> = (0..models.len()).collect();
                    order.sort_by_key(|&index| models[index].0);
                    for index in order {
                        let (size, color, name, loaded) = &models[index];
                        let x = x_of(*size).clamp(2.0, width - 2.0);
                        let lane = lanes
                            .iter()
                            .position(|&last| x - last >= 9.0)
                            .unwrap_or(lanes.len());
                        let capped = lane.min(4);
                        if lane >= lanes.len() {
                            lanes.push(x);
                        } else {
                            lanes[lane] = x;
                        }
                        markers.push(Marker { x, lane: capped, color: *color, name: name.clone(), size: *size, loaded: *loaded });
                    }

                    let dot_at = |marker: &Marker| {
                        point(
                            origin.x + px(marker.x),
                            origin.y + px(bar_y - 10.0 - marker.lane as f32 * LANE),
                        )
                    };
                    for marker in &markers {
                        let at = dot_at(marker);
                        window.paint_quad(gpui::fill(
                            Bounds {
                                origin: point(at.x - px(0.5), at.y),
                                size: Size { width: px(1.), height: origin.y + px(bar_y) - at.y },
                            },
                            marker.color.alpha(0.35),
                        ));
                    }
                    for (index, marker) in markers.iter().enumerate() {
                        let at = dot_at(marker);
                        let radius = if hovered == Some(index) { 5.0 } else { 3.5 };
                        if marker.loaded {
                            window.paint_quad(quad(
                                Bounds {
                                    origin: point(at.x - px(radius + 3.0), at.y - px(radius + 3.0)),
                                    size: Size { width: px((radius + 3.0) * 2.0), height: px((radius + 3.0) * 2.0) },
                                },
                                px(radius + 3.0),
                                marker.color.alpha(0.18),
                                px(1.),
                                marker.color,
                                Default::default(),
                            ));
                        }
                        window.paint_quad(quad(
                            Bounds {
                                origin: point(at.x - px(radius), at.y - px(radius)),
                                size: Size { width: px(radius * 2.0), height: px(radius * 2.0) },
                            },
                            px(radius),
                            marker.color,
                            px(0.),
                            theme::hairline(),
                            Default::default(),
                        ));
                    }

                    // Labels under the ruler.
                    let font = window.text_style().font();
                    let shape = |text: String, color: Hsla, window: &Window| {
                        let run = TextRun {
                            len: text.len(),
                            font: font.clone(),
                            color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        window.text_system().shape_line(text.into(), px(font::MICRO), &[run], None)
                    };
                    let label_y = origin.y + px(bar_y + BAR_HEIGHT + 8.0);
                    for (x, text) in [
                        (gpu_x, tf!("Límite de GPU · {}", "GPU limit · {}", human_memory(budget.gpu_limit))),
                        (ram_x, format!("RAM · {}", human_memory(budget.total_ram))),
                    ] {
                        window.paint_quad(gpui::fill(
                            Bounds {
                                origin: point(origin.x + px(x - 0.5), origin.y + px(bar_y - 2.0)),
                                size: Size { width: px(1.), height: px(BAR_HEIGHT + 8.0) },
                            },
                            theme::text_2(),
                        ));
                        let line = shape(text, theme::text_2(), window);
                        let label_x = (x - f(line.width) / 2.0).clamp(0.0, (width - f(line.width)).max(0.0));
                        line.paint(point(origin.x + px(label_x), label_y), px(16.), window, cx).ok();
                    }

                    // The hovered model's name, kept inside the card.
                    if let Some(marker) = hovered.and_then(|index| markers.get(index)) {
                        let at = dot_at(marker);
                        let text = format!("{} · {}", marker.name, human_size(marker.size));
                        let line = shape(text, theme::text_1(), window);
                        let w = f(line.width) + 16.0;
                        let x = (marker.x - w / 2.0).clamp(0.0, (width - w).max(0.0));
                        let y = (f(at.y - origin.y) - 30.0).max(0.0);
                        let chip = Bounds {
                            origin: point(origin.x + px(x), origin.y + px(y)),
                            size: Size { width: px(w), height: px(22.) },
                        };
                        window.paint_quad(quad(
                            chip,
                            px(6.),
                            theme::sheet_bg(),
                            px(1.),
                            theme::hairline_strong(),
                            Default::default(),
                        ));
                        line.paint(point(chip.origin.x + px(8.), chip.origin.y + px(3.)), px(16.), window, cx).ok();
                    }

                    // Hit testing: the nearest dot within reach.
                    let dots: Vec<(f32, f32)> = markers
                        .iter()
                        .map(|marker| {
                            let at = dot_at(marker);
                            (f(at.x), f(at.y))
                        })
                        .collect();
                    let entity = entity.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                        if phase != gpui::DispatchPhase::Bubble {
                            return;
                        }
                        let mouse = (f(event.position.x), f(event.position.y));
                        let in_bounds = bounds.contains(&event.position);
                        let found = in_bounds
                            .then(|| {
                                dots.iter()
                                    .enumerate()
                                    .map(|(index, dot)| {
                                        let dx = dot.0 - mouse.0;
                                        let dy = dot.1 - mouse.1;
                                        (index, dx * dx + dy * dy)
                                    })
                                    .filter(|(_, distance)| *distance <= 144.0)
                                    .min_by(|a, b| a.1.total_cmp(&b.1))
                                    .map(|(index, _)| index)
                            })
                            .flatten();
                        if found != hovered {
                            entity
                                .update(cx, |shell, cx| {
                                    shell.models.hovered_marker = found;
                                    cx.notify();
                                })
                                .ok();
                        }
                    });
                },
            )
            .w_full()
            .h(px(height))
            .into_any_element()
        })
}

fn legend_dot(color: Hsla, label: &'static str) -> impl IntoElement {
    row().gap(px(5.)).child(dot(color, 7.0)).child(
        div()
            .fs(font::MICRO)
            .text_color(theme::text_3())
            .child(label),
    )
}
