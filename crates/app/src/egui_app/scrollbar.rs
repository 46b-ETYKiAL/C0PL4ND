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
//!   track (search hits, OSC 133 shell prompts, and failed commands — each
//!   drawn in its own colour AND its own half of the track, so the three kinds
//!   are distinguishable rather than merged into one undifferentiated set);
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

/// The thumb rect within `track`. The thumb TOP is positioned over its TRAVEL
/// range (`track_height − thumb_height`) by the fraction-scrolled-back, so it is
/// the EXACT inverse of [`view_offset_for_pointer_y`]: fully scrolled up
/// (`view_offset == scrollback_len`) → track top; live (`view_offset == 0`) →
/// track bottom. Using the travel range rather than the full track height is what
/// keeps the thumb sitting under the pointer during a drag even when the thumb is
/// floored to `MIN_THUMB` on a long scrollback (the imperfect-tracking nit the
/// earlier full-track positioning left).
pub(crate) fn thumb_rect(m: &ScrollMetrics, track: egui::Rect) -> egui::Rect {
    let h = thumb_height(m, track.height());
    let denom = m.scrollback_len.max(1) as f32;
    let scrolled_back = m.scrollback_len - m.view_offset.min(m.scrollback_len);
    let frac = (scrolled_back as f32 / denom).clamp(0.0, 1.0);
    let travel = (track.height() - h).max(0.0);
    let top = track.top() + frac * travel;
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
    // own height), not the full track height — the thumb TOP can only move over
    // `track.height() - h`, so dividing by the full height meant the pointer could
    // never reach frac == 1.0 (dragging to the very bottom left a residual offset
    // instead of following live output). `frac` is the fraction SCROLLED BACK, over
    // `scrollback_len` — the exact inverse of `thumb_rect`. Guard the degenerate
    // case where the thumb fills the track (nothing to scroll → offset 0).
    let travel = (track.height() - h).max(f32::EPSILON);
    let frac = ((top - track.top()) / travel).clamp(0.0, 1.0);
    let scrolled_back = (frac * m.scrollback_len as f32).round() as usize;
    m.scrollback_len - scrolled_back.min(m.scrollback_len)
}

/// The track y (screen points) of an absolute content line — where a mark sits.
pub(crate) fn mark_y(m: &ScrollMetrics, track: egui::Rect, abs_line: usize) -> f32 {
    let total = m.total().max(1) as f32;
    let frac = (abs_line as f32 / total).clamp(0.0, 1.0);
    track.top() + frac * track.height()
}

/// What a [`ScrollMark`] records. The kind drives BOTH the tick's colour and
/// its geometry ([`mark_rect`]) so the three are told apart at a glance — and
/// still told apart by a colour-blind user, or in a screenshot rendered in
/// greyscale, because each kind occupies a different part of the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollMarkKind {
    /// An OSC 133 `;A`/`;B` shell-prompt mark — where a command was typed.
    /// The same marks the Ctrl+Shift+Up/Down jump-to-prompt chord walks.
    Prompt,
    /// An OSC 133 `;D` command-end mark whose reported exit code was NON-ZERO —
    /// "a command failed here". Only shells with prompt integration emit these.
    Error,
    /// A find-overlay (Ctrl+Shift+F) match in the focused pane.
    SearchHit,
}

/// A tick painted on the track: an absolute content line, what it records, and
/// (for a [`ScrollMarkKind::SearchHit`]) whether it is the currently-selected
/// match — drawn in the cursor colour so it stands out from the other hits.
/// `selected` is meaningless for the other kinds and is ignored for them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollMark {
    pub abs_line: usize,
    pub kind: ScrollMarkKind,
    pub selected: bool,
}

/// The colours the three mark kinds are painted in. Grouped so [`paint`] keeps
/// one palette argument instead of one colour per kind.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MarkColors {
    /// A non-selected search hit.
    pub search: egui::Color32,
    /// The CURRENTLY-SELECTED search hit (the cursor colour).
    pub selected: egui::Color32,
    /// A shell-prompt mark.
    pub prompt: egui::Color32,
    /// A failed-command mark.
    pub error: egui::Color32,
}

/// Half-height of a search-hit / prompt tick, in points (so the tick is 2pt).
const MARK_HALF_THICKNESS: f32 = 1.0;
/// Half-height of a FAILED-COMMAND tick — deliberately heavier than the others
/// so a failure reads as the loudest thing on the track.
const ERROR_MARK_HALF_THICKNESS: f32 = 1.5;

/// The rect a mark of `kind` occupies on `track` at track-y `y`.
///
/// The three kinds claim DIFFERENT horizontal extents so they never merge into
/// one undifferentiated set:
///
/// - [`ScrollMarkKind::Prompt`] — the LEFT half of the track,
/// - [`ScrollMarkKind::Error`] — the RIGHT half (and a heavier tick),
/// - [`ScrollMarkKind::SearchHit`] — the FULL width, so the thing the user is
///   actively hunting for is the widest mark and is never hidden behind a
///   prompt tick that happens to land on the same line.
///
/// Pure geometry (no painter, no `self`) so the distinctness is unit-testable.
pub(crate) fn mark_rect(kind: ScrollMarkKind, track: egui::Rect, y: f32) -> egui::Rect {
    let mid = track.left() + track.width() * 0.5;
    let (x0, x1, half_h) = match kind {
        ScrollMarkKind::Prompt => (track.left(), mid, MARK_HALF_THICKNESS),
        ScrollMarkKind::Error => (mid, track.right(), ERROR_MARK_HALF_THICKNESS),
        ScrollMarkKind::SearchHit => (track.left(), track.right(), MARK_HALF_THICKNESS),
    };
    egui::Rect::from_min_max(egui::pos2(x0, y - half_h), egui::pos2(x1, y + half_h))
}

/// The colour a mark of `kind` is painted in. A SELECTED search hit takes the
/// cursor colour (hue-distinct from the accent thumb); everything else takes
/// its kind's colour.
pub(crate) fn mark_color(
    kind: ScrollMarkKind,
    selected: bool,
    colors: &MarkColors,
) -> egui::Color32 {
    match kind {
        ScrollMarkKind::Prompt => colors.prompt,
        ScrollMarkKind::Error => colors.error,
        ScrollMarkKind::SearchHit if selected => colors.selected,
        ScrollMarkKind::SearchHit => colors.search,
    }
}

/// Paint order: prompts, then failures, then search hits LAST — so a search hit
/// that lands on the same line as a prompt is still the visible one (the user is
/// actively hunting for it).
fn paint_rank(kind: ScrollMarkKind) -> u8 {
    match kind {
        ScrollMarkKind::Prompt => 0,
        ScrollMarkKind::Error => 1,
        ScrollMarkKind::SearchHit => 2,
    }
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
    mark_colors: &MarkColors,
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
    // Marks on top, in kind order (prompts, failures, then search hits) so an
    // overlapping search hit wins the pixel. Each kind takes its own colour AND
    // its own slice of the track width (see `mark_rect`).
    let mut ordered: Vec<&ScrollMark> = marks.iter().collect();
    ordered.sort_by_key(|mk| paint_rank(mk.kind));
    for mk in ordered {
        let y = mark_y(m, track, mk.abs_line);
        let col = mark_color(mk.kind, mk.selected, mark_colors);
        painter.rect_filled(mark_rect(mk.kind, track, y), 0.0, col);
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

    /// `thumb_rect` and `view_offset_for_pointer_y` must be inverses: dropping the
    /// pointer on the CENTRE of the thumb drawn for an offset must recover that
    /// same offset. This is what makes the thumb sit under the pointer during a
    /// drag; the earlier full-track positioning failed it in the floored-thumb
    /// regime (a long scrollback where the thumb is clamped to MIN_THUMB).
    #[test]
    fn thumb_and_pointer_map_are_inverses_including_floored_thumb() {
        let t = track(); // 200 px tall
        for &(scrollback_len, rows) in &[(300usize, 40usize), (100_000, 40), (40, 40)] {
            for &offset in &[
                0usize,
                1,
                rows,
                scrollback_len / 3,
                scrollback_len - 1,
                scrollback_len,
            ] {
                if offset > scrollback_len {
                    continue;
                }
                let m = ScrollMetrics {
                    scrollback_len,
                    view_offset: offset,
                    rows,
                };
                let thumb = thumb_rect(&m, t);
                let recovered = view_offset_for_pointer_y(&m, t, thumb.center().y);
                // Rounding through pixel space costs at most one scrollback line
                // per track pixel; assert it round-trips within that tolerance.
                let tol = (scrollback_len as f32 / (t.height() - thumb.height()).max(1.0)).ceil()
                    as usize
                    + 1;
                let diff = recovered.abs_diff(offset);
                assert!(
                    diff <= tol,
                    "offset {offset} (sb={scrollback_len}, rows={rows}) recovered as \
                     {recovered} (diff {diff} > tol {tol})"
                );
            }
        }
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

    fn mark_colors() -> MarkColors {
        MarkColors {
            search: egui::Color32::from_rgb(1, 0, 0),
            selected: egui::Color32::from_rgb(2, 0, 0),
            prompt: egui::Color32::from_rgb(3, 0, 0),
            error: egui::Color32::from_rgb(4, 0, 0),
        }
    }

    /// The three mark kinds must be told apart WITHOUT colour: a prompt tick and
    /// a failure tick occupy DISJOINT halves of the track, and a search hit spans
    /// the whole width. Merging them into one geometry (the pre-existing
    /// full-width-tick-for-everything) fails this.
    #[test]
    fn mark_rect_gives_each_kind_a_distinct_slice_of_the_track() {
        let t = track();
        let y = 50.0;
        let prompt = mark_rect(ScrollMarkKind::Prompt, t, y);
        let error = mark_rect(ScrollMarkKind::Error, t, y);
        let hit = mark_rect(ScrollMarkKind::SearchHit, t, y);

        assert!(
            prompt.right() <= error.left() + f32::EPSILON,
            "a prompt tick ({prompt:?}) must not overlap a failure tick ({error:?}) — \
             they are what makes the kinds distinguishable in greyscale"
        );
        assert!(
            prompt.left() >= t.left() && error.right() <= t.right(),
            "both half-width ticks stay inside the track"
        );
        assert!(
            hit.left() <= prompt.left() && hit.right() >= error.right(),
            "a search hit ({hit:?}) spans the full track so it is never hidden \
             behind a same-line prompt tick"
        );
        assert!(
            error.height() > prompt.height(),
            "a failed command ({}) is drawn heavier than a prompt ({})",
            error.height(),
            prompt.height()
        );
        for (kind, r) in [
            (ScrollMarkKind::Prompt, prompt),
            (ScrollMarkKind::Error, error),
            (ScrollMarkKind::SearchHit, hit),
        ] {
            assert!(
                (r.center().y - y).abs() < f32::EPSILON,
                "{kind:?} must be centred on its content line's track y"
            );
            assert!(
                r.width() > 0.0 && r.height() > 0.0,
                "{kind:?} paints nothing"
            );
        }
    }

    /// Every kind takes its OWN colour, and only a SELECTED search hit takes the
    /// cursor colour. A palette that collapsed two kinds onto one colour would
    /// re-merge the sets this feature exists to separate.
    #[test]
    fn mark_color_is_distinct_per_kind_and_selection() {
        let c = mark_colors();
        let prompt = mark_color(ScrollMarkKind::Prompt, false, &c);
        let error = mark_color(ScrollMarkKind::Error, false, &c);
        let hit = mark_color(ScrollMarkKind::SearchHit, false, &c);
        let sel = mark_color(ScrollMarkKind::SearchHit, true, &c);
        assert_eq!(prompt, c.prompt);
        assert_eq!(error, c.error);
        assert_eq!(hit, c.search);
        assert_eq!(sel, c.selected);
        let all = [prompt, error, hit, sel];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two mark states collapsed onto the same colour");
            }
        }
        // `selected` is meaningless for the non-search kinds and must be ignored.
        assert_eq!(mark_color(ScrollMarkKind::Prompt, true, &c), c.prompt);
        assert_eq!(mark_color(ScrollMarkKind::Error, true, &c), c.error);
    }

    /// A search hit is painted AFTER a prompt on the same line, so it wins the
    /// overlapping pixels. Asserted on the rank function the painter sorts by.
    #[test]
    fn search_hits_paint_over_prompt_and_error_marks() {
        assert!(paint_rank(ScrollMarkKind::SearchHit) > paint_rank(ScrollMarkKind::Error));
        assert!(paint_rank(ScrollMarkKind::Error) > paint_rank(ScrollMarkKind::Prompt));
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
