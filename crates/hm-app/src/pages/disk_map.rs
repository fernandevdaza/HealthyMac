//! Painting the treemap. It lays itself out at paint time, because a squarified layout needs the
//! final bounds, and does its own hit testing for the same reason.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    canvas, point, prelude::*, px, quad, Bounds, CursorStyle, DispatchPhase, FontWeight,
    HitboxBehavior, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, SharedString, Size,
    TextRun, WeakEntity, Window,
};
use hm_core::fmt::human_size;
use hm_core::t;
use hm_system::types::DirNode;

use crate::logic::treemap::{squarify, Rect};
use crate::state::{HoverTile, Shell};
use crate::theme::{self, font, radius};
use crate::ui::f;

struct Tile {
    rect: Rect,
    /// Child indices from the current folder. Empty for the "other" tile.
    path: Vec<usize>,
}

/// Paints the map and does its own hit testing, because laying it out needs the final bounds.
pub(super) fn treemap_canvas(
    current: DirNode,
    hover: Option<HoverTile>,
    trail: Vec<usize>,
    entity: WeakEntity<Shell>,
) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, cx| {
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
            let origin_x = f(bounds.origin.x);
            let origin_y = f(bounds.origin.y);
            let area = Rect::new(
                origin_x,
                origin_y,
                f(bounds.size.width),
                f(bounds.size.height),
            );
            let tiles: Rc<RefCell<Vec<Tile>>> = Rc::new(RefCell::new(Vec::new()));

            if current.children.is_empty() {
                let color =
                    theme::treemap_tile(theme::TREEMAP_HUES[0], 0, 0, current.is_dir, false);
                window.paint_quad(quad(
                    bounds,
                    px(radius::MD),
                    color,
                    px(0.),
                    theme::hairline(),
                    Default::default(),
                ));
                paint_label(
                    window,
                    cx,
                    area,
                    if current.is_dir {
                        t!(
                            "Esta carpeta no tiene nada más que ver por dentro",
                            "Nothing more to see inside this folder"
                        )
                    } else {
                        &current.name
                    },
                    &human_size(current.size),
                );
            } else {
                let mut sizes: Vec<u64> = current.children.iter().map(|child| child.size).collect();
                sizes.push(current.other_size);
                let rects = squarify(&sizes, area);
                let other_index = current.children.len();

                for (index, rect) in rects.iter().enumerate() {
                    if rect.area() < 1.0 {
                        continue;
                    }
                    if index == other_index {
                        paint_tile(window, *rect, theme::surface_3(), false);
                        if rect.w > 60.0 && rect.h > 28.0 {
                            paint_label(
                                window,
                                cx,
                                *rect,
                                t!("Otros", "Other"),
                                &human_size(current.other_size),
                            );
                        }
                        continue;
                    }
                    let child = &current.children[index];
                    let hue = theme::TREEMAP_HUES[index % theme::TREEMAP_HUES.len()];
                    let path = vec![index];
                    let hovered = hover
                        .as_ref()
                        .is_some_and(|h| h.path.first() == Some(&index));
                    let nest = child.is_dir
                        && !child.children.is_empty()
                        && rect.w > 90.0
                        && rect.h > 70.0;
                    let exact = hover.as_ref().is_some_and(|h| h.path == path);
                    paint_tile(
                        window,
                        *rect,
                        theme::treemap_tile(
                            hue,
                            0,
                            index,
                            child.is_dir,
                            hovered && (!nest || exact),
                        ),
                        false,
                    );
                    if nest {
                        // Its children inside, under a header strip naming the folder.
                        let header = 22.0;
                        paint_header(window, cx, *rect, &child.name, &human_size(child.size));
                        let inner = Rect::new(
                            rect.x + 3.0,
                            rect.y + header,
                            rect.w - 6.0,
                            rect.h - header - 3.0,
                        );
                        let mut sizes: Vec<u64> = child.children.iter().map(|c| c.size).collect();
                        sizes.push(child.other_size);
                        let inner_rects = squarify(&sizes, inner);
                        for (sub, sub_rect) in inner_rects.iter().enumerate() {
                            if sub_rect.area() < 1.0 {
                                continue;
                            }
                            if sub == child.children.len() {
                                paint_tile(window, *sub_rect, theme::treemap_rest(hue), true);
                                continue;
                            }
                            let grandchild = &child.children[sub];
                            let sub_path = vec![index, sub];
                            let sub_hover = hover.as_ref().is_some_and(|h| h.path == sub_path);
                            paint_tile(
                                window,
                                *sub_rect,
                                theme::treemap_tile(hue, 1, sub, grandchild.is_dir, sub_hover),
                                true,
                            );
                            if sub_rect.w > 56.0 && sub_rect.h > 24.0 {
                                paint_label(
                                    window,
                                    cx,
                                    *sub_rect,
                                    &grandchild.name,
                                    &human_size(grandchild.size),
                                );
                            }
                            tiles.borrow_mut().push(Tile {
                                rect: *sub_rect,
                                path: sub_path,
                            });
                        }
                    } else if rect.w > 56.0 && rect.h > 24.0 {
                        paint_label(window, cx, *rect, &child.name, &human_size(child.size));
                    }
                    tiles.borrow_mut().push(Tile { rect: *rect, path });
                }
            }

            // Hit testing. Inner tiles come first, so the deepest one wins.
            let hover_tiles = tiles.clone();
            let hover_entity = entity.clone();
            let previous = hover.clone();
            let in_map = bounds;
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let found = if in_map.contains(&event.position) {
                    find_tile(&hover_tiles.borrow(), event.position.x, event.position.y)
                } else {
                    None
                };
                let next = found.map(|path| HoverTile { path });
                if next != previous {
                    hover_entity
                        .update(cx, |shell, cx| {
                            shell.disk.hover = next;
                            cx.notify();
                        })
                        .ok();
                }
            });

            let click_tiles = tiles;
            let click_entity = entity.clone();
            let click_trail = trail.clone();
            let hitbox_for_click = hitbox.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox_for_click.is_hovered(window)
                {
                    return;
                }
                let Some(path) =
                    find_tile(&click_tiles.borrow(), event.position.x, event.position.y)
                else {
                    return;
                };
                let trail = click_trail.clone();
                click_entity
                    .update(cx, |shell, cx| shell.drill_into(trail, path, cx))
                    .ok();
            });
        },
    )
    .w_full()
    .flex_1()
    .min_h(px(0.))
}

fn find_tile(tiles: &[Tile], x: Pixels, y: Pixels) -> Option<Vec<usize>> {
    tiles
        .iter()
        .filter(|tile| tile.rect.contains(f(x), f(y)))
        .max_by_key(|tile| tile.path.len())
        .map(|tile| tile.path.clone())
}

const GAP: f32 = 1.5;

fn inset(rect: Rect) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(rect.x + GAP / 2.0), px(rect.y + GAP / 2.0)),
        size: Size {
            width: px((rect.w - GAP).max(0.5)),
            height: px((rect.h - GAP).max(0.5)),
        },
    }
}

fn paint_tile(window: &mut Window, rect: Rect, color: Hsla, inner: bool) {
    let corner: f32 = if inner { 3.0 } else { 5.0 };
    let corner = corner.min(rect.w / 3.0).min(rect.h / 3.0);
    window.paint_quad(quad(
        inset(rect),
        px(corner),
        color,
        px(0.),
        theme::hairline(),
        Default::default(),
    ));
}

fn shape(
    window: &Window,
    text: SharedString,
    size: f32,
    weight: gpui::FontWeight,
    color: Hsla,
) -> gpui::ShapedLine {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text, px(size), &[run], None)
}

/// Shortens a label from the end until it fits, or gives up and returns nothing.
fn fitted(
    window: &Window,
    text: &str,
    size: f32,
    weight: gpui::FontWeight,
    color: Hsla,
    room: f32,
) -> Option<gpui::ShapedLine> {
    let line = shape(window, text.to_string().into(), size, weight, color);
    if f(line.width) <= room {
        return Some(line);
    }
    let characters: Vec<char> = text.chars().collect();
    let keep = ((room / f(line.width)) * characters.len() as f32) as usize;
    if keep < 4 {
        return None;
    }
    let short: String = characters[..keep.saturating_sub(1)]
        .iter()
        .chain(['…'].iter())
        .collect();
    Some(shape(window, short.into(), size, weight, color))
}

fn paint_label(window: &mut Window, cx: &mut gpui::App, rect: Rect, name: &str, size_text: &str) {
    let room = rect.w - 14.0;
    let x = px(rect.x + 7.0);
    let mut y = rect.y + 5.0;
    if let Some(line) = fitted(
        window,
        name,
        font::CAPTION,
        FontWeight::MEDIUM,
        theme::text_1(),
        room,
    ) {
        line.paint(point(x, px(y)), px(17.), window, cx).ok();
        y += 15.0;
    }
    if rect.h > 42.0 {
        if let Some(line) = fitted(
            window,
            size_text,
            font::MICRO,
            FontWeight::NORMAL,
            theme::text_1().alpha(0.72),
            room,
        ) {
            line.paint(point(x, px(y)), px(16.), window, cx).ok();
        }
    }
}

fn paint_header(window: &mut Window, cx: &mut gpui::App, rect: Rect, name: &str, size_text: &str) {
    let size_line = shape(
        window,
        size_text.to_string().into(),
        font::MICRO,
        FontWeight::NORMAL,
        theme::text_1().alpha(0.72),
    );
    let size_width = f(size_line.width);
    let room = rect.w - 18.0 - size_width - 8.0;
    if let Some(line) = fitted(
        window,
        name,
        font::CAPTION,
        FontWeight::SEMIBOLD,
        theme::text_1(),
        room,
    ) {
        line.paint(
            point(px(rect.x + 7.0), px(rect.y + 3.0)),
            px(17.),
            window,
            cx,
        )
        .ok();
    }
    size_line
        .paint(
            point(px(rect.x + rect.w - size_width - 8.0), px(rect.y + 3.5)),
            px(16.),
            window,
            cx,
        )
        .ok();
}
