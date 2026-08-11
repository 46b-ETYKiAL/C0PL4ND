//! Frameless window edge/corner resize (#24): the pure hit-test, the cursor
//! mapping, and the per-frame manual resize driver.
//!
//! A self-contained cluster lifted verbatim out of `egui_app/mod.rs`: three free
//! functions plus the four geometry constants only they read, reached from
//! `frame_tick` through the single `handle_frameless_resize` entry point. Its
//! `resize_tests` regression guard moves with it, so the test sits beside the
//! geometry it pins instead of staying behind in the parent module.

use super::*;

/// Width of the 4 edge resize zones, in logical px. Slim so they only intercept
/// pointer events right at the window border (#24).
const RESIZE_EDGE_PX: f32 = 8.0;
/// Side length of the 4 corner resize zones, in logical px. Slightly larger than
/// the edges so diagonal grabs are forgiving (#24).
const RESIZE_CORNER_PX: f32 = 12.0;

/// Which window-edge resize direction (if any) the pointer `p` is over, given
/// the window `rect` and the edge/corner band widths. Corners (within `corner`
/// of two sides) take priority over straight edges; the interior returns `None`.
/// Pure + unit-tested so the frameless-resize hit-testing can't silently regress
/// into eating clicks meant for the tabs / caption buttons / panes (#24).
fn resize_dir_at(
    p: egui::Pos2,
    rect: egui::Rect,
    edge: f32,
    corner: f32,
) -> Option<egui::ResizeDirection> {
    use egui::ResizeDirection as D;
    let (l, r, t, b) = (
        p.x - rect.left(),
        rect.right() - p.x,
        p.y - rect.top(),
        rect.bottom() - p.y,
    );
    // Outside the window → not a resize zone.
    if l < 0.0 || r < 0.0 || t < 0.0 || b < 0.0 {
        return None;
    }
    let (w, e, n, s) = (l <= edge, r <= edge, t <= edge, b <= edge);
    let (nw, ne, nn, ns) = (l <= corner, r <= corner, t <= corner, b <= corner);
    if (n && nw) || (w && nn) {
        Some(D::NorthWest)
    } else if (n && ne) || (e && nn) {
        Some(D::NorthEast)
    } else if (s && nw) || (w && ns) {
        Some(D::SouthWest)
    } else if (s && ne) || (e && ns) {
        Some(D::SouthEast)
    } else if n {
        Some(D::North)
    } else if s {
        Some(D::South)
    } else if w {
        Some(D::West)
    } else if e {
        Some(D::East)
    } else {
        None
    }
}

/// Minimum window size (logical points) the manual edge-resize enforces. Mirrors
/// the `with_min_inner_size` seed in `egui_main.rs`.
const MIN_WINDOW_W: f32 = 520.0;
const MIN_WINDOW_H: f32 = 360.0;

fn resize_cursor(dir: egui::ResizeDirection) -> egui::CursorIcon {
    use egui::{CursorIcon as C, ResizeDirection as D};
    match dir {
        D::North => C::ResizeNorth,
        D::South => C::ResizeSouth,
        D::East => C::ResizeEast,
        D::West => C::ResizeWest,
        D::NorthEast => C::ResizeNorthEast,
        D::NorthWest => C::ResizeNorthWest,
        D::SouthEast => C::ResizeSouthEast,
        D::SouthWest => C::ResizeSouthWest,
    }
}

/// Frameless window edge-resize — MANUAL, not the OS modal loop.
///
/// This app draws its own frameless titlebar and STRIPS `WS_SYSMENU` (to kill the
/// doubled native close button). `ViewportCommand::BeginResize` drives resize via
/// `WM_SYSCOMMAND | SC_SIZE`, a system-menu command that needs `WS_SYSMENU` — with
/// it stripped, BeginResize entered a broken OS modal-resize loop that PAUSED the
/// render thread and HUNG the window (garbled, unresponsive). So instead we resize
/// MANUALLY: hold the direction across frames and each frame apply the pointer's
/// motion to the window's inner size via `InnerSize`. No OS modal loop → the event
/// loop keeps pumping → no freeze, no `WS_SYSMENU` dependency. All math is in
/// egui's LOGICAL-POINT space (`viewport_rect`, `pointer.delta`, `InnerSize`,
/// `outer_rect`, `OuterPosition` all agree), so it is HiDPI-correct by construction.
///
/// All eight edges/corners resize. East/South only grow the inner size (top-left
/// stays put — one `InnerSize`). West/North also move the window's top-left via
/// `OuterPosition` so the OPPOSITE edge stays anchored, reading the current outer
/// origin from `ViewportInfo::outer_rect`; if the platform does not report it,
/// those origin-moving edges no-op rather than drift.
pub(super) fn handle_frameless_resize(ctx: &egui::Context) {
    use egui::ResizeDirection as D;
    let id = egui::Id::new("c0pl4nd_manual_resize_dir");

    // A resize in progress? Drive it from this frame's pointer motion until the
    // primary button is released.
    let active: Option<D> = ctx.data(|d| d.get_temp(id));
    if let Some(dir) = active {
        if !ctx.input(|i| i.pointer.primary_down()) {
            ctx.data_mut(|d| d.remove::<D>(id)); // released → stop resizing
            return;
        }
        ctx.set_cursor_icon(resize_cursor(dir));
        // Keep the grid from starting a text-selection while we own the drag.
        ctx.stop_dragging();
        let delta = ctx.input(|i| i.pointer.delta());
        if delta == egui::Vec2::ZERO {
            return;
        }
        let cur = ctx.viewport_rect().size();
        // Current outer origin (top-left) in LOGICAL points. eframe fills
        // ViewportInfo by dividing the physical winit rect by pixels_per_point, and
        // its OuterPosition/InnerSize command handlers multiply by that SAME ppp —
        // so the whole computation stays in one consistent logical space and is
        // HiDPI-correct. Decorations are off, so outer≈inner (no frame inset).
        let outer_min = ctx.input(|i| i.viewport().outer_rect.map(|r| r.min));

        let west = matches!(dir, D::West | D::NorthWest | D::SouthWest);
        let east = matches!(dir, D::East | D::NorthEast | D::SouthEast);
        let north = matches!(dir, D::North | D::NorthWest | D::NorthEast);
        let south = matches!(dir, D::South | D::SouthWest | D::SouthEast);

        // The origin-moving edges (West/North) keep the OPPOSITE edge fixed by
        // moving the window's top-left as they resize — which needs the current
        // outer origin. If the platform did not report it, skip rather than let the
        // window drift.
        if (west || north) && outer_min.is_none() {
            return;
        }
        let origin0 = outer_min.unwrap_or(egui::Pos2::ZERO);
        let mut origin = origin0;
        let mut nw = cur.x;
        let mut nh = cur.y;

        if east {
            nw = (cur.x + delta.x).max(MIN_WINDOW_W);
        } else if west {
            nw = (cur.x - delta.x).max(MIN_WINDOW_W);
            origin.x += cur.x - nw; // right edge stays put
        }
        if south {
            nh = (cur.y + delta.y).max(MIN_WINDOW_H);
        } else if north {
            nh = (cur.y - delta.y).max(MIN_WINDOW_H);
            origin.y += cur.y - nh; // bottom edge stays put
        }

        // Move first (if the origin changed), then resize. Both commands apply this
        // frame, so the final rect is the same regardless of order.
        if origin != origin0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(origin));
        }
        let ns = egui::vec2(nw, nh);
        if ns != cur {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(ns));
        }
        return;
    }

    // Not resizing: hint the cursor over a supported edge band and START a resize
    // on a primary press there. We do NOT gate on `egui_is_using_pointer()`: the
    // terminal grid's background senses the pointer, so egui reports "using
    // pointer" the instant the button lands ANYWHERE over the pane — that gate was
    // true on every press and silently blocked every resize (confirmed by the
    // resize-debug log). Instead we rely on the geometry: the edge band is only a
    // few logical px at the very window border (`RESIZE_EDGE_PX` / corners
    // `RESIZE_CORNER_PX`), well outboard of the tabs, caption buttons, and pane
    // splitters, so a press there is unambiguously an edge grab. `stop_dragging()`
    // cancels any text-selection the grid would otherwise begin under our drag.
    // SAFE with the manual resize (unlike BeginResize): no OS modal loop to hang.
    let Some(p) = ctx.pointer_latest_pos() else {
        return;
    };
    let Some(dir) = resize_dir_at(p, ctx.viewport_rect(), RESIZE_EDGE_PX, RESIZE_CORNER_PX) else {
        return;
    };
    ctx.set_cursor_icon(resize_cursor(dir));
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.data_mut(|d| d.insert_temp(id, dir));
        ctx.stop_dragging();
    }
}

#[cfg(test)]
mod resize_tests {
    //! Regression guard for the frameless edge-resize hit-testing (#24). The
    //! interior MUST NOT be a resize zone (that is what would make the resize
    //! overlay eat tab / caption / pane clicks); edges/corners must map to the
    //! right direction. Pure, so it runs every CI build and pins the geometry
    //! across window sizes. Ported from the SCR1B3 sibling app. The OS resize
    //! itself (`BeginResize` + `stop_dragging`) is OS-level and not headless-
    //! testable; this pins the pure hit-test that drives it.
    use super::resize_dir_at;
    use egui::{pos2, Rect, ResizeDirection as D};

    fn win() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 700.0))
    }

    #[test]
    fn interior_is_never_a_resize_zone() {
        assert_eq!(resize_dir_at(pos2(500.0, 350.0), win(), 6.0, 12.0), None);
        // A representative titlebar position — must NOT be grabbed as a resize.
        assert_eq!(resize_dir_at(pos2(574.0, 48.0), win(), 6.0, 12.0), None);
    }

    #[test]
    fn edges_map_to_their_direction() {
        assert_eq!(
            resize_dir_at(pos2(500.0, 1.0), win(), 6.0, 12.0),
            Some(D::North)
        );
        assert_eq!(
            resize_dir_at(pos2(500.0, 699.0), win(), 6.0, 12.0),
            Some(D::South)
        );
        assert_eq!(
            resize_dir_at(pos2(1.0, 350.0), win(), 6.0, 12.0),
            Some(D::West)
        );
        assert_eq!(
            resize_dir_at(pos2(999.0, 350.0), win(), 6.0, 12.0),
            Some(D::East)
        );
    }

    #[test]
    fn corners_take_priority_over_edges() {
        assert_eq!(
            resize_dir_at(pos2(2.0, 2.0), win(), 6.0, 12.0),
            Some(D::NorthWest)
        );
        assert_eq!(
            resize_dir_at(pos2(998.0, 2.0), win(), 6.0, 12.0),
            Some(D::NorthEast)
        );
        assert_eq!(
            resize_dir_at(pos2(2.0, 698.0), win(), 6.0, 12.0),
            Some(D::SouthWest)
        );
        assert_eq!(
            resize_dir_at(pos2(998.0, 698.0), win(), 6.0, 12.0),
            Some(D::SouthEast)
        );
        // On the top edge but within the corner band of the left side → NW.
        assert_eq!(
            resize_dir_at(pos2(8.0, 1.0), win(), 6.0, 12.0),
            Some(D::NorthWest)
        );
    }

    #[test]
    fn outside_the_window_is_none() {
        assert_eq!(resize_dir_at(pos2(-5.0, 350.0), win(), 6.0, 12.0), None);
        assert_eq!(resize_dir_at(pos2(500.0, 800.0), win(), 6.0, 12.0), None);
    }
}
