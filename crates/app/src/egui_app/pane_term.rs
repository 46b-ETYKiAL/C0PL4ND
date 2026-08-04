//! Per-pane live terminal state for the egui shell (Milestone 2).
//!
//! Each open pane in the [`super::grid`] tree is backed by a [`PaneTerm`]: a
//! live [`c0pl4nd_core::Session`] (PTY + reader thread + shared [`Terminal`]),
//! the active [`Theme`], and the cell metrics that map a pixel rect onto a
//! `(cols, rows)` grid. This module is the testable, bug-prone CORE of
//! Milestone 2:
//!
//! - [`PaneTerm::forward_key`] / [`PaneTerm::write_bytes`] push input to the PTY
//!   via the SHARED [`c0pl4nd_core::term::encode_key`] (the SAME escape-sequence
//!   encoder the winit shell uses — never re-derived here).
//! - [`PaneTerm::resize_to_px`] recomputes `(cols, rows)` from a pixel rect and
//!   resizes the PTY + reflows the grid, but ONLY when the dimensions actually
//!   change (debounced).
//! - [`PaneTerm::grid_rows`] snapshots the visible grid into per-row colour
//!   runs ready for the paint layer, reusing [`Theme::cell_colors`] so the
//!   foreground/background/inverse handling matches the winit renderer exactly.
//!
//! The glyphon GPU paint itself lives in [`super::term_render`]; this module is
//! UI-toolkit-free (no egui, no wgpu) so it can be driven headlessly with
//! simulated input — which is exactly the "typing reaches the PTY and the grid
//! updates" class of bug Milestone 2 must guard against.

use std::cell::RefCell;
use std::rc::Rc;

use c0pl4nd_core::term::{
    encode_key, encode_key_kitty, ColorSet, KeyEventKind, KeyModifiers, LogicalKey, MouseButton,
    MouseEventKind, MouseMode, MouseModifiers, Progress,
};
use c0pl4nd_core::{Session, Theme};

/// The fully-resolved rendition of one terminal cell: the colours [`Theme::cell_colors`]
/// produced plus the SGR attribute bits the core parsed. This is the ONLY colour
/// carrier the paint layer sees — the renderer never re-derives a colour, it just
/// draws what the core resolved (one source of truth, per the theme API contract).
///
/// `bg` is `Option` exactly as [`Theme::cell_colors`] returns it: `None` means
/// "the window default background", which the renderer SKIPS painting a quad for
/// (the overwhelmingly common case — a default-background cell needs no quad).
/// A `Some(_)` background is what makes `grep --color`, `ls` directory colours,
/// `git diff`, fzf's selected row and every TUI's selected row show their block.
///
/// Plain data (no egui / glyphon types) so this module stays headlessly testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunStyle {
    /// Effective foreground RGB (inverse already applied by `Theme::cell_colors`).
    pub fg: (u8, u8, u8),
    /// Effective background RGB, or `None` for the window default (skip the quad).
    pub bg: Option<(u8, u8, u8)>,
    /// SGR `1` — render with a bold face (or faux-bold when none is installed).
    pub bold: bool,
    /// SGR `3` — render italic/oblique.
    pub italic: bool,
    /// SGR `4` / `4:0..5` — the styled-underline selection.
    pub underline: c0pl4nd_core::grid::UnderlineStyle,
    /// SGR `58`/`59` — the underline's own colour, or `None` to use [`Self::fg`].
    pub underline_color: Option<(u8, u8, u8)>,
    /// SGR `9` — crossed-out.
    pub strikeout: bool,
    /// SGR `53` — a line ABOVE the cell (overline). Parsed into `CellFlags` but
    /// previously dropped at this boundary, so `\e[53m` had no visible effect.
    pub overline: bool,
}

impl RunStyle {
    /// A plain run in `fg` on the default background with no attributes — the
    /// shape the dead-session mono fallback and the pure-colour tests want.
    pub const fn plain(fg: (u8, u8, u8)) -> Self {
        RunStyle {
            fg,
            bg: None,
            bold: false,
            italic: false,
            underline: c0pl4nd_core::grid::UnderlineStyle::None,
            underline_color: None,
            strikeout: false,
            overline: false,
        }
    }

    /// Whether this run draws any line decoration (underline / strikeout /
    /// overline), i.e. whether the renderer's decoration pass has anything to do.
    pub fn has_decoration(&self) -> bool {
        self.strikeout
            || self.overline
            || self.underline != c0pl4nd_core::grid::UnderlineStyle::None
    }
}

/// A colour run: a string of consecutive glyphs that share one fully-resolved
/// [`RunStyle`], and that style. The egui paint layer turns each run into
/// background quads, glyph galleys, and line decorations; keeping the payload a
/// plain data struct keeps this module free of any egui/glyphon dependency.
pub type ColorRun = (String, RunStyle);

/// One contiguous horizontal span of grid CELLS that share a [`RunStyle`]:
/// `col..col + width` on a single row. Produced by [`row_cell_spans`] and
/// consumed by the renderer's background-quad and line-decoration passes, both
/// of which want per-span rectangles rather than per-glyph positions.
///
/// Unlike [`row_glyph_cells`], BLANK cells are INCLUDED — a run of spaces with a
/// coloured background is precisely the highlighted block a terminal must draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSpan {
    /// First grid column of the span (0-based).
    pub col: usize,
    /// Span width in grid CELLS (a wide glyph contributes 2).
    pub width: usize,
    /// The rendition every cell in the span shares.
    pub style: RunStyle,
}

/// Project one row's runs onto contiguous [`CellSpan`]s, MERGING neighbouring
/// runs that resolved to the same [`RunStyle`].
///
/// Merging matters for correctness, not just batching: two adjacent quads that
/// meet at a fractional-pixel boundary can leave a visible hairline seam, so the
/// renderer wants the widest possible span. `build_color_runs` already splits a
/// wide glyph into its own run even when its style matches its neighbours, and
/// this re-joins those — a coloured background behind CJK text is one solid
/// block, not one quad per glyph.
pub fn row_cell_spans(runs: &[ColorRun]) -> Vec<CellSpan> {
    let mut out: Vec<CellSpan> = Vec::new();
    let mut col = 0usize;
    for (text, style) in runs {
        let width: usize = text.chars().map(cell_render_width).sum();
        if width == 0 {
            continue;
        }
        match out.last_mut() {
            // Merge with the previous span when the style matches AND the spans
            // actually touch (they always do here, but the check keeps the
            // function total for hand-built inputs).
            Some(prev) if prev.style == *style && prev.col + prev.width == col => {
                prev.width += width;
            }
            _ => out.push(CellSpan {
                col,
                width,
                style: *style,
            }),
        }
        col += width;
    }
    out
}

/// The number of terminal CELLS a glyph occupies: 2 for an East-Asian wide /
/// fullwidth glyph (and wide emoji), 1 otherwise. Mirrors the core VT layer's
/// own `UnicodeWidthChar::width(c).max(1)` cell allocation (`term.rs` `print`),
/// so the renderer's column accounting agrees with how the grid stored the cells.
pub fn cell_render_width(c: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    if c.width().unwrap_or(1) >= 2 {
        2
    } else {
        1
    }
}

/// Copy the visible characters of one display row between cell columns `lo..=hi`
/// (inclusive), SKIPPING the blank continuation spacer the core writes after a
/// wide (width-2) glyph. A cell is that spacer iff its PREVIOUS cell in the row
/// is a width-2 glyph (`term.rs` `print` writes the glyph then one blank cell),
/// so the check is a pure char-width lookback — it works for scrollback rows too
/// (unlike the live-grid-only continuation bitset) and is correct even when the
/// selection starts exactly on a spacer. This mirrors the per-cell renderer's
/// wide-glyph handling so copied text matches what is drawn: no stray space is
/// emitted around a CJK / emoji glyph. Trailing whitespace is trimmed.
fn copy_row_text(row: &[c0pl4nd_core::Cell], lo: usize, hi: usize) -> String {
    let mut line = String::new();
    for c in lo..=hi {
        if c > 0 {
            if let Some(prev) = row.get(c - 1) {
                if cell_render_width(prev.c) >= 2 {
                    continue; // this cell is the wide glyph's spacer
                }
            }
        }
        if let Some(cell) = row.get(c) {
            line.push(cell.c);
        }
    }
    line.trim_end().to_string()
}

/// For one row's [`ColorRun`]s, the painted glyphs as `(char, colour, cell_col)`
/// — each NON-blank glyph paired with the grid CELL column it is painted at.
/// `cell_col` advances by [`cell_render_width`] per char, so a WIDE (width-2)
/// glyph shifts every following glyph by two cells; blank cells are skipped
/// (their background is painted separately) but still advance the column.
///
/// This is the SINGLE SOURCE OF TRUTH for per-cell glyph X positions: the
/// renderer paints each returned glyph at `origin.x + cell_col * cw`, so the
/// layout is font-advance-independent (a wide or fallback glyph can never shift
/// another cell — the failure mode that reverted the per-run approach). Pulling
/// it out as a pure function makes wide-glyph alignment unit-testable WITHOUT a
/// live display.
pub fn row_glyph_cells(runs: &[ColorRun]) -> Vec<(char, RunStyle, usize)> {
    let mut out = Vec::new();
    let mut col = 0usize;
    for (text, style) in runs {
        for c in text.chars() {
            if c != ' ' {
                out.push((c, *style, col));
            }
            col += cell_render_width(c);
        }
    }
    out
}

/// Build one visible row's [`ColorRun`]s from its cells, breaking a run on a
/// colour change AND at every WIDE (width-2) glyph, and SKIPPING the blank
/// continuation spacer the core writes after a wide glyph (`term.rs` `print`).
/// A wide glyph becomes its own single-char run. The renderer paints each cell
/// at its exact cell-column, so this keeps the run text free of the spacer's
/// double-counted width. Pure (colour resolution injected) so the wide-split +
/// spacer-skip is unit-testable without a live terminal.
///
/// KNOWN LIMITATION — combining marks: only each cell's BASE char (`cell.c`) is
/// emitted; the core stores combining marks (e.g. the U+0301 in a decomposed
/// `é`) in a parallel side-table (`Grid::grapheme_at`), and they are dropped
/// here. This is consistent with `Grid::to_text` (copy drops them too), so the
/// rendered and copied text agree. Compositing them would require this pipeline
/// to carry the full grapheme cluster through [`ColorRun`] AND egui's text
/// layout to shape base+mark into one glyph — egui does no HarfBuzz-grade
/// shaping, so a naive plumb-through can render a floating accent rather than a
/// combined glyph. That visual outcome cannot be verified in a headless build,
/// so it is deliberately NOT done blind here (see the `é` conformance case in
/// `crates/core/tests/vt_conformance.rs`, which pins the core's side-table model).
fn build_color_runs(
    cells: &[c0pl4nd_core::Cell],
    cols: usize,
    default_cell: &c0pl4nd_core::Cell,
    mut style_of: impl FnMut(&c0pl4nd_core::Cell) -> RunStyle,
) -> Vec<ColorRun> {
    let mut runs: Vec<ColorRun> = Vec::new();
    let mut run = String::new();
    let mut run_style: Option<RunStyle> = None;
    let mut col = 0;
    while col < cols {
        let cell = cells.get(col).unwrap_or(default_cell);
        let style = style_of(cell);
        if cell_render_width(cell.c) >= 2 {
            if let Some(pc) = run_style.take() {
                runs.push((std::mem::take(&mut run), pc));
            }
            runs.push((cell.c.to_string(), style));
            col += 2; // skip the trailing continuation spacer cell
            continue;
        }
        // Break the run on ANY rendition change — background included. Breaking
        // only on foreground (the pre-background behaviour) would merge a
        // highlighted cell into its unhighlighted neighbour's run and lose the
        // background boundary entirely.
        if run_style != Some(style) {
            if let Some(pc) = run_style.take() {
                runs.push((std::mem::take(&mut run), pc));
            }
            run_style = Some(style);
        }
        run.push(cell.c);
        col += 1;
    }
    if let Some(pc) = run_style {
        runs.push((run, pc));
    }
    runs
}

/// Damage-gated cache of the visible grid as per-row colour runs (the output of
/// [`PaneTerm::grid_rows`]). The renderer calls `grid_rows` every frame; this
/// cache lets an UNCHANGED pane (the blinking-cursor / idle-effect case) skip the
/// whole snapshot-and-group pass and reuse the previous frame's rows.
///
/// Validity is keyed on everything that can change the *rendered* runs without
/// going through a grid cell-write: the grid's own per-row damage bits
/// ([`Grid::is_damaged`]), the scrollback `view_offset`, the grid dimensions, the
/// DECSCNM reverse-screen flag (swaps default fg/bg without touching cells), and
/// the active theme (set out-of-band via [`PaneTerm::set_theme`], which clears
/// this cache). Anything that edits a cell already marks its row dirty, so a
/// cell change forces a rebuild.
struct RowSpanCache {
    rows: Rc<Vec<Vec<ColorRun>>>,
    view_offset: usize,
    cols: usize,
    grid_rows: usize,
    reverse: bool,
    valid: bool,
}

impl Default for RowSpanCache {
    fn default() -> Self {
        Self {
            rows: Rc::new(Vec::new()),
            view_offset: 0,
            cols: 0,
            grid_rows: 0,
            reverse: false,
            valid: false,
        }
    }
}

/// The cell metrics (in physical pixels) used to map a pane's pixel rect onto a
/// terminal `(cols, rows)` grid. Derived from the glyphon font metrics; a
/// monospace cell is `advance_w` wide and `line_h` tall.
#[derive(Debug, Clone, Copy)]
pub struct CellMetrics {
    /// Horizontal advance of one monospace cell, in physical pixels.
    pub advance_w: f32,
    /// Line height of one cell, in physical pixels.
    pub line_h: f32,
}

impl CellMetrics {
    /// Compute `(cols, rows)` for a pane of `px_w` × `px_h` physical pixels,
    /// each clamped to at least 1 (a degenerate zero-size pane still gets a
    /// 1×1 grid rather than a panic / zero-division).
    pub fn cols_rows(&self, px_w: f32, px_h: f32) -> (u16, u16) {
        let cols = (px_w / self.advance_w.max(1.0)).floor().max(1.0) as u16;
        let rows = (px_h / self.line_h.max(1.0)).floor().max(1.0) as u16;
        (cols, rows)
    }
}

/// OS-level side effects drained from one pane's terminal in a single frame,
/// returned by [`PaneTerm::pump_host_effects`] for the app shell to apply
/// globally. PTY query replies (DA / DSR / cursor-position / OSC color-query /
/// focus reports) are written straight back to the originating pane's own PTY
/// inside `pump_host_effects`, so they are NOT surfaced here — only the effects
/// that touch host-global state (the OS clipboard, the live theme, the taskbar)
/// are returned.
#[derive(Default)]
pub struct HostEffects {
    /// OSC 52 clipboard-write payloads (plaintext; the zeroizing buffer was
    /// drained into these owned `String`s). The app writes them to the OS
    /// clipboard.
    pub clipboard_writes: Vec<String>,
    /// OSC 4 / 10 / 11 / 12 / 104 color set/reset requests. The app applies each
    /// to its live theme and repaints.
    pub color_sets: Vec<ColorSet>,
    /// `true` if a desktop notification (OSC 9 / OSC 777) fired this drain. The
    /// app requests user attention (taskbar flash) when the window is unfocused.
    /// The notification TEXT is deliberately not surfaced — it can carry 2FA
    /// codes / secret URLs and must never be logged (privacy).
    pub notified: bool,
    /// `OSC 9 ; 4` taskbar-progress reports (C26) drained this frame, in emit
    /// order. The app drives the Windows taskbar-button progress segment from
    /// the LATEST one (`super::taskbar`). Empty in the common case; a build tool
    /// streaming progress fills it.
    pub progress: Vec<Progress>,
}

/// Write bytes to a pane's PTY, logging a failure ONLY when the session is
/// still live. A dead/closing session legitimately drops the write (its child
/// already exited — silent is correct there); a failure on a LIVE master (a
/// momentarily-full pipe, EINTR) used to silently drop a keystroke or a query
/// reply the running program may be blocking on, with no diagnostic. The
/// `is_alive()` check is evaluated only on the error path, so the happy path is
/// unchanged. `write_input` borrows `&mut` only for the call; the liveness probe
/// borrows `&` afterwards.
fn write_pty_logged(session: &mut Session, bytes: &[u8]) {
    if let Err(e) = session.write_input(bytes) {
        if session.is_alive() {
            tracing::warn!("PTY write failed on a live session: {e}");
        }
    }
}

/// Lightweight placement metadata for one inline image (Sixel / Kitty graphics)
/// that is VISIBLE in the current display window — pixel data is NOT included, so
/// the per-frame metadata sweep never clones image bytes. `(line, col, width,
/// height)` is the texture-cache key: it is stable across a scrollback-view
/// scroll (the absolute `line` only shifts on history eviction), so a visible
/// image's GPU texture is uploaded once, not every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleImageMeta {
    /// Absolute grid line of the image's top-left anchor (cache key).
    pub line: usize,
    /// Grid column of the image's top-left anchor.
    pub col: usize,
    /// Image width in pixels.
    pub width: usize,
    /// Image height in pixels.
    pub height: usize,
    /// Display row where the image's TOP sits, as an offset from the window top
    /// (`line − window_start`). May be NEGATIVE when the anchor has scrolled
    /// above the window top — a tall image is still partly visible, and the
    /// painter's `painter_at(rect)` clips the off-top portion.
    pub display_row: i32,
}

/// The signed display row for an image anchored at absolute `line`, given the
/// `window_start` absolute line (top of the visible window = scrollback_len −
/// view_offset) and the visible `rows`. Returns the offset of the image's TOP
/// from the window top (`line − window_start`), which may be NEGATIVE when the
/// anchor has scrolled above the top: a multi-row image whose top has left the
/// viewport must still render its visible remainder (the painter clips the
/// off-top portion). Returns `None` only when the anchor is at/below the window
/// bottom (`row >= rows`) — the image starts off the bottom, so nothing shows.
/// Pure so the bounds logic is unit-tested without a live terminal.
fn image_display_row(line: usize, window_start: usize, rows: usize) -> Option<i32> {
    let row = line as i64 - window_start as i64;
    if row >= rows as i64 {
        return None;
    }
    Some(row as i32)
}

/// Whether the OSC 133 command marks say a command is STILL RUNNING.
///
/// The shell brackets each command with `;C` (output starts) and `;D` (command
/// finished). The marks arrive in order and are only ever evicted from the FRONT
/// under the core's flood cap, so the ordering holds: a command is in flight
/// exactly when the LAST mark is a `;C` that no `;D` has closed.
///
/// **Limit — this reports what the SHELL reports.** A shell with no OSC 133
/// prompt integration emits no marks at all, so an empty slice is `false`: we
/// genuinely cannot tell, and a close-confirmation that fired on every close for
/// an unintegrated shell would be worse than none. This is deliberately the
/// conservative direction (miss rather than nag); it is the same constraint
/// [`PaneTerm::last_command_exit_code`] already documents for the status bar.
/// Pure, so the rule is exhaustively unit-tested with no shell and no PTY.
#[must_use]
fn command_in_flight(marks: &[c0pl4nd_core::term::osc::CommandMark]) -> bool {
    use c0pl4nd_core::term::osc::CommandMarkKind;
    matches!(
        marks.last().map(|m| m.kind),
        Some(CommandMarkKind::OutputStart)
    )
}

/// One pane's live terminal. Owns the PTY session and the rendering inputs the
/// egui shell needs each frame.
pub struct PaneTerm {
    /// The live PTY-backed session (None only if the shell failed to spawn —
    /// see [`PaneTerm::error`]).
    session: Option<Session>,
    /// A human-readable spawn error, shown as a fallback label when `session`
    /// is `None`. Never panics on a failed spawn — the pane degrades to a label.
    error: Option<String>,
    /// The active colour theme (glyph colours come from here, NOT egui Visuals).
    theme: Theme,
    /// The last `(cols, rows)` the PTY was sized to — used to debounce resizes.
    size: (u16, u16),
    /// Whether this pane's [`Session`] has had its UI-wake callback wired yet.
    /// The app wires it once (it needs an `egui::Context`, only available from a
    /// frame); the flag makes [`PaneTerm::wire_wake`] idempotent so the per-frame
    /// sweep does not re-register a fresh closure every frame.
    wake_wired: bool,
    /// Damage-gated cache of the last `grid_rows()` snapshot. Interior-mutable so
    /// the per-frame `grid_rows(&self)` read path can refresh it without forcing a
    /// `&mut` on every chrome accessor. `PaneTerm` lives on the UI thread only
    /// (the cross-thread boundary is the terminal's own `Mutex`), so a `RefCell`
    /// is sound here.
    span_cache: RefCell<RowSpanCache>,
}

impl PaneTerm {
    /// Spawn a pane backed by the platform default shell at `(cols, rows)`.
    /// Never panics: a spawn failure yields a pane whose [`PaneTerm::error`] is
    /// set and whose body renders an error label instead of a grid.
    ///
    /// Uses the canonical [`c0pl4nd_core::pty::DEFAULT_TERM`]; the shipping app
    /// goes through [`PaneTerm::spawn_with_term`] so the user's `term` config
    /// override is honoured.
    ///
    /// `allow(dead_code)`: the shipping `c0pl4nd-egui` binary spawns via
    /// [`PaneTerm::spawn_with_term`] (so the `term` config override applies); the
    /// no-override `spawn` is retained for the deterministic interaction tests.
    #[allow(dead_code)]
    pub fn spawn(theme: Theme, cols: u16, rows: u16) -> Self {
        Self::spawn_with_term(theme, cols, rows, None)
    }

    /// Like [`PaneTerm::spawn`] but with an explicit `TERM` override (the
    /// config-driven `term` key). `term = None` / `Some("")` uses the canonical
    /// [`c0pl4nd_core::pty::DEFAULT_TERM`]. This is the path the shipping
    /// `c0pl4nd-egui` binary takes so the child PTY's `TERM` matches the user's
    /// configuration.
    pub fn spawn_with_term(theme: Theme, cols: u16, rows: u16, term: Option<&str>) -> Self {
        match Session::spawn_shell_with_term(None, rows, cols, term) {
            Ok(session) => {
                // P0.3: enroll the shell in the kill-on-close job so it (and its
                // descendants) cannot outlive the app, even on a hard exit/crash.
                Self::assign_child_to_job(&session);
                Self {
                    session: Some(session),
                    error: None,
                    theme,
                    size: (cols, rows),
                    wake_wired: false,
                    span_cache: RefCell::new(RowSpanCache::default()),
                }
            }
            Err(e) => Self {
                session: None,
                error: Some(crate::user_error::shell_spawn_failed(e)),
                theme,
                size: (cols, rows),
                wake_wired: false,
                span_cache: RefCell::new(RowSpanCache::default()),
            },
        }
    }

    /// Like [`PaneTerm::spawn_with_term`] but starts the shell in an explicit
    /// working directory — the path used by layout-restore so a persisted pane
    /// re-opens where it was. A `cwd` that no longer names an existing directory
    /// falls back to the home dir inside the core spawn (a stale restored cwd is
    /// not an error), and a failed spawn degrades to an error label, never a
    /// panic — identical to [`spawn_with_term`](Self::spawn_with_term).
    pub fn spawn_in_with_term(
        theme: Theme,
        cols: u16,
        rows: u16,
        term: Option<&str>,
        cwd: Option<&str>,
    ) -> Self {
        match Session::spawn_shell_in_with_term(None, rows, cols, cwd, term) {
            Ok(session) => {
                // P0.3: enroll the shell in the kill-on-close job so it (and its
                // descendants) cannot outlive the app, even on a hard exit/crash.
                Self::assign_child_to_job(&session);
                Self {
                    session: Some(session),
                    error: None,
                    theme,
                    size: (cols, rows),
                    wake_wired: false,
                    span_cache: RefCell::new(RowSpanCache::default()),
                }
            }
            Err(e) => Self {
                session: None,
                error: Some(crate::user_error::shell_spawn_failed(e)),
                theme,
                size: (cols, rows),
                wake_wired: false,
                span_cache: RefCell::new(RowSpanCache::default()),
            },
        }
    }

    /// The pane's current working directory (OSC 7), if the shell reported one.
    /// Read under the terminal lock; `None` for a failed-spawn pane, a poisoned
    /// lock, or a shell that has not emitted OSC 7. Used by layout-persistence to
    /// snapshot where each pane was so a restored pane re-opens in the same dir.
    pub fn cwd(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let term_arc = session.terminal();
        let term = term_arc.lock().ok()?;
        term.cwd().map(str::to_string)
    }

    /// Spawn a pane running an explicit program (deterministic tests). Mirrors
    /// [`Session::spawn_program`]; same no-panic degradation on failure.
    ///
    /// `allow(dead_code)`: consumed by the `egui_terminal` interaction-test
    /// binary (via `#[path]`), not by the shipping `c0pl4nd-egui` binary — the
    /// same deliberate test-facing-public-API pattern the chrome accessors use.
    #[allow(dead_code)]
    pub fn spawn_program(theme: Theme, program: &str, args: &[&str], cols: u16, rows: u16) -> Self {
        Self::spawn_program_in(theme, program, args, cols, rows, None)
    }

    /// Like [`spawn_program`](Self::spawn_program) but starts the program in an
    /// explicit working directory — the named-shell-profile counterpart to
    /// [`spawn_in_with_term`](Self::spawn_in_with_term).
    ///
    /// Why it exists: `spawn_term_in` in `egui_app/mod.rs` picks ONE of two
    /// branches — the default profile (program `None`) takes
    /// `spawn_in_with_term` and carries the cwd, while a NAMED profile took
    /// `spawn_program`, which had no cwd parameter at all. So reopening a closed
    /// pane (or restoring a layout) under a named profile silently landed in the
    /// default directory. One missing parameter, both paths; this is the variant
    /// that closes it.
    ///
    /// `cwd = None` is the shell's own default directory. A `cwd` that no longer
    /// names an existing directory falls back to home inside the core spawn (a
    /// stale restored cwd is not an error), and a failed spawn degrades to an
    /// error label, never a panic — identical to
    /// [`spawn_program`](Self::spawn_program), which now delegates here so the
    /// two can never drift apart.
    #[allow(dead_code)]
    pub fn spawn_program_in(
        theme: Theme,
        program: &str,
        args: &[&str],
        cols: u16,
        rows: u16,
        cwd: Option<&str>,
    ) -> Self {
        match Session::spawn_program_in(program, args, rows, cols, cwd) {
            Ok(session) => {
                // P0.3: enroll the shell in the kill-on-close job so it (and its
                // descendants) cannot outlive the app, even on a hard exit/crash.
                Self::assign_child_to_job(&session);
                Self {
                    session: Some(session),
                    error: None,
                    theme,
                    size: (cols, rows),
                    wake_wired: false,
                    span_cache: RefCell::new(RowSpanCache::default()),
                }
            }
            Err(e) => Self {
                session: None,
                error: Some(crate::user_error::program_spawn_failed(e)),
                theme,
                size: (cols, rows),
                wake_wired: false,
                span_cache: RefCell::new(RowSpanCache::default()),
            },
        }
    }

    /// The spawn error, if the shell could not start. `None` for a live pane.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the backing shell is still running. `false` for a failed-spawn
    /// pane or one whose child has exited. (Test/observation API — see
    /// `spawn_program` note.)
    #[allow(dead_code)]
    pub fn is_alive(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_alive)
    }

    /// Assign a freshly-spawned session's shell to the process-wide
    /// `KILL_ON_JOB_CLOSE` job object (P0.3 no-orphan guarantee). Best-effort +
    /// a no-op until `job_object::init()` has run (so headless tests, which never
    /// init the job, do nothing). Called from every spawn path.
    fn assign_child_to_job(session: &Session) {
        if let Some(pid) = session.child_pid() {
            super::job_object::assign(pid);
        }
    }

    /// Kill this pane's shell WITHOUT dropping the session — a fast, non-blocking
    /// `TerminateProcess`. The app's fast-close path calls this on EVERY pane
    /// before `std::process::exit(0)` so all shells terminate in parallel and the
    /// per-pane blocking `ClosePseudoConsole` (the sequential close latency) is
    /// skipped entirely. No-op for a failed-spawn pane.
    pub fn kill_child(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.kill_child();
        }
    }

    /// The current PTY grid size `(cols, rows)`. (Test/observation API.)
    #[allow(dead_code)]
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// Wire this pane's UI-wake callback exactly once. The reader thread invokes
    /// `wake` after each chunk of PTY output so the render loop can sleep when
    /// idle and repaint the instant output arrives. `make_wake` is a thunk so the
    /// (cheap) callback `Arc` is only built on the first call for a live pane —
    /// the per-frame sweep that calls this is a no-op after the first wiring (or
    /// for a failed-spawn pane with no session).
    pub fn wire_wake(&mut self, make_wake: impl FnOnce() -> c0pl4nd_core::WakeFn) {
        if self.wake_wired {
            return;
        }
        if let Some(session) = &self.session {
            session.set_wake_callback(make_wake());
            self.wake_wired = true;
        }
    }

    /// Whether the terminal currently requests DECCKM application-cursor mode
    /// (`?1`) — drives whether arrows encode SS3 vs CSI. Locks the terminal
    /// briefly; defaults to `false` if the lock is poisoned.
    fn app_cursor(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        session
            .terminal()
            .lock()
            .map(|t| t.application_cursor_keys())
            .unwrap_or(false)
    }

    /// The terminal's active mouse-reporting mode (`?1000` / `?1002` / `?1003`),
    /// as requested by the focused application (vim/tmux/htop grab the mouse this
    /// way). Locks the terminal briefly, mirroring
    /// [`app_cursor`](Self::app_cursor), and returns [`MouseMode::Off`] when there
    /// is no live session or the lock is poisoned. Cheap and non-panicking, so it
    /// is safe to call once per frame from the status-bar paint.
    pub fn mouse_mode(&self) -> MouseMode {
        let Some(session) = &self.session else {
            return MouseMode::Off;
        };
        session
            .terminal()
            .lock()
            .map(|t| t.mouse_mode())
            .unwrap_or(MouseMode::Off)
    }

    /// Drain this pane's terminal-owed effects ONCE for the current frame.
    ///
    /// Two distinct destinations:
    /// - **PTY query replies** ([`Terminal::take_pty_response`]: device
    ///   attributes, cursor-position reports, OSC 4/10/11/12 color *queries*,
    ///   focus reports) are written STRAIGHT BACK to THIS pane's PTY — they are
    ///   answers this terminal owes the program running in it.
    /// - **Host-global effects** (OSC 52 clipboard writes, OSC 4/10/11/12/104
    ///   color *sets*, OSC 9/777 notifications, `OSC 9 ; 4` taskbar progress)
    ///   are returned in [`HostEffects`] for the app shell to apply once.
    ///
    /// The `OSC 9 ; 4` taskbar-progress reports are surfaced in
    /// [`HostEffects::progress`] and consumed by the app's taskbar wiring
    /// (`super::taskbar`) — previously this queue was drained-and-discarded
    /// (bounded-growth guard only, no UI). Without this whole drain the egui
    /// shell silently dropped every reply AND leaked the unread queues — the
    /// legacy winit shell drained them but the egui rewrite never ported the
    /// wiring.
    ///
    /// No-op (empty effects) for a failed-spawn pane or a poisoned terminal lock.
    pub fn pump_host_effects(&mut self) -> HostEffects {
        let mut out = HostEffects::default();
        let Some(session) = self.session.as_mut() else {
            return out;
        };
        // `terminal()` clones the Arc, so the immutable borrow of `session` ends
        // immediately — the `write_input(&mut self)` below is then free to take
        // the mutable borrow. Compute the reply bytes under the lock, drop the
        // lock, THEN write.
        let term_arc = session.terminal();
        let response = {
            let Ok(mut term) = term_arc.lock() else {
                return out;
            };
            // OSC 52 clipboard READ: answer any request the core PARKED for us.
            // The core only ever parks one when the user opted in
            // (`clipboard_read_allow`, default OFF) — a denied query was already
            // refused inside the parser with an empty-payload reply and never
            // reaches here, so this loop cannot leak a clipboard the user did
            // not open. Runs BEFORE `take_pty_response` so the answer ships in
            // the same drain rather than a frame later. A failed read yields
            // `None` → an empty reply, which still releases a program blocking
            // on the answer.
            for req in term.take_clipboard_reads() {
                let text = crate::clipboard_read::read_selection(req.selection);
                let text = text.as_ref().map(|t| t.as_str()).unwrap_or("");
                term.respond_clipboard_read(req.selection, text);
            }
            let response = term.take_pty_response();
            for mut cw in term.take_clipboard_writes() {
                // `ClipboardWrite` zeroizes its buffer on drop; take the text out
                // (leaving an empty buffer to drop) rather than moving the field
                // out of the Drop type.
                out.clipboard_writes.push(std::mem::take(&mut cw.text));
            }
            out.color_sets = term.take_color_sets();
            if !term.take_notifications().is_empty() {
                out.notified = true;
            }
            // OSC 9 ; 4 taskbar progress → surfaced for the app's taskbar wiring
            // (`super::taskbar`) to drive the Windows taskbar-button segment.
            // Draining here also keeps the queue from growing unbounded while a
            // build tool streams progress.
            out.progress = term.take_progress();
            response
        };
        if !response.is_empty() {
            write_pty_logged(session, &response);
        }
        out
    }

    /// Report a mouse event to the program running in this pane, IFF that
    /// program has grabbed the mouse (`?1000` / `?1002` / `?1003`, encoded per
    /// the negotiated `?1006`/`?1015` mode). Maps to the SHARED core encoder
    /// ([`Terminal::encode_mouse`]) so the wire bytes match the winit shell
    /// exactly. `col`/`row` are 1-based grid coordinates.
    ///
    /// Returns `true` when the event was encoded and written to the PTY — the
    /// caller then skips the local selection/scroll handling so the program owns
    /// the gesture. Returns `false` (a no-op) when mouse reporting is Off, the
    /// event encodes to nothing for the active mode, the lock is poisoned, or
    /// the pane has no live session.
    pub fn report_mouse(
        &mut self,
        button: MouseButton,
        mods: MouseModifiers,
        col: usize,
        row: usize,
        kind: MouseEventKind,
    ) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let term_arc = session.terminal();
        let bytes = {
            let Ok(term) = term_arc.lock() else {
                return false;
            };
            if term.mouse_mode() == MouseMode::Off {
                return false;
            }
            term.encode_mouse(button, mods, col, row, kind)
        };
        match bytes {
            Some(b) => {
                write_pty_logged(session, &b);
                true
            }
            None => false,
        }
    }

    /// Scroll the scrollback VIEW by `lines` (positive = back into older
    /// history, negative = forward toward the live bottom). Used for local
    /// mouse-wheel scrollback when the running program has NOT grabbed the mouse
    /// ([`MouseMode::Off`]). No-op on a poisoned lock or a dead pane.
    pub fn scroll_view(&mut self, lines: i32) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if let Ok(mut term) = session.terminal().lock() {
            if lines > 0 {
                term.scroll_up_view(lines as usize);
            } else if lines < 0 {
                term.scroll_down_view((-lines) as usize);
            }
        }
    }

    /// Report a window focus-in (`focused = true`) / focus-out to the running
    /// program IFF it armed focus reporting (DEC `?1004`). The reply bytes are
    /// queued into the terminal's PTY-response channel and delivered by
    /// [`pump_host_effects`](Self::pump_host_effects). No-op for a dead pane, a
    /// poisoned lock, or a program that did not arm `?1004` (so a focus change
    /// never leaks stray bytes to a shell that did not ask for them). Without
    /// this the egui shell never told vim/tmux about focus changes (FocusGained
    /// / FocusLost), unlike the legacy shell.
    pub fn report_focus(&mut self, focused: bool) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if let Ok(mut term) = session.terminal().lock() {
            if term.focus_reporting() {
                term.focus_report(focused);
            }
        }
    }

    /// Scroll the scrollback view to the previous (`forward = false`) or next
    /// (`forward = true`) shell-prompt mark (OSC 133 ; A), relative to the line
    /// at the top of the viewport. No-op when no mark lies in the requested
    /// direction (or no live pane). Ported from the legacy shell's
    /// `jump_to_prompt`; marks are captured for free by the OSC-133 handler.
    /// Returns `true` when the view moved (the caller repaints).
    pub fn jump_to_prompt(&mut self, forward: bool) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let term_arc = session.terminal();
        let Ok(mut term) = term_arc.lock() else {
            return false;
        };
        let scrollback = term.scrollback_len();
        // Absolute line currently at the top of the visible window.
        let top = scrollback.saturating_sub(term.view_offset());
        let target = {
            let marks = term.prompt_marks();
            if forward {
                marks.iter().copied().filter(|&m| m > top).min()
            } else {
                marks.iter().copied().filter(|&m| m < top).max()
            }
        };
        if let Some(line) = target {
            term.set_view_offset(scrollback.saturating_sub(line));
            true
        } else {
            false
        }
    }

    /// The current scroll-up offset (0 = following live output). Returns 0 for a
    /// dead pane or a poisoned lock (the live-output default).
    pub fn view_offset(&self) -> usize {
        let Some(session) = self.session.as_ref() else {
            return 0;
        };
        session
            .terminal()
            .lock()
            .map(|t| t.view_offset())
            .unwrap_or(0)
    }

    /// The number of lines currently retained in scrollback history. Returns 0
    /// for a dead pane / poisoned lock. Exposed for the scroll-edge test to
    /// confirm a non-empty history was built before asserting the chord.
    #[allow(dead_code)]
    pub fn scrollback_len(&self) -> usize {
        let Some(session) = self.session.as_ref() else {
            return 0;
        };
        session
            .terminal()
            .lock()
            .map(|t| t.scrollback_len())
            .unwrap_or(0)
    }

    /// Jump the scrollback view to the oldest retained line (the top edge).
    /// Returns `true` when the view moved (so the caller repaints). No-op for a
    /// dead pane / poisoned lock.
    pub fn scroll_to_top(&mut self) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        match session.terminal().lock() {
            Ok(mut term) => term.scroll_to_top(),
            Err(_) => false,
        }
    }

    /// Snap the scrollback view back to following live output (the bottom edge).
    /// No-op for a dead pane / poisoned lock.
    pub fn scroll_to_bottom(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if let Ok(mut term) = session.terminal().lock() {
            term.scroll_to_bottom();
        }
    }

    /// Clear this pane's scrollback history (everything scrolled off the top)
    /// while keeping the live screen, snapping the view back to live output. The
    /// direct analogue of feeding `ESC [ 3 J`; used by the right-click "Clear
    /// scrollback" menu item and the Ctrl+Shift+K chord. No-op for a dead pane /
    /// poisoned lock.
    pub fn clear_scrollback(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if let Ok(mut term) = session.terminal().lock() {
            term.clear_scrollback();
        }
    }

    /// This pane's entire buffer (scrollback + live screen) as copy-ready text,
    /// or `None` when it holds no non-blank text. Placed on the clipboard by the
    /// right-click "Copy all" menu item and the Ctrl+Shift+A chord — the
    /// whole-buffer analogue of the display-window-bound mouse-selection copy.
    /// `None` for a dead pane / poisoned lock.
    pub fn buffer_text(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        session.terminal().lock().ok()?.buffer_text()
    }

    /// Feed raw bytes straight into the emulator (bypassing the PTY) so a
    /// headless test can build deterministic scrollback. Test-only — the
    /// shipping binary feeds the emulator exclusively via the PTY pump, so this
    /// is dead code there (never called).
    #[allow(dead_code)]
    pub fn test_advance(&mut self, bytes: &[u8]) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if let Ok(mut term) = session.terminal().lock() {
            term.advance(bytes);
        }
    }

    /// Apply the configured scrollback line cap to this pane's live terminal.
    /// No-op for a dead pane / poisoned lock. The app calls this when the
    /// `scrollback_lines` config differs from what was last applied, so the
    /// setting actually takes effect (it was previously persisted but ignored).
    pub fn set_max_scrollback(&self, max_scrollback: usize) {
        if let Some(session) = self.session.as_ref() {
            if let Ok(mut term) = session.terminal().lock() {
                term.set_max_scrollback(max_scrollback);
            }
        }
    }

    /// Apply the `clipboard_read_allow` setting to this pane's live terminal —
    /// whether a program inside it may READ the system clipboard via
    /// `OSC 52 ; c ; ?`. DEFAULT-DENY; without this call the terminal keeps its
    /// own default (denied), so a wiring failure fails closed.
    ///
    /// Applied every frame like the scrollback cap, so flipping the setting takes
    /// effect immediately — including turning it back OFF, which also refuses any
    /// request still in flight rather than stranding the program that made it.
    /// No-op for a dead pane / poisoned lock.
    pub fn set_clipboard_read_allowed(&self, allowed: bool) {
        if let Some(session) = self.session.as_ref() {
            if let Ok(mut term) = session.terminal().lock() {
                if term.clipboard_read_enabled() != allowed {
                    term.set_clipboard_read_enabled(allowed);
                }
            }
        }
    }

    /// The ABSOLUTE line at the top of the visible window (`scrollback_len −
    /// view_offset`). Mouse selections are anchored to absolute lines so they
    /// survive scrolling / jump-to-prompt / new output (the display row a cell
    /// occupies changes as the view moves, but its absolute line does not). The
    /// painter and copy map absolute → current display row via this. Returns
    /// `None` for a dead pane or poisoned lock.
    pub fn window_start(&self) -> Option<usize> {
        let session = self.session.as_ref()?;
        let term_arc = session.terminal();
        let term = term_arc.lock().ok()?;
        Some(term.scrollback_len().saturating_sub(term.view_offset()))
    }

    /// Extract the text covered by a mouse selection between two 0-based
    /// `(display-row, column)` points (in either order) over the CURRENT display
    /// grid. Each selected row is collected left→right, trailing whitespace
    /// trimmed, and rows joined with `\n` — the conventional terminal copy
    /// shape. Returns `None` for an empty selection or a dead pane. Ported from
    /// the legacy shell's `selection_text` so the two shells copy identically.
    ///
    /// When `block` is `true` the selection is RECTANGULAR: every row is clipped
    /// to the same `[min_col, max_col]` column range (the min/max of the two
    /// endpoints' columns) instead of the line-wise first-row-to-last-row span.
    pub fn selection_text(
        &self,
        a: (usize, usize),
        b: (usize, usize),
        block: bool,
    ) -> Option<String> {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let session = self.session.as_ref()?;
        let rows = session.terminal().lock().ok()?.display_rows();
        let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        // Block mode: a fixed column range applied to every row.
        let block_lo = a.1.min(b.1);
        let block_hi = a.1.max(b.1).min(width.saturating_sub(1));
        let mut out = String::new();
        for r in start.0..=end.0 {
            let Some(row) = rows.get(r) else { break };
            let (lo, hi) = if block {
                (block_lo, block_hi)
            } else {
                let lo = if r == start.0 { start.1 } else { 0 };
                let hi = if r == end.0 {
                    end.1.min(width.saturating_sub(1))
                } else {
                    width.saturating_sub(1)
                };
                (lo, hi)
            };
            out.push_str(&copy_row_text(row, lo, hi));
            if r != end.0 {
                out.push('\n');
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// The current display row `r` as one `char` per grid COLUMN (the wide-glyph
    /// spacer cell is included as its blank char), so a caller can do
    /// column-accurate word-boundary detection that lines up with the grid
    /// coordinates the mouse hit-test produces. Empty for a dead pane, a poisoned
    /// lock, or an out-of-range row.
    pub fn display_row_chars(&self, r: usize) -> Vec<char> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let term_arc = session.terminal();
        let Ok(term) = term_arc.lock() else {
            return Vec::new();
        };
        term.display_rows()
            .get(r)
            .map(|row| row.iter().map(|cell| cell.c).collect())
            .unwrap_or_default()
    }

    /// Placement metadata for every inline image (Sixel / Kitty graphics)
    /// currently VISIBLE in the display window. Computed under the terminal lock
    /// WITHOUT cloning pixel data (the renderer fetches bytes via
    /// [`image_rgba`](Self::image_rgba) only on a texture-cache miss). Empty for
    /// a dead pane, a poisoned lock, or a pane with no on-screen images.
    pub fn visible_image_metas(&self) -> Vec<VisibleImageMeta> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let term_arc = session.terminal();
        let Ok(term) = term_arc.lock() else {
            return Vec::new();
        };
        let images = term.images();
        if images.is_empty() {
            return Vec::new();
        }
        let rows = term.grid().rows();
        // Absolute line at the top of the visible window (mirrors the legacy
        // shell's image draw): scrollback length minus how far we're scrolled up.
        let window_start = term.scrollback_len().saturating_sub(term.view_offset());
        images
            .iter()
            .filter_map(|img| {
                image_display_row(img.line, window_start, rows).map(|display_row| {
                    VisibleImageMeta {
                        line: img.line,
                        col: img.col,
                        width: img.image.width,
                        height: img.image.height,
                        display_row,
                    }
                })
            })
            .collect()
    }

    /// Clone the RGBA pixels of the image anchored at absolute `(line, col)`, if
    /// it is still present. Called ONLY on a texture-cache miss (first appearance
    /// or after a history-eviction line shift), so pixel bytes are copied at most
    /// once per uploaded texture rather than every frame. Returns
    /// `(width, height, rgba)`.
    pub fn image_rgba(&self, line: usize, col: usize) -> Option<(usize, usize, Vec<u8>)> {
        let session = self.session.as_ref()?;
        let term_arc = session.terminal();
        let term = term_arc.lock().ok()?;
        term.images()
            .iter()
            .find(|i| i.line == line && i.col == col)
            .map(|i| (i.image.width, i.image.height, i.image.rgba.clone()))
    }

    /// The terminal's current window title, as set by the running program via
    /// an OSC 0/2 escape (`ESC ] 0 ; <title> BEL`). Locks the terminal briefly,
    /// mirroring [`app_cursor`](Self::app_cursor) / [`mouse_mode`](Self::mouse_mode),
    /// and returns the trimmed title when non-empty. Returns `None` when there is
    /// no live session, the lock is poisoned, or the program has not set a title
    /// yet (empty) — so the tab strip can fall back to its `pane {id}` label.
    pub fn title(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let term = session.terminal();
        let guard = term.lock().ok()?;
        let title = guard.title().trim();
        if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        }
    }

    /// The exit code of the focused program's most recently finished command,
    /// derived from OSC 133 `D` command-end marks (shell prompt integration).
    /// Locks the terminal briefly, mirroring [`title`](Self::title) /
    /// [`mouse_mode`](Self::mouse_mode), and returns the core accessor's
    /// double-`Option`:
    ///
    /// - outer `None` — no live session, the lock is poisoned, or no command
    ///   has finished yet (so the status bar shows NO indicator);
    /// - inner `Option` — the OSC-133-reported exit code (`Some(0)` success,
    ///   `Some(code)` failure), or `None` when the shell reported no code.
    ///
    /// Cheap and non-panicking, so it is safe to call once per frame from the
    /// status-bar paint.
    pub fn last_command_exit_code(&self) -> Option<Option<i32>> {
        let session = self.session.as_ref()?;
        let term = session.terminal();
        let guard = term.lock().ok()?;
        guard.last_command_exit_code()
    }

    /// The ABSOLUTE content lines of this pane's captured OSC 133 `;A`/`;B`
    /// shell-prompt marks — the same set [`jump_to_prompt`](Self::jump_to_prompt)
    /// walks, surfaced so the scrollbar can also draw them as a map of where each
    /// command began. Already in the `history.len() + row` absolute space
    /// `window_start` uses, so no display-row mapping is needed.
    ///
    /// Empty for a dead pane / poisoned lock, and for the common case of a shell
    /// with no prompt integration (which emits no marks at all).
    pub fn prompt_mark_lines(&self) -> Vec<usize> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        session
            .terminal()
            .lock()
            .map(|t| t.prompt_marks().to_vec())
            .unwrap_or_default()
    }

    /// The ABSOLUTE content lines of the OSC 133 `;D` command-end marks whose
    /// shell-reported exit code was NON-ZERO — "a command failed here". The
    /// per-command counterpart to [`last_command_exit_code`](Self::last_command_exit_code),
    /// which only answers for the most recent one.
    ///
    /// A `;D` with no exit code at all is NOT a failure (the shell simply did not
    /// report one — the neutral "done" state), so it is excluded; so are `;C`
    /// output-start marks, which carry no code.
    pub fn failed_command_lines(&self) -> Vec<usize> {
        use c0pl4nd_core::term::osc::CommandMarkKind;
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        session
            .terminal()
            .lock()
            .map(|t| {
                t.command_marks()
                    .iter()
                    .filter(|m| {
                        matches!(
                            m.kind,
                            CommandMarkKind::CommandEnd {
                                exit_code: Some(code)
                            } if code != 0
                        )
                    })
                    .map(|m| m.line)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether this pane has a shell command STILL RUNNING — the signal the
    /// close path needs before it kills every child (`prepare_shutdown` reaps
    /// every PTY child with no prompt today, so an in-flight build/migration
    /// dies silently).
    ///
    /// Derived from the OSC 133 marks the terminal ALREADY captures — no process
    /// handle, no process-tree walk, no extra Win32 FFI. See
    /// [`command_in_flight`] for the rule and its limits.
    ///
    /// Gated on the shell still being alive: a pane whose shell died mid-command
    /// keeps its trailing `;C` mark forever, and reporting that as "running"
    /// would warn about a process that no longer exists. `false` for a
    /// failed-spawn pane, a dead shell, or a poisoned lock (the close path must
    /// never be blocked by an unreadable pane).
    ///
    /// `allow(dead_code)`: the consuming branch is the close path in
    /// `egui_app/mod.rs` (`prepare_shutdown` / `WindowCmd::Close` / the
    /// `close_requested` + Alt+F4 fast-exits), which is outside this change's
    /// file ownership — same deliberate accessor-ahead-of-its-call-site pattern
    /// as [`spawn_program`](Self::spawn_program) and [`is_alive`](Self::is_alive).
    #[allow(dead_code)]
    pub fn has_running_command(&self) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        if !session.is_alive() {
            return false;
        }
        session
            .terminal()
            .lock()
            .map(|t| command_in_flight(t.command_marks()))
            .unwrap_or(false)
    }

    /// Write raw bytes straight to the PTY (used for pasted text). Best-effort:
    /// a closed/dead session silently drops the write rather than panicking.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(session) = &mut self.session {
            write_pty_logged(session, bytes);
        }
    }

    /// Write a clipboard paste to the PTY through the core paste-injection guard
    /// ([`c0pl4nd_core::Terminal::frame_paste`]): an embedded `ESC[201~` is
    /// stripped and the text is bracket-wrapped iff the program enabled `?2004`.
    /// This is the ONLY path egui pastes take — never `write_bytes(raw)` — so a
    /// hostile clipboard payload cannot break out of bracketed paste and inject
    /// commands (pastejacking). The terminal is also scrolled to the bottom so
    /// the paste lands at the prompt.
    pub fn write_paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        let bytes = match session.terminal().lock() {
            Ok(mut term) => {
                term.scroll_to_bottom();
                term.frame_paste(text)
            }
            Err(_) => return,
        };
        write_pty_logged(session, &bytes);
    }

    /// The terminal's current kitty-keyboard-protocol flags (`0` = not
    /// negotiated → legacy encoding). Locks the terminal briefly, mirroring
    /// [`app_cursor`](Self::app_cursor), and returns `0` when there is no live
    /// session or the lock is poisoned.
    fn kitty_flags(&self) -> u8 {
        let Some(session) = &self.session else {
            return 0;
        };
        session
            .terminal()
            .lock()
            .map(|t| t.kitty_keyboard_flags())
            .unwrap_or(0)
    }

    /// Whether the focused program negotiated the kitty REPORT-EVENT-TYPES
    /// flag (bit 2). The egui input loop reads this to decide whether to also
    /// forward key RELEASE / REPEAT events (default: press-only).
    pub fn kitty_reports_event_types(&self) -> bool {
        self.kitty_flags() & 2 != 0
    }

    /// Encode a key PRESS via the SHARED core encoder and write the bytes to the
    /// PTY. Thin wrapper over [`forward_key_event`](Self::forward_key_event) so
    /// existing callers (and tests) that only deal in presses are unchanged.
    pub fn forward_key(&mut self, key: &LogicalKey, mods: KeyModifiers) -> Vec<u8> {
        self.forward_key_event(key, mods, KeyEventKind::Press)
    }

    /// Encode a key event (press / repeat / release) and write the resulting
    /// bytes to the PTY. Returns the bytes written (empty when the key encodes
    /// nothing or the session is dead) so tests can assert exactly what reached
    /// the wire.
    ///
    /// When the running program negotiated the kitty keyboard protocol
    /// (`kitty_flags() != 0`), the CSI-u encoder is tried first and used when it
    /// produces a sequence; otherwise the encoding falls back to the legacy
    /// [`encode_key`] (which only ever encodes presses — a release with no kitty
    /// encoding yields no bytes).
    pub fn forward_key_event(
        &mut self,
        key: &LogicalKey,
        mods: KeyModifiers,
        kind: KeyEventKind,
    ) -> Vec<u8> {
        let flags = self.kitty_flags();
        if flags != 0 {
            if let Some(bytes) = encode_key_kitty(key, mods, flags, kind) {
                self.write_bytes(&bytes);
                return bytes;
            }
        }
        // Legacy fallback. Only presses and repeats produce legacy bytes; a
        // release with no kitty encoding writes nothing.
        if kind == KeyEventKind::Release {
            return Vec::new();
        }
        let app_cursor = self.app_cursor();
        match encode_key(key, app_cursor, mods) {
            Some(bytes) => {
                self.write_bytes(&bytes);
                bytes
            }
            None => Vec::new(),
        }
    }

    /// Resize the PTY + reflow the grid to fit a pane of `px_w` × `px_h`
    /// physical pixels, given the cell metrics. DEBOUNCED: does nothing when the
    /// computed `(cols, rows)` equals the current size, so a sub-pixel drag does
    /// not thrash the PTY. Returns the new `(cols, rows)` when a resize was
    /// applied, `None` when it was a no-op.
    pub fn resize_to_px(
        &mut self,
        px_w: f32,
        px_h: f32,
        metrics: CellMetrics,
    ) -> Option<(u16, u16)> {
        let (cols, rows) = metrics.cols_rows(px_w, px_h);
        self.resize(cols, rows)
    }

    /// Resize the PTY + grid to an explicit `(cols, rows)`. Debounced against
    /// the current size (no-op when unchanged). Returns the new size when
    /// applied. A failed-spawn pane records the size but performs no PTY op.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Option<(u16, u16)> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if (cols, rows) == self.size {
            return None;
        }
        self.size = (cols, rows);
        if let Some(session) = &mut self.session {
            let _ = session.resize(rows, cols);
        }
        Some((cols, rows))
    }

    /// Snapshot the visible grid into per-row colour runs ready for the egui paint
    /// layer. Each row is grouped into runs of consecutive same-colour glyphs
    /// (cheap; one `Attrs` per run, not per glyph). Honours DECSCNM reverse-screen
    /// and SGR inverse via [`Theme::cell_colors`]. Returns `None` only when the
    /// session is dead (no terminal to read).
    ///
    /// **Damage-gated.** The grid tracks per-row damage; when nothing changed
    /// since the last call — no dirty rows, same scrollback `view_offset`, same
    /// dimensions, same reverse-screen flag — this returns the cached rows (an
    /// `Rc` clone, no grid work). That is the idle path: a pane with only a
    /// blinking cursor or a running CRT effect repaints from cache instead of
    /// re-grouping every row every frame (the cursor is an overlay drawn
    /// separately, not part of these runs, so it animates without dirtying the
    /// grid). On a miss it rebuilds, clears the grid's damage bits, and refreshes
    /// the cache. A theme change bypasses the grid (cells are unchanged), so
    /// [`PaneTerm::set_theme`] invalidates this cache explicitly.
    pub fn grid_rows(&self) -> Option<Rc<Vec<Vec<ColorRun>>>> {
        let session = self.session.as_ref()?;
        let term = session.terminal();
        let mut guard = term.lock().ok()?;

        let view_offset = guard.view_offset();
        let reverse = guard.reverse_screen();
        let cols = guard.grid().cols();
        let grid_rows = guard.grid().rows();
        let damaged = guard.grid().is_damaged();

        // Cache hit: nothing that affects the rendered runs has changed.
        {
            let cache = self.span_cache.borrow();
            if cache.valid
                && !damaged
                && cache.view_offset == view_offset
                && cache.cols == cols
                && cache.grid_rows == grid_rows
                && cache.reverse == reverse
            {
                return Some(Rc::clone(&cache.rows));
            }
        }

        // Miss: rebuild the per-row runs from the borrowing visible-rows iterator
        // (no whole-grid clone). History rows shorter than the grid width are
        // padded-on-read — columns at/past `row.len()` are treated as
        // `Cell::default()` so the output matches a width-padded grid without ever
        // materialising a padded `Vec`. Rows are grouped DIRECTLY into the final
        // `Vec<Vec<ColorRun>>` (one inner Vec per visible row) — no flat stream +
        // newline-split round-trip.
        let theme_fg =
            c0pl4nd_core::theme::parse_hex(&self.theme.foreground).unwrap_or((232, 230, 240));
        let theme_bg =
            c0pl4nd_core::theme::parse_hex(&self.theme.background).unwrap_or((18, 18, 18));
        // DECSCNM (`?5`): reverse-video screen swaps the default fg/bg.
        let (default_fg, default_bg) = if reverse {
            (theme_bg, theme_fg)
        } else {
            (theme_fg, theme_bg)
        };
        let default_cell = c0pl4nd_core::Cell::default();
        let mut rows_out: Vec<Vec<ColorRun>> = Vec::with_capacity(grid_rows);
        guard.for_visible_rows(|_, row| {
            let mut runs = build_color_runs(row, cols, &default_cell, |cell| {
                // ONE colour authority: `Theme::cell_colors` resolves fg AND the
                // optional bg (applying SGR inverse). Both halves are kept — the
                // background used to be thrown away here, which is why every
                // highlighted cell (grep matches, ls dir colours, TUI selected
                // rows) rendered as plain text, and why an inverse cell painted
                // its background colour ONTO the unchanged background, i.e.
                // invisibly.
                let (fg, bg) = self.theme.cell_colors(cell, default_fg, default_bg);
                RunStyle {
                    fg,
                    bg,
                    bold: cell.flags.bold,
                    italic: cell.flags.italic,
                    underline: cell.flags.underline_style,
                    // SGR 58/59: resolved through the SAME core colour API (never
                    // re-derived); `None` leaves the decoration in the run's fg.
                    underline_color: cell
                        .underline_color
                        .map(|c| self.theme.resolve_color(c, fg)),
                    strikeout: cell.flags.strikeout,
                    overline: cell.flags.overline,
                }
            });
            // BiDi (F3-2): reorder this row's logical-order runs into VISUAL
            // order for right-to-left scripts (Arabic/Hebrew). The fast path
            // returns the LTR-only row UNCHANGED (zero cost); only a row that
            // actually contains RTL content is reordered. Display-only — the
            // logical grid/cells are untouched (see `super::bidi`).
            if let Some(visual) = super::bidi::reorder_runs_visual(&runs) {
                runs = visual;
            }
            rows_out.push(runs);
        });
        // The renderer has now consumed this frame's damage; clear it so the next
        // frame's `is_damaged()` reflects only writes that arrive after this point.
        guard.clear_damage();
        drop(guard);

        let rc = Rc::new(rows_out);
        *self.span_cache.borrow_mut() = RowSpanCache {
            rows: Rc::clone(&rc),
            view_offset,
            cols,
            grid_rows,
            reverse,
            valid: true,
        };
        Some(rc)
    }

    /// The cursor cell as `(row, col)` (0-based, within the visible grid) when
    /// it should be DRAWN: the session is alive, the cursor is visible (DECTCEM
    /// `?25`), and the view is not scrolled back into history (a terminal hides
    /// the cursor while you scroll up). `None` otherwise — the caller draws no
    /// caret.
    pub fn cursor_cell(&self) -> Option<(usize, usize)> {
        let session = self.session.as_ref()?;
        let term = session.terminal();
        let guard = term.lock().ok()?;
        if !guard.is_cursor_visible() {
            return None;
        }
        guard.cursor_position()
    }

    /// The visible grid as plain text (used by tests to assert PTY output landed
    /// on screen, and as a headless render fallback). `None` for a dead session.
    pub fn grid_text(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let term = session.terminal();
        let guard = term.lock().ok()?;
        Some(guard.grid().to_text())
    }

    /// The active theme's default background as an `(r,g,b)` triple — the colour
    /// the egui pane body clears to behind the glyphon text.
    pub fn background_rgb(&self) -> (u8, u8, u8) {
        c0pl4nd_core::theme::parse_hex(&self.theme.background).unwrap_or((18, 18, 18))
    }

    /// Swap the pane's active colour theme. Existing panes hold their OWN theme
    /// clone (both [`grid_rows`](Self::grid_rows) glyph colours AND
    /// [`background_rgb`](Self::background_rgb) resolve from it), so a theme
    /// change in settings must be propagated here for the live panes to repaint
    /// in the new colours — otherwise the picker appears to do nothing.
    ///
    /// A theme change recolours every cell WITHOUT touching the grid (no cell
    /// write, no damage bit), so the [`grid_rows`](Self::grid_rows) damage cache
    /// would otherwise serve stale-coloured rows. Invalidate it here so the next
    /// frame rebuilds in the new colours.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.span_cache.borrow_mut().valid = false;
    }

    /// Test-only handle to the shared terminal, so a unit test can drive escape
    /// sequences (e.g. `?1000h`) directly into the parser — deterministically,
    /// without depending on the asynchronous PTY reader thread (which would make
    /// the test flaky). Returns `None` for a failed-spawn pane.
    ///
    /// `pub(super)` so the egui-shell wiring tests in `mod_tests.rs` can drive a
    /// pane's parser directly (e.g. the OSC 9;4 -> taskbar pump wire); still
    /// `#[cfg(test)]`, so it exists in no shipping build.
    #[cfg(test)]
    pub(super) fn terminal_for_test(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<c0pl4nd_core::Terminal>>> {
        self.session.as_ref().map(Session::terminal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn void_theme() -> Theme {
        Theme::builtin_void()
    }

    // ---- OSC 133 "a command is still running" (the close-warning signal) ----

    /// Build a mark slice for the pure rule without any terminal at all.
    fn marks(
        kinds: &[c0pl4nd_core::term::osc::CommandMarkKind],
    ) -> Vec<c0pl4nd_core::term::osc::CommandMark> {
        kinds
            .iter()
            .enumerate()
            .map(|(i, k)| c0pl4nd_core::term::osc::CommandMark { kind: *k, line: i })
            .collect()
    }

    #[test]
    fn command_in_flight_is_true_only_for_an_unclosed_output_start() {
        use c0pl4nd_core::term::osc::CommandMarkKind::{CommandEnd, OutputStart};
        let end0 = CommandEnd { exit_code: Some(0) };
        let end1 = CommandEnd { exit_code: Some(1) };
        let end_none = CommandEnd { exit_code: None };

        // No marks: an unintegrated shell reports nothing, so we cannot tell.
        // Deliberately the conservative answer — see `command_in_flight`'s doc.
        assert!(!command_in_flight(&[]), "no marks must not claim a command");
        // A bare `;C` with no closing `;D` is the whole point: RUNNING.
        assert!(command_in_flight(&marks(&[OutputStart])));
        // `;C` then `;D` — the command finished, whatever its exit code.
        assert!(!command_in_flight(&marks(&[OutputStart, end0])));
        assert!(!command_in_flight(&marks(&[OutputStart, end1])));
        assert!(!command_in_flight(&marks(&[OutputStart, end_none])));
        // A finished command followed by a NEW `;C` is running again.
        assert!(command_in_flight(&marks(&[OutputStart, end0, OutputStart])));
        // ...and closing that one settles back to idle.
        assert!(!command_in_flight(&marks(&[
            OutputStart,
            end0,
            OutputStart,
            end1
        ])));
        // Only the LAST mark decides — a long idle history must not read as busy.
        assert!(!command_in_flight(&marks(&[
            OutputStart,
            end0,
            OutputStart,
            end0,
            OutputStart,
            end0
        ])));
        // A lone `;D` (integration switched on mid-session) is idle, not running.
        assert!(!command_in_flight(&marks(&[end0])));
    }

    /// The accessor must read the LIVE terminal, not a snapshot: driving the real
    /// OSC 133 bytes through the parser has to flip it both ways. This is the
    /// wire from "the shell said `;C`" to "the close path would warn".
    #[test]
    fn has_running_command_follows_the_live_osc133_marks() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        // A fresh pane has no marks at all → not running.
        assert!(
            !pane.has_running_command(),
            "a fresh pane must not claim a running command"
        );
        // No shell on this box → the None default above is the assertion.
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        // The shell announces a command's output starts here (`OSC 133 ; C`).
        term.lock().unwrap().advance(b"\x1b]133;C\x07");
        assert!(
            pane.has_running_command(),
            "after ESC]133;C BEL the pane must report a command in flight"
        );
        // ...and the matching `;D` clears it.
        term.lock().unwrap().advance(b"\x1b]133;D;0\x07");
        assert!(
            !pane.has_running_command(),
            "after ESC]133;D the command has finished — no warning"
        );
    }

    /// A failed-spawn pane has no session; the close path must get a plain
    /// `false` rather than a panic or an unwrap on `None`.
    #[test]
    fn has_running_command_is_false_for_a_failed_spawn_pane() {
        let pane =
            PaneTerm::spawn_program(void_theme(), "c0pl4nd-no-such-program-exists", &[], 80, 24);
        assert!(
            pane.error().is_some(),
            "the bogus program must have failed to spawn"
        );
        assert!(
            !pane.has_running_command(),
            "a failed-spawn pane must never block the close path"
        );
    }

    // ---- spawn_program_in: the named-profile cwd that used to be dropped ----

    /// A uniquely-named directory under the OS temp dir. The unique component is
    /// what the assertions match on, so Windows 8.3 short-name mangling of the
    /// PARENT (`RUNNER~1`) can never make a correct spawn look wrong.
    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("c0pl4nd-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the test cwd");
        dir
    }

    /// Poll the pane's visible grid for `needle` until `timeout`. The PTY reader
    /// is a background thread, so the output arrives asynchronously; returns the
    /// last grid seen so a failure message can show what DID land.
    fn wait_for_grid(pane: &PaneTerm, needle: &str, timeout: std::time::Duration) -> String {
        let deadline = std::time::Instant::now() + timeout;
        let mut last = String::new();
        loop {
            last = pane.grid_text().unwrap_or(last);
            if last.contains(needle) {
                return last;
            }
            if std::time::Instant::now() >= deadline {
                return last;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// A program that prints its own working directory, so the assertion is on
    /// where the child ACTUALLY ran — not merely on the argument being stored.
    fn print_cwd_program() -> (&'static str, Vec<&'static str>) {
        #[cfg(windows)]
        {
            ("cmd.exe", vec!["/C", "cd"])
        }
        #[cfg(not(windows))]
        {
            ("/bin/sh", vec!["-c", "pwd"])
        }
    }

    /// THE regression this variant exists for: with a named shell profile the
    /// reopen/restore path had no way to pass a cwd, so the pane came back in the
    /// default directory. The child must print the directory we asked for.
    ///
    /// Asserts the value reached its USE (the spawned process's real cwd), not
    /// that it was stored somewhere — a `spawn_program_in` that accepted `cwd`
    /// and then dropped it on the floor would pass any "a pane appeared" check.
    #[test]
    fn spawn_program_in_starts_the_child_in_the_requested_directory() {
        let dir = unique_dir("spawn-in");
        let unique = dir
            .file_name()
            .and_then(|s| s.to_str())
            .expect("unique component")
            .to_string();
        let (program, args) = print_cwd_program();
        let pane = PaneTerm::spawn_program_in(void_theme(), program, &args, 80, 24, dir.to_str());
        if pane.error().is_some() {
            // No shell available on this host: nothing to observe. Not a pass
            // for the behaviour — just an absent platform.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let grid = wait_for_grid(&pane, &unique, std::time::Duration::from_secs(20));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            grid.contains(&unique),
            "the child must RUN in the requested cwd; wanted {unique:?} in the \
             grid, got:\n{grid}"
        );
    }

    /// The home directory a cwd-less spawn is documented to fall back to
    /// (`pty::dirs_home`). `None` on a host with neither variable set.
    fn home_dir() -> Option<String> {
        #[cfg(windows)]
        let v = std::env::var_os("USERPROFILE");
        #[cfg(not(windows))]
        let v = std::env::var_os("HOME");
        v.and_then(|s| s.into_string().ok())
            .filter(|s| !s.is_empty())
    }

    /// Case-insensitively: does the grid show the child running in `dir`?
    /// Windows reports drive letters and path case inconsistently, so an exact
    /// match would be flaky where the behaviour is correct.
    fn grid_shows_dir(grid: &str, dir: &str) -> bool {
        grid.to_lowercase().contains(&dir.to_lowercase())
    }

    /// The companion direction: `cwd = None` lands in HOME — the fallback
    /// `PtyProcess::spawn_program_in_with_term` applies when no directory is
    /// given. `spawn_program` now delegates to `spawn_program_in(.., None)`, so
    /// this pins that the delegation did not change the old behaviour.
    ///
    /// Asserts POSITIVELY (it landed in home), not merely that it avoided one
    /// nominated directory: a weaker "not in dir X" form passes for a spawn that
    /// landed in any of the thousands of other wrong directories.
    #[test]
    fn spawn_program_without_a_cwd_lands_in_home() {
        let Some(home) = home_dir() else {
            return; // No home on this host: the fallback is unobservable.
        };
        let (program, args) = print_cwd_program();
        let pane = PaneTerm::spawn_program(void_theme(), program, &args, 80, 24);
        if pane.error().is_some() {
            return;
        }
        let grid = wait_for_grid(&pane, &home, std::time::Duration::from_secs(20));
        assert!(
            grid_shows_dir(&grid, &home),
            "a cwd-less spawn must fall back to home ({home:?}); got:\n{grid}"
        );
    }

    /// A `cwd` that no longer exists (a stale restored layout) must degrade to
    /// the home fallback the core spawn provides — never a failed pane, and
    /// never the missing path. Again asserted positively (it IS home) plus the
    /// negative (it is NOT the vanished directory).
    #[test]
    fn spawn_program_in_falls_back_to_home_when_the_cwd_is_gone() {
        let dir = unique_dir("spawn-stale");
        let unique = dir
            .file_name()
            .and_then(|s| s.to_str())
            .expect("unique component")
            .to_string();
        let stale = dir.to_str().expect("utf8 path").to_string();
        // Delete it BEFORE spawning: this is exactly a restored-but-removed dir.
        std::fs::remove_dir_all(&dir).expect("remove the test cwd");
        let (program, args) = print_cwd_program();
        let pane = PaneTerm::spawn_program_in(void_theme(), program, &args, 80, 24, Some(&stale));
        assert!(
            pane.error().is_none(),
            "a stale cwd must not fail the spawn, got: {:?}",
            pane.error()
        );
        let Some(home) = home_dir() else {
            return;
        };
        let grid = wait_for_grid(&pane, &home, std::time::Duration::from_secs(20));
        assert!(
            grid_shows_dir(&grid, &home),
            "a vanished cwd must fall back to home ({home:?}); got:\n{grid}"
        );
        assert!(
            !grid.contains(&unique),
            "the child cannot be running in a directory that does not exist; \
             got:\n{grid}"
        );
    }

    /// SGR 53 (overline) must reach the renderer's decoration pass. `has_decoration`
    /// is the gate that pass consults per span, so an overline-only run MUST report
    /// `true` — otherwise the pass skips it and `\e[53m` stays invisible (the exact
    /// dormancy this wires: the flag was parsed into `CellFlags` but dropped here).
    #[test]
    fn overline_only_run_reports_a_decoration() {
        let mut s = RunStyle::plain((200, 200, 200));
        assert!(!s.has_decoration(), "a plain run has nothing to decorate");
        s.overline = true;
        assert!(
            s.has_decoration(),
            "an overline-only run must be decorated, or the paint pass skips it"
        );
    }

    /// The cell → RunStyle mapping must carry `CellFlags::overline` through, not
    /// drop it. Builds a real overline cell and asserts the run it produces is
    /// overlined — would fail if the build site forgot the field.
    #[test]
    fn cell_overline_flag_reaches_the_run_style() {
        use c0pl4nd_core::Cell;
        let mut cell = Cell {
            c: 'x',
            ..Cell::default()
        };
        cell.flags.overline = true;
        let theme = void_theme();
        let (fg, bg) = theme.cell_colors(&cell, (255, 255, 255), (0, 0, 0));
        // The same construction the render path uses (kept in sync with the
        // `grid_rows` build site).
        let style = RunStyle {
            fg,
            bg,
            bold: cell.flags.bold,
            italic: cell.flags.italic,
            underline: cell.flags.underline_style,
            underline_color: None,
            strikeout: cell.flags.strikeout,
            overline: cell.flags.overline,
        };
        assert!(
            style.overline,
            "the overline flag was dropped building the run"
        );
        assert!(style.has_decoration());
    }

    #[test]
    fn cell_render_width_is_two_for_wide_glyphs() {
        assert_eq!(cell_render_width('a'), 1);
        assert_eq!(cell_render_width('ｱ'), 1); // halfwidth katakana
        assert_eq!(cell_render_width('漢'), 2); // CJK ideograph
        assert_eq!(cell_render_width('Ａ'), 2); // fullwidth Latin
        assert_eq!(cell_render_width('😀'), 2); // wide emoji
    }

    #[test]
    fn build_color_runs_splits_wide_glyphs_and_skips_the_continuation_spacer() {
        use c0pl4nd_core::Cell;
        let c = |ch: char| Cell {
            c: ch,
            ..Cell::default()
        };
        // Grid: 'a', '漢' (wide), ' ' (continuation spacer the core wrote), 'b'.
        let cells = vec![c('a'), c('漢'), c(' '), c('b')];
        let runs = build_color_runs(&cells, 4, &Cell::default(), |_| RunStyle::plain((1, 2, 3)));
        // The wide glyph is its OWN run and the spacer is NOT emitted, so the run
        // column accounting never double-counts the wide glyph's width.
        assert_eq!(
            runs,
            vec![
                ("a".to_string(), RunStyle::plain((1, 2, 3))),
                ("漢".to_string(), RunStyle::plain((1, 2, 3))),
                ("b".to_string(), RunStyle::plain((1, 2, 3))),
            ]
        );
    }

    #[test]
    fn build_color_runs_breaks_runs_on_colour_change() {
        use c0pl4nd_core::Cell;
        let c = |ch: char| Cell {
            c: ch,
            ..Cell::default()
        };
        let cells = vec![c('a'), c('b'), c('c')];
        let runs = build_color_runs(&cells, 3, &Cell::default(), |cell| {
            if cell.c == 'c' {
                RunStyle::plain((0, 255, 0))
            } else {
                RunStyle::plain((255, 0, 0))
            }
        });
        assert_eq!(
            runs,
            vec![
                ("ab".to_string(), RunStyle::plain((255, 0, 0))),
                ("c".to_string(), RunStyle::plain((0, 255, 0)))
            ]
        );
    }

    #[test]
    fn copy_row_text_skips_wide_glyph_spacers() {
        use c0pl4nd_core::Cell;
        let c = |ch: char| Cell {
            c: ch,
            ..Cell::default()
        };
        // Grid layout of "a漢b": 'a', '漢' (wide), ' ' (continuation spacer the
        // core writes after the wide glyph), 'b'. Copying the whole row must
        // yield "a漢b" with NO stray space from the spacer.
        let row = vec![c('a'), c('漢'), c(' '), c('b')];
        assert_eq!(copy_row_text(&row, 0, 3), "a漢b");
        // Selecting only the wide glyph + its spacer copies just the glyph.
        assert_eq!(copy_row_text(&row, 1, 2), "漢");
    }

    #[test]
    fn copy_row_text_skips_spacer_even_when_selection_starts_on_it() {
        use c0pl4nd_core::Cell;
        let c = |ch: char| Cell {
            c: ch,
            ..Cell::default()
        };
        // Selection STARTS on the continuation spacer (index 2) of the wide
        // glyph at index 1. The lookback inspects index 1 (a width-2 glyph) and
        // recognises index 2 as the spacer, so no stray space is copied.
        let row = vec![c('a'), c('漢'), c(' '), c('b')];
        assert_eq!(copy_row_text(&row, 2, 3), "b");
    }

    #[test]
    fn copy_row_text_keeps_real_spaces_and_trims_trailing() {
        use c0pl4nd_core::Cell;
        let c = |ch: char| Cell {
            c: ch,
            ..Cell::default()
        };
        // A genuine space (not preceded by a wide glyph) is kept; trailing
        // whitespace is trimmed.
        let row = vec![c('a'), c(' '), c('b'), c(' '), c(' ')];
        assert_eq!(copy_row_text(&row, 0, 4), "a b");
    }

    #[test]
    fn row_glyph_cells_places_wide_glyphs_at_correct_cells() {
        // Runs as build_color_runs produces them (the wide glyph is its own run).
        // 'a' at cell 0; '漢' (width 2) at cell 1; 'b' at cell 3 — the wide glyph
        // shifts 'b' by two cells. THIS is the property that makes CJK/emoji
        // render cell-accurately; it is verified here without a live display.
        let runs = vec![
            ("a".to_string(), RunStyle::plain((1, 1, 1))),
            ("漢".to_string(), RunStyle::plain((2, 2, 2))),
            ("b".to_string(), RunStyle::plain((3, 3, 3))),
        ];
        assert_eq!(
            row_glyph_cells(&runs),
            vec![
                ('a', RunStyle::plain((1, 1, 1)), 0),
                ('漢', RunStyle::plain((2, 2, 2)), 1),
                ('b', RunStyle::plain((3, 3, 3)), 3),
            ]
        );
    }

    #[test]
    fn row_glyph_cells_skips_blanks_but_still_advances_the_column() {
        // A blank is not painted (background is drawn separately) but still
        // advances the cell column, so 'b' lands at cell 2.
        let runs = vec![("a b".to_string(), RunStyle::plain((9, 9, 9)))];
        assert_eq!(
            row_glyph_cells(&runs),
            vec![
                ('a', RunStyle::plain((9, 9, 9)), 0),
                ('b', RunStyle::plain((9, 9, 9)), 2)
            ]
        );
    }

    #[test]
    fn row_glyph_cells_handles_two_adjacent_wide_glyphs() {
        let runs = vec![
            ("漢".to_string(), RunStyle::plain((1, 1, 1))),
            ("字".to_string(), RunStyle::plain((2, 2, 2))),
            ("x".to_string(), RunStyle::plain((3, 3, 3))),
        ];
        // 漢@0, 字@2 (after the first wide glyph), x@4 (after the second).
        assert_eq!(
            row_glyph_cells(&runs),
            vec![
                ('漢', RunStyle::plain((1, 1, 1)), 0),
                ('字', RunStyle::plain((2, 2, 2)), 2),
                ('x', RunStyle::plain((3, 3, 3)), 4)
            ]
        );
    }

    /// A theme change must reach a live pane's rendered colours. The pane holds
    /// its own theme clone (background + glyphs resolve from it), so `set_theme`
    /// must update `background_rgb` — this is the propagation that makes the
    /// settings theme-picker visibly change the panes.
    #[test]
    fn set_theme_repaints_pane_background() {
        let mut pane = PaneTerm::spawn(Theme::builtin_named("itasha-void").unwrap(), 80, 24);
        let before = pane.background_rgb();
        pane.set_theme(Theme::builtin_named("ghost-paper").unwrap());
        let after = pane.background_rgb();
        assert_ne!(
            before, after,
            "set_theme must change the pane's resolved background colour"
        );
    }

    #[test]
    fn cell_metrics_cols_rows_floors_and_clamps() {
        let m = CellMetrics {
            advance_w: 10.0,
            line_h: 20.0,
        };
        assert_eq!(m.cols_rows(105.0, 81.0), (10, 4));
        // Degenerate zero size still yields a 1x1 grid (no zero-division panic).
        assert_eq!(m.cols_rows(0.0, 0.0), (1, 1));
    }

    #[test]
    fn forward_key_encodes_via_shared_core_path() {
        // Drive a real shell pane and assert the bytes the key produced — the
        // SAME bytes the core encoder yields, proving reuse (not re-derivation).
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        // If the shell could not spawn on this box, the encode path is still
        // exercised (write is a no-op but the returned bytes are asserted).
        let mut pane = pane;
        let bytes = pane.forward_key(&LogicalKey::Text("x".into()), KeyModifiers::NONE);
        assert_eq!(bytes, b"x".to_vec(), "text key forwards its UTF-8 bytes");
        let enter = pane.forward_key(&LogicalKey::Enter, KeyModifiers::NONE);
        assert_eq!(enter, b"\r".to_vec(), "Enter forwards CR");
    }

    #[test]
    fn resize_is_debounced() {
        let mut pane = PaneTerm::spawn(void_theme(), 80, 24);
        assert_eq!(pane.size(), (80, 24));
        // Same size → no-op.
        assert_eq!(pane.resize(80, 24), None);
        // Different size → applied.
        assert_eq!(pane.resize(40, 12), Some((40, 12)));
        assert_eq!(pane.size(), (40, 12));
        // Idempotent again.
        assert_eq!(pane.resize(40, 12), None);
    }

    /// The status-bar mouse-mode badge is driven by [`PaneTerm::mouse_mode`].
    /// A fresh pane reports [`MouseMode::Off`] (no badge); after the focused app
    /// enables DEC `?1000` mouse tracking the accessor must reflect it (badge
    /// shown). Drives the shared terminal directly — deterministic, no reliance
    /// on the async PTY reader thread.
    #[test]
    fn mouse_mode_reflects_terminal_state() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        // Default: no mouse reporting → no badge.
        assert_eq!(
            pane.mouse_mode(),
            MouseMode::Off,
            "a fresh pane must report MouseMode::Off (badge hidden)"
        );
        // If the shell could not spawn on this box, there is no terminal to
        // drive; the Off default above is still the meaningful assertion.
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        // App enables ?1000 (normal button tracking) — the badge-trigger state.
        term.lock().unwrap().advance(b"\x1b[?1000h");
        assert_eq!(
            pane.mouse_mode(),
            MouseMode::Normal,
            "after ?1000h the accessor must report Normal (badge shown)"
        );
        // ?1003 (any-event) is also a reporting mode (badge stays shown).
        term.lock().unwrap().advance(b"\x1b[?1003h");
        assert_eq!(pane.mouse_mode(), MouseMode::AnyEvent);
        // App disables tracking → back to Off (badge hidden again).
        term.lock().unwrap().advance(b"\x1b[?1003l");
        assert_eq!(
            pane.mouse_mode(),
            MouseMode::Off,
            "after ?1003l the accessor must report Off (badge hidden)"
        );
    }

    /// [`PaneTerm::report_mouse`] must gate on the program's mouse mode: a fresh
    /// pane (`?1000` not requested) reports NOTHING (returns false), so the host
    /// keeps the gesture for local selection/scroll; after the program enables
    /// `?1000` a button press is encoded and written (returns true). This is the
    /// wiring that makes the mouse work in vim/tmux/htop — the canonical egui
    /// shell did not report mouse at all before it.
    #[test]
    fn report_mouse_gates_on_mouse_mode() {
        let mut pane = PaneTerm::spawn(void_theme(), 80, 24);
        let mods = MouseModifiers::default();
        // Off by default → no report (host keeps the gesture).
        assert!(
            !pane.report_mouse(MouseButton::Left, mods, 3, 5, MouseEventKind::Press),
            "a press while mouse mode is Off must not be reported"
        );
        // If the shell could not spawn there is no terminal to enable ?1000 on;
        // the Off assertion above is still the meaningful one.
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        term.lock().unwrap().advance(b"\x1b[?1000h");
        assert!(
            pane.report_mouse(MouseButton::Left, mods, 3, 5, MouseEventKind::Press),
            "after ?1000h a button press must be encoded and written to the PTY"
        );
        // ?1000 reports buttons only — a bare-motion event encodes to nothing, so
        // it must NOT consume the gesture.
        assert!(
            !pane.report_mouse(MouseButton::None, mods, 3, 5, MouseEventKind::Motion),
            "?1000 reports buttons only — bare motion must not be reported"
        );
    }

    /// [`PaneTerm::pump_host_effects`] must drain ALL of the terminal's per-frame
    /// queues in one call: write back the PTY query reply (so a program querying
    /// the cursor position gets an answer), surface the OSC 52 clipboard write,
    /// the OSC 4 color set, and the OSC 9 notification for the host, and leave
    /// every queue empty so none can grow unbounded. The canonical egui shell
    /// dropped (and leaked) every one of these before this wiring.
    #[test]
    fn pump_host_effects_drains_every_queue() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        {
            let mut t = term.lock().unwrap();
            t.advance(b"\x1b[6n"); // cursor-position report → pty_response
            t.advance(b"\x1b]52;c;aGVsbG8=\x07"); // OSC 52 write "hello"
            t.advance(b"\x1b]4;1;rgb:ff/00/00\x07"); // OSC 4 set index 1 = red
            t.advance(b"\x1b]9;Build complete\x07"); // OSC 9 desktop notification
            t.advance(b"\x1b]9;4;1;42\x07"); // OSC 9;4 taskbar progress: Normal 42%
        }
        let mut pane = pane;
        let fx = pane.pump_host_effects();
        assert_eq!(
            fx.clipboard_writes,
            vec!["hello".to_string()],
            "OSC 52 write must surface as a clipboard write"
        );
        assert_eq!(
            fx.color_sets,
            vec![ColorSet::Indexed {
                index: 1,
                rgb: (255, 0, 0),
            }],
            "OSC 4 set must surface as an indexed color set"
        );
        assert!(fx.notified, "OSC 9 must mark a notification as received");
        assert_eq!(
            fx.progress,
            vec![Progress {
                state: c0pl4nd_core::term::ProgressState::Normal,
                percent: 42,
            }],
            "OSC 9;4 progress must be SURFACED (not drained-and-discarded) for the taskbar"
        );
        // The PTY reply and every other queue must now be drained.
        let mut t = term.lock().unwrap();
        assert!(
            t.take_pty_response().is_empty(),
            "pump must have drained + written back the cursor-position reply"
        );
        assert!(
            t.take_clipboard_writes().is_empty(),
            "clipboard queue drained"
        );
        assert!(t.take_color_sets().is_empty(), "color-set queue drained");
        assert!(
            t.take_notifications().is_empty(),
            "notification queue drained"
        );
        assert!(t.take_progress().is_empty(), "progress queue drained");
    }

    /// A freshly spawned pane must DENY OSC 52 clipboard reads, and the refusal
    /// must be the empty-payload reply rather than silence. This is the
    /// fail-closed half: a pane that is never told about the setting keeps the
    /// safe default, so a wiring failure cannot open the hole.
    #[test]
    fn pane_denies_clipboard_reads_until_told_otherwise() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        let mut t = term.lock().unwrap();
        assert!(
            !t.clipboard_read_enabled(),
            "a fresh pane must deny clipboard reads"
        );
        t.advance(b"\x1b]52;c;?\x07");
        assert!(
            t.take_clipboard_reads().is_empty(),
            "a denied read must never be parked for the host to serve"
        );
        assert_eq!(
            t.take_pty_response(),
            b"\x1b]52;c;\x07".to_vec(),
            "the refusal must be an empty-payload OSC 52 reply, not silence"
        );
    }

    /// The config → terminal wire: `set_clipboard_read_allowed` must actually
    /// move the emulator's gate in BOTH directions. Without this the Settings
    /// checkbox would persist a value that changed nothing (a dead setting), and
    /// — worse — turning it back off would not close the hole.
    #[test]
    fn set_clipboard_read_allowed_moves_the_terminal_gate_both_ways() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        assert!(!term.lock().unwrap().clipboard_read_enabled());

        pane.set_clipboard_read_allowed(true);
        assert!(
            term.lock().unwrap().clipboard_read_enabled(),
            "opting in must reach the emulator"
        );

        pane.set_clipboard_read_allowed(false);
        assert!(
            !term.lock().unwrap().clipboard_read_enabled(),
            "opting back OUT must reach the emulator too"
        );
        // And a request parked while it was on must have been refused, not left
        // to strand the program that made it.
        let mut t = term.lock().unwrap();
        assert!(t.take_clipboard_reads().is_empty());
    }

    /// `pump_host_effects` must SERVE a parked OSC 52 read in the same drain:
    /// answer it, then ship the reply. Asserting the response queue is empty
    /// afterwards is what pins the ordering — if the answer were produced AFTER
    /// `take_pty_response`, the reply would sit here for a frame instead of
    /// going out with this drain.
    #[test]
    fn pump_host_effects_serves_a_parked_clipboard_read() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        pane.set_clipboard_read_allowed(true);
        {
            let mut t = term.lock().unwrap();
            t.advance(b"\x1b]52;c;?\x07");
            assert_eq!(
                t.take_clipboard_reads().len(),
                1,
                "precondition: an opted-in read parks a request"
            );
            // Re-park it for the pump to find (the assert above drained it).
            t.advance(b"\x1b]52;c;?\x07");
        }
        let mut pane = pane;
        let _ = pane.pump_host_effects();
        let mut t = term.lock().unwrap();
        assert!(
            t.take_clipboard_reads().is_empty(),
            "pump must consume the parked read request"
        );
        assert!(
            t.take_pty_response().is_empty(),
            "pump must answer BEFORE draining the response queue, so the reply \
             ships in this drain rather than a frame later"
        );
    }

    /// [`PaneTerm::scroll_view`] drives local scrollback: after enough output to
    /// fill the scrollback, scrolling back raises the view offset off the live
    /// bottom and scrolling forward past the bottom clamps to offset 0. This is
    /// the mouse-wheel scrollback the canonical egui shell lacked (the wheel did
    /// nothing — you could not scroll up to read history).
    #[test]
    fn scroll_view_moves_the_scrollback_offset() {
        let pane = PaneTerm::spawn(void_theme(), 80, 6);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        // Produce many more lines than the 6-row screen so there is scrollback.
        {
            let mut t = term.lock().unwrap();
            for i in 0..40 {
                t.advance(format!("line {i}\r\n").as_bytes());
            }
            assert_eq!(
                t.view_offset(),
                0,
                "output pins the view to the live bottom"
            );
        }
        let mut pane = pane;
        pane.scroll_view(5); // back into history
        assert!(
            term.lock().unwrap().view_offset() > 0,
            "scrolling back must raise the view offset off the bottom"
        );
        pane.scroll_view(-1000); // forward, clamped to the bottom
        assert_eq!(
            term.lock().unwrap().view_offset(),
            0,
            "scrolling forward past the bottom clamps to the live view"
        );
    }

    /// The tab strip is driven by [`PaneTerm::title`]. A fresh pane has no
    /// program-set title yet, so the accessor returns `None` (the strip falls
    /// back to `pane {id}`); after the running program emits an OSC 0 title
    /// (`ESC ] 0 ; <title> BEL`) the accessor must reflect it. Drives the shared
    /// terminal directly — deterministic, no reliance on the async PTY reader.
    #[test]
    fn title_reflects_osc_set_title() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        // Default: no program-set title → None (tab falls back to "pane {id}").
        assert_eq!(
            pane.title(),
            None,
            "a fresh pane must report no title (tab strip uses the pane-id fallback)"
        );
        // If the shell could not spawn on this box there is no terminal to
        // drive; the None default above is still the meaningful assertion.
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        // The program sets its window title via OSC 0 (BEL-terminated).
        term.lock().unwrap().advance(b"\x1b]0;mytitle\x07");
        assert_eq!(
            pane.title(),
            Some("mytitle".to_string()),
            "after ESC]0;mytitle BEL the accessor must report the OSC title"
        );
    }

    /// The status-bar exit-code indicator is driven by
    /// [`PaneTerm::last_command_exit_code`]. A fresh pane has no finished
    /// command (no OSC 133 `D` mark yet), so the accessor returns `None` (no
    /// indicator). After the running shell emits `OSC 133 ; D ; <code>` the
    /// accessor must reflect the latest code. Drives the shared terminal
    /// directly — deterministic, no reliance on the async PTY reader or on a
    /// real prompt-integrated shell.
    #[test]
    fn last_command_exit_code_reflects_osc133_command_end() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        // Default: no finished command → None (status bar shows no indicator).
        assert_eq!(
            pane.last_command_exit_code(),
            None,
            "a fresh pane must report no finished command (no indicator)"
        );
        // If the shell could not spawn on this box there is no terminal to
        // drive; the None default above is still the meaningful assertion.
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        // The shell reports a successful command end (`OSC 133 ; D ; 0`).
        term.lock().unwrap().advance(b"\x1b]133;D;0\x07");
        assert_eq!(
            pane.last_command_exit_code(),
            Some(Some(0)),
            "after ESC]133;D;0 BEL the accessor must report success (code 0)"
        );
        // A subsequent failing command end (`OSC 133 ; D ; 1`) supersedes it.
        term.lock().unwrap().advance(b"\x1b]133;D;1\x07");
        assert_eq!(
            pane.last_command_exit_code(),
            Some(Some(1)),
            "the accessor must report the MOST RECENT command's exit code"
        );
    }

    #[test]
    fn resize_to_px_maps_through_metrics() {
        let mut pane = PaneTerm::spawn(void_theme(), 80, 24);
        let m = CellMetrics {
            advance_w: 10.0,
            line_h: 20.0,
        };
        // 200px / 10 = 20 cols ; 200px / 20 = 10 rows.
        assert_eq!(pane.resize_to_px(200.0, 200.0, m), Some((20, 10)));
        assert_eq!(pane.size(), (20, 10));
    }

    /// `grid_rows` is damage-gated: an unchanged grid reuses the cached `Rc`
    /// (a real cache hit — zero rebuild), and the rebuilt rows carry the text
    /// that was written. The cache-hit identity check is guarded on "still
    /// clean" so a racing async shell-prompt write can never flake it.
    #[test]
    fn grid_rows_is_damage_gated_and_content_correct() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        // If the shell could not spawn on this box there is no terminal to read.
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        term.lock().unwrap().advance(b"hello world");
        let r1 = pane.grid_rows().expect("live session yields rows");
        // r1 cleared the grid's damage. If no async PTY output has arrived since,
        // a second call with NO new writes must reuse the cached Rc.
        if !term.lock().unwrap().grid().is_damaged() {
            let r2 = pane.grid_rows().expect("live session yields rows");
            assert!(
                Rc::ptr_eq(&r1, &r2),
                "an unchanged, undamaged grid reuses the cached rows (no rebuild)"
            );
        }
        let joined: String = r1
            .iter()
            .flat_map(|row| row.iter().map(|(t, _)| t.as_str()))
            .collect();
        assert!(
            joined.contains("hello world"),
            "rebuilt rows carry the written text; got {joined:?}"
        );
    }

    /// A theme change recolours cells WITHOUT a grid write (no damage bit), so
    /// `set_theme` must invalidate the row cache — otherwise stale-coloured rows
    /// would be served. This holds regardless of any async reader activity.
    #[test]
    fn grid_rows_cache_invalidated_by_set_theme() {
        let mut pane = PaneTerm::spawn(void_theme(), 80, 24);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        term.lock().unwrap().advance(b"x");
        let r1 = pane.grid_rows().expect("live session yields rows");
        pane.set_theme(Theme::builtin_named("itasha-void").unwrap());
        let r2 = pane.grid_rows().expect("live session yields rows");
        assert!(
            !Rc::ptr_eq(&r1, &r2),
            "set_theme invalidates the damage-gated row cache"
        );
    }

    /// `report_focus` only emits a DEC ?1004 report when the program armed it;
    /// armed, it queues ESC[I on focus-in and ESC[O on focus-out (the reply the
    /// pump then writes back to the PTY). Without the arm-gate a focus change
    /// would leak stray bytes to a shell that never asked for focus events.
    #[test]
    fn report_focus_only_reports_when_armed() {
        let pane = PaneTerm::spawn(void_theme(), 80, 24);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        let mut pane = pane;
        // Not armed → no bytes queued.
        pane.report_focus(false);
        assert!(
            term.lock().unwrap().take_pty_response().is_empty(),
            "no focus report unless ?1004 is armed"
        );
        // Arm ?1004, then focus-out → ESC[O, focus-in → ESC[I.
        term.lock().unwrap().advance(b"\x1b[?1004h");
        pane.report_focus(false);
        assert_eq!(
            term.lock().unwrap().take_pty_response(),
            b"\x1b[O",
            "focus-out reports ESC[O"
        );
        pane.report_focus(true);
        assert_eq!(
            term.lock().unwrap().take_pty_response(),
            b"\x1b[I",
            "focus-in reports ESC[I"
        );
    }

    /// `jump_to_prompt(false)` scrolls the view back to the previous OSC 133
    /// prompt mark; with no mark in the requested direction it is a no-op.
    #[test]
    fn jump_to_prompt_scrolls_to_a_prompt_mark() {
        let pane = PaneTerm::spawn(void_theme(), 80, 3);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        {
            let mut t = term.lock().unwrap();
            // Several prompts (OSC 133;A) with output between them, so there is
            // scrollback and multiple marks above the live bottom.
            for _ in 0..10 {
                t.advance(b"\x1b]133;A\x07prompt\r\nout\r\n");
            }
            assert_eq!(t.view_offset(), 0, "starts pinned to the live bottom");
        }
        let mut pane = pane;
        assert!(
            pane.jump_to_prompt(false),
            "jumping back reaches an earlier prompt"
        );
        assert!(
            term.lock().unwrap().view_offset() > 0,
            "the view scrolled up off the live bottom to the prompt"
        );
    }

    /// `selection_text` extracts the covered display cells, trims trailing
    /// whitespace per row, joins rows with `\n`, and orders the endpoints (so a
    /// bottom-up drag yields the same text as top-down). Empty selection → None.
    #[test]
    fn selection_text_extracts_ordered_trimmed_rows() {
        let pane = PaneTerm::spawn(void_theme(), 80, 4);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        {
            let mut t = term.lock().unwrap();
            t.advance(b"hello world\r\nsecond line\r\n");
        }
        // Single-row slice: cols 0..=4 of row 0 → "hello".
        assert_eq!(
            pane.selection_text((0, 0), (0, 4), false).as_deref(),
            Some("hello")
        );
        // Two-row selection, given BOTTOM-UP — must order to the same result and
        // join with a newline; trailing blanks on each row are trimmed.
        let two = pane.selection_text((1, 10), (0, 0), false);
        assert_eq!(
            two.as_deref(),
            Some("hello world\nsecond line"),
            "rows ordered + joined with newline, trailing blanks trimmed"
        );
        // An empty (zero-width, all-blank) selection on a blank row → None.
        assert_eq!(pane.selection_text((3, 0), (3, 0), false), None);
    }

    /// In BLOCK mode every row is clipped to the SAME `[min_col, max_col]` column
    /// range (the rectangular selection), not the line-wise first-row-to-last-row
    /// span. A drag from (row0,col6) to (row1,col10) over "hello world" /
    /// "second line" must yield the column slice 6..=10 of EACH row.
    #[test]
    fn selection_text_block_mode_clips_every_row_to_the_column_range() {
        let pane = PaneTerm::spawn(void_theme(), 80, 4);
        let Some(term) = pane.terminal_for_test() else {
            return;
        };
        {
            let mut t = term.lock().unwrap();
            t.advance(b"hello world\r\nsecond line\r\n");
        }
        // Columns 6..=10 of EACH row: "hello world"[6..=10] = "world";
        // "second line"[6..=10] = " line" (col 6 is the space; only TRAILING
        // whitespace is trimmed, so the leading space is kept).
        let block = pane.selection_text((0, 6), (1, 10), true);
        assert_eq!(
            block.as_deref(),
            Some("world\n line"),
            "block mode clips BOTH rows to the same column range 6..=10"
        );
        // The SAME endpoints line-wise would instead run row0 from col6 to EOL and
        // row1 from start to col10 — a different, larger extraction.
        let linewise = pane.selection_text((0, 6), (1, 10), false);
        assert_eq!(
            linewise.as_deref(),
            Some("world\nsecond line"),
            "line-wise mode spans first-row-tail + last-row-head"
        );
    }

    /// `image_display_row` maps an absolute image line to a 0-based display row,
    /// skipping images scrolled above or below the visible window.
    #[test]
    fn image_display_row_maps_and_bounds() {
        // window covers absolute lines 100..=123 (window_start=100, rows=24).
        assert_eq!(image_display_row(100, 100, 24), Some(0)); // top of window
        assert_eq!(image_display_row(110, 100, 24), Some(10));
        assert_eq!(image_display_row(123, 100, 24), Some(23)); // last visible row
        assert_eq!(image_display_row(124, 100, 24), None); // one past the bottom
                                                           // A tall image whose anchor has scrolled ABOVE the top stays renderable
                                                           // with a NEGATIVE display row — the painter clips the off-top portion.
                                                           // (Previously this returned None and the whole image vanished the instant
                                                           // its top row left the viewport — the P1 bug.)
        assert_eq!(image_display_row(99, 100, 24), Some(-1)); // anchor 1 row above top
        assert_eq!(image_display_row(90, 100, 24), Some(-10)); // 10 rows above top
    }
}
