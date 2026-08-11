//! The terminal grid model — the data the renderer draws.
//!
//! A [`Grid`] is a rectangular array of [`Cell`]s plus a capped scrollback
//! ring. It is deliberately renderer-agnostic: the GPU layer reads a snapshot
//! each frame, and the VT parser ([`crate::term`]) is the only writer.

use serde::{Deserialize, Serialize};

/// An RGBA color. Defaults to "use the theme's default" via [`Color::Default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Color {
    /// Inherit the theme default foreground/background.
    #[default]
    Default,
    /// One of the 16 ANSI indices (0-15).
    Indexed(u8),
    /// A direct 24-bit color.
    Rgb(u8, u8, u8),
}

/// Underline rendition style (C20 — styled underlines, SGR `4:n`).
///
/// `None` is no underline. The renderer (in the app crate) maps each variant to
/// the appropriate line style; the core only parses + stores the selection. The
/// legacy plain `underline: bool` is preserved as a derived accessor
/// ([`CellFlags::underline`]) so existing renderer/extraction code keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnderlineStyle {
    /// No underline (SGR `24` / `4:0`).
    #[default]
    None,
    /// Single underline (SGR `4` / `4:1`).
    Single,
    /// Double underline (SGR `4:2` / `21`).
    Double,
    /// Curly / "undercurl" underline (SGR `4:3`) — nvim LSP diagnostics.
    Curly,
    /// Dotted underline (SGR `4:4`).
    Dotted,
    /// Dashed underline (SGR `4:5`).
    Dashed,
}

/// Per-cell rendition attributes. Dependency-free flag set (serde-friendly).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CellFlags {
    /// Bold / increased-intensity rendition (SGR `1`).
    pub bold: bool,
    /// Dim / faint / decreased-intensity rendition (SGR `2`). Mutually
    /// cancelled with [`CellFlags::bold`] by SGR `22`, which per ECMA-48 resets
    /// BOTH intensities. The colour model blends a dim foreground toward its
    /// background (see `theme::dim_foreground`).
    pub dim: bool,
    /// Italic rendition (SGR `3`).
    pub italic: bool,
    /// Styled-underline selection (C20). `UnderlineStyle::None` means no
    /// underline. Use [`CellFlags::underline`] for a plain on/off check.
    pub underline_style: UnderlineStyle,
    /// Slow-blink rendition (SGR `5`, cancelled by SGR `25`). The core only
    /// records the request; whether to animate is the renderer's decision (and
    /// must honour the reduced-motion preference).
    pub blink: bool,
    /// Rapid-blink rendition (SGR `6`, cancelled by SGR `25`). Recorded
    /// separately from [`CellFlags::blink`] so a renderer can distinguish the
    /// two rates; most terminals render both identically.
    pub rapid_blink: bool,
    /// Reverse-video rendition: foreground and background are swapped (SGR `7`).
    pub inverse: bool,
    /// Concealed / hidden rendition (SGR `8`, cancelled by SGR `28`). The
    /// character still occupies the cell and is still copyable — only its
    /// foreground is painted in the background colour.
    pub conceal: bool,
    /// Strikethrough / crossed-out rendition (SGR `9`).
    pub strikeout: bool,
    /// Overline rendition (SGR `53`, cancelled by SGR `55`).
    pub overline: bool,
}

impl CellFlags {
    /// No attributes set.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::grid::{CellFlags, UnderlineStyle};
    ///
    /// let f = CellFlags::empty();
    /// assert!(!f.bold);
    /// assert!(!f.dim);
    /// assert!(!f.italic);
    /// assert!(!f.blink);
    /// assert!(!f.rapid_blink);
    /// assert!(!f.inverse);
    /// assert!(!f.conceal);
    /// assert!(!f.strikeout);
    /// assert!(!f.overline);
    /// assert_eq!(f.underline_style, UnderlineStyle::None);
    /// // No rendition implies no underline.
    /// assert!(!f.underline());
    /// ```
    pub const fn empty() -> Self {
        CellFlags {
            bold: false,
            dim: false,
            italic: false,
            underline_style: UnderlineStyle::None,
            blink: false,
            rapid_blink: false,
            inverse: false,
            conceal: false,
            strikeout: false,
            overline: false,
        }
    }

    /// Whether ANY underline is active (legacy boolean accessor). `true` for
    /// every [`UnderlineStyle`] variant except [`UnderlineStyle::None`].
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::grid::{CellFlags, UnderlineStyle};
    ///
    /// let mut f = CellFlags::empty();
    /// assert!(!f.underline());
    ///
    /// // Any non-None style counts as "underlined".
    /// f.underline_style = UnderlineStyle::Curly;
    /// assert!(f.underline());
    /// ```
    pub fn underline(&self) -> bool {
        self.underline_style != UnderlineStyle::None
    }
}

/// A single grid cell.
///
/// Deliberately `Copy`: every field is a small `Copy` scalar/enum. Combining
/// marks / variation selectors (the only heap-allocating per-position state)
/// live in a `Grid`-side parallel table ([`Grid::combining`]) keyed by cell
/// index, NOT on the cell — this keeps `Cell` cheap to clone (the visible grid
/// is snapshotted every render frame) and shrinks scrollback `Vec<Cell>` RSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// The printed character occupying this cell (a space for an empty cell).
    pub c: char,
    /// Foreground (text) colour.
    pub fg: Color,
    /// Background colour.
    pub bg: Color,
    /// Per-cell rendition attributes (bold, italic, underline, …).
    pub flags: CellFlags,
    /// Explicit underline color (C20, SGR `58`). `None` means the underline
    /// inherits the foreground color. Only meaningful when
    /// `flags.underline_style != UnderlineStyle::None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline_color: Option<Color>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            c: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            underline_color: None,
        }
    }
}

/// The terminal grid: a `rows x cols` matrix of cells.
#[derive(Debug, Clone)]
pub struct Grid {
    rows: usize,
    cols: usize,
    cells: Vec<Cell>,
    /// Per-row damage flags: `row_dirty[r] == true` means row `r`'s cells changed
    /// since the last `clear_damage`. Length is always `rows`. The whole-grid
    /// "is anything damaged?" question is `row_dirty.iter().any(..)`; the per-row
    /// granularity lets the renderer rebuild spans only for the rows that changed
    /// (the rest reuse their cached layout). Replaces the old single `damaged`
    /// bool — `is_damaged()` preserves the exact previous semantics.
    row_dirty: Vec<bool>,
    /// Per-row soft-wrap flags (`wrapped[r] == true` means row `r` filled the
    /// width and the logical line continues on row `r + 1`, i.e. there was NO
    /// hard newline between them). Drives non-lossy reflow on resize. Length is
    /// always `rows`.
    wrapped: Vec<bool>,
    /// Per-cell continuation flags: `continuation[idx] == true` marks the blank
    /// trailing spacer of a width-2 (wide) glyph, so the caller can find the
    /// glyph's base cell (for attaching combining marks, C27). Length is always
    /// `rows * cols`. Parallel to `cells`.
    continuation: Vec<bool>,
    /// Per-cell combining marks / variation selectors appended to the base
    /// grapheme (C27 / C34). `combining[idx] == None` (the common case) means
    /// the cell at `idx` renders just its base char. When present, the rendered
    /// grapheme is `cells[idx].c` followed by these chars. Bounded to
    /// [`Grid::MAX_COMBINING`] chars per cell so a hostile stream cannot grow it
    /// without limit. Length is always `rows * cols`. Parallel to `cells` — held
    /// here, off the [`Cell`], so `Cell` stays `Copy` (the visible grid is
    /// cloned every render frame; scrollback stores `Vec<Cell>` rows).
    combining: Vec<Option<String>>,
}

impl Grid {
    /// Maximum combining marks appended to a single base grapheme. Past this,
    /// further zero-width marks are dropped (a stream-cannot-exhaust-memory
    /// bound; real text never stacks this many).
    pub const MAX_COMBINING: usize = 8;

    /// Create a blank grid of `rows` × `cols` cells. Both dimensions are
    /// clamped to a minimum of 1 so a degenerate `0`-sized grid is never built.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::grid::Grid;
    ///
    /// let g = Grid::new(3, 4);
    /// assert_eq!(g.rows(), 3);
    /// assert_eq!(g.cols(), 4);
    /// // Every cell starts blank, and a fresh grid is fully damaged.
    /// assert_eq!(g.cell(0, 0).unwrap().c, ' ');
    /// assert!(g.is_damaged());
    ///
    /// // Zero dimensions are clamped up to 1 (never a 0-sized grid).
    /// let degenerate = Grid::new(0, 0);
    /// assert_eq!(degenerate.rows(), 1);
    /// assert_eq!(degenerate.cols(), 1);
    /// ```
    pub fn new(rows: usize, cols: usize) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Grid {
            rows,
            cols,
            cells: vec![Cell::default(); rows * cols],
            row_dirty: vec![true; rows],
            wrapped: vec![false; rows],
            continuation: vec![false; rows * cols],
            combining: vec![None; rows * cols],
        }
    }

    /// Mark a single row dirty (its cells changed). No-op out of bounds.
    #[inline]
    fn mark_row(&mut self, row: usize) {
        if let Some(d) = self.row_dirty.get_mut(row) {
            *d = true;
        }
    }

    /// Mark an inclusive row range `[top, bottom]` dirty (a scroll/region op
    /// rewrote every row in it). Clamped to the grid.
    #[inline]
    fn mark_rows(&mut self, top: usize, bottom: usize) {
        let bottom = bottom.min(self.rows.saturating_sub(1));
        for r in top..=bottom {
            if let Some(d) = self.row_dirty.get_mut(r) {
                *d = true;
            }
        }
    }

    /// Mark every row dirty (clear / resize / full-grid scroll).
    #[inline]
    fn mark_all(&mut self) {
        for d in &mut self.row_dirty {
            *d = true;
        }
    }

    /// Whether the cell at `(row, col)` is the trailing spacer of a wide glyph.
    pub fn is_continuation(&self, row: usize, col: usize) -> bool {
        if row < self.rows && col < self.cols {
            self.continuation[self.idx(row, col)]
        } else {
            false
        }
    }

    /// Write a cell AND mark it as a wide-glyph continuation spacer. Used by the
    /// VT layer for the blank second cell of a width-2 glyph (C7/C27).
    pub fn set_continuation(&mut self, row: usize, col: usize, cell: Cell) {
        if row < self.rows && col < self.cols {
            let i = self.idx(row, col);
            if self.cells[i] != cell || !self.continuation[i] || self.combining[i].is_some() {
                self.cells[i] = cell;
                self.continuation[i] = true;
                // A fresh write is its own base grapheme — drop any prior marks.
                self.combining[i] = None;
                self.mark_row(row);
            }
        }
    }

    /// Append a combining mark / variation selector to the base grapheme at
    /// `(row, col)` (C27 / C34). No-op out of bounds or past the per-cell cap
    /// ([`Grid::MAX_COMBINING`]).
    pub fn push_combining_at(&mut self, row: usize, col: usize, mark: char) {
        if row < self.rows && col < self.cols {
            let i = self.idx(row, col);
            let s = self.combining[i].get_or_insert_with(String::new);
            if s.chars().count() < Self::MAX_COMBINING {
                s.push(mark);
            }
            self.mark_row(row);
        }
    }

    /// The full grapheme rendered at `(row, col)`: the base char plus any
    /// combining marks held in the side-table (C27 / C34). Allocates only when
    /// combining marks are present. Returns an empty string out of bounds.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::grid::{Cell, Grid};
    ///
    /// let mut g = Grid::new(1, 2);
    /// g.set(0, 0, Cell { c: 'e', ..Default::default() });
    /// assert_eq!(g.grapheme_at(0, 0), "e");
    ///
    /// // A combining acute accent fuses onto the base char.
    /// g.push_combining_at(0, 0, '\u{0301}');
    /// assert_eq!(g.grapheme_at(0, 0), "e\u{0301}");
    ///
    /// // Out of bounds yields an empty string, never a panic.
    /// assert_eq!(g.grapheme_at(9, 9), "");
    /// ```
    pub fn grapheme_at(&self, row: usize, col: usize) -> String {
        if row < self.rows && col < self.cols {
            let i = self.idx(row, col);
            let base = self.cells[i].c;
            match &self.combining[i] {
                Some(extra) => {
                    let mut g = String::with_capacity(1 + extra.len());
                    g.push(base);
                    g.push_str(extra);
                    g
                }
                None => base.to_string(),
            }
        } else {
            String::new()
        }
    }

    /// The number of rows in the grid.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The number of columns in the grid.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Whether ANY row changed since the last `clear_damage` (preserves the exact
    /// semantics of the former single `damaged` bool).
    pub fn is_damaged(&self) -> bool {
        self.row_dirty.iter().any(|&d| d)
    }

    /// Whether row `r` changed since the last `clear_damage`. Out-of-range rows
    /// are reported clean. Lets the renderer rebuild only the rows that changed.
    pub fn is_row_dirty(&self, row: usize) -> bool {
        self.row_dirty.get(row).copied().unwrap_or(false)
    }

    /// Mark every row clean. Call after the renderer has consumed this frame's
    /// damage so the next [`Grid::is_row_dirty`] reflects only new changes.
    pub fn clear_damage(&mut self) {
        for d in &mut self.row_dirty {
            *d = false;
        }
    }

    /// Force the next frame to redraw EVERY row (e.g. after a scroll-view change
    /// where the visible window moved but the grid cells did not).
    pub fn touch(&mut self) {
        self.mark_all();
    }

    /// Whether row `r` soft-wrapped into the next row (no hard newline between).
    pub fn is_wrapped(&self, r: usize) -> bool {
        self.wrapped.get(r).copied().unwrap_or(false)
    }

    /// Record (or clear) the soft-wrap flag for row `r`.
    pub fn set_wrapped(&mut self, r: usize, wrapped: bool) {
        if let Some(slot) = self.wrapped.get_mut(r) {
            *slot = wrapped;
        }
    }

    fn idx(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    /// Borrow the cell at `(row, col)`, or `None` if the coordinates are out of
    /// range.
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        if row < self.rows && col < self.cols {
            Some(&self.cells[self.idx(row, col)])
        } else {
            None
        }
    }

    /// Write `cell` at `(row, col)`. Out-of-range coordinates are ignored. The
    /// affected row is marked dirty and any wide-glyph continuation marker on
    /// the cell is cleared.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::grid::{Cell, Grid};
    ///
    /// let mut g = Grid::new(2, 2);
    /// g.clear_damage();
    /// g.set(0, 1, Cell { c: 'z', ..Default::default() });
    /// assert_eq!(g.cell(0, 1).unwrap().c, 'z');
    /// // Writing a changed cell re-damages its row.
    /// assert!(g.is_row_dirty(0));
    ///
    /// // Out-of-range writes are silently ignored (no panic, no effect).
    /// g.set(99, 99, Cell { c: 'q', ..Default::default() });
    /// assert!(g.cell(99, 99).is_none());
    /// ```
    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        if row < self.rows && col < self.cols {
            let i = self.idx(row, col);
            // A normal write always clears any wide-glyph continuation marker AND
            // any combining marks — the cell is now its own fresh base grapheme.
            if self.cells[i] != cell || self.continuation[i] || self.combining[i].is_some() {
                self.cells[i] = cell;
                self.continuation[i] = false;
                self.combining[i] = None;
                self.mark_row(row);
            }
        }
    }

    /// Clear the whole grid to blank cells.
    pub fn clear(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
        for w in &mut self.wrapped {
            *w = false;
        }
        for k in &mut self.continuation {
            *k = false;
        }
        for m in &mut self.combining {
            *m = None;
        }
        self.mark_all();
    }

    /// Resize, preserving top-left content. Marks the grid damaged.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let mut next = vec![Cell::default(); rows * cols];
        let mut next_cont = vec![false; rows * cols];
        let mut next_comb: Vec<Option<String>> = vec![None; rows * cols];
        for r in 0..rows.min(self.rows) {
            for c in 0..cols.min(self.cols) {
                next[r * cols + c] = self.cells[self.idx(r, c)];
                next_cont[r * cols + c] = self.continuation[self.idx(r, c)];
                next_comb[r * cols + c] = self.combining[self.idx(r, c)].clone();
            }
        }
        let mut next_wrapped = vec![false; rows];
        // A row only stays "wrapped" if the width is unchanged; a width change
        // invalidates the flag (the proper fix is Terminal-level reflow, which
        // rebuilds these flags from logical lines).
        for (slot, &old) in next_wrapped
            .iter_mut()
            .zip(self.wrapped.iter())
            .take(rows.min(self.rows))
        {
            *slot = old && cols == self.cols;
        }
        self.rows = rows;
        self.cols = cols;
        self.cells = next;
        self.wrapped = next_wrapped;
        self.continuation = next_cont;
        self.combining = next_comb;
        // Resize the per-row damage vector to the new height and mark all dirty.
        self.row_dirty = vec![true; self.rows];
    }

    /// Scroll the whole grid up by one line (top line lost, bottom blank).
    pub fn scroll_up(&mut self) {
        let _ = self.scroll_up_returning();
    }

    /// Scroll up by one line, returning the dropped top row (for scrollback).
    pub fn scroll_up_returning(&mut self) -> Vec<Cell> {
        let dropped: Vec<Cell> = self.cells.drain(0..self.cols).collect();
        self.cells
            .extend(std::iter::repeat_n(Cell::default(), self.cols));
        // Keep the continuation bitset parallel: drop the top row's flags, add a
        // blank (non-continuation) bottom row.
        self.continuation.drain(0..self.cols);
        self.continuation
            .extend(std::iter::repeat_n(false, self.cols));
        // Keep the combining side-table parallel: drop the top row's marks, add
        // a blank (mark-free) bottom row.
        self.combining.drain(0..self.cols);
        self.combining.extend(std::iter::repeat_n(None, self.cols));
        // Shift wrap flags up by one; the new bottom row starts unwrapped.
        if !self.wrapped.is_empty() {
            self.wrapped.remove(0);
            self.wrapped.push(false);
        }
        // The whole grid shifted up — every row's content changed.
        self.mark_all();
        dropped
    }

    /// Scroll lines `[top, bottom]` (inclusive, 0-based) up by `n`, returning the
    /// `n` rows dropped off `top` (oldest first) so the caller can route them to
    /// scrollback. Rows below the bottom margin are untouched. New blank rows
    /// appear at the bottom of the region. `top`/`bottom` are clamped to the
    /// grid; `n` is clamped to the region height.
    pub fn scroll_region_up(&mut self, top: usize, bottom: usize, n: usize) -> Vec<Vec<Cell>> {
        let bottom = bottom.min(self.rows.saturating_sub(1));
        if top > bottom {
            return Vec::new();
        }
        let height = bottom - top + 1;
        let n = n.min(height);
        let mut dropped: Vec<Vec<Cell>> = Vec::with_capacity(n);
        for r in top..top + n {
            dropped.push(self.row(r).to_vec());
        }
        // Shift surviving rows up by n within the region.
        for r in top..=bottom {
            let src = r + n;
            for c in 0..self.cols {
                let dst = self.idx(r, c);
                // The continuation bitset (wide-glyph spacer flags) MUST move in
                // lockstep with cells/combining — otherwise a CJK/emoji base or
                // its spacer desyncs after a region scroll and the renderer
                // skips/doubles the wide glyph. (`src` is read before it is
                // overwritten because rows are processed top→bottom and src>dst.)
                let (cell, comb, cont) = if src <= bottom {
                    let s = self.idx(src, c);
                    (
                        self.cells[s],
                        self.combining[s].clone(),
                        self.continuation[s],
                    )
                } else {
                    (Cell::default(), None, false)
                };
                self.cells[dst] = cell;
                self.combining[dst] = comb;
                self.continuation[dst] = cont;
            }
            self.wrapped[r] = if src <= bottom {
                self.wrapped[src]
            } else {
                false
            };
        }
        // Every row in the scrolled region was rewritten.
        self.mark_rows(top, bottom);
        dropped
    }

    /// Scroll lines `[top, bottom]` (inclusive, 0-based) down by `n`. New blank
    /// rows appear at `top`; rows scrolled past `bottom` are discarded. Used by
    /// reverse-index and DL/IL.
    pub fn scroll_region_down(&mut self, top: usize, bottom: usize, n: usize) {
        let bottom = bottom.min(self.rows.saturating_sub(1));
        if top > bottom {
            return;
        }
        let height = bottom - top + 1;
        let n = n.min(height);
        // Shift surviving rows down by n within the region (iterate top→bottom
        // from the bottom so we don't overwrite sources before reading them).
        for r in (top..=bottom).rev() {
            for c in 0..self.cols {
                let dst = self.idx(r, c);
                // Continuation bitset moves in lockstep (see scroll_region_up).
                // `r-n` is read before being overwritten because rows are
                // processed bottom→top and the source row is above the dest.
                let (cell, comb, cont) = if r >= top + n {
                    let s = self.idx(r - n, c);
                    (
                        self.cells[s],
                        self.combining[s].clone(),
                        self.continuation[s],
                    )
                } else {
                    (Cell::default(), None, false)
                };
                self.cells[dst] = cell;
                self.combining[dst] = comb;
                self.continuation[dst] = cont;
            }
            self.wrapped[r] = if r >= top + n {
                self.wrapped[r - n]
            } else {
                false
            };
        }
        // Every row in the scrolled region was rewritten.
        self.mark_rows(top, bottom);
    }

    /// Insert `count` blank cells at `(row, col)`, shifting the rest of the line
    /// right (ICH). Cells pushed past the right edge are lost.
    pub fn insert_blanks(&mut self, row: usize, col: usize, count: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let count = count.min(self.cols - col);
        // Shift right: walk from the right edge inward. Continuation flags move
        // in lockstep so a shifted wide glyph keeps its spacer (c-count is read
        // before being overwritten because we walk right→left and src<dst).
        for c in (col..self.cols).rev() {
            let dst = self.idx(row, c);
            let (cell, comb, cont) = if c >= col + count {
                let s = self.idx(row, c - count);
                (
                    self.cells[s],
                    self.combining[s].clone(),
                    self.continuation[s],
                )
            } else {
                (Cell::default(), None, false)
            };
            self.cells[dst] = cell;
            self.combining[dst] = comb;
            self.continuation[dst] = cont;
        }
        self.mark_row(row);
    }

    /// Delete `count` cells at `(row, col)`, shifting the rest of the line left
    /// (DCH). Blank cells fill in at the right edge.
    pub fn delete_chars(&mut self, row: usize, col: usize, count: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let count = count.min(self.cols - col);
        // Continuation flags move in lockstep so a shifted wide glyph keeps its
        // spacer (c+count is read before being overwritten because we walk
        // left→right and src>dst).
        for c in col..self.cols {
            let dst = self.idx(row, c);
            let (cell, comb, cont) = if c + count < self.cols {
                let s = self.idx(row, c + count);
                (
                    self.cells[s],
                    self.combining[s].clone(),
                    self.continuation[s],
                )
            } else {
                (Cell::default(), None, false)
            };
            self.cells[dst] = cell;
            self.combining[dst] = comb;
            self.continuation[dst] = cont;
        }
        self.mark_row(row);
    }

    /// Erase `count` cells at `(row, col)` to blank without shifting (ECH).
    pub fn erase_chars(&mut self, row: usize, col: usize, count: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let end = (col + count).min(self.cols);
        for c in col..end {
            let dst = self.idx(row, c);
            self.cells[dst] = Cell::default();
            self.combining[dst] = None;
            // Blanking a cell clears its wide-glyph spacer flag too (mirrors the
            // normal `set` write path); otherwise a stale spacer survives ECH.
            self.continuation[dst] = false;
        }
        self.mark_row(row);
    }

    /// Borrow one row's cells as a slice.
    pub fn row(&self, r: usize) -> &[Cell] {
        let start = r * self.cols;
        &self.cells[start..start + self.cols]
    }

    /// Render the grid to a plain `String` (one line per row) — used by tests
    /// and the headless smoke test before the GPU renderer is wired.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::grid::{Cell, Grid};
    ///
    /// let mut g = Grid::new(2, 2);
    /// g.set(0, 0, Cell { c: 'h', ..Default::default() });
    /// g.set(0, 1, Cell { c: 'i', ..Default::default() });
    /// // One line per row, trailing newline per row; unset cells stay blank.
    /// assert_eq!(g.to_text(), "hi\n  \n");
    /// ```
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(self.rows * (self.cols + 1));
        for r in 0..self.rows {
            for c in 0..self.cols {
                out.push(self.cells[self.idx(r, c)].c);
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ich_dch_ech_ignore_out_of_bounds_coords() {
        // insert_blanks / delete_chars / erase_chars each guard against a row or
        // col past the grid before indexing. Removing the guard panics on the
        // slice index, so an out-of-bounds call must be a silent no-op that
        // leaves every existing cell (and the damage flag) untouched.
        let mut g = Grid::new(2, 3);
        g.set(
            0,
            0,
            Cell {
                c: 'A',
                ..Default::default()
            },
        );
        g.set(
            1,
            2,
            Cell {
                c: 'Z',
                ..Default::default()
            },
        );
        g.clear_damage();
        for (r, c) in [(5usize, 0usize), (0usize, 9usize), (99usize, 99usize)] {
            g.insert_blanks(r, c, 2);
            g.delete_chars(r, c, 2);
            g.erase_chars(r, c, 2);
        }
        assert_eq!(g.cell(0, 0).unwrap().c, 'A', "OOB op must not mutate cells");
        assert_eq!(g.cell(1, 2).unwrap().c, 'Z');
        assert!(
            !g.is_damaged(),
            "an out-of-bounds no-op must not mark damage"
        );
    }

    #[test]
    fn new_grid_is_blank_and_damaged() {
        let g = Grid::new(3, 4);
        assert_eq!(g.rows(), 3);
        assert_eq!(g.cols(), 4);
        assert!(g.is_damaged());
        assert_eq!(g.cell(0, 0).unwrap().c, ' ');
    }

    #[test]
    fn set_marks_damage_only_on_change() {
        let mut g = Grid::new(2, 2);
        g.clear_damage();
        assert!(!g.is_damaged());
        g.set(
            0,
            0,
            Cell {
                c: 'x',
                ..Default::default()
            },
        );
        assert!(g.is_damaged());
        g.clear_damage();
        g.set(
            0,
            0,
            Cell {
                c: 'x',
                ..Default::default()
            },
        );
        assert!(
            !g.is_damaged(),
            "rewriting the same cell must not re-damage"
        );
    }

    #[test]
    fn scroll_up_drops_top_line() {
        let mut g = Grid::new(2, 1);
        g.set(
            0,
            0,
            Cell {
                c: 'a',
                ..Default::default()
            },
        );
        g.set(
            1,
            0,
            Cell {
                c: 'b',
                ..Default::default()
            },
        );
        g.scroll_up();
        assert_eq!(g.cell(0, 0).unwrap().c, 'b');
        assert_eq!(g.cell(1, 0).unwrap().c, ' ');
    }

    #[test]
    fn resize_preserves_top_left() {
        let mut g = Grid::new(2, 2);
        g.set(
            0,
            0,
            Cell {
                c: 'q',
                ..Default::default()
            },
        );
        g.resize(4, 4);
        assert_eq!(g.cell(0, 0).unwrap().c, 'q');
        assert_eq!(g.rows(), 4);
    }

    fn put(g: &mut Grid, r: usize, c: usize, ch: char) {
        g.set(
            r,
            c,
            Cell {
                c: ch,
                ..Default::default()
            },
        );
    }

    #[test]
    fn insert_blanks_shifts_right() {
        let mut g = Grid::new(1, 5);
        for (i, ch) in "abcde".chars().enumerate() {
            put(&mut g, 0, i, ch);
        }
        g.insert_blanks(0, 1, 2); // a__bc (de pushed off)
        assert_eq!(g.cell(0, 0).unwrap().c, 'a');
        assert_eq!(g.cell(0, 1).unwrap().c, ' ');
        assert_eq!(g.cell(0, 2).unwrap().c, ' ');
        assert_eq!(g.cell(0, 3).unwrap().c, 'b');
        assert_eq!(g.cell(0, 4).unwrap().c, 'c');
    }

    #[test]
    fn delete_chars_shifts_left() {
        let mut g = Grid::new(1, 5);
        for (i, ch) in "abcde".chars().enumerate() {
            put(&mut g, 0, i, ch);
        }
        g.delete_chars(0, 1, 2); // ade__ (b,c removed)
        assert_eq!(g.cell(0, 0).unwrap().c, 'a');
        assert_eq!(g.cell(0, 1).unwrap().c, 'd');
        assert_eq!(g.cell(0, 2).unwrap().c, 'e');
        assert_eq!(g.cell(0, 3).unwrap().c, ' ');
        assert_eq!(g.cell(0, 4).unwrap().c, ' ');
    }

    #[test]
    fn erase_chars_blanks_without_shift() {
        let mut g = Grid::new(1, 5);
        for (i, ch) in "abcde".chars().enumerate() {
            put(&mut g, 0, i, ch);
        }
        g.erase_chars(0, 1, 2); // a__de
        assert_eq!(g.cell(0, 0).unwrap().c, 'a');
        assert_eq!(g.cell(0, 1).unwrap().c, ' ');
        assert_eq!(g.cell(0, 2).unwrap().c, ' ');
        assert_eq!(g.cell(0, 3).unwrap().c, 'd');
        assert_eq!(g.cell(0, 4).unwrap().c, 'e');
    }

    #[test]
    fn scroll_region_up_within_margins() {
        let mut g = Grid::new(4, 1);
        for (r, ch) in "abcd".chars().enumerate() {
            put(&mut g, r, 0, ch);
        }
        // Scroll rows 1..=2 up by 1: row0 'a' fixed, row3 'd' fixed.
        let dropped = g.scroll_region_up(1, 2, 1);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0][0].c, 'b');
        assert_eq!(g.cell(0, 0).unwrap().c, 'a');
        assert_eq!(g.cell(1, 0).unwrap().c, 'c');
        assert_eq!(g.cell(2, 0).unwrap().c, ' ');
        assert_eq!(g.cell(3, 0).unwrap().c, 'd');
    }

    #[test]
    fn scroll_region_down_within_margins() {
        let mut g = Grid::new(4, 1);
        for (r, ch) in "abcd".chars().enumerate() {
            put(&mut g, r, 0, ch);
        }
        // Scroll rows 1..=2 down by 1: blank at row1, c shifts to row2.
        g.scroll_region_down(1, 2, 1);
        assert_eq!(g.cell(0, 0).unwrap().c, 'a');
        assert_eq!(g.cell(1, 0).unwrap().c, ' ');
        assert_eq!(g.cell(2, 0).unwrap().c, 'b');
        assert_eq!(g.cell(3, 0).unwrap().c, 'd');
    }

    // ---- C20: styled-underline model ----

    #[test]
    fn underline_style_default_is_none() {
        let f = CellFlags::empty();
        assert_eq!(f.underline_style, UnderlineStyle::None);
        assert!(!f.underline(), "empty flags report no underline");
    }

    #[test]
    fn underline_boolean_accessor_tracks_style() {
        let mut f = CellFlags::empty();
        f.underline_style = UnderlineStyle::Curly;
        assert!(f.underline(), "any style counts as underlined");
        f.underline_style = UnderlineStyle::None;
        assert!(!f.underline());
    }

    #[test]
    fn cell_default_has_no_underline_color() {
        let c = Cell::default();
        assert_eq!(c.underline_color, None);
        // The grapheme of a fresh grid cell is just its base char (no marks).
        let g = Grid::new(1, 1);
        assert_eq!(g.grapheme_at(0, 0), " ");
    }

    // ---- Cell is Copy / size shrink ----

    /// `Cell` must be `Copy` (compile-time): the visible grid is cloned every
    /// render frame and scrollback stores `Vec<Cell>` rows — a non-`Copy` cell
    /// (the old `combining: Option<String>` field) forced a deep clone of every
    /// heap string per frame.
    #[test]
    fn cell_is_copy_and_small() {
        const _: () = {
            const fn _assert_copy<T: Copy>() {}
            _assert_copy::<Cell>();
        };
        // Combining marks moved to the Grid side-table, so Cell holds only
        // small Copy scalars/enums. Assert the struct stayed compact.
        assert!(
            std::mem::size_of::<Cell>() <= 32,
            "Cell grew past 32 bytes: {}",
            std::mem::size_of::<Cell>()
        );
    }

    // ---- C27 / C34: combining marks via the Grid side-table ----

    /// A combining mark pushed via the grid round-trips through `grapheme_at`.
    #[test]
    fn push_combining_builds_grapheme() {
        let mut g = Grid::new(1, 4);
        g.set(0, 0, cell('e'));
        g.push_combining_at(0, 0, '\u{0301}'); // combining acute accent
        assert_eq!(g.grapheme_at(0, 0), "e\u{0301}");
    }

    /// The per-cell combining cap ([`Grid::MAX_COMBINING`]) is enforced.
    #[test]
    fn push_combining_is_bounded() {
        let mut g = Grid::new(1, 1);
        g.set(0, 0, cell('x'));
        for _ in 0..(Grid::MAX_COMBINING + 5) {
            g.push_combining_at(0, 0, '\u{0301}');
        }
        // grapheme = base char + exactly MAX_COMBINING marks.
        let count = g.grapheme_at(0, 0).chars().count() - 1;
        assert_eq!(count, Grid::MAX_COMBINING, "combining marks cap enforced");
    }

    /// A normal `set` at a cell that already holds combining marks clears them —
    /// the cell becomes a fresh base grapheme.
    #[test]
    fn set_clears_existing_combining() {
        let mut g = Grid::new(1, 2);
        g.set(0, 0, cell('e'));
        g.push_combining_at(0, 0, '\u{0301}');
        assert_eq!(g.grapheme_at(0, 0), "e\u{0301}");
        g.set(0, 0, cell('z'));
        assert_eq!(g.grapheme_at(0, 0), "z", "set drops prior combining marks");
    }

    /// The combining side-table is threaded through cell-moving mutations EXACTLY
    /// parallel to `cells`: a mark survives a whole-grid scroll onto the row it
    /// moves to. This is the load-bearing correctness proof for the side-table.
    #[test]
    fn combining_survives_whole_grid_scroll() {
        let mut g = Grid::new(3, 4);
        g.set(1, 2, cell('e'));
        g.push_combining_at(1, 2, '\u{0301}');
        assert_eq!(g.grapheme_at(1, 2), "e\u{0301}");
        // Whole-grid scroll-up: row 1 content moves to row 0.
        g.scroll_up();
        assert_eq!(
            g.grapheme_at(0, 2),
            "e\u{0301}",
            "combining mark followed its cell up one row"
        );
        // The vacated bottom row carries no stale marks.
        assert_eq!(g.grapheme_at(2, 2), " ");
    }

    /// Same proof for a bounded region scroll (`scroll_region_up`): the mark
    /// shifts up within the region exactly as the base char does.
    #[test]
    fn combining_survives_region_scroll() {
        let mut g = Grid::new(4, 4);
        g.set(2, 1, cell('a'));
        g.push_combining_at(2, 1, '\u{0302}'); // combining circumflex
                                               // Scroll rows 1..=3 up by 1: row 2 content moves to row 1.
        let _ = g.scroll_region_up(1, 3, 1);
        assert_eq!(
            g.grapheme_at(1, 1),
            "a\u{0302}",
            "combining mark shifted up within the scroll region"
        );
    }

    /// `delete_chars` shifts the combining side-table left in lockstep with the
    /// base cells.
    #[test]
    fn combining_shifts_with_delete_chars() {
        let mut g = Grid::new(1, 5);
        g.set(0, 2, cell('m'));
        g.push_combining_at(0, 2, '\u{0303}'); // combining tilde
                                               // Delete 2 cells at col 0 — the marked cell shifts from col 2 to col 0.
        g.delete_chars(0, 0, 2);
        assert_eq!(g.grapheme_at(0, 0), "m\u{0303}");
    }

    // ---- Per-row damage tracking ----

    fn cell(ch: char) -> Cell {
        Cell {
            c: ch,
            ..Default::default()
        }
    }

    /// `is_damaged()` preserves the exact previous semantics: a fresh grid is
    /// damaged; clearing makes it clean; any single-row change re-damages it.
    #[test]
    fn is_damaged_matches_any_row_dirty() {
        let mut g = Grid::new(4, 6);
        assert!(g.is_damaged(), "a fresh grid is damaged");
        g.clear_damage();
        assert!(!g.is_damaged(), "clear_damage makes it clean");
        g.set(2, 1, cell('x'));
        assert!(g.is_damaged(), "a change re-damages");
        assert_eq!(g.is_damaged(), (0..g.rows()).any(|r| g.is_row_dirty(r)));
    }

    /// A `set` marks ONLY its own row dirty.
    #[test]
    fn set_marks_only_its_row() {
        let mut g = Grid::new(5, 6);
        g.clear_damage();
        g.set(2, 1, cell('x'));
        for r in 0..5 {
            assert_eq!(
                g.is_row_dirty(r),
                r == 2,
                "only row 2 dirty after set, got r={r}"
            );
        }
    }

    /// `erase_chars` / `delete_chars` / `insert_blanks` mark only their row.
    #[test]
    fn line_ops_mark_only_their_row() {
        for op in 0..3 {
            let mut g = Grid::new(5, 8);
            g.clear_damage();
            match op {
                0 => g.erase_chars(3, 0, 4),
                1 => g.delete_chars(3, 0, 2),
                _ => g.insert_blanks(3, 0, 2),
            }
            for r in 0..5 {
                assert_eq!(
                    g.is_row_dirty(r),
                    r == 3,
                    "op {op}: only row 3 dirty, got r={r}"
                );
            }
        }
    }

    /// `scroll_region_up` marks every row in `[top, bottom]` dirty and nothing
    /// outside it (rows above/below the margin are untouched).
    #[test]
    fn scroll_region_marks_only_the_region() {
        let mut g = Grid::new(6, 6);
        g.clear_damage();
        let _ = g.scroll_region_up(1, 3, 1);
        for r in 0..6 {
            let expect = (1..=3).contains(&r);
            assert_eq!(g.is_row_dirty(r), expect, "scroll_region_up [1,3]: r={r}");
        }

        let mut g = Grid::new(6, 6);
        g.clear_damage();
        g.scroll_region_down(2, 4, 1);
        for r in 0..6 {
            let expect = (2..=4).contains(&r);
            assert_eq!(g.is_row_dirty(r), expect, "scroll_region_down [2,4]: r={r}");
        }
    }

    /// A whole-grid scroll and `clear` mark EVERY row dirty.
    #[test]
    fn full_scroll_and_clear_mark_all_rows() {
        let mut g = Grid::new(4, 6);
        g.clear_damage();
        g.scroll_up();
        assert!(
            (0..4).all(|r| g.is_row_dirty(r)),
            "scroll_up marks all rows"
        );

        g.clear_damage();
        g.clear();
        assert!((0..4).all(|r| g.is_row_dirty(r)), "clear marks all rows");
    }

    /// `resize` re-sizes the dirty vector to the new height and marks all dirty
    /// (so `is_row_dirty` is valid for the new last row).
    #[test]
    fn resize_resizes_and_marks_all_dirty() {
        let mut g = Grid::new(3, 4);
        g.clear_damage();
        g.resize(7, 4);
        assert_eq!(g.rows(), 7);
        assert!(
            (0..7).all(|r| g.is_row_dirty(r)),
            "resize marks every new row dirty"
        );
        // is_row_dirty is in-range for the new height (no panic / stale length).
        assert!(g.is_row_dirty(6));
        assert!(!g.is_row_dirty(7), "out-of-range row reads clean");
    }

    // --- wide-glyph continuation-bitset must move in lockstep with cells ---
    // Regression: scroll_region_up/down + insert_blanks/delete_chars moved
    // `cells`/`combining` but NOT `continuation`, desyncing a wide glyph's
    // spacer flag and corrupting CJK/emoji rendering after a line edit/scroll.

    #[test]
    fn scroll_region_up_moves_continuation_flags() {
        let mut g = Grid::new(4, 2);
        // Row 2 holds a wide glyph: base at col0, continuation spacer at col1.
        put(&mut g, 2, 0, '世');
        g.set_continuation(2, 1, Cell::default());
        assert!(g.is_continuation(2, 1));
        g.scroll_region_up(1, 2, 1); // row2 → row1
        assert!(
            g.is_continuation(1, 1),
            "continuation flag must follow the cell up"
        );
        assert!(
            !g.is_continuation(2, 1),
            "vacated row's spacer flag is cleared"
        );
    }

    #[test]
    fn scroll_region_down_moves_continuation_flags() {
        let mut g = Grid::new(4, 2);
        put(&mut g, 1, 0, '世');
        g.set_continuation(1, 1, Cell::default());
        g.scroll_region_down(1, 2, 1); // row1 → row2
        assert!(
            g.is_continuation(2, 1),
            "continuation flag must follow the cell down"
        );
        assert!(!g.is_continuation(1, 1), "vacated top-of-region cleared");
    }

    #[test]
    fn insert_blanks_moves_continuation_flags() {
        let mut g = Grid::new(1, 5);
        // Wide glyph at cols 2-3 (base col2, spacer col3).
        put(&mut g, 0, 2, '世');
        g.set_continuation(0, 3, Cell::default());
        g.insert_blanks(0, 0, 1); // shift right by 1: col3 spacer → col4
        assert!(
            g.is_continuation(0, 4),
            "spacer shifts right with its glyph"
        );
        assert!(
            !g.is_continuation(0, 3),
            "old spacer position now holds the base, not a spacer"
        );
    }

    #[test]
    fn delete_chars_moves_continuation_flags() {
        let mut g = Grid::new(1, 5);
        put(&mut g, 0, 2, '世');
        g.set_continuation(0, 3, Cell::default());
        g.delete_chars(0, 0, 1); // shift left by 1: col3 spacer → col2
        assert!(g.is_continuation(0, 2), "spacer shifts left with its glyph");
        assert!(
            !g.is_continuation(0, 3),
            "old spacer position is no longer a continuation"
        );
    }

    #[test]
    fn erase_chars_clears_continuation_flags() {
        let mut g = Grid::new(1, 5);
        put(&mut g, 0, 2, '世');
        g.set_continuation(0, 3, Cell::default());
        g.erase_chars(0, 3, 1); // blank the spacer cell
        assert!(
            !g.is_continuation(0, 3),
            "erased cell's spacer flag is cleared (mirrors the `set` write path)"
        );
    }

    // ---- degenerate dimensions, accessors, and edge branches ----

    /// `Grid::new(0, 0)` clamps both dimensions to ≥ 1 so a degenerate grid is
    /// never built (the `.max(1)` clamps).
    #[test]
    fn new_clamps_zero_dimensions_to_one() {
        let g = Grid::new(0, 0);
        assert_eq!(g.rows(), 1);
        assert_eq!(g.cols(), 1);
        assert_eq!(g.cell(0, 0).unwrap().c, ' ');
    }

    /// `resize` likewise clamps a zero target to 1×1.
    #[test]
    fn resize_clamps_zero_to_one() {
        let mut g = Grid::new(3, 3);
        g.resize(0, 0);
        assert_eq!(g.rows(), 1);
        assert_eq!(g.cols(), 1);
    }

    /// `cell()` returns `None` out of range (the row/col bounds guard).
    #[test]
    fn cell_out_of_range_is_none() {
        let g = Grid::new(2, 2);
        assert!(g.cell(2, 0).is_none(), "row past end");
        assert!(g.cell(0, 2).is_none(), "col past end");
        assert!(g.cell(0, 0).is_some());
    }

    /// `set` / `is_continuation` / `grapheme_at` ignore out-of-range coordinates
    /// without panicking and report the documented defaults.
    #[test]
    fn out_of_range_mutators_and_accessors_are_safe() {
        let mut g = Grid::new(2, 2);
        g.set(9, 9, cell('z')); // no-op, no panic
        g.set_continuation(9, 9, cell('z')); // no-op
        g.push_combining_at(9, 9, '\u{0301}'); // no-op
        assert!(!g.is_continuation(9, 9), "OOB continuation reads false");
        assert_eq!(g.grapheme_at(9, 9), "", "OOB grapheme is empty");
    }

    /// `touch()` forces every row dirty after a clear (the scroll-view-moved
    /// case where cells did not change but the window did).
    #[test]
    fn touch_marks_every_row_dirty() {
        let mut g = Grid::new(3, 3);
        g.clear_damage();
        assert!(!g.is_damaged());
        g.touch();
        assert!((0..3).all(|r| g.is_row_dirty(r)), "touch dirties all rows");
    }

    /// `set_wrapped` / `is_wrapped` round-trip the soft-wrap flag, and an
    /// out-of-range row reads `false` and is a no-op to set.
    #[test]
    fn wrapped_flag_round_trips_and_is_bounds_safe() {
        let mut g = Grid::new(3, 4);
        assert!(!g.is_wrapped(0), "default unwrapped");
        g.set_wrapped(1, true);
        assert!(g.is_wrapped(1));
        g.set_wrapped(1, false);
        assert!(!g.is_wrapped(1));
        // OOB row: read false, set is a no-op (no panic, no growth).
        assert!(!g.is_wrapped(99));
        g.set_wrapped(99, true);
        assert!(!g.is_wrapped(99));
    }

    /// On a same-WIDTH resize a wrapped row keeps its flag; on a width CHANGE the
    /// flag is invalidated (cleared) per the reflow contract.
    #[test]
    fn resize_preserves_wrap_only_when_width_unchanged() {
        let mut g = Grid::new(3, 4);
        g.set_wrapped(0, true);
        // Same width (4) → flag survives.
        g.resize(5, 4);
        assert!(g.is_wrapped(0), "same-width resize keeps the wrap flag");

        let mut g2 = Grid::new(3, 4);
        g2.set_wrapped(0, true);
        // Width change (4→6) → flag invalidated.
        g2.resize(3, 6);
        assert!(
            !g2.is_wrapped(0),
            "width-changing resize clears the wrap flag"
        );
    }

    /// `scroll_region_up` with `top > bottom` returns an empty vec and changes
    /// nothing (the early-return guard).
    #[test]
    fn scroll_region_up_inverted_range_is_noop() {
        let mut g = Grid::new(4, 1);
        for (r, ch) in "abcd".chars().enumerate() {
            put(&mut g, r, 0, ch);
        }
        let dropped = g.scroll_region_up(3, 1, 1);
        assert!(dropped.is_empty(), "inverted range drops nothing");
        // Content untouched.
        assert_eq!(g.cell(0, 0).unwrap().c, 'a');
        assert_eq!(g.cell(3, 0).unwrap().c, 'd');
    }

    /// `scroll_region_down` with `top > bottom` is a no-op (the early return).
    #[test]
    fn scroll_region_down_inverted_range_is_noop() {
        let mut g = Grid::new(4, 1);
        for (r, ch) in "abcd".chars().enumerate() {
            put(&mut g, r, 0, ch);
        }
        g.scroll_region_down(3, 1, 1);
        assert_eq!(g.cell(0, 0).unwrap().c, 'a');
        assert_eq!(g.cell(3, 0).unwrap().c, 'd');
    }

    /// `scroll_up_returning` hands back the dropped top row's cells so the caller
    /// can route them to scrollback. Asserts the exact dropped content.
    #[test]
    fn scroll_up_returning_yields_dropped_top_row() {
        let mut g = Grid::new(2, 2);
        put(&mut g, 0, 0, 'a');
        put(&mut g, 0, 1, 'b');
        put(&mut g, 1, 0, 'c');
        let dropped = g.scroll_up_returning();
        assert_eq!(dropped.len(), 2, "dropped row has cols cells");
        assert_eq!(dropped[0].c, 'a');
        assert_eq!(dropped[1].c, 'b');
        // The bottom row scrolled up; new bottom is blank.
        assert_eq!(g.cell(0, 0).unwrap().c, 'c');
        assert_eq!(g.cell(1, 0).unwrap().c, ' ');
    }

    /// `row()` borrows exactly `cols` cells for the requested row, in order.
    #[test]
    fn row_borrows_full_row_slice() {
        let mut g = Grid::new(2, 3);
        for (c, ch) in "xyz".chars().enumerate() {
            put(&mut g, 1, c, ch);
        }
        let r = g.row(1);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].c, 'x');
        assert_eq!(r[2].c, 'z');
    }

    /// `set_continuation` writing the SAME cell+flag twice does not re-damage
    /// (the change-detection guard), but the first write does damage the row.
    #[test]
    fn set_continuation_damage_is_change_gated() {
        let mut g = Grid::new(2, 2);
        g.clear_damage();
        g.set_continuation(0, 1, Cell::default());
        assert!(g.is_damaged(), "first continuation write damages the row");
        g.clear_damage();
        g.set_continuation(0, 1, Cell::default());
        assert!(
            !g.is_damaged(),
            "rewriting the identical continuation spacer must not re-damage"
        );
    }

    /// `clear` blanks cells, wrap flags, continuation flags, and combining marks
    /// all at once. Asserts each side-table is reset.
    #[test]
    fn clear_resets_all_side_tables() {
        let mut g = Grid::new(2, 2);
        g.set(0, 0, cell('a'));
        g.push_combining_at(0, 0, '\u{0301}');
        g.set_continuation(0, 1, Cell::default());
        g.set_wrapped(0, true);
        g.clear();
        assert_eq!(g.cell(0, 0).unwrap().c, ' ', "cell blanked");
        assert_eq!(g.grapheme_at(0, 0), " ", "combining cleared");
        assert!(!g.is_continuation(0, 1), "continuation cleared");
        assert!(!g.is_wrapped(0), "wrap cleared");
    }
}
