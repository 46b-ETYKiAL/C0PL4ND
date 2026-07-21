//! Terminal-grid selection, hit-testing, and overlay painting.
//!
//! Selection model + line/block extraction geometry, cell hit-testing, word
//! bounds, and the link-underline / search-highlight overlay painters —
//! extracted from the `egui_app` god-module. Pure geometry fns are GPU-free and
//! unit-testable. Re-exported into `egui_app` via `pub(crate) use grid_interaction::*`.

use std::collections::HashMap;

use super::grid::PaneId;
use super::{effective_row_pitch, grid_text_origin, pane_term, theme, Direction};

/// An in-progress or completed mouse text selection over a pane.
/// `anchor` is where the drag began, `head` the current end — both
/// `(ABSOLUTE-line, column)`, where the absolute line is `window_start +
/// display_row` at the moment the cell was hit. Anchoring to absolute scrollback
/// lines (not display rows) keeps the selection over the SAME content as the
/// view scrolls / jumps to a prompt / receives new output — the painter and copy
/// map absolute → current display row via [`selection_visible_rows`]. A selection
/// where `anchor == head` is an empty (click, not drag) selection.
/// A test-only view of the active selection: `(anchor, head, is_block)` in
/// `(absolute-line, column)` coordinates. Returned by
/// [`super::C0pl4ndApp::test_selection`] for the interaction tests.
pub(crate) type TestSelection = ((usize, usize), (usize, usize), bool);

/// Whether a mouse selection extracts text LINE-WISE (the default — the first
/// row runs from the anchor column to end-of-row, inner rows are full, the last
/// row runs to the head column) or as a rectangular BLOCK (every row clipped to
/// the same `[min_col, max_col]` column range). Block mode is engaged by holding
/// Alt while dragging.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SelectionMode {
    #[default]
    Linewise,
    Block,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Selection {
    pub(crate) pane: PaneId,
    pub(crate) anchor: (usize, usize),
    pub(crate) head: (usize, usize),
    pub(crate) mode: SelectionMode,
}

/// Map an absolute-line selection to display-row endpoint tuples for the CURRENT
/// visible window, or `None` when the whole selection has scrolled out of view.
///
/// `anchor`/`head` are `(absolute-line, col)` in either order; `window_start` is
/// the absolute line at the top of the visible window; `rows` is the visible row
/// count. Each endpoint's row becomes `absolute - window_start`; an endpoint that
/// has scrolled ABOVE the top clamps to row 0 / col 0 (begin at the visible top),
/// and one BELOW the bottom clamps to the last row / end-of-line (`usize::MAX`,
/// which both consumers clamp to the real width). This keeps the highlight and
/// the copied text tracking the selected content across any view change, copying
/// the visible portion of a partly-scrolled-out selection. Pure + unit-tested.
pub(crate) fn selection_visible_rows(
    anchor: (usize, usize),
    head: (usize, usize),
    window_start: usize,
    rows: usize,
) -> Option<((usize, usize), (usize, usize))> {
    if rows == 0 {
        return None;
    }
    let (start, end) = if anchor <= head {
        (anchor, head)
    } else {
        (head, anchor)
    };
    let win_end = window_start + rows; // exclusive
    if end.0 < window_start || start.0 >= win_end {
        return None; // entirely above or below the visible window
    }
    let start_disp = if start.0 >= window_start {
        (start.0 - window_start, start.1)
    } else {
        (0, 0) // scrolled above the top → from the visible top-left
    };
    let end_disp = if end.0 < win_end {
        (end.0 - window_start, end.1)
    } else {
        (rows - 1, usize::MAX) // scrolled below the bottom → last row, to line end
    };
    Some((start_disp, end_disp))
}

/// One find-overlay match converted to CELL coordinates: the visual row and the
/// `[col_start, col_end)` character columns the match spans. Built in
/// [`super::C0pl4ndApp::cell_spans_for_search`] from the byte spans the core matcher
/// returns, so the painter never re-derives columns from bytes.
#[derive(Clone, Copy)]
pub(crate) struct CellSpan {
    /// Visual row (line index into the pane's grid text).
    pub(crate) line: usize,
    /// First character column of the match (inclusive).
    pub(crate) col_start: usize,
    /// One-past-the-last character column of the match (exclusive).
    pub(crate) col_end: usize,
}

/// The find-overlay highlight inputs for ONE pane render: the cell spans to tint
/// plus the index of the active (selected) span. Borrowed from a per-frame
/// `Vec<CellSpan>` for the focused pane only while the overlay is open.
#[derive(Clone, Copy)]
pub(crate) struct SearchHighlight<'a> {
    /// Every match span in CELL coordinates over the pane's grid text.
    pub(crate) spans: &'a [CellSpan],
    /// Index into `spans` of the currently-selected match (the one Enter / F3
    /// cycles to); drawn with an outline so it stands out from the dim tints.
    pub(crate) selected: usize,
}

/// The byte offset `byte` within `line` converted to a terminal CELL column.
/// Each char contributes its cell width (2 for an East-Asian wide / fullwidth
/// glyph, 1 otherwise) — NOT a flat char count — so a span before/after a wide
/// glyph lands on the same cell column the per-cell grid renderer positions that
/// glyph at. The core matcher returns BYTE spans (`str::find` / `Regex::find`
/// offsets); a multi-byte OR wide glyph before the match would otherwise
/// mis-count the column. Clamps to the line length so a stale span (the grid
/// scrolled since the match was computed) can never index past the row.
pub(crate) fn byte_to_col(line: &str, byte: usize) -> usize {
    let b = byte.min(line.len());
    line.char_indices()
        .take_while(|(i, _)| *i < b)
        .map(|(_, c)| pane_term::cell_render_width(c))
        .sum()
}

/// Map a pointer position (POINTS, in screen space) to the `(row, col)` grid
/// cell under it, given the grid text `origin` (top-left of the first cell) and
/// the cell size `(cw, ch)` in points. Returns `None` when the position is above
/// or left of the grid (a negative cell index). Pure so the Ctrl-click hit test
/// is unit-testable without an egui frame. Out-of-range high indices are NOT
/// clamped here — the caller's span list simply won't contain a matching span.
pub(crate) fn cell_at_pos(
    pos: egui::Pos2,
    origin: egui::Pos2,
    cw: f32,
    ch: f32,
) -> Option<(usize, usize)> {
    if pos.x < origin.x || pos.y < origin.y || cw <= 0.0 || ch <= 0.0 {
        return None;
    }
    let col = ((pos.x - origin.x) / cw).floor() as usize;
    let row = ((pos.y - origin.y) / ch).floor() as usize;
    Some((row, col))
}

/// Whether two 1-D ranges overlap (open-interval test), used by directional
/// pane focus to require orthogonal-axis overlap between two pane rects.
pub(crate) fn ranges_overlap(a: egui::Rangef, b: egui::Rangef) -> bool {
    a.min < b.max && b.min < a.max
}

/// The pane geometrically adjacent to `focus` in `dir` among `rects`. A
/// candidate must lie in the requested direction (its centre past the focused
/// centre on the primary axis) AND overlap the focused pane on the orthogonal
/// axis; among those the nearest on the primary axis wins, tie-broken by
/// orthogonal-centre proximity. `None` when there is no such neighbour (or
/// `focus` has no rect). Pure (no `self`) so it is unit-testable against
/// synthetic layouts.
pub(crate) fn neighbor_in_rects(
    rects: &HashMap<PaneId, egui::Rect>,
    focus: PaneId,
    dir: Direction,
) -> Option<PaneId> {
    let f = rects.get(&focus)?;
    let fc = f.center();
    let mut best: Option<(PaneId, f32, f32)> = None;
    for (&pid, r) in rects {
        if pid == focus {
            continue;
        }
        let c = r.center();
        let (primary, ortho, in_dir, overlap) = match dir {
            Direction::Left => (
                fc.x - c.x,
                (c.y - fc.y).abs(),
                c.x < fc.x,
                ranges_overlap(f.y_range(), r.y_range()),
            ),
            Direction::Right => (
                c.x - fc.x,
                (c.y - fc.y).abs(),
                c.x > fc.x,
                ranges_overlap(f.y_range(), r.y_range()),
            ),
            Direction::Up => (
                fc.y - c.y,
                (c.x - fc.x).abs(),
                c.y < fc.y,
                ranges_overlap(f.x_range(), r.x_range()),
            ),
            Direction::Down => (
                c.y - fc.y,
                (c.x - fc.x).abs(),
                c.y > fc.y,
                ranges_overlap(f.x_range(), r.x_range()),
            ),
        };
        if !in_dir || !overlap {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, bp, bo)) => primary < bp || (primary == bp && ortho < bo),
        };
        if better {
            best = Some((pid, primary, ortho));
        }
    }
    best.map(|(id, _, _)| id)
}

/// True for a character that double-click word-selection treats as part of a
/// "word". Beyond alphanumerics this keeps the path / URL / identifier
/// punctuation (`_-./~:@`) so a double-click grabs a whole filename, flag, or
/// URL rather than stopping at the first dot or slash — matching the default
/// word class of mainstream terminals.
pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || "_-./~:@".contains(c)
}

/// The inclusive `(start_col, end_col)` span of the word under `col` in `row`
/// (one `char` per grid column). A maximal run of [`is_word_char`] cells around
/// `col`; clicking a non-word cell (whitespace, punctuation outside the word
/// class) selects just that single cell `(col, col)`. Pure + column-indexed so
/// it is wide-glyph-safe and unit-testable without a live grid.
pub(crate) fn word_bounds(row: &[char], col: usize) -> (usize, usize) {
    if row.get(col).copied().map(is_word_char) != Some(true) {
        return (col, col);
    }
    let mut start = col;
    while start > 0 && row.get(start - 1).copied().map(is_word_char) == Some(true) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < row.len() && row.get(end + 1).copied().map(is_word_char) == Some(true) {
        end += 1;
    }
    (start, end)
}

/// Strip characters that are dangerous to render in app chrome from `s`,
/// returning a cleaned copy. A program (or a remote SSH host) controls the OSC
/// 0/2 terminal title and any OSC-8 / detected hyperlink URI; rendering those
/// strings verbatim in a tab label or link preview is a spoofing surface
/// (bidi-override "evil.com<U+202E>gpj.exe", zero-width obfuscation, embedded
/// control codes). This is a WHITELIST: we keep ordinary printable text — including
/// non-ASCII printable glyphs (accented Latin, CJK, emoji) — and drop only the
/// dangerous set:
///
/// - C0 controls `U+0000..=U+001F` and `U+007F`, and C1 controls
///   `U+0080..=U+009F`. For a one-line chrome label there is no legitimate
///   `\t`/`\n`/`\r`, so all control chars (including those) are removed.
/// - Bidirectional formatting: the embeddings/overrides `U+202A..=U+202E`
///   (LRE/RLE/PDF/LRO/RLO), the isolates `U+2066..=U+2069`
///   (LRI/RLI/FSI/PDI), and the marks `U+200E`/`U+200F` (LRM/RLM).
/// - Zero-width: `U+200B..=U+200D` (ZWSP/ZWNJ/ZWJ) and `U+FEFF` (ZWNBSP / BOM).
///
/// `pub(crate)` so any future chrome path that shows attacker-controlled text
/// (e.g. an OSC-8 hyperlink-URI preview) can reuse the exact same filter.
pub(crate) fn scrub_display_text(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            // Drop all control characters (C0 + DEL + C1). `char::is_control`
            // covers U+0000..=U+001F, U+007F, and U+0080..=U+009F.
            if c.is_control() {
                return false;
            }
            !matches!(
                c,
                // Bidi embeddings / overrides + isolates + marks.
                '\u{202A}'..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{200E}'
                    | '\u{200F}'
                    // Zero-width joiners/non-joiners/space + BOM/ZWNBSP.
                    | '\u{200B}'..='\u{200D}'
                    | '\u{FEFF}'
            )
        })
        .collect()
}

/// The URL whose cell span covers grid cell `(row, col)`, or `None`. Scans the
/// precomputed `(CellSpan, url)` links (built by
/// [`super::C0pl4ndApp::cell_spans_for_hyperlinks`]); the column test is half-open
/// `[col_start, col_end)`, matching how the spans were built.
pub(crate) fn link_url_at_cell(
    links: &[(CellSpan, String)],
    row: usize,
    col: usize,
) -> Option<&str> {
    links
        .iter()
        .find(|(s, _)| s.line == row && col >= s.col_start && col < s.col_end)
        .map(|(_, url)| url.as_str())
}

/// The URL SPAN whose cells cover `(row, col)`, if any — the geometry the
/// hover-underline affordance paints (the sibling of [`link_url_at_cell`], which
/// returns the URL string).
pub(crate) fn link_span_at_cell(
    links: &[(CellSpan, String)],
    row: usize,
    col: usize,
) -> Option<&CellSpan> {
    links
        .iter()
        .find(|(s, _)| s.line == row && col >= s.col_start && col < s.col_end)
        .map(|(s, _)| s)
}

/// Underline a SINGLE URL span (the hovered link's discoverability affordance),
/// slightly heavier than the Ctrl-held all-links underline so the hovered link
/// reads as the actionable one. Same geometry as [`paint_link_underlines`].
pub(crate) fn paint_one_link_underline(
    painter: &egui::Painter,
    origin: egui::Pos2,
    cw: f32,
    ch: f32,
    colors: &theme::ChromeColors,
    s: &CellSpan,
) {
    let col_end = s.col_end.max(s.col_start + 1);
    let x0 = origin.x + s.col_start as f32 * cw;
    let x1 = origin.x + col_end as f32 * cw;
    let y = origin.y + s.line as f32 * ch + ch - 1.0;
    painter.line_segment(
        [egui::pos2(x0, y), egui::pos2(x1, y)],
        egui::Stroke::new(1.5f32, colors.accent),
    );
}

/// Cell `(width, height)` in POINTS for the terminal grid: the width is the
/// monospace `M` advance; the height is the EFFECTIVE row pitch
/// ([`effective_row_pitch`] of the natural galley height and the configured
/// `line_height_px`) — the SAME pitch `paint_grid_native` draws rows at, so
/// hyperlink underlines, the Ctrl-click hit test, and the search highlight all
/// land exactly on the rendered glyph grid regardless of the Line-height
/// setting.
pub(crate) fn monospace_cell_points(
    painter: &egui::Painter,
    font_size: f32,
    line_height_px: f32,
) -> (f32, f32) {
    let size = painter
        .layout_job(egui::text::LayoutJob::single_section(
            "M".to_string(),
            egui::text::TextFormat {
                font_id: egui::FontId::monospace(font_size),
                ..Default::default()
            },
        ))
        .size();
    (size.x.max(1.0), effective_row_pitch(size.y, line_height_px))
}

/// Underline every Ctrl-clickable URL span over the rendered grid (drawn only
/// while the modifier is held — see the caller). A thin accent line under each
/// span's cells signals "this is a link"; GPU-free (one `line_segment` per span).
/// The painter's clip rect keeps an over-wide span inside the pane.
pub(crate) fn paint_link_underlines(
    painter: &egui::Painter,
    origin: egui::Pos2,
    cw: f32,
    ch: f32,
    colors: &theme::ChromeColors,
    links: &[(CellSpan, String)],
) {
    for (s, _) in links {
        let col_end = s.col_end.max(s.col_start + 1);
        let x0 = origin.x + s.col_start as f32 * cw;
        let x1 = origin.x + col_end as f32 * cw;
        // Baseline-ish: 1px above the cell bottom so the rule reads as an
        // underline rather than a row separator.
        let y = origin.y + s.line as f32 * ch + ch - 1.0;
        painter.line_segment(
            [egui::pos2(x0, y), egui::pos2(x1, y)],
            egui::Stroke::new(1.0f32, colors.accent),
        );
    }
}

/// Paint the find-overlay highlight over a pane's rendered grid: a dim tint
/// quad behind every match span and an accent outline around the active one.
/// Cell geometry is derived from the SAME monospace probe-galley the cursor
/// uses, so the quads land on the cell grid. GPU-free (egui rects only). A
/// match whose `line` exceeds the visible row count is skipped (the grid may
/// have scrolled since the match set was computed mid-frame).
// Geometry primitive: every argument is an independent painting parameter
// (surface, cell metrics, colours), like `glyph_button` above. Grouping them into
// a struct would only move the same fields behind one name.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_search_highlight(
    painter: &egui::Painter,
    rect: egui::Rect,
    font_size: f32,
    line_height_px: f32,
    padding: f32,
    colors: &theme::ChromeColors,
    current_match: egui::Color32,
    hl: SearchHighlight<'_>,
) {
    if hl.spans.is_empty() {
        return;
    }
    // Cell size in POINTS — identical to the cursor's/grid's metric (the `M`
    // advance for width, the effective row pitch for height) so the highlight
    // aligns with the glyphs at any Line-height setting.
    let (cw, ch) = monospace_cell_points(painter, font_size, line_height_px);
    let origin = grid_text_origin(rect, padding);

    for (idx, s) in hl.spans.iter().enumerate() {
        // Spans are already in cell coordinates (built by `cell_spans_for_search`
        // via `byte_to_col`). The painter's clip rect keeps any over-wide quad
        // inside the pane, so no extra bounds math is needed.
        let col_end = s.col_end.max(s.col_start + 1);
        let x0 = origin.x + s.col_start as f32 * cw;
        let w = (col_end - s.col_start) as f32 * cw;
        let y0 = origin.y + s.line as f32 * ch;
        let span = egui::Rect::from_min_size(egui::pos2(x0, y0), egui::vec2(w, ch));
        if idx == hl.selected {
            // The CURRENT match gets its own distinct FILL, not merely an
            // outline over the same tint every other match uses. With one shared
            // colour the active match was near-indistinguishable at a glance —
            // an outline reads as a border, not as "this is the one you are on",
            // which is the whole point of a find overlay. A solid-ish fill plus
            // the outline makes it unmistakable.
            painter.rect_filled(span, 1.0, current_match.gamma_multiply(0.75));
            painter.rect_stroke(
                span,
                1.0,
                egui::Stroke::new(1.5f32, current_match),
                egui::StrokeKind::Inside,
            );
        } else {
            // Dim accent tint behind every OTHER match.
            painter.rect_filled(span, 1.0, colors.accent.gamma_multiply(0.30));
        }
    }
}
