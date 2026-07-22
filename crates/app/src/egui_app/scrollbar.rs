//! Right-side terminal scrollbar: pure geometry + an egui-rects-only painter.
//!
//! The terminal grid is custom-painted and uses NO `egui::ScrollArea`, so this
//! module owns the scrollbar rect, its hit-testing geometry, and its paint. The
//! grid's scrollback model is: `scrollback_len` lines have scrolled off the top,
//! the live screen shows `rows` lines, and `view_offset` (0 = following the live
//! bottom) is how far the view is scrolled UP. The *total* addressable content is
//! therefore `scrollback_len + rows`, and the visible window starts at absolute
//! line `window_start = scrollback_len - view_offset`.
//!
//! The scrollbar maps that model onto a vertical track:
//! - the THUMB spans the visible window `[window_start, window_start + rows)` as a
//!   fraction of the total content, so its height is ∝ visible/total and its
//!   position tracks the scroll offset;
//! - a MARK at absolute content line `L` sits at fraction `L / total` down the
//!   track (used for search-hit positions);
//! - dragging the thumb scrubs the view; clicking the trough above/below pages.
//!
//! The geometry functions are pure (GPU-free, no `self`) so they are unit-tested
//! directly; [`paint`] is exercised by the `qa_wide_glyph_snapshot` visual QA.

use super::theme::ChromeColors;

/// Track width in points — a slim overlay bar, like a modern terminal.
pub(crate) const BAR_WIDTH: f32 = 8.0;
/// Inset from the pane edges so the bar clears the focus-ring/bezel stroke.
pub(crate) const BAR_MARGIN: f32 = 3.0;
/// The thumb never shrinks below this (points) so it stays grabbable even when a
/// tiny viewport sits over a huge scrollback.
const MIN_THUMB: f32 = 24.0;

/// The scroll state of one pane this frame — everything the geometry needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollMetrics {
    /// Lines retained in scrollback history (scrolled off the top).
    pub scrollback_len: usize,
    /// Scroll-up offset from the live bottom (0 = following live output).
    pub view_offset: usize,
    /// Visible screen rows.
    pub rows: usize,
}

impl ScrollMetrics {
    /// Total addressable content rows = history + the live screen.
    pub fn total(&self) -> usize {
        self.scrollback_len + self.rows
    }

    /// Absolute line at the top of the visible window
    /// (`scrollback_len - view_offset`, saturating so an over-scroll clamps to 0).
    pub fn window_start(&self) -> usize {
        self.scrollback_len.saturating_sub(self.view_offset)
    }

    /// Whether there is anything to scroll — the bar AUTO-HIDES when everything
    /// fits (no scrollback), like a modern terminal.
    pub fn scrollable(&self) -> bool {
        self.scrollback_len > 0 && self.rows > 0
    }
}

/// The scrollbar track rect on the right edge of a pane `rect`.
pub(crate) fn track_rect(pane: egui::Rect) -> egui::Rect {
    let right = pane.right() - BAR_MARGIN;
    let left = right - BAR_WIDTH;
    egui::Rect::from_min_max(
        egui::pos2(left, pane.top() + BAR_MARGIN),
        egui::pos2(right, pane.bottom() - BAR_MARGIN),
    )
}

/// Thumb height in points: `track_h * visible/total`, floored at [`MIN_THUMB`]
/// (never taller than the track).
pub(crate) fn thumb_height(m: &ScrollMetrics, track_h: f32) -> f32 {
    if track_h <= 0.0 {
        return 0.0;
    }
    let total = m.total().max(1) as f32;
    let raw = track_h * (m.rows as f32 / total);
    raw.clamp(MIN_THUMB.min(track_h), track_h)
}

/// The thumb rect within `track`: top at `window_start/total` of the track,
/// clamped so the thumb always stays inside the track.
pub(crate) fn thumb_rect(m: &ScrollMetrics, track: egui::Rect) -> egui::Rect {
    let h = thumb_height(m, track.height());
    let total = m.total().max(1) as f32;
    let top_frac = m.window_start() as f32 / total;
    let top = (track.top() + top_frac * track.height())
        .clamp(track.top(), (track.bottom() - h).max(track.top()));
    egui::Rect::from_min_size(egui::pos2(track.left(), top), egui::vec2(track.width(), h))
}

/// The target `view_offset` that centres the thumb under `pointer_y` — the map
/// used for both a thumb DRAG (scrub) and a trough click that lands on the thumb.
/// Inverts [`thumb_rect`]'s positioning: pointer → thumb-top → `window_start` →
/// `view_offset`.
pub(crate) fn view_offset_for_pointer_y(
    m: &ScrollMetrics,
    track: egui::Rect,
    pointer_y: f32,
) -> usize {
    if m.scrollback_len == 0 || track.height() <= 0.0 {
        return 0;
    }
    let h = thumb_height(m, track.height());
    let top = (pointer_y - h * 0.5).clamp(track.top(), (track.bottom() - h).max(track.top()));
    // Normalize by the thumb's actual TRAVEL range (track height minus the thumb's
    // own height), not the full track height. The thumb TOP can only move over
    // `track.height() - h`, so dividing by the full height meant the pointer could
    // never reach frac == 1.0 — dragging to the very bottom left a residual offset
    // instead of following live output (offset 0). Guard the degenerate case where
    // the thumb fills the track (nothing to scroll → offset 0).
    let travel = (track.height() - h).max(f32::EPSILON);
    let frac = ((top - track.top()) / travel).clamp(0.0, 1.0);
    let total = m.total().max(1) as f32;
    let window_start = ((frac * total).round() as usize).min(m.scrollback_len);
    m.scrollback_len - window_start
}

/// The track y (screen points) of an absolute content line — where a mark sits.
pub(crate) fn mark_y(m: &ScrollMetrics, track: egui::Rect, abs_line: usize) -> f32 {
    let total = m.total().max(1) as f32;
    let frac = (abs_line as f32 / total).clamp(0.0, 1.0);
    track.top() + frac * track.height()
}

/// A tick painted on the track: an absolute content line + whether it is the
/// currently-selected one (drawn in the cursor colour so it stands out).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollMark {
    pub abs_line: usize,
    pub selected: bool,
}

/// Paint the track, its marks, and the thumb — egui rects only (GPU-free). The
/// track + thumb take the theme colours; `active` (hovered or dragged) brightens
/// the thumb. Marks are painted LAST (on top of the thumb) so an in-viewport
/// search hit is still visible where it overlaps the thumb.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint(
    painter: &egui::Painter,
    track: egui::Rect,
    m: &ScrollMetrics,
    colors: &ChromeColors,
    cursor_color: egui::Color32,
    active: bool,
    marks: &[ScrollMark],
) {
    let radius = track.width() * 0.5;
    // Faint track so the bar reads as a channel without dominating the pane.
    painter.rect_filled(track, radius, colors.bezel.gamma_multiply(0.28));
    // Thumb (brighter when the pointer is on it or dragging).
    let thumb = thumb_rect(m, track);
    let thumb_col = if active {
        colors.accent
    } else {
        colors.accent.gamma_multiply(0.6)
    };
    painter.rect_filled(thumb, radius, thumb_col);
    // Marks on top: a thin full-width tick per hit; the selected one in the
    // cursor colour (hue-distinct from the accent thumb), the rest in the fg.
    for mk in marks {
        let y = mark_y(m, track, mk.abs_line);
        let col = if mk.selected { cursor_color } else { colors.fg };
        let tick = egui::Rect::from_min_max(
            egui::pos2(track.left(), y - 1.0),
            egui::pos2(track.right(), y + 1.0),
        );
        painter.rect_filled(tick, 0.0, col);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> egui::Rect {
        // A 100pt-tall track from y=0..100, x=0..8.
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(8.0, 100.0))
    }

    #[test]
    fn total_and_window_start_follow_the_scrollback_model() {
        let m = ScrollMetrics {
            scrollback_len: 300,
            view_offset: 0,
            rows: 40,
        };
        assert_eq!(m.total(), 340, "total = history + screen");
        assert_eq!(
            m.window_start(),
            300,
            "at the live bottom the window starts after all of history"
        );
        let up = ScrollMetrics {
            view_offset: 300,
            ..m
        };
        assert_eq!(
            up.window_start(),
            0,
            "scrolled fully up, the window starts at line 0"
        );
    }

    #[test]
    fn autohide_when_nothing_to_scroll() {
        let none = ScrollMetrics {
            scrollback_len: 0,
            view_offset: 0,
            rows: 40,
        };
        assert!(
            !none.scrollable(),
            "no scrollback → nothing to scroll → the bar auto-hides"
        );
        let some = ScrollMetrics {
            scrollback_len: 1,
            ..none
        };
        assert!(some.scrollable(), "any scrollback makes the bar appear");
    }

    #[test]
    fn thumb_height_is_proportional_and_floored() {
        // visible/total = 40/340 ≈ 0.1176 → 11.76pt, but MIN_THUMB (24) floors it.
        let m = ScrollMetrics {
            scrollback_len: 300,
            view_offset: 0,
            rows: 40,
        };
        assert!((thumb_height(&m, 100.0) - MIN_THUMB).abs() < f32::EPSILON);

        // A big viewport over a small scrollback → thumb is a real proportion.
        let big = ScrollMetrics {
            scrollback_len: 20,
            view_offset: 0,
            rows: 80,
        };
        // 80/100 = 0.8 → 80pt.
        assert!((thumb_height(&big, 100.0) - 80.0).abs() < 0.001);
        assert!(
            thumb_height(&big, 100.0) <= 100.0,
            "the thumb never exceeds the track"
        );
    }

    #[test]
    fn thumb_sits_at_the_bottom_when_following_live_output() {
        let m = ScrollMetrics {
            scrollback_len: 300,
            view_offset: 0,
            rows: 40,
        };
        let t = track();
        let thumb = thumb_rect(&m, t);
        assert!(
            (thumb.bottom() - t.bottom()).abs() < 0.5,
            "at the live bottom the thumb bottom aligns with the track bottom: {thumb:?}"
        );
    }

    #[test]
    fn thumb_sits_at_the_top_when_scrolled_fully_up() {
        let m = ScrollMetrics {
            scrollback_len: 300,
            view_offset: 300,
            rows: 40,
        };
        let t = track();
        let thumb = thumb_rect(&m, t);
        assert!(
            (thumb.top() - t.top()).abs() < f32::EPSILON,
            "scrolled fully up the thumb top aligns with the track top: {thumb:?}"
        );
    }

    #[test]
    fn thumb_stays_inside_the_track_at_every_offset() {
        let t = track();
        for off in [0usize, 1, 50, 150, 299, 300] {
            let m = ScrollMetrics {
                scrollback_len: 300,
                view_offset: off,
                rows: 40,
            };
            let thumb = thumb_rect(&m, t);
            assert!(
                thumb.top() >= t.top() - 0.001 && thumb.bottom() <= t.bottom() + 0.001,
                "thumb escaped the track at offset {off}: {thumb:?} vs {t:?}"
            );
        }
    }

    #[test]
    fn pointer_to_offset_round_trips_top_and_bottom() {
        let m = ScrollMetrics {
            scrollback_len: 300,
            view_offset: 0,
            rows: 40,
        };
        let t = track();
        // Pointer at the very top → scrolled fully up (max offset = scrollback_len).
        assert_eq!(view_offset_for_pointer_y(&m, t, t.top()), 300);
        // Pointer at the very bottom → follow live output (offset 0).
        assert_eq!(view_offset_for_pointer_y(&m, t, t.bottom()), 0);
        // A pointer in the middle lands somewhere strictly between the extremes.
        let mid = view_offset_for_pointer_y(&m, t, t.center().y);
        assert!(
            mid > 0 && mid < 300,
            "mid-track maps to a mid offset: {mid}"
        );
    }

    #[test]
    fn pointer_to_offset_never_exceeds_the_scrollback() {
        let m = ScrollMetrics {
            scrollback_len: 12,
            view_offset: 0,
            rows: 40,
        };
        let t = track();
        for y in [-50.0, 0.0, 25.0, 50.0, 100.0, 500.0] {
            let off = view_offset_for_pointer_y(&m, t, y);
            assert!(off <= 12, "offset {off} exceeded scrollback for y={y}");
        }
    }

    #[test]
    fn mark_y_places_history_top_and_live_bottom() {
        let m = ScrollMetrics {
            scrollback_len: 300,
            view_offset: 0,
            rows: 40,
        };
        let t = track();
        // Line 0 (oldest history) → track top.
        assert!((mark_y(&m, t, 0) - t.top()).abs() < f32::EPSILON);
        // The last content line → near the track bottom.
        let last = mark_y(&m, t, m.total() - 1);
        assert!(last > t.center().y && last <= t.bottom());
    }
}
