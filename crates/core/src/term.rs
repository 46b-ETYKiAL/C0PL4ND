//! VT/ANSI interpreter.
//!
//! Drives a [`Grid`] from a raw PTY byte stream using the `vte` parser (the
//! same state machine alacritty is built on). Implements the common subset
//! needed by real shells and TUIs: printable text with line wrap, CR/LF/BS/
//! TAB, SGR colour + attributes, cursor positioning, and erase. OSC 7 (cwd)
//! and the window title are captured; OSC 133 semantic zones hook in here.

use crate::grid::{Cell, CellFlags, Color, Grid, UnderlineStyle};
use std::collections::VecDeque;
use vte::{Params, Parser, Perform};
use zeroize::{Zeroize, Zeroizing};

mod charset;
pub mod keys;
pub mod osc;
pub(crate) mod palette;

use charset::{dec_line_draw, is_variation_selector, Charset};
pub use keys::{encode_key, encode_key_kitty, KeyEventKind, KeyModifiers, LogicalKey};
use osc::{base64_decode, base64_encode, format_color_reply, parse_color_spec, Rgb};
pub use osc::{
    ClipboardSelection, ClipboardWrite, ColorSet, CommandMark, CommandMarkKind, DynamicColor,
    Notification, Progress, ProgressState,
};

/// Default scrollback line cap when not configured.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// Maximum depth of the kitty keyboard-protocol flag stack
/// (`CSI > flags u` push / `CSI < n u` pop). Bounds memory against a hostile
/// program issuing an unbalanced stream of pushes; when the cap is reached the
/// oldest entry is dropped rather than growing without limit.
const KITTY_KBD_STACK_MAX: usize = 16;

/// A decoded inline image anchored to a grid position (absolute line + column).
#[derive(Debug, Clone)]
pub struct TerminalImage {
    /// The decoded image pixels.
    pub image: crate::image::DecodedImage,
    /// Absolute grid line the image's top-left anchor sits on.
    pub line: usize,
    /// Grid column the image's top-left anchor sits on.
    pub col: usize,
}

/// An in-progress Kitty graphics transmission, accumulated across chunks and
/// keyed by image id. Per the Kitty protocol the format and dimensions ride on
/// the FIRST chunk only — continuation chunks (`m=1`) resend just `m` + payload
/// — so `format`/`width`/`height` are captured at creation and reused when the
/// `m=0` boundary finalises the image.
#[derive(Debug, Clone)]
struct KittyChunk {
    /// Accumulated, still-base64-encoded payload across every chunk.
    payload: Vec<u8>,
    /// Pixel format declared on the first chunk (24 = RGB, 32 = RGBA, 100 = PNG).
    format: u16,
    width: usize,
    height: usize,
}

/// The current drawing pen (colours + attributes applied to printed cells).
#[derive(Debug, Clone)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: CellFlags,
    /// Underline color (C20, SGR 58/59). `None` = inherit foreground.
    underline_color: Option<Color>,
}

impl Default for Pen {
    fn default() -> Self {
        Pen {
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            underline_color: None,
        }
    }
}

/// Mouse tracking mode requested by the application via DEC private modes
/// 1000/1002/1003. The host event loop consults this to decide whether — and
/// for which events — to forward mouse activity to the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    /// No mouse reporting (the default).
    #[default]
    Off,
    /// `?1000` — report button press and release only.
    Normal,
    /// `?1002` — report press/release plus motion while a button is held.
    ButtonEvent,
    /// `?1003` — report press/release plus all motion (even with no button).
    AnyEvent,
}

/// Wire encoding for mouse reports. Selected by DEC private modes 1006/1015;
/// defaults to the legacy X10/normal byte encoding when neither is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    /// Legacy `CSI M Cb Cx Cy` with each value offset by 32 (X10/normal).
    #[default]
    X10,
    /// `?1006` — SGR extended: `CSI < b ; x ; y M` (press) / `m` (release).
    Sgr,
    /// `?1015` — urxvt: `CSI b ; x ; y M` (decimal, all values offset by 32).
    Urxvt,
}

/// Cursor glyph shape requested via DECSCUSR (`CSI Ps SP q`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// Filled rectangle (the default).
    #[default]
    Block,
    /// Underline bar at the cell baseline.
    Underline,
    /// Thin vertical bar at the cell's left edge.
    Bar,
}

/// DEC private mode state (`CSI ? Pm h` set / `CSI ? Pm l` reset).
///
/// Each field tracks one terminal mode the application can toggle. The host
/// renderer and event loop read these via [`Terminal`]'s getters to drive
/// cursor visibility, mouse forwarding, paste bracketing, and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecModes {
    /// `?25` DECTCEM — cursor visible. Default `true`.
    pub cursor_visible: bool,
    /// `?2004` — bracketed paste mode.
    pub bracketed_paste: bool,
    /// `?1` DECCKM — cursor-key application mode (affects arrow-key encoding).
    pub application_cursor_keys: bool,
    /// `?7` DECAWM — autowrap at the right margin. Default `true`.
    pub autowrap: bool,
    /// `?12` — cursor blink (att610). Independent of DECSCUSR's blink bit.
    pub cursor_blink: bool,
    /// `?1004` — focus in/out reporting (`CSI I` / `CSI O`).
    pub focus_reporting: bool,
    /// `?2026` — synchronized output (batch updates between begin/end).
    pub sync_output: bool,
    /// Active mouse tracking mode (`?1000` / `?1002` / `?1003`).
    pub mouse_mode: MouseMode,
    /// Active mouse report encoding (`?1006` / `?1015`).
    pub mouse_encoding: MouseEncoding,
}

impl Default for DecModes {
    fn default() -> Self {
        DecModes {
            cursor_visible: true,
            bracketed_paste: false,
            application_cursor_keys: false,
            autowrap: true,
            cursor_blink: false,
            focus_reporting: false,
            sync_output: false,
            mouse_mode: MouseMode::Off,
            mouse_encoding: MouseEncoding::X10,
        }
    }
}

/// Saved primary-screen state captured when switching to the alternate screen.
#[derive(Debug, Clone)]
struct SavedScreen {
    grid: Grid,
    row: usize,
    col: usize,
    pen: Pen,
}

/// Saved cursor state for DECSC (`ESC 7`) / DECRC (`ESC 8`) and the ANSI.SYS
/// `CSI s` / `CSI u` aliases. Captures position, pen, and the active charset
/// selection so a restore round-trips the full drawing context.
#[derive(Debug, Clone)]
struct SavedCursor {
    row: usize,
    col: usize,
    pen: Pen,
    charset_g0: Charset,
}

/// Map a mode-active boolean to the DECRQM `Pv` value: 1 = set, 2 = reset.
fn bool_mode(active: bool) -> u8 {
    if active {
        1
    } else {
        2
    }
}

/// Saturate a 16-bit SGR colour channel / palette index to `u8`. A bare `as u8`
/// WRAPS (`300 → 44`), turning an out-of-range truecolor channel or 256-colour
/// index into a silently wrong colour; clamping to 255 is the xterm-compatible
/// behaviour for out-of-range operands.
fn clamp_u8(v: u16) -> u8 {
    v.min(255) as u8
}

/// Decode an ASCII-hex byte string (XTGETTCAP capability name) into a UTF-8
/// string. Returns `None` on odd length, non-hex bytes, or invalid UTF-8.
fn hex_decode(hex: &[u8]) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    String::from_utf8(out).ok()
}

/// Encode bytes as uppercase ASCII-hex (for XTGETTCAP replies).
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// Default tab-stop interval (a stop every N columns), matching xterm/VT100.
const DEFAULT_TAB_INTERVAL: usize = 8;

/// Build the default per-column tab-stop bitset for a `cols`-wide screen: a stop
/// at every multiple of [`DEFAULT_TAB_INTERVAL`] (column 0, 8, 16, …).
fn default_tab_stops(cols: usize) -> Vec<bool> {
    (0..cols).map(|c| c % DEFAULT_TAB_INTERVAL == 0).collect()
}

/// A mouse button (or wheel direction) for [`Terminal::encode_mouse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left button (button 0).
    Left,
    /// Middle button (button 1).
    Middle,
    /// Right button (button 2).
    Right,
    /// Wheel scrolled up (button 4).
    WheelUp,
    /// Wheel scrolled down (button 5).
    WheelDown,
    /// No button — used for bare-motion reports under `?1003`.
    None,
}

/// The kind of mouse event being reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    /// Button pressed down.
    Press,
    /// Button released.
    Release,
    /// Pointer moved (drag with `?1002`, or any motion with `?1003`).
    Motion,
}

/// Keyboard modifiers held during a mouse event. These OR into the button byte
/// per the xterm protocol (shift=4, meta/alt=8, control=16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseModifiers {
    /// Shift held during the event (adds 4 to the button byte).
    pub shift: bool,
    /// Alt/Meta held during the event (adds 8 to the button byte).
    pub alt: bool,
    /// Control held during the event (adds 16 to the button byte).
    pub control: bool,
}

/// Mutable terminal screen state (the `vte::Perform` implementor).
struct Screen {
    grid: Grid,
    row: usize,
    col: usize,
    pen: Pen,
    /// Lines scrolled off the top, newest at the back. Capped at `max_scrollback`.
    history: VecDeque<Vec<Cell>>,
    /// Parallel to `history`: `history_wrapped[i] == true` means history line `i`
    /// soft-wrapped into the next line (no hard newline). Kept the same length as
    /// `history` so reflow can reconstruct logical lines across the history/grid
    /// boundary.
    history_wrapped: VecDeque<bool>,
    max_scrollback: usize,
    /// Lines scrolled up from the live bottom (0 = following live output).
    view_offset: usize,
    /// Captured window title (OSC 0/2).
    title: String,
    /// Captured working directory (OSC 7).
    cwd: Option<String>,
    /// Absolute line indices where a shell prompt began (OSC 133 ; A),
    /// for jump-to-prompt. Absolute = history length + grid row at mark time.
    prompt_marks: Vec<usize>,
    /// Hyperlink URIs seen via OSC 8, in arrival order.
    hyperlinks: Vec<String>,
    /// Decoded inline images (Sixel via DCS, Kitty via APC), anchored to grid
    /// positions.
    images: Vec<TerminalImage>,
    /// In-progress Sixel DCS payload accumulator (Some between hook and unhook).
    sixel_accum: Option<Vec<u8>>,
    /// In-progress XTGETTCAP DCS payload accumulator (`DCS + q … ST`, C30). Some
    /// between hook and unhook when the DCS is an XTGETTCAP request, exclusive
    /// with `sixel_accum`.
    xtgettcap_accum: Option<Vec<u8>>,
    /// In-progress Kitty transmissions keyed by image id, accumulated across
    /// `m=1` … `m=0` chunk boundaries (decoded once at the `m=0` boundary).
    /// Format/width/height are captured from the FIRST chunk — the Kitty spec
    /// carries them only on the first chunk of a multi-chunk transmission.
    kitty_chunks: std::collections::HashMap<u32, KittyChunk>,
    /// Transmit-stored Kitty images (`a=t`) keyed by image id, available for a
    /// later `a=p` display. Bounded against hostile streams.
    kitty_store: std::collections::HashMap<u32, crate::image::DecodedImage>,
    /// DEC private mode flags toggled via `CSI ? Pm h` / `CSI ? Pm l`.
    dec_modes: DecModes,
    /// Requested cursor shape (DECSCUSR `CSI Ps SP q`).
    cursor_shape: CursorShape,
    /// Requested cursor blink from DECSCUSR (separate from `?12`).
    cursor_shape_blink: bool,
    /// Saved primary screen, present iff the alternate screen is active.
    /// Switching to the alt screen stashes the primary grid + cursor + pen
    /// here; switching back restores them. Scrollback (`history`) is NOT
    /// touched while the alt screen is active, so full-screen TUIs never
    /// pollute the user's scrollback.
    saved_primary: Option<SavedScreen>,
    /// Saved cursor for DECSC/DECRC (`ESC 7`/`ESC 8`) + ANSI.SYS `CSI s`/`CSI u`.
    saved_cursor: Option<SavedCursor>,
    /// Top margin of the scroll region (DECSTBM), 0-based inclusive. Defaults
    /// to row 0 (full screen).
    scroll_top: usize,
    /// Bottom margin of the scroll region (DECSTBM), 0-based inclusive. Defaults
    /// to the last row (full screen).
    scroll_bottom: usize,
    /// Active G0 charset designation (`ESC ( 0` / `ESC ( B`). Shift-In (SI) /
    /// Shift-Out (SO) toggle between this and `charset_g1`.
    charset_g0: Charset,
    /// Active G1 charset designation (`ESC ) 0` / `ESC ) B`).
    charset_g1: Charset,
    /// True when SO (0x0E) invoked G1 into GL; SI (0x0F) returns to G0.
    use_g1: bool,
    /// Per-column tab-stop flags (`tab_stops[c] == true` means a stop sits at
    /// column `c`). Length is always `grid.cols()`. Defaults to a stop every 8
    /// columns. Set by HTS (`ESC H`), cleared by TBC (`CSI g`/`CSI 3 g`).
    tab_stops: Vec<bool>,
    /// Last grapheme printed, for REP (`CSI b`).
    last_print: Option<char>,
    /// IRM insert mode (`CSI 4 h` / `l`, C25). When set, printing a glyph shifts
    /// the rest of the line right instead of overwriting.
    insert_mode: bool,
    /// DECSCNM reverse-video screen (`?5`, C25). When set, the renderer swaps
    /// default fg/bg for the whole screen. Stored here, read via a getter.
    reverse_screen: bool,
    /// DECOM origin mode (`?6`, C25). When set, cursor row addressing (CUP/VPA)
    /// is relative to the scroll region's top margin and clamped to it.
    origin_mode: bool,
    /// Kitty keyboard-protocol progressive-enhancement flag stack
    /// (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>). The CURRENT flags
    /// are `*kitty_kbd_stack.last().unwrap_or(&0)` — an empty stack means the
    /// legacy encoding is in force. Pushed by `CSI > flags u`, popped by
    /// `CSI < n u`, replaced/ORed/cleared by `CSI = flags ; mode u`, and queried
    /// by `CSI ? u`. Depth is capped at `KITTY_KBD_STACK_MAX` (bounded memory:
    /// a hostile program cannot grow it without limit — the oldest entry is
    /// dropped when the cap is reached, mirroring kitty's own bounded stack).
    kitty_kbd_stack: Vec<u8>,
    /// OSC 9 ; 4 progress reports (`ConEmu`/Windows-Terminal taskbar progress),
    /// drained by the app to drive a tab/taskbar indicator (C26).
    pending_progress: Vec<osc::Progress>,
    /// OSC 133 command zones (C28): each command's output-start and end (with
    /// optional exit code) marks, accumulated for prompt success/fail glyphs and
    /// command-duration display. Capture-only — never reported back to the PTY.
    command_marks: Vec<osc::CommandMark>,
    /// Pending bytes to write back to the PTY (OSC 4/10/11/12 query replies and
    /// OSC 52 read replies). Drained by [`Terminal::take_pty_response`]. The
    /// terminal NEVER queues a reply for OSC 133 marks (anti-CVE, capture-only).
    pty_response: Vec<u8>,
    /// OSC 52 clipboard WRITE requests, drained by the app. READs are
    /// DEFAULT-OFF (see `clipboard_read_enabled`) to avoid the canonical
    /// host-clipboard-exfiltration vulnerability.
    pending_clipboard_writes: Vec<ClipboardWrite>,
    /// OSC 52 clipboard READ requests awaiting a host answer. Populated ONLY
    /// while `clipboard_read_enabled` is true; a denied read is answered inline
    /// with an empty payload and never lands here.
    pending_clipboard_reads: Vec<osc::ClipboardReadRequest>,
    clipboard_read_enabled: bool,
    /// OSC 4 / 10 / 11 / 12 / 104 / 11x color-set requests, drained by the app
    /// so it can apply them to its live theme.
    pending_color_sets: Vec<ColorSet>,
    /// OSC 9 / OSC 777 desktop notifications, drained by the app.
    pending_notifications: Vec<Notification>,
    /// XTWINOPS 22/23 t title stack.
    title_stack: Vec<String>,
    /// Live indexed palette (256 entries) used to answer OSC 4 queries and
    /// OSC 104 resets. Seeded from the xterm default; OSC 4 sets update it.
    palette: [Rgb; 256],
    /// Immutable default palette for OSC 104 reset.
    default_palette: [Rgb; 256],
    /// Live dynamic colors (default fg/bg/cursor) for OSC 10/11/12 queries.
    dynamic_fg: Rgb,
    dynamic_bg: Rgb,
    dynamic_cursor: Rgb,
    /// Defaults for OSC 110/111/112 reset.
    default_dynamic_fg: Rgb,
    default_dynamic_bg: Rgb,
    default_dynamic_cursor: Rgb,
}

impl Screen {
    fn new(rows: usize, cols: usize, max_scrollback: usize) -> Self {
        Screen {
            grid: Grid::new(rows, cols),
            row: 0,
            col: 0,
            pen: Pen::default(),
            history: VecDeque::new(),
            history_wrapped: VecDeque::new(),
            max_scrollback,
            view_offset: 0,
            title: String::new(),
            cwd: None,
            prompt_marks: Vec::new(),
            hyperlinks: Vec::new(),
            images: Vec::new(),
            sixel_accum: None,
            xtgettcap_accum: None,
            kitty_chunks: std::collections::HashMap::new(),
            kitty_store: std::collections::HashMap::new(),
            dec_modes: DecModes::default(),
            cursor_shape: CursorShape::default(),
            cursor_shape_blink: false,
            saved_primary: None,
            saved_cursor: None,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            charset_g0: Charset::default(),
            charset_g1: Charset::default(),
            use_g1: false,
            tab_stops: default_tab_stops(cols),
            last_print: None,
            insert_mode: false,
            reverse_screen: false,
            origin_mode: false,
            kitty_kbd_stack: Vec::new(),
            pending_progress: Vec::new(),
            command_marks: Vec::new(),
            pty_response: Vec::new(),
            pending_clipboard_writes: Vec::new(),
            pending_clipboard_reads: Vec::new(),
            clipboard_read_enabled: false,
            pending_color_sets: Vec::new(),
            pending_notifications: Vec::new(),
            title_stack: Vec::new(),
            palette: palette::build_default_palette(),
            default_palette: palette::build_default_palette(),
            // xterm protocol defaults: white on black, white cursor.
            dynamic_fg: (229, 229, 229),
            dynamic_bg: (0, 0, 0),
            dynamic_cursor: (255, 255, 255),
            default_dynamic_fg: (229, 229, 229),
            default_dynamic_bg: (0, 0, 0),
            default_dynamic_cursor: (255, 255, 255),
        }
    }

    // ------------------------------------------------------------------
    // OSC color / clipboard / notification handlers (called by osc_dispatch).
    // ------------------------------------------------------------------

    /// Handles `OSC 4 ; i ; spec [; i ; spec ...]` (set/query indexed colors).
    ///
    /// Multiple `index;spec` pairs may appear. A `spec` of `?` queues a query
    /// reply reporting the current palette value; any other spec sets it (and
    /// queues a [`ColorSet`] for the app to apply to its theme).
    fn handle_osc_4(&mut self, params: &[&[u8]]) {
        let mut i = 1;
        while i + 1 < params.len() {
            let idx = std::str::from_utf8(params[i])
                .ok()
                .and_then(|s| s.trim().parse::<u16>().ok());
            let spec = String::from_utf8_lossy(params[i + 1]);
            if let Some(idx) = idx {
                if idx <= 255 {
                    let idx = idx as u8;
                    if spec.trim() == "?" {
                        let reply = format_color_reply(self.palette[idx as usize]);
                        let resp = format!("\x1b]4;{};{}\x07", idx, reply);
                        self.push_pty_response(resp.as_bytes());
                    } else if let Some(rgb) = parse_color_spec(&spec) {
                        self.palette[idx as usize] = rgb;
                        self.pending_color_sets
                            .push(ColorSet::Indexed { index: idx, rgb });
                    }
                }
            }
            i += 2;
        }
    }

    /// Handles `OSC 10/11/12 ; spec` (set/query default fg/bg/cursor).
    fn handle_dynamic_color(&mut self, params: &[&[u8]], which: DynamicColor) {
        let Some(spec) = params.get(1) else {
            return;
        };
        let spec = String::from_utf8_lossy(spec);
        if spec.trim() == "?" {
            let current = match which {
                DynamicColor::Foreground => self.dynamic_fg,
                DynamicColor::Background => self.dynamic_bg,
                DynamicColor::Cursor => self.dynamic_cursor,
            };
            let code = match which {
                DynamicColor::Foreground => 10,
                DynamicColor::Background => 11,
                DynamicColor::Cursor => 12,
            };
            let resp = format!("\x1b]{};{}\x07", code, format_color_reply(current));
            self.push_pty_response(resp.as_bytes());
        } else if let Some(rgb) = parse_color_spec(&spec) {
            match which {
                DynamicColor::Foreground => self.dynamic_fg = rgb,
                DynamicColor::Background => self.dynamic_bg = rgb,
                DynamicColor::Cursor => self.dynamic_cursor = rgb,
            }
            self.pending_color_sets
                .push(ColorSet::Dynamic { which, rgb });
        }
    }

    /// Handles `OSC 104 [; i ...]` (reset indexed palette entries to defaults).
    ///
    /// With no index arguments, resets the entire palette. Otherwise resets only
    /// the named indices. Each reset queues a [`ColorSet`] carrying the default
    /// color so the app can mirror the change.
    fn handle_osc_104(&mut self, params: &[&[u8]]) {
        if params.len() <= 1 {
            self.palette = self.default_palette;
            for (idx, &rgb) in self.default_palette.iter().enumerate() {
                self.pending_color_sets.push(ColorSet::Indexed {
                    index: idx as u8,
                    rgb,
                });
            }
            return;
        }
        for raw in &params[1..] {
            if let Some(idx) = std::str::from_utf8(raw)
                .ok()
                .and_then(|s| s.trim().parse::<u16>().ok())
            {
                if idx <= 255 {
                    let idx = idx as u8;
                    let rgb = self.default_palette[idx as usize];
                    self.palette[idx as usize] = rgb;
                    self.pending_color_sets
                        .push(ColorSet::Indexed { index: idx, rgb });
                }
            }
        }
    }

    /// Resets a dynamic color (OSC 110/111/112) to its default value.
    fn reset_dynamic_color(&mut self, which: DynamicColor) {
        let rgb = match which {
            DynamicColor::Foreground => self.default_dynamic_fg,
            DynamicColor::Background => self.default_dynamic_bg,
            DynamicColor::Cursor => self.default_dynamic_cursor,
        };
        match which {
            DynamicColor::Foreground => self.dynamic_fg = rgb,
            DynamicColor::Background => self.dynamic_bg = rgb,
            DynamicColor::Cursor => self.dynamic_cursor = rgb,
        }
        self.pending_color_sets
            .push(ColorSet::Dynamic { which, rgb });
    }

    /// Handles `OSC 52 ; <selection> ; <base64|?>` (clipboard set/query).
    ///
    /// WRITE-only by default. A `?` payload is a clipboard READ request, gated
    /// on [`Terminal::set_clipboard_read_enabled`] (DEFAULT-OFF — an
    /// on-by-default clipboard read is the canonical OSC 52 exfiltration hole:
    /// anything with a handle on the tty could siphon whatever the user last
    /// copied, passwords and tokens included).
    ///
    /// - **Denied (the default)** — the terminal answers the query ITSELF with
    ///   an empty-payload OSC 52 reply and queues nothing. It never consults the
    ///   host clipboard, so zero bytes leak; and because it still ANSWERS, a
    ///   program blocking on the reply is released instead of hanging until its
    ///   own timeout (see [`osc::format_clipboard_reply`]).
    /// - **Allowed** — the request is queued as a
    ///   [`osc::ClipboardReadRequest`] for the app to drain
    ///   ([`Terminal::take_clipboard_reads`]). Even then the core never reads the
    ///   host clipboard itself; the app supplies the text via
    ///   [`Terminal::respond_clipboard_read`].
    fn handle_osc_52(&mut self, params: &[&[u8]]) {
        let sel_bytes = params.get(1).copied().unwrap_or(b"c");
        let payload = params.get(2).copied().unwrap_or(b"");

        // Selection: first recognised char; empty -> clipboard.
        let selection = sel_bytes
            .iter()
            .find_map(|&b| match b {
                b'c' | b's' | b'0' => Some(ClipboardSelection::Clipboard),
                b'p' => Some(ClipboardSelection::Primary),
                _ => None,
            })
            .unwrap_or(ClipboardSelection::Clipboard);

        if payload == b"?" {
            if self.clipboard_read_enabled {
                // Opted in: surface the request so the host can answer it with
                // real clipboard text. Bounded — a program spamming `?` must not
                // grow this queue without limit (oldest dropped, matching every
                // other PTY-driven buffer).
                self.pending_clipboard_reads
                    .push(osc::ClipboardReadRequest { selection });
                while self.pending_clipboard_reads.len() > Self::CLIPBOARD_READS_MAX {
                    self.pending_clipboard_reads.remove(0);
                }
            } else {
                // DENIED (the default). Answer with an EMPTY payload: the host
                // clipboard is never consulted, so nothing leaks — but the
                // requesting program still gets a well-formed reply and resumes
                // instead of blocking on a response that never comes.
                let reply = osc::format_clipboard_reply(selection, "");
                self.push_pty_response(reply.as_bytes());
            }
            return;
        }

        // Size-cap BEFORE decoding: a base64 payload is ~4/3 the decoded size, so
        // reject early when even the encoded length cannot fit under the cap. This
        // bounds the allocation a hostile multi-megabyte OSC 52 write can force.
        if payload.len() / 4 * 3 > Self::OSC52_WRITE_MAX_BYTES {
            return;
        }
        if let Some(decoded) = base64_decode(payload) {
            // Drop oversized writes (defence-in-depth vs. the pre-decode check).
            if decoded.len() > Self::OSC52_WRITE_MAX_BYTES {
                return;
            }
            // Wipe the transient decoded byte buffer on drop (P-V3): it holds
            // the plaintext clipboard payload before it is re-encoded into the
            // `ClipboardWrite` (which itself zeroizes on drop). Without this the
            // intermediate `Vec<u8>` would leave the plaintext in a freed
            // allocation recoverable from a crash dump / pagefile.
            let decoded = Zeroizing::new(decoded);
            let text = String::from_utf8_lossy(&decoded).into_owned();
            self.pending_clipboard_writes
                .push(ClipboardWrite { selection, text });
            while self.pending_clipboard_writes.len() > Self::CLIPBOARD_WRITES_MAX {
                self.pending_clipboard_writes.remove(0);
            }
        }
    }

    /// Handles `OSC 9 ; 4 ; <state> ; <percent>` (C26 — taskbar/tab progress).
    ///
    /// State 0 removes the indicator, 1 = normal, 2 = error, 3 = indeterminate,
    /// 4 = warning. `percent` is clamped to 0-100 and ignored for states 0/3.
    /// Pushes a [`osc::Progress`] into the drained queue; never replies to PTY.
    fn handle_osc_9_4(&mut self, params: &[&[u8]]) {
        let state_n = params
            .get(2)
            .and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|s| s.trim().parse::<u8>().ok())
            .unwrap_or(0);
        let percent = params
            .get(3)
            .and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
            .min(100) as u8;
        let state = match state_n {
            1 => osc::ProgressState::Normal,
            2 => osc::ProgressState::Error,
            3 => osc::ProgressState::Indeterminate,
            4 => osc::ProgressState::Warning,
            _ => osc::ProgressState::Remove,
        };
        let percent = match state {
            osc::ProgressState::Remove | osc::ProgressState::Indeterminate => 0,
            _ => percent,
        };
        self.pending_progress.push(osc::Progress { state, percent });
        while self.pending_progress.len() > Self::PROGRESS_MAX {
            self.pending_progress.remove(0);
        }
    }

    /// Pushes the current title onto the title stack (XTWINOPS `CSI 22 t`).
    fn push_title(&mut self) {
        self.title_stack.push(self.title.clone());
        // Bound the stack: an unbalanced `CSI 22 t` flood (push without a
        // matching `CSI 23 t` pop) would otherwise grow it without limit. Drop
        // the oldest saved title on overflow (xterm bounds its stack likewise).
        while self.title_stack.len() > Self::TITLE_STACK_MAX {
            self.title_stack.remove(0);
        }
    }

    /// Pops a title from the title stack into the current title
    /// (XTWINOPS `CSI 23 t`). No-op when the stack is empty.
    fn pop_title(&mut self) {
        if let Some(t) = self.title_stack.pop() {
            self.title = t;
        }
    }

    /// Apply a DEC private mode set/reset to one mode number.
    ///
    /// `set` is `true` for `CSI ? Pm h`, `false` for `CSI ? Pm l`. Unknown mode
    /// numbers are ignored (the same forgiving posture real terminals take).
    fn set_dec_mode(&mut self, mode: u16, set: bool) {
        match mode {
            1 => self.dec_modes.application_cursor_keys = set,
            5 => self.reverse_screen = set, // DECSCNM — reverse-video screen (C25).
            6 => {
                // DECOM origin mode (C25): toggling it homes the cursor to the
                // origin (top margin / top-left), per the VT spec.
                self.origin_mode = set;
                self.row = if set { self.scroll_top } else { 0 };
                self.col = 0;
            }
            7 => self.dec_modes.autowrap = set,
            12 => self.dec_modes.cursor_blink = set,
            25 => self.dec_modes.cursor_visible = set,
            // Alternate screen — 1049 saves/clears, 47/1047 are the older forms.
            47 | 1047 | 1049 => self.set_alt_screen(mode, set),
            1000 => {
                self.dec_modes.mouse_mode = if set {
                    MouseMode::Normal
                } else {
                    MouseMode::Off
                };
            }
            1002 => {
                self.dec_modes.mouse_mode = if set {
                    MouseMode::ButtonEvent
                } else {
                    MouseMode::Off
                };
            }
            1003 => {
                self.dec_modes.mouse_mode = if set {
                    MouseMode::AnyEvent
                } else {
                    MouseMode::Off
                };
            }
            1006 => {
                self.dec_modes.mouse_encoding = if set {
                    MouseEncoding::Sgr
                } else {
                    MouseEncoding::X10
                };
            }
            1015 => {
                self.dec_modes.mouse_encoding = if set {
                    MouseEncoding::Urxvt
                } else {
                    MouseEncoding::X10
                };
            }
            1004 => self.dec_modes.focus_reporting = set,
            2004 => self.dec_modes.bracketed_paste = set,
            2026 => self.dec_modes.sync_output = set,
            _ => {}
        }
    }

    /// Enter or leave the alternate screen.
    ///
    /// `1049` saves the cursor + clears + switches to a blank alt grid on set,
    /// and restores the cursor on reset. `1047` clears the alt screen when
    /// switching away. `47` switches without saving/restoring the cursor.
    /// Re-applying a set while already on the alt screen (or a reset while
    /// already on the primary) is a no-op so duplicate sequences are safe.
    fn set_alt_screen(&mut self, mode: u16, set: bool) {
        let saves_cursor = mode == 1049;
        if set {
            if self.saved_primary.is_some() {
                return; // Already on the alt screen.
            }
            let rows = self.grid.rows();
            let cols = self.grid.cols();
            let saved = SavedScreen {
                grid: std::mem::replace(&mut self.grid, Grid::new(rows, cols)),
                row: self.row,
                col: self.col,
                pen: self.pen.clone(),
            };
            self.saved_primary = Some(saved);
            // A fresh alt grid starts blank; home the cursor for 1049/1047.
            if saves_cursor || mode == 1047 {
                self.row = 0;
                self.col = 0;
            }
        } else {
            let Some(saved) = self.saved_primary.take() else {
                return; // Already on the primary screen.
            };
            if mode == 1047 {
                // 1047 clears the alt screen on the way out.
                self.grid.clear();
            }
            self.grid = saved.grid;
            self.grid.touch();
            if saves_cursor {
                self.row = saved.row;
                self.col = saved.col;
                self.pen = saved.pen;
            }
            // For 47/1047 the cursor is left where it is (xterm behaviour), but
            // clamp it so it can never index outside the restored grid.
            self.row = self.row.min(self.grid.rows() - 1);
            self.col = self.col.min(self.grid.cols() - 1);
        }
    }

    /// Apply DECSCUSR (`CSI Ps SP q`) cursor-shape selection.
    fn set_cursor_shape(&mut self, ps: u16) {
        let (shape, blink) = match ps {
            0 | 1 => (CursorShape::Block, true),
            2 => (CursorShape::Block, false),
            3 => (CursorShape::Underline, true),
            4 => (CursorShape::Underline, false),
            5 => (CursorShape::Bar, true),
            6 => (CursorShape::Bar, false),
            _ => return,
        };
        self.cursor_shape = shape;
        self.cursor_shape_blink = blink;
    }

    /// True when a scroll region narrower than the full screen is active.
    fn region_is_full(&self) -> bool {
        self.scroll_top == 0 && self.scroll_bottom + 1 >= self.grid.rows()
    }

    /// Clamp the scroll region to the current grid bounds (called after resize
    /// and at DECSTBM-set time). A region that became invalid resets to full.
    fn clamp_scroll_region(&mut self) {
        let last = self.grid.rows().saturating_sub(1);
        if self.scroll_bottom > last {
            self.scroll_bottom = last;
        }
        if self.scroll_top > self.scroll_bottom {
            self.scroll_top = 0;
            self.scroll_bottom = last;
        }
    }

    // ------------------------------------------------------------------
    // Tab stops (C19): HTS (ESC H) / TBC (CSI g) / CHT (CSI I) / CBT (CSI Z)
    // and the `\t` / 0x09 advance.
    // ------------------------------------------------------------------

    /// Resize the tab-stop bitset to the current column count, seeding any
    /// newly-exposed columns with the default every-8 stops and preserving the
    /// stops that still fit. Called after a grid resize.
    fn resize_tab_stops(&mut self, cols: usize) {
        let old = self.tab_stops.len();
        if cols == old {
            return;
        }
        self.tab_stops.resize(cols, false);
        // Seed columns beyond the old width with the default stops so a widened
        // screen tabs sensibly into the new region.
        for c in old..cols {
            self.tab_stops[c] = c % DEFAULT_TAB_INTERVAL == 0;
        }
    }

    /// HTS (`ESC H`): set a tab stop at the current column.
    fn set_tab_stop(&mut self) {
        if let Some(slot) = self.tab_stops.get_mut(self.col) {
            *slot = true;
        }
    }

    /// TBC (`CSI g` / `CSI 0 g`): clear the stop at the cursor column.
    /// `CSI 3 g` clears every stop.
    fn clear_tab_stop(&mut self, mode: u16) {
        match mode {
            0 => {
                if let Some(slot) = self.tab_stops.get_mut(self.col) {
                    *slot = false;
                }
            }
            3 => {
                for slot in &mut self.tab_stops {
                    *slot = false;
                }
            }
            _ => {}
        }
    }

    /// CHT (`CSI I`) / `\t`: advance the cursor forward `n` tab stops, stopping
    /// at the last column. When no stop lies ahead the cursor lands on the last
    /// column (matching xterm).
    fn tab_forward(&mut self, n: usize) {
        let last = self.grid.cols().saturating_sub(1);
        for _ in 0..n.max(1) {
            if self.col >= last {
                self.col = last;
                break;
            }
            let mut next = self.col + 1;
            while next < last && !self.tab_stops.get(next).copied().unwrap_or(false) {
                next += 1;
            }
            self.col = next;
        }
    }

    /// CBT (`CSI Z`): move the cursor back `n` tab stops, stopping at column 0.
    fn tab_back(&mut self, n: usize) {
        for _ in 0..n.max(1) {
            if self.col == 0 {
                break;
            }
            let mut prev = self.col - 1;
            while prev > 0 && !self.tab_stops.get(prev).copied().unwrap_or(false) {
                prev -= 1;
            }
            self.col = prev;
        }
    }

    /// C14 (focus reporting, core half): when DEC mode `?1004` is enabled, queue
    /// the focus-event report — `ESC [ I` on focus-in, `ESC [ O` on focus-out —
    /// into `pty_response` for the app to write to the PTY. A no-op when `?1004`
    /// is off, so the host may call it unconditionally on every focus change.
    fn focus_report(&mut self, focused: bool) {
        if !self.dec_modes.focus_reporting {
            return;
        }
        let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
        self.push_pty_response(seq);
    }

    /// C16 — reflow the scrollback + visible grid to a new column width without
    /// losing characters.
    ///
    /// Logical lines (runs of physical rows joined across soft-wrap boundaries)
    /// are reconstructed from `history` + `history_wrapped` + the grid's
    /// per-row wrap flags, then re-laid-out at `new_cols`. A hard newline is
    /// never merged across the wrap boundary (only `wrapped == true` rows join).
    /// Scrollback is preserved: the re-laid-out rows fill the bottom `new_rows`
    /// into the grid and the remainder back into history. The cursor is mapped
    /// to its logical position on a best-effort basis (exact when the cursor's
    /// logical line is fully materialised in the visible grid).
    fn reflow_resize(&mut self, new_rows: usize, new_cols: usize) {
        let old_rows = self.grid.rows();
        let old_cols = self.grid.cols();

        // --- Step 1: gather every physical row (history first, then grid) with
        // its soft-wrap flag, and remember which physical row holds the cursor.
        struct PhysRow {
            cells: Vec<Cell>,
            wrapped: bool,
        }
        let mut phys: Vec<PhysRow> = Vec::with_capacity(self.history.len() + old_rows);
        for (i, row) in self.history.iter().enumerate() {
            let mut cells = row.clone();
            cells.resize(old_cols, Cell::default());
            phys.push(PhysRow {
                cells,
                wrapped: self.history_wrapped.get(i).copied().unwrap_or(false),
            });
        }
        let cursor_phys = self.history.len() + self.row.min(old_rows.saturating_sub(1));
        let cursor_col = self.col.min(old_cols);
        for r in 0..old_rows {
            phys.push(PhysRow {
                cells: self.grid.row(r).to_vec(),
                wrapped: self.grid.is_wrapped(r),
            });
        }

        // Drop trailing fully-blank rows that sit BELOW the cursor row: they are
        // unused screen space, not content, and would otherwise push real lines
        // into scrollback on reflow. The cursor row itself is always retained.
        let mut keep = phys.len();
        while keep > cursor_phys + 1 {
            let pr = &phys[keep - 1];
            let blank = pr.cells.iter().all(|c| *c == Cell::default());
            if blank && !pr.wrapped {
                keep -= 1;
            } else {
                break;
            }
        }
        phys.truncate(keep);

        // --- Step 2: join physical rows into logical lines. Track, for the
        // cursor, the absolute cell offset within its logical line.
        struct LogicalLine {
            cells: Vec<Cell>,
        }
        let mut logical: Vec<LogicalLine> = Vec::new();
        let mut cur: Vec<Cell> = Vec::new();
        let mut cursor_logical: Option<usize> = None;
        let mut cursor_offset: usize = 0;

        for (idx, pr) in phys.iter().enumerate() {
            // If the cursor lives on this physical row, its offset is the cells
            // accumulated so far in this logical line plus the cursor column.
            if idx == cursor_phys {
                cursor_logical = Some(logical.len());
                cursor_offset = cur.len() + cursor_col;
            }
            cur.extend_from_slice(&pr.cells);
            if !pr.wrapped {
                // Hard line ending — trim trailing blank cells so padding does
                // not get re-wrapped, but keep at least the cursor's reach.
                let mut end = cur.len();
                while end > 0 && cur[end - 1] == Cell::default() {
                    end -= 1;
                }
                if cursor_logical == Some(logical.len()) {
                    end = end.max(cursor_offset);
                }
                cur.truncate(end);
                logical.push(LogicalLine {
                    cells: std::mem::take(&mut cur),
                });
            }
        }
        // A trailing soft-wrapped run with no hard terminator (the live last
        // line) still forms a logical line.
        if !cur.is_empty() || cursor_logical == Some(logical.len()) {
            logical.push(LogicalLine {
                cells: std::mem::take(&mut cur),
            });
        }

        // --- Step 3: re-lay-out each logical line into new_cols-wide rows.
        let mut out_rows: Vec<Vec<Cell>> = Vec::new();
        let mut out_wrapped: Vec<bool> = Vec::new();
        // Maps (logical index, offset) -> (out_row, out_col) for the cursor.
        let mut cursor_out: Option<(usize, usize)> = None;

        for (li, line) in logical.iter().enumerate() {
            let cells = &line.cells;
            if cells.is_empty() {
                // Empty logical line -> one blank row, hard ending.
                if cursor_logical == Some(li) {
                    cursor_out = Some((out_rows.len(), 0));
                }
                out_rows.push(vec![Cell::default(); new_cols]);
                out_wrapped.push(false);
                continue;
            }
            let mut start = 0usize;
            while start < cells.len() {
                let end = (start + new_cols).min(cells.len());
                let mut row: Vec<Cell> = cells[start..end].to_vec();
                let continues = end < cells.len();
                // Cursor falls in this segment?
                if cursor_logical == Some(li)
                    && cursor_offset >= start
                    && (cursor_offset < end || (!continues && cursor_offset <= cells.len()))
                {
                    let col = (cursor_offset - start).min(new_cols.saturating_sub(1));
                    cursor_out = Some((out_rows.len(), col));
                }
                row.resize(new_cols, Cell::default());
                out_rows.push(row);
                out_wrapped.push(continues);
                start = end;
            }
        }

        if out_rows.is_empty() {
            out_rows.push(vec![Cell::default(); new_cols]);
            out_wrapped.push(false);
        }

        // --- Step 4: split into bottom `new_rows` (grid) + remainder (history).
        // Keep the cursor's row visible: if the cursor maps into history, clamp
        // the visible window down so it stays on screen.
        let total = out_rows.len();
        let mut grid_start = total.saturating_sub(new_rows);
        if let Some((cr, _)) = cursor_out {
            if cr < grid_start {
                grid_start = cr;
            }
        }
        // Rebuild history from the rows above the visible window.
        self.history.clear();
        self.history_wrapped.clear();
        for i in 0..grid_start {
            self.history.push_back(out_rows[i].clone());
            self.history_wrapped.push_back(out_wrapped[i]);
        }
        while self.history.len() > self.max_scrollback {
            self.history.pop_front();
            self.history_wrapped.pop_front();
        }

        // Rebuild the grid from the visible window, padding to new_rows.
        let mut new_grid = Grid::new(new_rows, new_cols);
        for vr in 0..new_rows {
            let src = grid_start + vr;
            if src < total {
                for (c, cell) in out_rows[src].iter().enumerate() {
                    new_grid.set(vr, c, *cell);
                }
                new_grid.set_wrapped(vr, out_wrapped[src]);
            }
        }
        self.grid = new_grid;

        // --- Step 5: place the cursor (best-effort).
        match cursor_out {
            Some((cr, cc)) if cr >= grid_start => {
                self.row = (cr - grid_start).min(new_rows - 1);
                self.col = cc.min(new_cols - 1);
            }
            _ => {
                // Cursor scrolled into history or could not be mapped: clamp to
                // the last visible row, preserving column where possible.
                self.row = new_rows - 1;
                self.col = self.col.min(new_cols - 1);
            }
        }
        self.view_offset = 0;
        self.resize_tab_stops(new_cols);
    }

    fn newline(&mut self) {
        // At the bottom margin: scroll the region up by one. Otherwise just
        // advance the row. When the region spans the whole screen, the dropped
        // top line feeds scrollback (unless on the alt screen).
        if self.row == self.scroll_bottom {
            if self.region_is_full() {
                self.scroll_up_into_history(1);
            } else {
                // Region scroll: top line is discarded (not scrollback).
                self.grid
                    .scroll_region_up(self.scroll_top, self.scroll_bottom, 1);
            }
        } else if self.row + 1 < self.grid.rows() {
            self.row += 1;
        }
    }

    /// Scroll the whole grid up by `n` lines, routing each dropped top line
    /// (with its soft-wrap flag, captured BEFORE the scroll shifts the flags)
    /// into scrollback. The grid always scrolls; the history push is suppressed
    /// on the alternate screen — a full-screen TUI (vim/less/htop) scrolling its
    /// own buffer must never flood the user's scrollback — and when scrollback
    /// is disabled. Shared by LF (`newline`) and SU (`CSI S`) on a full region.
    fn scroll_up_into_history(&mut self, n: usize) {
        let mut evicted = 0usize;
        for _ in 0..n {
            let dropped_wrapped = self.grid.is_wrapped(0);
            let dropped = self.grid.scroll_up_returning();
            if self.max_scrollback > 0 && self.saved_primary.is_none() {
                self.history.push_back(dropped);
                self.history_wrapped.push_back(dropped_wrapped);
                while self.history.len() > self.max_scrollback {
                    self.history.pop_front();
                    self.history_wrapped.pop_front();
                    evicted += 1;
                }
            }
        }
        // Rebase anchored metadata (images / prompt_marks / command_marks) by the
        // number of scrollback rows EVICTED (audit EN-2). Anchors use an absolute
        // `history.len() + grid_row` line index, which is invariant under plain
        // scrolling (grid_row falls by one exactly as history.len() rises), so no
        // rebase is needed there. But each `pop_front` shifts the whole timeline
        // down by one, so without this the anchors dangle (point at the wrong
        // physical line) once enough routine scrolling evicts scrollback —
        // previously only a full scrollback CLEAR re-anchored. The shift logic is
        // identical to a clear of `evicted` rows.
        if evicted > 0 {
            self.reanchor_after_scrollback_clear(evicted);
        }
    }

    /// Reverse index (`ESC M`): move up one line within the scroll region,
    /// scrolling the region down when already at the top margin.
    fn reverse_index(&mut self) {
        if self.row == self.scroll_top {
            self.grid
                .scroll_region_down(self.scroll_top, self.scroll_bottom, 1);
        } else if self.row > 0 {
            self.row -= 1;
        }
    }

    /// DECSC (`ESC 7`) / `CSI s`: stash cursor position, pen, and charset.
    fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            row: self.row,
            col: self.col,
            pen: self.pen.clone(),
            charset_g0: self.charset_g0,
        });
    }

    /// DECRC (`ESC 8`) / `CSI u`: restore a previously-saved cursor. When none
    /// was saved, home the cursor (xterm behaviour for a bare restore).
    fn restore_cursor(&mut self) {
        if let Some(s) = self.saved_cursor.clone() {
            self.row = s.row.min(self.grid.rows().saturating_sub(1));
            self.col = s.col.min(self.grid.cols().saturating_sub(1));
            self.pen = s.pen;
            self.charset_g0 = s.charset_g0;
        } else {
            self.row = 0;
            self.col = 0;
        }
    }

    /// RIS (`ESC c`) hard reset and DECSTR (`CSI ! p`) soft reset share the
    /// recovery surface a garbled terminal needs: clear the screen, home the
    /// cursor, reset the pen, scroll region, charsets, and saved cursor. RIS
    /// additionally drops scrollback; the `hard` flag selects that.
    fn reset_state(&mut self, hard: bool) {
        self.grid.clear();
        self.row = 0;
        self.col = 0;
        self.pen = Pen::default();
        self.scroll_top = 0;
        self.scroll_bottom = self.grid.rows().saturating_sub(1);
        self.charset_g0 = Charset::Ascii;
        self.charset_g1 = Charset::Ascii;
        self.use_g1 = false;
        self.tab_stops = default_tab_stops(self.grid.cols());
        self.saved_cursor = None;
        self.dec_modes = DecModes::default();
        self.last_print = None;
        self.insert_mode = false;
        self.reverse_screen = false;
        self.origin_mode = false;
        if hard {
            self.history.clear();
            self.history_wrapped.clear();
            self.view_offset = 0;
            self.saved_primary = None;
            // A hard reset also clears the grid (above), so every anchor's grid
            // row is gone too — drop all anchored metadata rather than re-base
            // (audit finding #4: bound the otherwise-unpruned metadata Vecs).
            self.images.clear();
            self.prompt_marks.clear();
            self.command_marks.clear();
            self.hyperlinks.clear();
            // Wipe any still-pending OSC 52 clipboard write payloads on a hard
            // reset (P-V3): each `ClipboardWrite` zeroizes its `text` on drop,
            // so explicitly clearing the queue here scrubs sensitive plaintext
            // that the app had not yet drained.
            self.clear_pending_clipboard_writes();
            // An un-drained OSC 52 read request is a program still blocking on a
            // reply. A hard reset must not strand it, so refuse each with the
            // empty-payload answer rather than silently forgetting it.
            self.deny_pending_clipboard_reads();
        }
    }

    /// Answer every un-drained OSC 52 read request with the empty-payload deny
    /// reply, then clear the queue.
    ///
    /// Used wherever pending reads must be abandoned — a hard reset, or the user
    /// switching `clipboard_read_allow` back off while a request is in flight.
    /// Answering rather than dropping preserves the no-hang guarantee: the
    /// requesting program gets a well-formed reply carrying zero clipboard bytes
    /// instead of blocking forever on one that never arrives.
    fn deny_pending_clipboard_reads(&mut self) {
        for req in std::mem::take(&mut self.pending_clipboard_reads) {
            let reply = osc::format_clipboard_reply(req.selection, "");
            self.push_pty_response(reply.as_bytes());
        }
    }

    /// Zeroize and drop every pending OSC 52 clipboard write (P-V3). Each
    /// [`ClipboardWrite`] wipes its `text` on drop, and dropping the drained
    /// `Vec` runs those `Drop` impls; resetting the `Vec` to empty afterwards
    /// frees its backing capacity. Sensitive plaintext never lingers in the
    /// freed allocation.
    fn clear_pending_clipboard_writes(&mut self) {
        // `drain(..)` yields owned `ClipboardWrite`s; dropping each at the end
        // of the loop body wipes its `text`. `Vec::clear()` would do the same
        // (it drops in place), but draining makes the wipe-on-consume contract
        // explicit and is robust to future field additions.
        for mut w in self.pending_clipboard_writes.drain(..) {
            w.zeroize();
        }
    }

    fn full_reset(&mut self) {
        self.reset_state(true);
    }

    fn sgr(&mut self, params: &Params) {
        // Each top-level param may carry colon-joined sub-parameters (e.g.
        // `4:3` curly underline, `58:2::r:g:b` underline color). Snapshot the
        // sub-parameter lists so colon-form attributes (C20) and the legacy
        // semicolon-form extended colors are both handled.
        let groups: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();
        // The flat first-subparameter view for the legacy `38;5;n` etc. walk.
        let codes: Vec<u16> = groups
            .iter()
            .map(|g| g.first().copied().unwrap_or(0))
            .collect();
        if codes.is_empty() {
            self.pen = Pen::default();
            return;
        }
        let mut i = 0;
        while i < codes.len() {
            match codes[i] {
                0 => self.pen = Pen::default(),
                1 => self.pen.flags.bold = true,
                2 => self.pen.flags.dim = true,
                3 => self.pen.flags.italic = true,
                4 => {
                    // C20 — styled underline. `4` alone = single. The colon
                    // sub-parameter form `4:n` selects the style (0=off, 1=single,
                    // 2=double, 3=curly, 4=dotted, 5=dashed).
                    let style = match groups[i].get(1).copied() {
                        None | Some(1) => UnderlineStyle::Single,
                        Some(0) => UnderlineStyle::None,
                        Some(2) => UnderlineStyle::Double,
                        Some(3) => UnderlineStyle::Curly,
                        Some(4) => UnderlineStyle::Dotted,
                        Some(5) => UnderlineStyle::Dashed,
                        Some(_) => UnderlineStyle::Single,
                    };
                    self.pen.flags.underline_style = style;
                }
                5 => self.pen.flags.blink = true,
                6 => self.pen.flags.rapid_blink = true,
                7 => self.pen.flags.inverse = true,
                8 => self.pen.flags.conceal = true,
                9 => self.pen.flags.strikeout = true,
                21 => self.pen.flags.underline_style = UnderlineStyle::Double,
                22 => {
                    // ECMA-48: "normal intensity" cancels BOTH bold (1) and
                    // faint (2) — there is no separate dim-off code.
                    self.pen.flags.bold = false;
                    self.pen.flags.dim = false;
                }
                23 => self.pen.flags.italic = false,
                24 => self.pen.flags.underline_style = UnderlineStyle::None,
                25 => {
                    // "Blink off" cancels both the slow (5) and rapid (6) rates.
                    self.pen.flags.blink = false;
                    self.pen.flags.rapid_blink = false;
                }
                27 => self.pen.flags.inverse = false,
                28 => self.pen.flags.conceal = false,
                29 => self.pen.flags.strikeout = false,
                30..=37 => self.pen.fg = Color::Indexed((codes[i] - 30) as u8),
                40..=47 => self.pen.bg = Color::Indexed((codes[i] - 40) as u8),
                90..=97 => self.pen.fg = Color::Indexed((codes[i] - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((codes[i] - 100 + 8) as u8),
                39 => self.pen.fg = Color::Default,
                49 => self.pen.bg = Color::Default,
                58 => {
                    // C20 — underline color. Colon form `58:2::r:g:b` (RGB) or
                    // `58:5:n` (indexed) carries everything in one group;
                    // semicolon form `58;2;r;g;b` / `58;5;n` spreads across the
                    // following top-level params (consumed via `i` advance).
                    if let Some(color) = self.parse_extended_color(&groups[i], &codes, &mut i) {
                        self.pen.underline_color = Some(color);
                    }
                }
                53 => self.pen.flags.overline = true,
                55 => self.pen.flags.overline = false,
                59 => self.pen.underline_color = None,
                38 | 48 => {
                    let target_is_fg = codes[i] == 38;
                    if let Some(color) = self.parse_extended_color(&groups[i], &codes, &mut i) {
                        if target_is_fg {
                            self.pen.fg = color;
                        } else {
                            self.pen.bg = color;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// Parse an extended-color operand for SGR 38/48/58. Handles BOTH the colon
    /// sub-parameter form (everything packed into `group`, e.g. `38:2::r:g:b` or
    /// `38:5:n`) and the legacy semicolon form (spread across the following
    /// top-level `codes`, consumed by advancing `i`). Returns the parsed color,
    /// or `None` when the operand is malformed/truncated.
    fn parse_extended_color(&self, group: &[u16], codes: &[u16], i: &mut usize) -> Option<Color> {
        // Colon sub-parameter form: the kind + channels live inside `group`.
        if group.len() >= 2 {
            let kind = group[1];
            if kind == 5 {
                // CLAMP, never wrap: an out-of-range palette index (>255) must
                // saturate to 255, not wrap mod 256 (a bare `as u8` turns
                // `38:5:300` into index 44 — a silently wrong colour). `clamp_u8`.
                return group.get(2).map(|&n| Color::Indexed(clamp_u8(n)));
            }
            if kind == 2 {
                // `58:2::r:g:b` carries an empty colorspace slot at index 2, so
                // the channels may start at index 2 OR 3. Take the LAST three
                // values present after the kind as r/g/b.
                let chans: Vec<u16> = group[2..].to_vec();
                if chans.len() >= 3 {
                    let r = clamp_u8(chans[chans.len() - 3]);
                    let g = clamp_u8(chans[chans.len() - 2]);
                    let b = clamp_u8(chans[chans.len() - 1]);
                    return Some(Color::Rgb(r, g, b));
                }
                return None;
            }
            return None;
        }
        // Legacy semicolon form: kind + channels are subsequent top-level codes.
        match codes.get(*i + 1).copied() {
            Some(5) => {
                let n = codes.get(*i + 2).copied()?;
                *i += 2;
                Some(Color::Indexed(clamp_u8(n)))
            }
            Some(2) => {
                let r = codes.get(*i + 2).copied()?;
                let g = codes.get(*i + 3).copied()?;
                let b = codes.get(*i + 4).copied()?;
                *i += 4;
                Some(Color::Rgb(clamp_u8(r), clamp_u8(g), clamp_u8(b)))
            }
            _ => None,
        }
    }

    /// Resolve a 0-based target row for an absolute cursor move (CUP/VPA),
    /// honouring DECOM origin mode (C25). When origin mode is set, the row is
    /// relative to `scroll_top` and clamped to `[scroll_top, scroll_bottom]`;
    /// otherwise it is an absolute screen row clamped to the grid.
    fn resolve_row(&self, target: usize) -> usize {
        if self.origin_mode {
            (self.scroll_top + target).min(self.scroll_bottom)
        } else {
            target.min(self.grid.rows().saturating_sub(1))
        }
    }

    /// DECRQM reply (C33): emit `CSI [?] Ps ; Pv $ y` reporting mode `ps`'s
    /// current value. `Pv` is 1 (set), 2 (reset), or 0 (not recognised).
    fn report_mode(&mut self, ps: u16, private: bool) {
        let value: u8 = if private {
            match ps {
                1 => bool_mode(self.dec_modes.application_cursor_keys),
                5 => bool_mode(self.reverse_screen),
                6 => bool_mode(self.origin_mode),
                7 => bool_mode(self.dec_modes.autowrap),
                12 => bool_mode(self.dec_modes.cursor_blink),
                25 => bool_mode(self.dec_modes.cursor_visible),
                1000 => bool_mode(self.dec_modes.mouse_mode == MouseMode::Normal),
                1002 => bool_mode(self.dec_modes.mouse_mode == MouseMode::ButtonEvent),
                1003 => bool_mode(self.dec_modes.mouse_mode == MouseMode::AnyEvent),
                1004 => bool_mode(self.dec_modes.focus_reporting),
                1006 => bool_mode(self.dec_modes.mouse_encoding == MouseEncoding::Sgr),
                2004 => bool_mode(self.dec_modes.bracketed_paste),
                2026 => bool_mode(self.dec_modes.sync_output),
                47 | 1047 | 1049 => bool_mode(self.saved_primary.is_some()),
                _ => 0,
            }
        } else {
            match ps {
                4 => bool_mode(self.insert_mode), // IRM
                _ => 0,
            }
        };
        let lead = if private { "?" } else { "" };
        let resp = format!("\x1b[{lead}{ps};{value}$y");
        self.push_pty_response(resp.as_bytes());
    }

    /// XTGETTCAP reply (C30): the DCS `q` payload is a space-separated list of
    /// hex-encoded terminfo capability names. For each recognised capability we
    /// reply `DCS 1 + r <hex-name> = <hex-value> ST`; for unrecognised ones we
    /// reply the invalid form `DCS 0 + r <hex-name> ST`. Apps that gate styled
    /// underline / truecolor on this must get a reply or they hang.
    fn report_xtgettcap(&mut self, payload: &[u8]) {
        for token in payload.split(|&b| b == b';') {
            if token.is_empty() {
                continue;
            }
            let Some(name) = hex_decode(token) else {
                continue;
            };
            // Recognised capabilities. `Co`/`colors` = 256, `TN` (terminal name)
            // = "xterm-256color", `RGB` = present (truecolor).
            let value: Option<&str> = match name.as_str() {
                "Co" | "colors" => Some("256"),
                "TN" | "name" => Some("xterm-256color"),
                "RGB" => Some(""), // boolean capability — present, empty value.
                _ => None,
            };
            let resp = match value {
                Some(v) => {
                    let name_hex = hex_encode(name.as_bytes());
                    if v.is_empty() {
                        format!("\x1bP1+r{name_hex}\x1b\\")
                    } else {
                        let val_hex = hex_encode(v.as_bytes());
                        format!("\x1bP1+r{name_hex}={val_hex}\x1b\\")
                    }
                }
                None => {
                    let name_hex = hex_encode(name.as_bytes());
                    format!("\x1bP0+r{name_hex}\x1b\\")
                }
            };
            self.push_pty_response(resp.as_bytes());
        }
    }

    fn first_param(params: &Params, default: u16) -> usize {
        let v = params
            .iter()
            .next()
            .and_then(|p| p.first().copied())
            .unwrap_or(0);
        if v == 0 {
            default as usize
        } else {
            v as usize
        }
    }

    /// The current kitty keyboard-protocol flags (top of the stack, or 0).
    fn kitty_kbd_flags(&self) -> u8 {
        *self.kitty_kbd_stack.last().unwrap_or(&0)
    }

    /// Handle a kitty-keyboard-protocol control sequence. The caller has already
    /// established that `action == 'u'` and that a distinguishing intermediate
    /// (`?` / `>` / `<` / `=`) is present, so the bare-`CSI u` SCORC alias never
    /// reaches here. `nth_param` reads the n-th `;`-separated CSI parameter's
    /// first value (kitty's control params carry no `:` sub-params).
    fn kitty_keyboard_control(&mut self, params: &Params, intermediates: &[u8]) {
        let nth_param = |n: usize| -> Option<u8> {
            params
                .iter()
                .nth(n)
                .and_then(|p| p.first().copied())
                .map(|v| v as u8)
        };
        if intermediates.contains(&b'?') {
            // Query current flags: reply `CSI ? <flags> u` (decimal).
            let resp = format!("\x1b[?{}u", self.kitty_kbd_flags());
            self.push_pty_response(resp.as_bytes());
        } else if intermediates.contains(&b'>') {
            // Push: param0 (default 0) onto the stack, bounded at the cap.
            let flags = nth_param(0).unwrap_or(0);
            if self.kitty_kbd_stack.len() >= KITTY_KBD_STACK_MAX {
                // Drop the oldest entry to keep memory bounded under a hostile
                // unbalanced-push stream; kitty's own stack is likewise bounded.
                self.kitty_kbd_stack.remove(0);
            }
            self.kitty_kbd_stack.push(flags);
        } else if intermediates.contains(&b'<') {
            // Pop: n (default 1) entries, saturating to an empty stack.
            let n = nth_param(0)
                .map(|v| v as usize)
                .filter(|&v| v != 0)
                .unwrap_or(1);
            let new_len = self.kitty_kbd_stack.len().saturating_sub(n);
            self.kitty_kbd_stack.truncate(new_len);
        } else if intermediates.contains(&b'=') {
            // Set: flags = param0 (default 0), mode = param1 (default 1).
            //   mode 1 = replace all bits; 2 = OR (set the given bits);
            //   3 = AND-NOT (clear the given bits). Operates on the current top;
            //   when the stack is empty the current value is treated as 0 and the
            //   result is pushed so the negotiation takes effect immediately.
            let flags = nth_param(0).unwrap_or(0);
            let mode = nth_param(1).filter(|&v| v != 0).unwrap_or(1);
            let current = self.kitty_kbd_flags();
            let next = match mode {
                2 => current | flags,
                3 => current & !flags,
                _ => flags, // mode 1 (and any unrecognised mode) replaces.
            };
            if let Some(top) = self.kitty_kbd_stack.last_mut() {
                *top = next;
            } else {
                self.kitty_kbd_stack.push(next);
            }
        }
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        use unicode_width::UnicodeWidthChar;

        // Apply the active charset (DEC line-drawing maps 0x60..0x7e).
        let active = if self.use_g1 {
            self.charset_g1
        } else {
            self.charset_g0
        };
        let c = if active == Charset::DecLineDrawing {
            dec_line_draw(c)
        } else {
            c
        };

        // C27 / C34 — a zero-width char (a combining mark, or a variation
        // selector VS15 U+FE0E / VS16 U+FE0F) does not occupy its own cell: it
        // attaches to the previous cell's base grapheme. This keeps `é` (e +
        // U+0301) and emoji presentation selectors in a single cell.
        let raw_width = UnicodeWidthChar::width(c).unwrap_or(0);
        if (raw_width == 0 || is_variation_selector(c)) && self.col > 0 {
            let prev = self.col - 1;
            // Append to the base cell, not the wide-glyph continuation spacer.
            let base = if prev > 0 && self.grid.is_continuation(self.row, prev) {
                prev - 1
            } else {
                prev
            };
            self.grid.push_combining_at(self.row, base, c);
            return;
        }

        self.last_print = Some(c);

        // East-Asian width: a wide glyph occupies two columns. A leading
        // zero-width char (no previous cell to attach to) still lands in a cell
        // so it is not lost.
        let width = raw_width.max(1);
        let cols = self.grid.cols();

        // IRM (insert mode, C25): when set, a printed glyph SHIFTS the rest of
        // the line right by its width rather than overwriting.
        if self.insert_mode && self.col < cols {
            self.grid.insert_blanks(self.row, self.col, width);
        }

        if self.col >= cols {
            if self.dec_modes.autowrap {
                // The current row filled to the width: mark it soft-wrapped so
                // reflow on resize can rejoin it with the continuation row.
                self.grid.set_wrapped(self.row, true);
                self.col = 0;
                self.newline();
            } else {
                // DECAWM off (`?7l`): clamp to the last column and overwrite it
                // in place rather than wrapping to the next line.
                self.col = cols - 1;
            }
        }

        // A width-2 glyph that would straddle the right edge wraps to the next
        // line first (the trailing cell is left blank), matching xterm.
        if width == 2 && self.col + 1 >= cols {
            if self.dec_modes.autowrap {
                self.grid.set_wrapped(self.row, true);
                self.col = 0;
                self.newline();
            } else {
                // No room and no wrap: drop the wide glyph rather than corrupt.
                return;
            }
        }

        self.grid.set(
            self.row,
            self.col,
            Cell {
                c,
                fg: self.pen.fg,
                bg: self.pen.bg,
                flags: self.pen.flags,
                underline_color: self.pen.underline_color,
            },
        );
        self.col += 1;
        if width == 2 {
            // Continuation cell: a blank spacer that the renderer skips. Keeping
            // it as a space keeps grid->text extraction sane (one visible glyph).
            self.grid.set_continuation(
                self.row,
                self.col,
                Cell {
                    c: ' ',
                    fg: self.pen.fg,
                    bg: self.pen.bg,
                    flags: self.pen.flags,
                    underline_color: self.pen.underline_color,
                },
            );
            self.col += 1;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.col = 0,
            b'\t' => self.tab_forward(1),
            0x08 => {
                self.col = self.col.saturating_sub(1);
            }
            0x0e => self.use_g1 = true,  // SO — invoke G1 into GL.
            0x0f => self.use_g1 = false, // SI — return to G0.
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        // Charset designation: `ESC ( <set>` for G0, `ESC ) <set>` for G1.
        if intermediates == b"(" || intermediates == b")" {
            let cs = match byte {
                b'0' => Charset::DecLineDrawing,
                // B = US-ASCII; 'A'/'1'/'2' are other 8-bit-ish sets we treat as
                // ASCII (we don't draw their glyph variants).
                _ => Charset::Ascii,
            };
            if intermediates == b"(" {
                self.charset_g0 = cs;
            } else {
                self.charset_g1 = cs;
            }
            return;
        }
        // Non-intermediate single-byte escapes.
        if intermediates.is_empty() {
            match byte {
                b'7' => self.save_cursor(),    // DECSC
                b'8' => self.restore_cursor(), // DECRC
                b'H' => self.set_tab_stop(),   // HTS — set tab stop at cursor.
                b'D' => self.newline(),        // IND — index (down, scroll region).
                b'M' => self.reverse_index(),  // RI — reverse index (up).
                b'E' => {
                    // NEL — next line: CR + LF.
                    self.col = 0;
                    self.newline();
                }
                b'c' => self.full_reset(), // RIS — reset to initial state.
                _ => {}
            }
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // DECRQM — request mode (C33): `CSI ? Ps $ p` (private) or
        // `CSI Ps $ p` (ANSI). Reply `CSI [?] Ps ; Pv $ y` where Pv is the
        // mode value (0 = unrecognised, 1 = set, 2 = reset). Apps that probe
        // modes hang without a reply.
        if action == 'p' && intermediates.contains(&b'$') {
            let ps = params
                .iter()
                .next()
                .and_then(|p| p.first().copied())
                .unwrap_or(0);
            let private = intermediates.contains(&b'?');
            self.report_mode(ps, private);
            return;
        }
        // DEC private mode set/reset: `CSI ? Pm h` / `CSI ? Pm l`. The `?`
        // arrives in the intermediates. Every param in the sequence is applied
        // (e.g. `CSI ? 1049 ; 1006 h` toggles both modes).
        if intermediates.contains(&b'?') && (action == 'h' || action == 'l') {
            let set = action == 'h';
            for p in params.iter() {
                if let Some(&m) = p.first() {
                    self.set_dec_mode(m, set);
                }
            }
            return;
        }
        // ANSI (non-private) mode set/reset: `CSI Pm h` / `CSI Pm l`. The only
        // ANSI mode we model is IRM (insert/replace, mode 4 — C25).
        if intermediates.is_empty() && (action == 'h' || action == 'l') {
            let set = action == 'h';
            for p in params.iter() {
                if p.first().copied() == Some(4) {
                    self.insert_mode = set;
                }
            }
            return;
        }
        // DECSCUSR: `CSI Ps SP q` — the space (0x20) is the intermediate.
        if action == 'q' && intermediates.contains(&b' ') {
            let ps = params
                .iter()
                .next()
                .and_then(|p| p.first().copied())
                .unwrap_or(0);
            self.set_cursor_shape(ps);
            return;
        }
        // DECSTR soft reset: `CSI ! p` — the `!` is the intermediate.
        if action == 'p' && intermediates.contains(&b'!') {
            self.reset_state(false);
            return;
        }
        // Kitty keyboard protocol progressive-enhancement control sequences
        // (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>). These ALL carry
        // a distinguishing intermediate (`?`, `>`, `<`, or `=`); the bare `CSI u`
        // with NO intermediate is the unrelated ANSI.SYS SCORC cursor-restore
        // alias handled in the `match action` below — it MUST stay untouched.
        if action == 'u'
            && (intermediates.contains(&b'?')
                || intermediates.contains(&b'>')
                || intermediates.contains(&b'<')
                || intermediates.contains(&b'='))
        {
            self.kitty_keyboard_control(params, intermediates);
            return;
        }
        match action {
            'm' => self.sgr(params),
            'H' | 'f' => {
                // Cursor position: row;col, 1-based.
                let mut it = params.iter();
                let row = it
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1)
                    .max(1) as usize;
                let col = it
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1)
                    .max(1) as usize;
                self.row = self.resolve_row(row - 1);
                self.col = (col - 1).min(self.grid.cols() - 1);
            }
            'A' => {
                // CUU — cursor up N. Stops at the TOP scroll margin when the
                // cursor starts inside/below the region; a cursor above the
                // region is bounded by the physical top (row 0). Per DEC STD
                // 070 / xterm the cursor must not cross the margin it is bounded
                // by — without this, relative motion below a DECSTBM status-line
                // region walks into the reserved rows.
                let n = Self::first_param(params, 1);
                let floor = if self.row >= self.scroll_top {
                    self.scroll_top
                } else {
                    0
                };
                self.row = self.row.saturating_sub(n).max(floor);
            }
            'B' => {
                // CUD — cursor down N. Stops at the BOTTOM scroll margin when the
                // cursor starts inside/above the region; below the region it is
                // bounded by the physical bottom (xterm/DEC STD 070).
                let n = Self::first_param(params, 1);
                let ceil = if self.row <= self.scroll_bottom {
                    self.scroll_bottom
                } else {
                    self.grid.rows() - 1
                };
                self.row = (self.row + n).min(ceil);
            }
            'C' => self.col = (self.col + Self::first_param(params, 1)).min(self.grid.cols() - 1),
            'D' => self.col = self.col.saturating_sub(Self::first_param(params, 1)),
            'E' => {
                // CNL — cursor next line: down N, column 1.
                self.row = (self.row + Self::first_param(params, 1)).min(self.grid.rows() - 1);
                self.col = 0;
            }
            'F' => {
                // CPL — cursor previous line: up N, column 1.
                self.row = self.row.saturating_sub(Self::first_param(params, 1));
                self.col = 0;
            }
            'G' => {
                // CHA — cursor horizontal absolute (1-based column).
                self.col = (Self::first_param(params, 1) - 1).min(self.grid.cols() - 1);
            }
            'I' => {
                // CHT — cursor forward tabulation (advance N tab stops).
                self.tab_forward(Self::first_param(params, 1));
            }
            'Z' => {
                // CBT — cursor backward tabulation (back N tab stops).
                self.tab_back(Self::first_param(params, 1));
            }
            'g' => {
                // TBC — tab clear. 0 = clear stop at cursor, 3 = clear all.
                let mode = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                self.clear_tab_stop(mode);
            }
            'd' => {
                // VPA — vertical position absolute (1-based row). Honours DECOM.
                self.row = self.resolve_row(Self::first_param(params, 1) - 1);
            }
            'r' => {
                // DECSTBM — set top/bottom scroll margins (1-based). No params
                // (or both default) resets to the full screen.
                let mut it = params.iter();
                let top = it.next().and_then(|p| p.first().copied()).unwrap_or(0) as usize;
                let bot = it.next().and_then(|p| p.first().copied()).unwrap_or(0) as usize;
                let last = self.grid.rows() - 1;
                let top = if top == 0 { 0 } else { top - 1 };
                let bottom = if bot == 0 { last } else { (bot - 1).min(last) };
                // A valid region needs top < bottom; otherwise reset to full.
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                } else {
                    self.scroll_top = 0;
                    self.scroll_bottom = last;
                }
                // DECSTBM homes the cursor (to origin / top-left).
                self.row = self.scroll_top;
                self.col = 0;
            }
            'L'
                // IL — insert N blank lines at the cursor row, within the scroll
                // region. Lines below shift down; lines past the bottom are lost.
                if self.row >= self.scroll_top && self.row <= self.scroll_bottom => {
                    let n = Self::first_param(params, 1);
                    self.grid
                        .scroll_region_down(self.row, self.scroll_bottom, n);
                    self.col = 0;
                }
            'M'
                // DL — delete N lines at the cursor row, within the scroll
                // region. Lines below shift up; blanks fill at the bottom.
                if self.row >= self.scroll_top && self.row <= self.scroll_bottom => {
                    let n = Self::first_param(params, 1);
                    self.grid.scroll_region_up(self.row, self.scroll_bottom, n);
                    self.col = 0;
                }
            'S' => {
                // SU — scroll the scroll region up N lines. On a full-screen
                // region (no margins) the scrolled-off top lines feed scrollback
                // exactly like an LF-driven scroll (xterm behaviour) — a `tput
                // indn`/pager `CSI n S` otherwise loses lines an LF would have
                // preserved. A margined region discards them (no scrollback).
                let n = Self::first_param(params, 1);
                if self.region_is_full() {
                    self.scroll_up_into_history(n);
                } else {
                    self.grid
                        .scroll_region_up(self.scroll_top, self.scroll_bottom, n);
                }
            }
            'T' => {
                // SD — scroll the scroll region down N lines.
                let n = Self::first_param(params, 1);
                self.grid
                    .scroll_region_down(self.scroll_top, self.scroll_bottom, n);
            }
            '@' => {
                // ICH — insert N blank characters at the cursor (shift right).
                let n = Self::first_param(params, 1);
                self.grid.insert_blanks(self.row, self.col, n);
            }
            'P' => {
                // DCH — delete N characters at the cursor (shift left).
                let n = Self::first_param(params, 1);
                self.grid.delete_chars(self.row, self.col, n);
            }
            'X' => {
                // ECH — erase N characters at the cursor (blank, no shift).
                let n = Self::first_param(params, 1);
                self.grid.erase_chars(self.row, self.col, n);
            }
            'b' => {
                // REP — repeat the last printed grapheme N times.
                //
                // `n` is an attacker-controllable CSI parameter and `print`
                // scrolls the whole grid + scrollback, so an unbounded loop
                // (`x\x1b[2000000000b`) would freeze the reader thread — the
                // iTerm2 REP DoS class. Clamp to `MAX_REP`: repeating a single
                // grapheme more than that just scrolls identical content off the
                // top of the scrollback, so the clamp is lossless for any
                // realistic use (filling an 80×10000 scrollback is ~800K) while
                // killing the DoS. See dgl.cx/2023/09/ansi-terminal-security.
                if let Some(c) = self.last_print {
                    const MAX_REP: usize = 1 << 20; // 1,048,576
                    let n = Self::first_param(params, 1).min(MAX_REP);
                    for _ in 0..n {
                        self.print(c);
                    }
                }
            }
            's' => self.save_cursor(),    // SCOSC — ANSI.SYS save cursor.
            'u' => self.restore_cursor(), // SCORC — ANSI.SYS restore cursor.
            'c' => {
                // DA — device attributes. Primary (`CSI c` / `CSI 0 c`) reports
                // a VT220-class terminal with 132-column + selective-erase. The
                // `>` intermediate selects secondary DA (terminal id + version).
                if intermediates.contains(&b'>') {
                    // Secondary DA: CSI > 0 ; 0 ; 0 c  (VT100-family, no firmware).
                    self.push_pty_response(b"\x1b[>0;0;0c");
                } else {
                    // Primary DA: VT220 with 132-col (1), selective erase (6),
                    // ANSI color (22). Capability-probing apps need a reply or
                    // they hang.
                    self.push_pty_response(b"\x1b[?62;1;6;22c");
                }
            }
            'n' => {
                // DSR — device status report. `5n` → terminal OK; `6n` (and the
                // DECXCPR `?6n` form) → cursor position report.
                let ps = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                match ps {
                    // Route BOTH replies through push_pty_response so the
                    // PTY_RESPONSE_MAX write-amplification cap is enforced — a
                    // `\x1b[5n`/`\x1b[6n` flood between UI drains would otherwise
                    // grow pty_response without bound (the cap's own doc claims
                    // it is the single sink for every device reply).
                    5 => self.push_pty_response(b"\x1b[0n"),
                    6 => {
                        // CPR: 1-based row;col. The DEC private form (`?6n`) uses
                        // the same body here.
                        let resp = format!("\x1b[{};{}R", self.row + 1, self.col + 1);
                        self.push_pty_response(resp.as_bytes());
                    }
                    _ => {}
                }
            }
            'J' => {
                // Erase in display. 0 = cursor→end, 1 = start→cursor, 2 = all,
                // 3 = all + scrollback.
                let mode = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let cols = self.grid.cols();
                let rows = self.grid.rows();
                match mode {
                    0 => {
                        // Cursor to end of screen.
                        for c in self.col..cols {
                            self.grid.set(self.row, c, Cell::default());
                        }
                        for r in (self.row + 1)..rows {
                            for c in 0..cols {
                                self.grid.set(r, c, Cell::default());
                            }
                        }
                    }
                    1 => {
                        // Start of screen to cursor (inclusive).
                        for r in 0..self.row {
                            for c in 0..cols {
                                self.grid.set(r, c, Cell::default());
                            }
                        }
                        for c in 0..=self.col.min(cols - 1) {
                            self.grid.set(self.row, c, Cell::default());
                        }
                    }
                    2 => {
                        self.grid.clear();
                        self.row = 0;
                        self.col = 0;
                    }
                    3 => {
                        // Erase scrollback (the `clear` command's second half) —
                        // the SHARED path the right-click "Clear scrollback" menu
                        // item and the Ctrl+Shift+K chord also take. The live grid
                        // stays; anchored metadata is re-based / dropped inside
                        // `clear_scrollback` (audit #4).
                        self.clear_scrollback();
                    }
                    _ => {}
                }
            }
            'K' => {
                // Erase in line. 0 = cursor→EOL, 1 = BOL→cursor, 2 = whole line.
                let mode = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let cols = self.grid.cols();
                match mode {
                    0 => {
                        for c in self.col..cols {
                            self.grid.set(self.row, c, Cell::default());
                        }
                    }
                    1 => {
                        for c in 0..=self.col.min(cols - 1) {
                            self.grid.set(self.row, c, Cell::default());
                        }
                    }
                    2 => {
                        for c in 0..cols {
                            self.grid.set(self.row, c, Cell::default());
                        }
                    }
                    _ => {}
                }
            }
            't' => {
                // XTWINOPS. We implement only the title-stack ops; geometry
                // window-manipulation ops are intentionally ignored (the app
                // owns the window). 22;n pushes the title, 23;n pops it; n
                // selects icon(1)/window(2)/both(0) title — we keep a single
                // title, so n is accepted but treated uniformly.
                let op = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                match op {
                    22 => self.push_title(),
                    23 => self.pop_title(),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        let code = std::str::from_utf8(params[0]).ok();
        match code {
            Some("0") | Some("2") => {
                if let Some(t) = params.get(1).and_then(|p| std::str::from_utf8(p).ok()) {
                    // Cap the STORED title length. A title is a short window label;
                    // an escape sequence that stuffs a multi-megabyte OSC 2 string
                    // is a memory-DoS / desktop-flood vector (CyberArk title-abuse
                    // class), so retain at most TITLE_MAX_CHARS. The tab strip
                    // already truncates the DISPLAY; this bounds the stored value.
                    self.title = t.chars().take(Self::TITLE_MAX_CHARS).collect();
                }
            }
            Some("7") => {
                if let Some(uri) = params.get(1).and_then(|p| std::str::from_utf8(p).ok()) {
                    self.cwd = Some(uri.to_string());
                }
            }
            Some("8") => {
                // OSC 8 ; params ; URI  — capture non-empty URIs for click/open.
                if let Some(uri) = params.get(2).and_then(|p| std::str::from_utf8(p).ok()) {
                    if !uri.is_empty() {
                        self.push_hyperlink(uri.to_string());
                    }
                }
            }
            Some("133") => {
                // OSC 133 semantic-prompt zones. A/B mark prompt start/end
                // (jump-to-prompt); C/D mark command-output-start / command-end
                // with an optional exit code (C28). We never route any
                // report-back into the PTY (security: iTerm2 CVE-2024-38395/
                // 38396 class — capture only).
                let kind = params.get(1).copied();
                let abs = self.history.len() + self.row;
                match kind {
                    Some(b"A") | Some(b"B") if self.prompt_marks.last() != Some(&abs) => {
                        self.push_prompt_mark(abs);
                    }
                    Some(b"A") | Some(b"B") => {}
                    Some(b"C") => {
                        self.push_command_mark(osc::CommandMark {
                            kind: osc::CommandMarkKind::OutputStart,
                            line: abs,
                        });
                    }
                    Some(b"D") => {
                        // `OSC 133 ; D [; exit_code]`. The exit code may be the
                        // 3rd field directly or embedded in a `D;aid=…` form;
                        // parse the first field that looks like an integer.
                        let exit_code = params.get(2).and_then(|p| {
                            std::str::from_utf8(p).ok().and_then(|s| {
                                s.split([';', '='])
                                    .find_map(|tok| tok.trim().parse::<i32>().ok())
                            })
                        });
                        self.push_command_mark(osc::CommandMark {
                            kind: osc::CommandMarkKind::CommandEnd { exit_code },
                            line: abs,
                        });
                    }
                    _ => {}
                }
            }
            Some("4") => self.handle_osc_4(params),
            Some("10") => self.handle_dynamic_color(params, DynamicColor::Foreground),
            Some("11") => self.handle_dynamic_color(params, DynamicColor::Background),
            Some("12") => self.handle_dynamic_color(params, DynamicColor::Cursor),
            Some("52") => self.handle_osc_52(params),
            Some("104") => self.handle_osc_104(params),
            Some("110") => self.reset_dynamic_color(DynamicColor::Foreground),
            Some("111") => self.reset_dynamic_color(DynamicColor::Background),
            Some("112") => self.reset_dynamic_color(DynamicColor::Cursor),
            Some("9") if params.get(1).copied() == Some(b"4".as_slice()) => {
                // OSC 9 ; 4 ; state ; percent — taskbar/tab progress (C26).
                self.handle_osc_9_4(params);
            }
            Some("9") => {
                // OSC 9: desktop notification (body only).
                if let Some(body) = params
                    .get(1)
                    .and_then(|p| std::str::from_utf8(p).ok())
                    .filter(|b| !b.is_empty())
                {
                    self.pending_notifications.push(Notification {
                        title: String::new(),
                        body: body.to_string(),
                    });
                    while self.pending_notifications.len() > Self::NOTIFICATIONS_MAX {
                        self.pending_notifications.remove(0);
                    }
                }
            }
            // OSC 777: rxvt-unicode extension.
            // Format: OSC 777 ; notify ; <title> ; <body> ST
            Some("777") if params.get(1).copied() == Some(b"notify".as_slice()) => {
                let title = params
                    .get(2)
                    .and_then(|p| std::str::from_utf8(p).ok())
                    .unwrap_or("")
                    .to_string();
                let body = params
                    .get(3)
                    .and_then(|p| std::str::from_utf8(p).ok())
                    .unwrap_or("")
                    .to_string();
                if !title.is_empty() || !body.is_empty() {
                    self.pending_notifications
                        .push(Notification { title, body });
                    while self.pending_notifications.len() > Self::NOTIFICATIONS_MAX {
                        self.pending_notifications.remove(0);
                    }
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, _params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // `DCS + q … ST` is an XTGETTCAP capability request (C30) — the `+`
        // arrives as an intermediate. `DCS q …` (no intermediate) is a Sixel
        // image. The two are disambiguated by the intermediate.
        if action == 'q' && intermediates.contains(&b'+') {
            self.xtgettcap_accum = Some(Vec::new());
        } else if action == 'q' {
            self.sixel_accum = Some(Vec::new());
        }
    }

    fn put(&mut self, byte: u8) {
        if let Some(buf) = &mut self.sixel_accum {
            // Bound the payload so a hostile stream can't exhaust memory.
            if buf.len() < 8 * 1024 * 1024 {
                buf.push(byte);
            }
        } else if let Some(buf) = &mut self.xtgettcap_accum {
            // XTGETTCAP names are tiny; bound generously against hostile input.
            if buf.len() < 4096 {
                buf.push(byte);
            }
        }
    }

    fn unhook(&mut self) {
        if let Some(buf) = self.sixel_accum.take() {
            if let Some(img) = crate::image::decode_sixel(&buf) {
                let line = self.history.len() + self.row;
                self.push_image(TerminalImage {
                    image: img,
                    line,
                    col: self.col,
                });
            }
        } else if let Some(buf) = self.xtgettcap_accum.take() {
            self.report_xtgettcap(&buf);
        }
    }
}

impl Screen {
    // ------------------------------------------------------------------
    // Kitty graphics protocol (APC `ESC _ G ... ST`). Driven by the APC
    // pre-filter in `Terminal::advance`, which extracts complete bodies the
    // `vte` parser silently discards (vte 0.15 has no APC `Perform` callback).
    // ------------------------------------------------------------------

    /// Caps on the transmit-store so a hostile `a=t` stream cannot exhaust
    /// memory: at most this many stored images, and this many total bytes.
    const KITTY_STORE_MAX_IMAGES: usize = 64;
    const KITTY_STORE_MAX_BYTES: usize = 64 * 1024 * 1024;

    /// Handle one complete Kitty graphics APC body: the bytes between `ESC _`
    /// and the ST terminator, i.e. starting with `G`. Non-`G` APCs are the
    /// caller's responsibility to filter out (it only routes `ESC _ G` here).
    fn handle_kitty_apc(&mut self, body: &[u8]) {
        // Strip the leading 'G' graphics introducer.
        let body = match body.split_first() {
            Some((b'G', rest)) => rest,
            _ => return,
        };
        let cmd = match crate::image::parse_kitty(body) {
            Some(c) => c,
            None => return,
        };

        match cmd.action {
            'd' => {
                // Delete: clear all stored images and any in-flight chunks.
                self.kitty_store.clear();
                self.kitty_chunks.clear();
            }
            'p' => {
                // Display a previously transmit-stored image at the cursor.
                if let Some(img) = self.kitty_store.get(&cmd.id).cloned() {
                    self.place_kitty_image(img);
                }
            }
            't' | 'T' => {
                // Accumulate this chunk's base64 text keyed by image id. Format
                // + dimensions ride on the FIRST chunk only (continuation
                // chunks resend just `m` + payload), so capture them at creation
                // and reuse them when the m=0 boundary finalises the image.
                //
                // Bound the number of distinct in-flight chunk sets: a hostile
                // stream can open many ids and never send the finalizing m=0
                // chunk. Refuse a NEW id once the in-flight count is at the cap
                // (existing transfers still complete); each set's payload is
                // independently byte-capped below.
                if !self.kitty_chunks.contains_key(&cmd.id)
                    && self.kitty_chunks.len() >= Self::KITTY_CHUNKS_MAX
                {
                    return;
                }
                let chunk = self
                    .kitty_chunks
                    .entry(cmd.id)
                    .or_insert_with(|| KittyChunk {
                        payload: Vec::new(),
                        format: cmd.format,
                        width: cmd.width,
                        height: cmd.height,
                    });
                if chunk.payload.len() + cmd.payload.len() <= 8 * 1024 * 1024 {
                    chunk.payload.extend_from_slice(&cmd.payload);
                } else {
                    // Oversized accumulation — drop the in-flight chunk set.
                    self.kitty_chunks.remove(&cmd.id);
                    return;
                }
                if cmd.more {
                    // More chunks follow — wait for the m=0 boundary.
                    return;
                }
                // Final (or only) chunk: decode the accumulated base64 once,
                // then decode pixels per the FIRST chunk's declared format.
                let chunk = match self.kitty_chunks.remove(&cmd.id) {
                    Some(c) => c,
                    None => return,
                };
                let raw = match osc::base64_decode(&chunk.payload) {
                    Some(r) => r,
                    None => return,
                };
                let decoded =
                    match crate::image::decode_kitty(chunk.format, chunk.width, chunk.height, &raw)
                    {
                        Some(d) => d,
                        None => return,
                    };
                if cmd.action == 't' {
                    // Transmit-only: store by id for a later a=p display.
                    self.store_kitty_image(cmd.id, decoded);
                } else {
                    // a=T: display now, and also store if an id was given.
                    if cmd.id != 0 {
                        self.store_kitty_image(cmd.id, decoded.clone());
                    }
                    self.place_kitty_image(decoded);
                }
            }
            // Unknown action — ignore.
            _ => {}
        }
    }

    /// Anchor a decoded Kitty image at the current cursor position.
    fn place_kitty_image(&mut self, image: crate::image::DecodedImage) {
        let line = self.history.len() + self.row;
        self.push_image(TerminalImage {
            image,
            line,
            col: self.col,
        });
    }

    /// Insert a decoded image into the transmit-store, evicting the oldest-keyed
    /// entries when the count or total-byte caps are exceeded.
    fn store_kitty_image(&mut self, id: u32, image: crate::image::DecodedImage) {
        self.kitty_store.insert(id, image);
        // Enforce the image-count cap (evict lowest ids first — deterministic).
        while self.kitty_store.len() > Self::KITTY_STORE_MAX_IMAGES {
            if let Some(&min_id) = self.kitty_store.keys().min() {
                self.kitty_store.remove(&min_id);
            } else {
                break;
            }
        }
        // Enforce the total-byte cap.
        let mut total: usize = self.kitty_store.values().map(|i| i.rgba.len()).sum();
        while total > Self::KITTY_STORE_MAX_BYTES && self.kitty_store.len() > 1 {
            if let Some(&min_id) = self.kitty_store.keys().min() {
                if let Some(removed) = self.kitty_store.remove(&min_id) {
                    total -= removed.rgba.len();
                }
            } else {
                break;
            }
        }
    }

    // ------------------------------------------------------------------
    // Anchored-metadata bounds (audit finding #4). The `images`,
    // `hyperlinks`, `prompt_marks`, and `command_marks` Vecs accumulate as a
    // hostile (or merely chatty) stream emits inline images / OSC 8 links /
    // OSC 133 marks. Left unbounded they are a slow memory-exhaustion DoS.
    // Two orthogonal bounds keep them honest, mirroring the Kitty-store
    // discipline above:
    //   1. a hard COUNT cap per Vec (ring-buffer: drop oldest on overflow), and
    //      for `images` an additional total-RGBA-BYTE cap. This is the load-
    //      bearing memory bound — it holds under any flood, capped or not.
    //   2. re-anchoring on a scrollback CLEAR (`reanchor_after_scrollback_clear`):
    //      when the whole history is erased (full reset / ESC[3J), each anchor's
    //      `line` is shifted down by the erased history length so it keeps the
    //      `history.len() + grid_row` convention the renderer + jump-to-prompt
    //      rely on (window.rs); anchors that pointed into the now-erased
    //      scrollback (would go negative) are dropped.
    // ------------------------------------------------------------------

    /// Max retained inline images. A ring-buffer cap — the oldest image is
    /// dropped when a new one would exceed it. Mirrors the Kitty transmit-store
    /// image cap so on-screen + stored image memory share one discipline.
    const IMAGES_MAX: usize = 256;
    /// Max total decoded-RGBA bytes across retained images (matches the Kitty
    /// transmit-store byte cap). Oldest images are dropped until under the cap.
    const IMAGES_MAX_BYTES: usize = 64 * 1024 * 1024;
    /// Max retained OSC 8 hyperlink URIs (oldest dropped on overflow).
    const HYPERLINKS_MAX: usize = 4096;
    /// Max chars retained for the window title (OSC 0/2). A title is a short
    /// label; a multi-megabyte OSC 2 string is a memory-DoS vector, so the stored
    /// value is truncated to this length (the tab strip truncates display separately).
    const TITLE_MAX_CHARS: usize = 512;
    /// Max bytes of a single OSC 52 clipboard-WRITE payload we accept (after
    /// base64-decode). A real yank is small; a multi-megabyte OSC 52 write is a
    /// memory-DoS, so oversized writes are dropped. READ is already default-off.
    const OSC52_WRITE_MAX_BYTES: usize = 1024 * 1024;
    /// Max retained OSC 133 prompt marks (oldest dropped on overflow).
    const PROMPT_MARKS_MAX: usize = 4096;
    /// Max retained OSC 133 command marks (oldest dropped on overflow).
    const COMMAND_MARKS_MAX: usize = 8192;
    /// Max bytes of a single OSC 8 hyperlink URI we retain (S-7). A real
    /// hyperlink is a short URL; an escape sequence that stuffs a multi-megabyte
    /// "URI" is a memory-amplification / phishing-obfuscation vector. URIs over
    /// this cap are skipped (not truncated — a truncated URL is itself a
    /// spoofing risk). 2 KiB comfortably covers legitimate links.
    const HYPERLINK_URI_MAX_BYTES: usize = 2 * 1024;
    /// Hard cap on the total bytes queued in `pty_response` between drains
    /// (S-7). Every device-status / DA / DSR / DECRQM / XTGETTCAP reply is a
    /// fixed, bounded format, but a hostile program can issue an unbounded
    /// *stream* of queries to amplify our reply bytes (a write-amplification
    /// DoS). Once the queue reaches this cap, further replies are dropped until
    /// the app drains it — the queries themselves are still parsed, only the
    /// reply emission is bounded. 64 KiB is far above any legitimate burst of
    /// query replies for one frame.
    const PTY_RESPONSE_MAX: usize = 64 * 1024;
    /// Max queued desktop notifications (OSC 9 / 777) between UI drains. The UI
    /// drains every frame, so this only bounds a hostile flood of notification
    /// escapes; the oldest is dropped on overflow (ring buffer, like the marks).
    const NOTIFICATIONS_MAX: usize = 256;
    /// Max queued OSC 52 clipboard-WRITE payloads between drains. A real yank is
    /// occasional and the last write wins, so a small cap suffices; oldest
    /// dropped on overflow. (Each payload is already byte-capped on entry.)
    const CLIPBOARD_WRITES_MAX: usize = 64;
    /// Max queued OSC 52 clipboard-READ requests between drains. Only ever
    /// non-empty when reads are opted into; a program spamming `OSC 52 ; c ; ?`
    /// must not grow this without bound, so the oldest is dropped on overflow.
    /// Small — the app drains every frame and each request carries no payload.
    const CLIPBOARD_READS_MAX: usize = 16;
    /// Max queued OSC 9;4 progress updates between drains. Only the latest state
    /// is meaningful, so a small cap bounds a flood; oldest dropped on overflow.
    const PROGRESS_MAX: usize = 256;
    /// Max depth of the XTWINOPS title stack (`CSI 22 t` push). Every other
    /// PTY-driven buffer is capped; an unbalanced `CSI 22 t` flood otherwise
    /// grows this without bound. xterm bounds its title stack too; oldest entry
    /// dropped on overflow.
    const TITLE_STACK_MAX: usize = 64;
    /// Max number of distinct in-flight Kitty chunked-transmission sets. Each
    /// set's payload is already byte-capped, but a hostile stream that opens
    /// many distinct ids and never sends the finalizing `m=0` chunk would grow
    /// the chunk map without bound; the oldest in-flight set is dropped on
    /// overflow (matches the kitty transmit-store eviction discipline).
    const KITTY_CHUNKS_MAX: usize = 16;

    /// Append an inline image, then enforce the count + byte caps by dropping
    /// the oldest entries (ring-buffer). The single push path for both Sixel
    /// (`unhook`) and Kitty (`place_kitty_image`) anchors.
    fn push_image(&mut self, img: TerminalImage) {
        self.images.push(img);
        while self.images.len() > Self::IMAGES_MAX {
            self.images.remove(0);
        }
        let mut total: usize = self.images.iter().map(|i| i.image.rgba.len()).sum();
        while total > Self::IMAGES_MAX_BYTES && self.images.len() > 1 {
            let removed = self.images.remove(0);
            total -= removed.image.rgba.len();
        }
    }

    /// Queue bytes to be written back to the PTY, bounded by
    /// [`Self::PTY_RESPONSE_MAX`] (S-7). This is the single sink every device
    /// reply (DA / DSR / CPR / DECRQM / XTGETTCAP / OSC color / OSC 52 read)
    /// flows through, so the write-amplification cap is enforced uniformly. A
    /// reply that would push the queue past the cap is dropped wholesale (never
    /// truncated mid-sequence, which would emit a malformed escape) until the
    /// app drains the queue via `take_pty_response`.
    fn push_pty_response(&mut self, bytes: &[u8]) {
        if self.pty_response.len().saturating_add(bytes.len()) > Self::PTY_RESPONSE_MAX {
            // Drop the reply rather than emit a partial escape sequence. The
            // query was still parsed; only the (bounded) reply is suppressed.
            return;
        }
        self.pty_response.extend_from_slice(bytes);
    }

    /// Append an OSC 8 hyperlink, then drop the oldest until under the cap.
    ///
    /// Two S-7 bounds gate the URI before it is stored: (1) a length cap — a URI
    /// over [`Self::HYPERLINK_URI_MAX_BYTES`] is skipped (an over-long "URI" is
    /// a memory-amplification / obfuscation vector, and a *truncated* URL is a
    /// spoofing risk, so we drop it rather than store a clipped form); (2) a
    /// scheme allow-list — only `http`/`https`/`file` URIs are captured. Other
    /// schemes (`javascript:`, `data:`, `vbscript:`, …) are the OSC 8 phishing
    /// surface and are rejected. The existing capture-not-auto-activate posture
    /// is preserved: stored links are surfaced for the user to inspect, never
    /// auto-opened.
    fn push_hyperlink(&mut self, uri: String) {
        if uri.len() > Self::HYPERLINK_URI_MAX_BYTES {
            return;
        }
        if !Self::is_allowed_hyperlink_scheme(&uri) {
            return;
        }
        self.hyperlinks.push(uri);
        while self.hyperlinks.len() > Self::HYPERLINKS_MAX {
            self.hyperlinks.remove(0);
        }
    }

    /// Whether an OSC 8 URI carries an allowed scheme (`http`/`https`/`file`,
    /// case-insensitive). A bare-relative URI (no `scheme:` prefix) is rejected
    /// — OSC 8 links are expected to be absolute. The scheme is the substring
    /// before the first `:`; anything else (`javascript:`, `data:`, …) is the
    /// phishing / code-exec surface and is denied.
    ///
    /// SECURITY INVARIANT: `file:` is permitted here for DISPLAY/CAPTURE ONLY
    /// (so e.g. `ls --hyperlink`'s `file://` links render as styled links). A
    /// stored OSC 8 `file:` URI MUST NEVER be passed to an opener — the clickable
    /// path is fed exclusively by the http(s)-only `hyperlink::find_urls`
    /// extractor (locked by `clickable_extractor_is_http_s_only_*`), NOT by the
    /// `hyperlinks()` accessor. Wiring `hyperlinks()` to a clickable/opener
    /// surface would re-introduce the `file://host/share/evil.exe` ctrl-click
    /// vector removed in PR #170; do not do so without dropping `file:` here.
    fn is_allowed_hyperlink_scheme(uri: &str) -> bool {
        match uri.split_once(':') {
            Some((scheme, _)) => {
                scheme.eq_ignore_ascii_case("http")
                    || scheme.eq_ignore_ascii_case("https")
                    || scheme.eq_ignore_ascii_case("file")
            }
            None => false,
        }
    }

    /// Append an OSC 133 prompt mark, then drop the oldest until under the cap.
    fn push_prompt_mark(&mut self, abs: usize) {
        self.prompt_marks.push(abs);
        while self.prompt_marks.len() > Self::PROMPT_MARKS_MAX {
            self.prompt_marks.remove(0);
        }
    }

    /// Append an OSC 133 command mark, then drop the oldest until under the cap.
    fn push_command_mark(&mut self, mark: osc::CommandMark) {
        self.command_marks.push(mark);
        while self.command_marks.len() > Self::COMMAND_MARKS_MAX {
            self.command_marks.remove(0);
        }
    }

    /// Reconcile anchored metadata after the scrollback history was erased
    /// (`history.clear()`), where `erased` is the history length immediately
    /// BEFORE the clear. Anchors use the `history.len() + grid_row` convention;
    /// erasing `erased` lines of scrollback means every anchor's `line` shifts
    /// down by `erased`. Anchors that pointed at or below the erased scrollback
    /// (`line < erased`) referenced content that is now gone and are dropped;
    /// the rest are re-based so they keep pointing at the same live grid row.
    /// Hyperlinks carry no line anchor (arrival-order only) and are untouched.
    fn reanchor_after_scrollback_clear(&mut self, erased: usize) {
        if erased == 0 {
            return;
        }
        self.images.retain(|i| i.line >= erased);
        for i in self.images.iter_mut() {
            i.line -= erased;
        }
        self.prompt_marks.retain(|&m| m >= erased);
        for m in self.prompt_marks.iter_mut() {
            *m -= erased;
        }
        self.command_marks.retain(|m| m.line >= erased);
        for m in self.command_marks.iter_mut() {
            m.line -= erased;
        }
    }

    /// Erase the scrollback history, keeping the live grid. Drops `history` /
    /// `history_wrapped`, snaps the view back to live output, and re-bases the
    /// anchored metadata by the erased length (dropping anchors that pointed into
    /// the now-gone scrollback) via [`reanchor_after_scrollback_clear`]. This is
    /// the single implementation the `ESC [ 3 J` (ED-3) handler, the right-click
    /// "Clear scrollback" menu item, and the Ctrl+Shift+K chord all share.
    fn clear_scrollback(&mut self) {
        let erased = self.history.len();
        self.history.clear();
        self.history_wrapped.clear();
        self.view_offset = 0;
        self.reanchor_after_scrollback_clear(erased);
    }
}

/// Bound on a single accumulated APC body (mirrors the Sixel cap). A body that
/// exceeds this is dropped — a hostile stream cannot exhaust memory.
const KITTY_APC_MAX_BYTES: usize = 8 * 1024 * 1024;

/// State of the APC pre-filter that runs ahead of the `vte` parser.
///
/// `vte` 0.15's `Perform` trait has no APC callback, so APC strings
/// (`ESC _ … ST`) — exactly how the Kitty graphics protocol transmits — are
/// routed to vte's internal `SosPmApcString` state and silently discarded.
/// This small persistent state machine extracts complete `ESC _ G … ST` bodies
/// before vte sees them and passes every other byte through unchanged. An APC
/// can be split across `advance()` calls (PTY reads are arbitrary chunks), so
/// the state lives on the [`Terminal`] across calls.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ApcFilter {
    /// Not inside any escape sequence.
    Normal,
    /// Saw a lone `ESC` in `Normal`; deciding whether it introduces an APC.
    Esc,
    /// Inside an APC body (`ESC _` seen); accumulating until ST.
    /// `is_kitty` records whether the body began with `G` (Kitty graphics);
    /// non-Kitty APCs are accumulated only so we can swallow them (match vte).
    Apc { is_kitty: bool, seen_first: bool },
    /// Inside an APC and just saw `ESC`; `\` completes the ST terminator.
    ApcEsc,
}

/// A terminal: VT parser + screen state. Feed it PTY bytes; read its grid.
pub struct Terminal {
    parser: Parser,
    screen: Screen,
    /// Persistent APC pre-filter state (Kitty graphics extraction).
    apc_state: ApcFilter,
    /// Accumulated bytes of the in-progress APC body (between `ESC _` and ST).
    apc_accum: Vec<u8>,
    /// Reusable scratch for the slow-path [`Terminal::advance`] passthrough
    /// batch. Logically per-`advance`-scoped (cleared at the top of the slow
    /// path, exactly like `apc_accum`), but owned by the terminal so the
    /// allocation amortises across calls instead of being remade every frame.
    passthrough: Vec<u8>,
    /// Trailing bytes of a multibyte UTF-8 codepoint whose sequence was split
    /// across this `advance()` call and the next (the PTY reader's 64 KiB buffer
    /// can split any char). Held back and prepended next call so the parser only
    /// ever sees WHOLE codepoints — `vte` 0.15 otherwise drops the byte that
    /// follows a partial codepoint completed at a call boundary (data loss for
    /// accented-Latin / CJK / emoji at read boundaries). Bounded to < 4 bytes.
    utf8_pending: Vec<u8>,
}

/// Index where a trailing INCOMPLETE UTF-8 sequence begins in `bytes`, or
/// `bytes.len()` when the chunk already ends on a codepoint boundary. Only a
/// genuinely incomplete multibyte tail (a lead byte missing some of its
/// continuation bytes) is reported; complete codepoints and stray/invalid bytes
/// are left in place (invalid-byte handling is position-dependent by nature and
/// must NOT be buffered). Bounded: a UTF-8 codepoint is <= 4 bytes, so this
/// scans at most the last 3 bytes.
fn utf8_tail_boundary(bytes: &[u8]) -> usize {
    let n = bytes.len();
    // Walk back over up to 3 trailing continuation bytes (`0b10xx_xxxx`).
    let mut cont = 0usize;
    while cont < 3 && cont < n && (bytes[n - 1 - cont] & 0xC0) == 0x80 {
        cont += 1;
    }
    if cont == n {
        // Nothing but continuation bytes in view — not a held partial.
        return n;
    }
    let lead = bytes[n - 1 - cont];
    let needed = if lead < 0x80 {
        1 // ASCII (includes ESC) — already complete
    } else if lead >> 5 == 0b110 {
        2
    } else if lead >> 4 == 0b1110 {
        3
    } else if lead >> 3 == 0b1_1110 {
        4
    } else {
        1 // stray continuation / invalid lead — leave as-is
    };
    if needed > cont + 1 {
        // The trailing sequence is incomplete: hold from its lead byte.
        n - 1 - cont
    } else {
        n
    }
}

impl Terminal {
    /// Construct a terminal of `rows` × `cols` with the default scrollback cap
    /// ([`DEFAULT_SCROLLBACK`]). Use [`Terminal::with_scrollback`] for an
    /// explicit cap.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::term::Terminal;
    ///
    /// let t = Terminal::new(24, 80);
    /// assert_eq!(t.grid().rows(), 24);
    /// assert_eq!(t.grid().cols(), 80);
    /// ```
    pub fn new(rows: usize, cols: usize) -> Self {
        Self::with_scrollback(rows, cols, DEFAULT_SCROLLBACK)
    }

    /// Update the scrollback line cap on a live terminal (the user changed the
    /// `scrollback_lines` config). Raising it takes effect immediately (future
    /// lines are retained up to the new cap); lowering it is enforced LAZILY by
    /// the existing eviction path as new lines scroll into history — eager
    /// truncation here would have to replicate the eviction bookkeeping (image
    /// line anchors, prompt marks) and risk desync, so the cap is simply lowered
    /// and honoured on the next history push.
    pub fn set_max_scrollback(&mut self, max_scrollback: usize) {
        self.screen.max_scrollback = max_scrollback;
    }

    /// Construct with an explicit scrollback line cap.
    pub fn with_scrollback(rows: usize, cols: usize, max_scrollback: usize) -> Self {
        Terminal {
            parser: Parser::new(),
            screen: Screen::new(rows, cols, max_scrollback),
            apc_state: ApcFilter::Normal,
            apc_accum: Vec::new(),
            passthrough: Vec::new(),
            utf8_pending: Vec::new(),
        }
    }

    /// Feed raw PTY bytes through the VT state machine.
    ///
    /// An APC pre-filter runs first: it extracts complete Kitty graphics APC
    /// bodies (`ESC _ G … ST`) that `vte` would otherwise silently discard, and
    /// forwards every non-APC byte to the `vte` parser in its original order.
    /// Passthrough bytes are batched in a scratch buffer and flushed to the
    /// parser on each state transition so byte ordering is preserved exactly.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::term::Terminal;
    ///
    /// let mut t = Terminal::new(2, 10);
    /// t.advance(b"hi");
    /// let grid = t.grid();
    /// assert_eq!(grid.cell(0, 0).unwrap().c, 'h');
    /// assert_eq!(grid.cell(0, 1).unwrap().c, 'i');
    ///
    /// // A CR+LF moves the cursor to the start of the next row.
    /// t.advance(b"\r\nx");
    /// assert_eq!(t.grid().cell(1, 0).unwrap().c, 'x');
    /// ```
    pub fn advance(&mut self, bytes: &[u8]) {
        // UTF-8 read-boundary reassembly. The PTY reader can split a multibyte
        // codepoint's bytes across two `advance()` calls; `vte` 0.15 then drops
        // the byte FOLLOWING a codepoint that completes at a call boundary (a
        // real data-loss bug for accented-Latin / CJK / emoji at the 64 KiB
        // boundary). We hold back any trailing INCOMPLETE UTF-8 sequence and
        // prepend it next call, so the inner parser only ever sees whole
        // codepoints. ESC (0x1b) is a complete ASCII byte and is never held, so
        // escape-sequence handling is unaffected.
        if self.utf8_pending.is_empty() {
            let cut = utf8_tail_boundary(bytes);
            if cut < bytes.len() {
                self.utf8_pending.extend_from_slice(&bytes[cut..]);
            }
            self.advance_complete(&bytes[..cut]);
        } else {
            let mut combined = std::mem::take(&mut self.utf8_pending);
            combined.extend_from_slice(bytes);
            let cut = utf8_tail_boundary(&combined);
            self.utf8_pending.extend_from_slice(&combined[cut..]);
            self.advance_complete(&combined[..cut]);
        }
    }

    /// Feed a chunk that is guaranteed to END on a UTF-8 codepoint boundary
    /// (the [`Terminal::advance`] wrapper holds back any trailing partial
    /// sequence). This is the real APC-prefilter + vte driver.
    fn advance_complete(&mut self, bytes: &[u8]) {
        // Fast path: when no escape sequence is in flight and the chunk has no
        // ESC byte, hand it straight to vte (the overwhelmingly common case).
        // `memchr` is SIMD-accelerated (AVX2/NEON) — on a large paste / `cat
        // bigfile` the common case is "no ESC", so the `is_none()` short-circuit
        // is pure win over the scalar `bytes.contains(&0x1b)` byte scan.
        if self.apc_state == ApcFilter::Normal && memchr::memchr(0x1b, bytes).is_none() {
            self.parser.advance(&mut self.screen, bytes);
            return;
        }

        // Reuse the terminal-owned scratch instead of allocating a fresh Vec
        // every slow-path call. Move it out so the loop can also `&mut self`
        // (`self.parser`/`self.apc_*`), clear it (keeps the capacity), and put
        // it back at the end — the grown allocation survives to the next call.
        let mut passthrough = std::mem::take(&mut self.passthrough);
        passthrough.clear();
        // Index-based walk so the `Normal` state can SIMD-skip runs of plain
        // (non-ESC) bytes via `memchr` instead of pushing one byte at a time —
        // byte-for-byte identical to the per-byte loop, just bulk-copied.
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match self.apc_state {
                ApcFilter::Normal => {
                    // Bulk-copy the run of non-ESC bytes up to the next ESC (or
                    // end of buffer) in one `extend_from_slice`, then handle the
                    // ESC (if any) on the next iteration. This is the hot path for
                    // bulk output that DOES contain an occasional escape.
                    match memchr::memchr(0x1b, &bytes[i..]) {
                        None => {
                            // No further ESC: copy the rest and finish the walk.
                            passthrough.extend_from_slice(&bytes[i..]);
                            i = bytes.len();
                            continue;
                        }
                        Some(0) => {
                            // Current byte IS the ESC — hold it until we know.
                            self.apc_state = ApcFilter::Esc;
                            i += 1;
                            continue;
                        }
                        Some(rel) => {
                            // Copy the plain run, then position on the ESC.
                            passthrough.extend_from_slice(&bytes[i..i + rel]);
                            self.apc_state = ApcFilter::Esc;
                            i += rel + 1;
                            continue;
                        }
                    }
                }
                ApcFilter::Esc => {
                    if b == 0x5f {
                        // `ESC _` — APC string begins. Flush pending text first.
                        if !passthrough.is_empty() {
                            self.parser.advance(&mut self.screen, &passthrough);
                            passthrough.clear();
                        }
                        self.apc_accum.clear();
                        self.apc_state = ApcFilter::Apc {
                            is_kitty: false,
                            seen_first: false,
                        };
                    } else if b == 0x1b {
                        // Another ESC: emit the held ESC, stay in Esc for this one.
                        passthrough.push(0x1b);
                    } else {
                        // Not an APC: re-emit the held ESC + this byte intact so
                        // vte parses the original escape sequence unchanged.
                        passthrough.push(0x1b);
                        passthrough.push(b);
                        self.apc_state = ApcFilter::Normal;
                    }
                }
                ApcFilter::Apc {
                    is_kitty,
                    seen_first,
                } => {
                    if b == 0x07 {
                        // BEL terminates the APC.
                        self.finish_apc(is_kitty);
                    } else if b == 0x1b {
                        // Possible ST (`ESC \`) — wait for the next byte.
                        self.apc_state = ApcFilter::ApcEsc;
                    } else {
                        let is_kitty = if !seen_first { b == b'G' } else { is_kitty };
                        // Only accumulate Kitty bodies; bound the size.
                        if is_kitty && self.apc_accum.len() < KITTY_APC_MAX_BYTES {
                            self.apc_accum.push(b);
                        }
                        self.apc_state = ApcFilter::Apc {
                            is_kitty,
                            seen_first: true,
                        };
                    }
                }
                ApcFilter::ApcEsc => {
                    // We are inside an APC whose previous byte was ESC.
                    let was_kitty = self.apc_accum.first() == Some(&b'G');
                    if b == 0x5c {
                        // `ESC \` = ST — the APC ends here.
                        self.finish_apc(was_kitty);
                    } else if b == 0x07 {
                        // ESC then BEL: BEL still terminates the APC.
                        self.finish_apc(was_kitty);
                    } else if b == 0x1b {
                        // ESC ESC inside an APC — stay waiting for the terminator.
                        // (The intervening ESC is not part of the body.)
                    } else {
                        // The ESC was embedded in the body, not a terminator.
                        // Re-enter Apc and accumulate this byte (Kitty only).
                        if was_kitty && self.apc_accum.len() < KITTY_APC_MAX_BYTES {
                            self.apc_accum.push(b);
                        }
                        self.apc_state = ApcFilter::Apc {
                            is_kitty: was_kitty,
                            seen_first: true,
                        };
                    }
                }
            }
            // The Normal arm `continue`s after advancing `i` itself (it may
            // bulk-skip a run); every other arm consumes exactly one byte and
            // falls through to here.
            i += 1;
        }

        // Flush any trailing passthrough text. A half-finished escape/APC stays
        // in `self.apc_state` for the next advance() call.
        if !passthrough.is_empty() {
            self.parser.advance(&mut self.screen, &passthrough);
        }

        // Hand the (now-grown) scratch back to the terminal so its capacity is
        // reused next call. Logically per-`advance`-scoped — it is cleared at
        // the top of the slow path, never read across calls.
        self.passthrough = passthrough;
    }

    /// Complete an APC body: dispatch Kitty graphics, swallow everything else
    /// (matching vte's discard behaviour), then return to Normal.
    fn finish_apc(&mut self, is_kitty: bool) {
        if is_kitty {
            let body = std::mem::take(&mut self.apc_accum);
            self.screen.handle_kitty_apc(&body);
        }
        self.apc_accum.clear();
        self.apc_state = ApcFilter::Normal;
    }

    /// Borrow the active screen grid (the cell matrix the renderer reads).
    pub fn grid(&self) -> &Grid {
        &self.screen.grid
    }

    /// Mutably borrow the active screen grid.
    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.screen.grid
    }

    /// Clear the grid's per-row damage flags after the renderer has consumed
    /// them. Call this exactly once per frame, immediately after snapshotting the
    /// damaged rows, so the next frame's [`Grid::is_damaged`] reflects only writes
    /// that happened since this snapshot. The reader thread re-marks rows dirty as
    /// PTY output arrives, so a cell changed after this call is still redrawn next
    /// frame.
    pub fn clear_damage(&mut self) {
        self.screen.grid.clear_damage();
    }

    /// The current window title, as set by the program via OSC 0/2.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::term::Terminal;
    ///
    /// let mut t = Terminal::new(2, 20);
    /// assert_eq!(t.title(), "");
    /// // OSC 2 sets the window title; ST here is BEL (0x07).
    /// t.advance(b"\x1b]2;hello\x07");
    /// assert_eq!(t.title(), "hello");
    /// ```
    pub fn title(&self) -> &str {
        &self.screen.title
    }

    /// The current working directory reported by the shell (OSC 7), or `None`
    /// if the shell has not reported one.
    pub fn cwd(&self) -> Option<&str> {
        self.screen.cwd.as_deref()
    }

    /// Absolute line indices of captured shell-prompt marks (OSC 133).
    pub fn prompt_marks(&self) -> &[usize] {
        &self.screen.prompt_marks
    }

    /// Hyperlink URIs captured via OSC 8, in arrival order.
    pub fn hyperlinks(&self) -> &[String] {
        &self.screen.hyperlinks
    }

    /// Decoded inline images (Sixel) anchored to grid positions.
    pub fn images(&self) -> &[TerminalImage] {
        &self.screen.images
    }

    /// The current DEC private mode flags (cursor visibility, mouse, paste, …).
    pub fn dec_modes(&self) -> DecModes {
        self.screen.dec_modes
    }

    /// Whether the cursor should be drawn (DECTCEM `?25`). Default `true`.
    pub fn is_cursor_visible(&self) -> bool {
        self.screen.dec_modes.cursor_visible
    }

    /// Whether the alternate screen is currently active. While active, the
    /// visible-grid accessors ([`Terminal::grid`], [`Terminal::display_rows`])
    /// transparently return the alt grid and scrollback is not extended.
    pub fn alt_screen_active(&self) -> bool {
        self.screen.saved_primary.is_some()
    }

    /// Whether bracketed-paste mode (`?2004`) is enabled.
    ///
    /// Integration point: every host paste path MUST go through [`Self::frame_paste`]
    /// (which consults this flag), never write clipboard bytes to the PTY raw.
    pub fn bracketed_paste(&self) -> bool {
        self.screen.dec_modes.bracketed_paste
    }

    /// Frame a clipboard paste for safe delivery to the PTY — the canonical
    /// paste-injection ("pastejacking") guard. EVERY UI paste path must route
    /// through this; writing clipboard bytes to the PTY raw is the bug class this
    /// closes (CyberArk pastejacking; the bracketed-paste-bypass CVEs in
    /// MinTTY/Xshell/ZOC).
    ///
    /// - When the running program enabled bracketed-paste mode (`?2004`), the
    ///   text is wrapped in `ESC[200~ … ESC[201~` AND every embedded `ESC[201~`
    ///   end-sentinel is stripped first, so a hostile clipboard payload cannot
    ///   terminate the bracket early and have the shell execute the bytes that
    ///   follow as typed commands.
    /// - When bracketed mode is off, the bytes are returned unwrapped (the
    ///   shell's line discipline consumes them). The embedded-newline-executes
    ///   risk on this path is mitigated UI-side by the multi-line-paste confirm
    ///   gate (`paste_warn_multiline`); a bare `ESC[201~` here is inert noise but
    ///   is still stripped for consistency.
    ///
    /// Pure (`&self`) so both the egui and the legacy winit UIs share ONE
    /// hardened implementation and the paths cannot drift apart again.
    pub fn frame_paste(&self, text: &str) -> Vec<u8> {
        // Strip any embedded end-sentinel FIRST on BOTH paths (in bracketed mode
        // it is the active injection vector; unbracketed it is inert but removing
        // it keeps the two paths identical and audit-simple). The sentinel
        // contains ESC, so this must run BEFORE the control-stripping filter
        // below (which would otherwise remove the ESC and defeat the match).
        let cleaned: String = text
            .replace("\x1b[201~", "")
            // Defense-in-depth: drop C0 (incl. ESC/BEL/DEL) and C1 control
            // characters EXCEPT tab/newline/carriage-return, so a hostile
            // clipboard payload cannot smuggle escape sequences through a paste
            // even if the program is NOT in bracketed mode. Tab + newlines are
            // kept because they are legitimate in multi-line/indented pastes
            // (and the embedded-newline-executes risk is separately gated by the
            // UI multi-line-paste confirm). Matches WezTerm's paste filter set.
            .chars()
            .filter(|&c| {
                let cp = c as u32;
                matches!(c, '\t' | '\n' | '\r')
                    || (cp >= 0x20 && cp != 0x7f && !(0x80..=0x9f).contains(&cp))
            })
            .collect();
        if self.bracketed_paste() {
            let mut b = Vec::with_capacity(cleaned.len() + 12);
            b.extend_from_slice(b"\x1b[200~");
            b.extend_from_slice(cleaned.as_bytes());
            b.extend_from_slice(b"\x1b[201~");
            b
        } else {
            cleaned.into_bytes()
        }
    }

    /// The active mouse tracking mode (`?1000` / `?1002` / `?1003`).
    pub fn mouse_mode(&self) -> MouseMode {
        self.screen.dec_modes.mouse_mode
    }

    /// The active mouse report encoding (`?1006` / `?1015`).
    pub fn mouse_encoding(&self) -> MouseEncoding {
        self.screen.dec_modes.mouse_encoding
    }

    /// Whether DECCKM cursor-key application mode (`?1`) is enabled. The host
    /// uses this to choose between `ESC O A` (application) and `ESC [ A`
    /// (normal) arrow-key encodings.
    pub fn application_cursor_keys(&self) -> bool {
        self.screen.dec_modes.application_cursor_keys
    }

    /// The current kitty keyboard-protocol progressive-enhancement flags
    /// (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>). `0` means no
    /// program has negotiated the protocol and the legacy key encoding is in
    /// force; a non-zero value is the bitset on top of the flag stack
    /// (bit1 disambiguate, bit2 report-event-types, bit4 report-alternate-keys,
    /// bit8 report-all-keys-as-escape-codes, bit16 report-associated-text). The
    /// host reads this to decide whether to route key events through the CSI-u
    /// encoder (`encode_key_kitty`) instead of the legacy `encode_key`.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.screen.kitty_kbd_flags()
    }

    /// Whether autowrap (DECAWM `?7`) is enabled. Default `true`.
    pub fn autowrap(&self) -> bool {
        self.screen.dec_modes.autowrap
    }

    /// Whether focus in/out reporting (`?1004`) is enabled. When `true`, the
    /// host sends `ESC [ I` on focus-in and `ESC [ O` on focus-out.
    pub fn focus_reporting(&self) -> bool {
        self.screen.dec_modes.focus_reporting
    }

    /// Whether synchronized output (`?2026`) is currently requested.
    pub fn sync_output(&self) -> bool {
        self.screen.dec_modes.sync_output
    }

    /// Drains pending bytes to write back to the PTY.
    ///
    /// These are replies the terminal owes the running program: OSC 4/10/11/12
    /// color query responses and OSC 52 clipboard read responses (only when
    /// reads are opted into). The host event loop calls this each frame and
    /// writes any returned bytes to the PTY master. Returns an empty vec when
    /// there is nothing to send. The terminal NEVER queues a reply for OSC 133
    /// marks (anti-CVE, capture-only).
    pub fn take_pty_response(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.screen.pty_response)
    }

    /// C14 — focus reporting (core half). The host calls this on every window
    /// focus change (e.g. a winit `WindowEvent::Focused(b)`): `true` for
    /// focus-in, `false` for focus-out. When DEC mode `?1004` is enabled, it
    /// queues `ESC [ I` (in) / `ESC [ O` (out) into the PTY-response buffer,
    /// which the host drains via [`Terminal::take_pty_response`]. When `?1004`
    /// is disabled (the default) it is a no-op, so the host can call it
    /// unconditionally without checking [`Terminal::focus_reporting`] first.
    pub fn focus_report(&mut self, focused: bool) {
        self.screen.focus_report(focused);
    }

    /// Drains the oldest pending OSC 52 clipboard-write request, if any.
    ///
    /// The host calls this each frame to learn whether a program requested a
    /// clipboard update, then applies it to the host clipboard. Only WRITE
    /// requests are ever produced here; clipboard READ requests are ignored
    /// unless [`Terminal::set_clipboard_read_enabled`] is opted into.
    pub fn take_clipboard_write(&mut self) -> Option<ClipboardWrite> {
        if self.screen.pending_clipboard_writes.is_empty() {
            None
        } else {
            Some(self.screen.pending_clipboard_writes.remove(0))
        }
    }

    /// Drains all pending OSC 52 clipboard-write requests at once.
    pub fn take_clipboard_writes(&mut self) -> Vec<ClipboardWrite> {
        std::mem::take(&mut self.screen.pending_clipboard_writes)
    }

    /// Enables (or disables) responding to OSC 52 clipboard READ requests.
    ///
    /// DEFAULT-OFF. While disabled (the default), an `OSC 52 ; c ; ?` query is
    /// answered by the terminal itself with an EMPTY payload — the host
    /// clipboard is never consulted, so nothing leaks (the canonical OSC 52 read
    /// vulnerability), while the requesting program still receives a reply and
    /// does not hang. Even when enabled, the core never reads the host clipboard
    /// itself: it queues the request for
    /// [`Terminal::take_clipboard_reads`] and the host supplies the text via
    /// [`Terminal::respond_clipboard_read`].
    ///
    /// Switching this OFF also refuses any request still queued from while it
    /// was on, so flipping the setting can never strand a blocked program.
    pub fn set_clipboard_read_enabled(&mut self, enabled: bool) {
        self.screen.clipboard_read_enabled = enabled;
        if !enabled {
            self.screen.deny_pending_clipboard_reads();
        }
    }

    /// Drains all pending OSC 52 clipboard READ requests at once.
    ///
    /// The ONLY read-drain. There is deliberately no singular
    /// `take_clipboard_read` companion to the write side's
    /// [`Terminal::take_clipboard_write`]: reads are consumed by the host's
    /// per-frame `for req in term.take_clipboard_reads()` loop, so a one-at-a-time
    /// drain has no caller and offers nothing the batch drain does not
    /// (`take_clipboard_reads().into_iter().next()`). One shipped as dormant `pub`
    /// API — `pub` on a `pub` type, so `dead_code` never fired — and was removed
    /// rather than left to look supported.
    ///
    /// Always empty unless clipboard reads were opted into: a denied read is
    /// answered inline by the terminal and never queued. The host answers a
    /// drained request with [`Terminal::respond_clipboard_read`].
    pub fn take_clipboard_reads(&mut self) -> Vec<osc::ClipboardReadRequest> {
        std::mem::take(&mut self.screen.pending_clipboard_reads)
    }

    /// Returns whether OSC 52 clipboard READ responses are enabled.
    pub fn clipboard_read_enabled(&self) -> bool {
        self.screen.clipboard_read_enabled
    }

    /// Queues an OSC 52 clipboard READ reply for the given selection and text.
    ///
    /// No-op unless [`Terminal::clipboard_read_enabled`] is `true`. The host
    /// supplies the clipboard text so the core never reads the clipboard itself.
    /// Encoded as standard base64, matching the OSC 52 write encoding. The
    /// queued bytes are drained via [`Terminal::take_pty_response`].
    pub fn respond_clipboard_read(&mut self, selection: ClipboardSelection, text: &str) {
        if !self.screen.clipboard_read_enabled {
            return;
        }
        let encoded = base64_encode(text.as_bytes());
        let resp = osc::format_clipboard_reply(selection, &encoded);
        // Route through the capped sink so PTY_RESPONSE_MAX is honoured
        // uniformly (the cap's doc claims it is the single sink for every
        // reply). This path is host-gated (clipboard_read_enabled, default-off).
        self.screen.push_pty_response(resp.as_bytes());
    }

    /// Drains all pending color-set requests (OSC 4 / 10 / 11 / 12 / 104 / 11x).
    ///
    /// The host applies each to its live theme. Reset operations are surfaced as
    /// sets carrying the default color value.
    pub fn take_color_sets(&mut self) -> Vec<ColorSet> {
        std::mem::take(&mut self.screen.pending_color_sets)
    }

    /// Drains the oldest pending desktop notification (OSC 9 / OSC 777).
    pub fn take_notification(&mut self) -> Option<Notification> {
        if self.screen.pending_notifications.is_empty() {
            None
        } else {
            Some(self.screen.pending_notifications.remove(0))
        }
    }

    /// Drains all pending desktop notifications at once.
    pub fn take_notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.screen.pending_notifications)
    }

    /// Whether DECSCNM reverse-video screen mode (`?5`, C25) is active. When
    /// `true`, the renderer should swap the default fg/bg for the whole screen.
    pub fn reverse_screen(&self) -> bool {
        self.screen.reverse_screen
    }

    /// Whether IRM insert mode (`CSI 4 h`, C25) is active. Exposed for the
    /// renderer/host; printing already honours it internally.
    pub fn insert_mode(&self) -> bool {
        self.screen.insert_mode
    }

    /// Whether DECOM origin mode (`?6`, C25) is active (cursor addressing is
    /// relative to the scroll region).
    pub fn origin_mode(&self) -> bool {
        self.screen.origin_mode
    }

    /// Drains all pending `OSC 9 ; 4` taskbar/tab progress reports (C26). The
    /// host applies the most recent to its tab/taskbar progress indicator.
    pub fn take_progress(&mut self) -> Vec<osc::Progress> {
        std::mem::take(&mut self.screen.pending_progress)
    }

    /// The captured OSC 133 command-zone marks (C28): output-start (`C`) and
    /// command-end (`D`, with optional exit code). Capture-only — the terminal
    /// never reports these back to the PTY.
    pub fn command_marks(&self) -> &[osc::CommandMark] {
        &self.screen.command_marks
    }

    /// The exit code of the most recently *finished* command, derived from the
    /// captured OSC 133 `D` marks (C28). The shell only records these when its
    /// prompt is integrated (`OSC 133 ; D [; exit_code]`), so a bare shell with
    /// no prompt integration yields no command-end mark at all.
    ///
    /// The double `Option` distinguishes three states the status bar cares about:
    ///
    /// - `None` — no command has finished yet (no `D` mark): the host shows NO
    ///   indicator.
    /// - `Some(None)` — a command finished but the shell supplied no exit code
    ///   (`OSC 133 ; D` with no third field): the host can show a neutral
    ///   "done" indicator.
    /// - `Some(Some(code))` — a command finished with the reported `code`:
    ///   `0` is success, anything else is a failure with that code.
    ///
    /// Scans the captured marks from newest to oldest for the latest
    /// [`osc::CommandMarkKind::CommandEnd`]; ignores [`osc::CommandMarkKind::OutputStart`]
    /// (`C`) marks, which carry no exit code.
    pub fn last_command_exit_code(&self) -> Option<Option<i32>> {
        self.screen
            .command_marks
            .iter()
            .rev()
            .find_map(|mark| match mark.kind {
                osc::CommandMarkKind::CommandEnd { exit_code } => Some(exit_code),
                osc::CommandMarkKind::OutputStart => None,
            })
    }

    /// Returns the current indexed-palette color (0-255) as an `(r, g, b)`
    /// triple. Reflects OSC 4 sets and OSC 104 resets.
    pub fn palette_color(&self, index: u8) -> (u8, u8, u8) {
        self.screen.palette[index as usize]
    }

    /// Returns the current dynamic color (fg/bg/cursor) as an `(r, g, b)`
    /// triple. Reflects OSC 10/11/12 sets and OSC 110/111/112 resets.
    pub fn dynamic_color(&self, which: DynamicColor) -> (u8, u8, u8) {
        match which {
            DynamicColor::Foreground => self.screen.dynamic_fg,
            DynamicColor::Background => self.screen.dynamic_bg,
            DynamicColor::Cursor => self.screen.dynamic_cursor,
        }
    }

    /// Returns the current window title-stack depth (XTWINOPS 22/23).
    pub fn title_stack_depth(&self) -> usize {
        self.screen.title_stack.len()
    }

    /// The requested cursor shape (DECSCUSR `CSI Ps SP q`). Default block.
    pub fn cursor_shape(&self) -> CursorShape {
        self.screen.cursor_shape
    }

    /// Whether the cursor should blink. This is `true` if either DECSCUSR
    /// selected a blinking shape or `?12` (att610 blink) is set.
    pub fn cursor_blink(&self) -> bool {
        self.screen.cursor_shape_blink || self.screen.dec_modes.cursor_blink
    }

    /// The cursor position in **display space** (`(row, col)`, 0-based) — the
    /// coordinate system of [`Terminal::display_rows`], so the renderer can map
    /// it straight onto the visible grid. Returns `None` when the cursor is not
    /// in the visible window: the live cursor sits in the bottom `grid.rows()`
    /// lines, so any non-zero scrollback `view_offset` scrolls it out of view
    /// (a terminal hides the cursor while you scroll back). The column is
    /// clamped to the last grid column.
    pub fn cursor_position(&self) -> Option<(usize, usize)> {
        if self.screen.view_offset != 0 {
            return None;
        }
        let cols = self.screen.grid.cols();
        let row = self
            .screen
            .row
            .min(self.screen.grid.rows().saturating_sub(1));
        let col = self.screen.col.min(cols.saturating_sub(1));
        Some((row, col))
    }

    /// Encode a mouse event into the byte sequence to write to the PTY, using
    /// the terminal's currently-active mouse mode and encoding.
    ///
    /// `col` and `row` are **1-based** cell coordinates. Returns `None` when no
    /// report should be sent — either mouse reporting is off, or the event kind
    /// is not enabled by the active mode (e.g. motion under `?1000`). This lets
    /// the host's event loop call it unconditionally and forward only when a
    /// sequence is produced.
    ///
    /// Encodings:
    /// - **SGR (`?1006`)**: `CSI < b ; x ; y M` for press/motion, `… m` for
    ///   release. No coordinate clamping is needed (decimal, unbounded).
    /// - **urxvt (`?1015`)**: `CSI ( b+32 ) ; x ; y M` (decimal, all offset 32).
    /// - **X10/normal**: `CSI M Cb Cx Cy` with each byte = value + 32, with
    ///   coordinates clamped to 223 (the max a single offset byte can carry).
    pub fn encode_mouse(
        &self,
        button: MouseButton,
        modifiers: MouseModifiers,
        col: usize,
        row: usize,
        kind: MouseEventKind,
    ) -> Option<Vec<u8>> {
        let mode = self.screen.dec_modes.mouse_mode;
        if mode == MouseMode::Off {
            return None;
        }
        // Gate the event by the active mode's interest set.
        match kind {
            MouseEventKind::Motion => match mode {
                MouseMode::Normal => return None, // ?1000 reports buttons only.
                MouseMode::ButtonEvent => {
                    // ?1002 reports motion only while a button is held.
                    if button == MouseButton::None {
                        return None;
                    }
                }
                MouseMode::AnyEvent => {} // ?1003 reports all motion.
                MouseMode::Off => return None,
            },
            MouseEventKind::Press | MouseEventKind::Release => {}
        }

        // Low button bits per xterm: left=0, middle=1, right=2, release=3
        // (legacy only). Wheel buttons set bit 6 (64).
        let (mut cb, is_wheel): (u32, bool) = match button {
            MouseButton::Left => (0, false),
            MouseButton::Middle => (1, false),
            MouseButton::Right => (2, false),
            MouseButton::WheelUp => (64, true),
            MouseButton::WheelDown => (65, true),
            MouseButton::None => (3, false), // "no button" base for motion.
        };
        // Motion adds 32 (the drag/motion bit).
        if kind == MouseEventKind::Motion {
            cb += 32;
        }
        // Modifier bits: shift=4, meta/alt=8, control=16.
        if modifiers.shift {
            cb += 4;
        }
        if modifiers.alt {
            cb += 8;
        }
        if modifiers.control {
            cb += 16;
        }

        match self.screen.dec_modes.mouse_encoding {
            MouseEncoding::Sgr => {
                // SGR uses the *unoffset* button value; release uses a final
                // `m`. Wheel events are always reported as a press (`M`).
                let final_byte = if kind == MouseEventKind::Release && !is_wheel {
                    'm'
                } else {
                    'M'
                };
                Some(format!("\x1b[<{cb};{col};{row}{final_byte}").into_bytes())
            }
            MouseEncoding::Urxvt => {
                // urxvt: decimal, button value offset by 32, always final `M`.
                let b = cb + 32;
                Some(format!("\x1b[{b};{col};{row}M").into_bytes())
            }
            MouseEncoding::X10 => {
                // Legacy: single bytes, each offset by 32. Release collapses to
                // button 3 (the protocol cannot distinguish which button was
                // released). Coordinates clamp to 223 (255 - 32).
                let cb_byte = if kind == MouseEventKind::Release && !is_wheel {
                    // Keep modifier/motion bits but force the low button to 3.
                    (cb & !0b11) | 0b11
                } else {
                    cb
                };
                let cx = (col.min(223) as u32) + 32;
                let cy = (row.min(223) as u32) + 32;
                let mut out = b"\x1b[M".to_vec();
                out.push((cb_byte + 32) as u8);
                out.push(cx as u8);
                out.push(cy as u8);
                Some(out)
            }
        }
    }

    /// Number of lines retained in scrollback history.
    pub fn scrollback_len(&self) -> usize {
        self.screen.history.len()
    }

    /// Current scroll-up offset (0 = following live output).
    pub fn view_offset(&self) -> usize {
        self.screen.view_offset
    }

    /// The active scroll region `(top, bottom)` rows (0-based, inclusive) on the
    /// current grid. A full-screen region is `(0, rows-1)`. Exposed for the
    /// resize-region regression test (the blank-pane-on-split guard): growing the
    /// grid past the spawn height must keep a full-screen region full-screen.
    #[cfg(test)]
    pub(crate) fn scroll_region(&self) -> (usize, usize) {
        (self.screen.scroll_top, self.screen.scroll_bottom)
    }

    /// Scroll the view up by `n` lines (toward history), clamped.
    pub fn scroll_up_view(&mut self, n: usize) {
        let max = self.screen.history.len();
        self.screen.view_offset = (self.screen.view_offset + n).min(max);
        self.screen.grid.touch();
    }

    /// Scroll the view down by `n` lines (toward live), clamped.
    pub fn scroll_down_view(&mut self, n: usize) {
        self.screen.view_offset = self.screen.view_offset.saturating_sub(n);
        self.screen.grid.touch();
    }

    /// Set the absolute scroll-up offset (clamped to history length).
    pub fn set_view_offset(&mut self, offset: usize) {
        let max = self.screen.history.len();
        self.screen.view_offset = offset.min(max);
        self.screen.grid.touch();
    }

    /// Snap back to following live output.
    pub fn scroll_to_bottom(&mut self) {
        if self.screen.view_offset != 0 {
            self.screen.view_offset = 0;
            self.screen.grid.touch();
        }
    }

    /// Jump to the very top of the scrollback (the oldest retained line).
    ///
    /// The mirror of [`scroll_to_bottom`](Self::scroll_to_bottom): it pins the
    /// view at the maximum scroll-up offset (the full history length), so the
    /// first retained line sits at the top of the viewport. A no-op when the
    /// view is already at the top. Returns `true` when the offset moved (so the
    /// caller can request a repaint only when something changed).
    pub fn scroll_to_top(&mut self) -> bool {
        let max = self.screen.history.len();
        if self.screen.view_offset != max {
            self.screen.view_offset = max;
            self.screen.grid.touch();
            true
        } else {
            false
        }
    }

    /// Walk the `rows` rows currently visible (accounting for scrollback offset)
    /// WITHOUT cloning, invoking `f(visible_row_index, row_cells)` for each.
    ///
    /// Zero-allocation: history rows are borrowed straight from the `history`
    /// deque (`&hist[line]`) and grid rows straight from `grid.row(..)` (which
    /// already returns `&[Cell]`). A history row that is shorter than the grid
    /// width is passed AS-IS — the caller is responsible for treating columns at
    /// or past `row.len()` as [`Cell::default()`] (pad-on-read), so no padded
    /// `Vec` is ever materialised. `cols()` is available on [`Terminal::grid`]
    /// for callers that need the full visible width.
    ///
    /// The closure runs while the terminal is borrowed; keep it allocation-light
    /// and NEVER re-enter the terminal (e.g. re-lock) from inside it.
    ///
    /// When `view_offset == 0` this walks exactly the live grid.
    pub fn for_visible_rows(&self, mut f: impl FnMut(usize, &[Cell])) {
        let rows = self.screen.grid.rows();
        let hist = &self.screen.history;
        let total = hist.len() + rows;
        // Bottom-anchored window of `rows` lines, shifted up by view_offset.
        let end = total.saturating_sub(self.screen.view_offset);
        let start = end.saturating_sub(rows);
        let mut visible = 0;
        for line in start..end {
            if line < hist.len() {
                // History row borrowed in place; may be shorter than `cols` —
                // the caller pads-on-read.
                f(visible, &hist[line]);
            } else {
                f(visible, self.screen.grid.row(line - hist.len()));
            }
            visible += 1;
        }
        // Pad with empty rows only when the visible window is short (total <
        // rows); normally never fires because the grid alone has `rows` rows.
        while visible < rows {
            f(visible, &[]);
            visible += 1;
        }
    }

    /// The `rows` rows currently visible, accounting for scrollback offset.
    /// When `view_offset == 0` this is exactly the live grid.
    ///
    /// Thin allocating wrapper over [`Terminal::for_visible_rows`]: each borrowed
    /// row is cloned and padded to the grid width so the returned matrix is
    /// always `rows` × `cols`. Output is byte-identical to the historical
    /// hand-rolled implementation — callers on a hot path should prefer the
    /// borrowing iterator and pad-on-read instead.
    pub fn display_rows(&self) -> Vec<Vec<Cell>> {
        let rows = self.screen.grid.rows();
        let cols = self.screen.grid.cols();
        let mut out = Vec::with_capacity(rows);
        self.for_visible_rows(|_, row| {
            let mut owned = row.to_vec();
            owned.resize(cols, Cell::default());
            out.push(owned);
        });
        out
    }

    /// All buffered lines (history + live grid) as plain text — used by search.
    pub fn all_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .screen
            .history
            .iter()
            .map(|row| row.iter().map(|c| c.c).collect::<String>())
            .collect();
        lines.extend(self.screen.grid.to_text().lines().map(|s| s.to_string()));
        lines
    }

    /// Erase the scrollback history while keeping the live screen, snapping the
    /// view back to live output. The direct analogue of feeding `ESC [ 3 J`
    /// (ED-3) — the target of the right-click "Clear scrollback" menu item and
    /// the Ctrl+Shift+K chord. Anchored metadata (images / prompt marks / command
    /// marks) is re-based; anchors that pointed into the now-gone scrollback are
    /// dropped.
    pub fn clear_scrollback(&mut self) {
        self.screen.clear_scrollback();
    }

    /// The ENTIRE buffer (scrollback history + live grid) as copy-ready text, or
    /// `None` when it holds no non-blank text. This is the whole-buffer analogue
    /// of the display-window-bound mouse-selection copy: it is the payload the
    /// right-click "Copy all" menu item and the Ctrl+Shift+A chord place on the
    /// clipboard.
    ///
    /// Uses the terminal copy convention so the result matches drawn text: the
    /// blank continuation spacer written after a wide (width-2) glyph is skipped
    /// (a pure char-width lookback, mirroring the app layer's `copy_row_text`),
    /// each row's trailing whitespace is trimmed, and trailing blank rows (the
    /// empty bottom of the live grid) are dropped. Rows are joined with `\n` with
    /// no trailing newline.
    pub fn buffer_text(&self) -> Option<String> {
        use unicode_width::UnicodeWidthChar;
        let row_text = |cells: &[Cell]| -> String {
            let mut s = String::new();
            for (i, cell) in cells.iter().enumerate() {
                // Skip the wide-glyph continuation spacer: a cell whose PREVIOUS
                // cell is a width-2 glyph (the core writes the glyph then one
                // blank cell), so no stray space is emitted around a CJK / emoji.
                if i > 0 {
                    if let Some(prev) = cells.get(i - 1) {
                        if UnicodeWidthChar::width(prev.c).unwrap_or(1) >= 2 {
                            continue;
                        }
                    }
                }
                s.push(cell.c);
            }
            s.trim_end().to_string()
        };
        let rows = self.screen.grid.rows();
        let mut lines: Vec<String> = Vec::with_capacity(self.screen.history.len() + rows);
        for row in self.screen.history.iter() {
            lines.push(row_text(row));
        }
        for r in 0..rows {
            lines.push(row_text(self.screen.grid.row(r)));
        }
        // Drop trailing blank lines (the empty bottom of the live grid) so a copy
        // of an otherwise-empty screen yields no text and a normal buffer has no
        // long blank tail.
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Resize the terminal to `rows` × `cols` (each clamped to a minimum of 1),
    /// reflowing the primary screen and adjusting the scroll region. The
    /// alternate screen is resized without reflow per the usual VT semantics.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let old_cols = self.screen.grid.cols();

        // Whether the scroll region was full-screen RELATIVE TO THE OLD HEIGHT —
        // captured BEFORE the grid is resized. This is load-bearing for the
        // grow-resize case: a default full-screen region (e.g. 0..=23 on the
        // 80x24 spawn) must be recognised as full-screen and EXPANDED to the new
        // height. Testing `scroll_bottom + 1 >= grid.rows()` AFTER the grid has
        // already grown (line below) mis-classifies that full region as a
        // *custom* one and freezes it at the old bottom — which then makes a
        // multi-line shell redraw scroll all content out of the restricted
        // region, leaving the pane blank. (The blank-pane-on-split bug: spawn at
        // 24 rows, grow past 24, conhost's resize-redraw scrolls within 0..=23.)
        let region_was_full = self.screen.region_is_full();

        // C16 — reflow on resize. When the column count changes on the PRIMARY
        // screen, re-wrap soft-wrapped logical lines to the new width instead of
        // truncating (the lossy `Grid::resize` path). The alternate screen holds
        // a full-screen TUI that redraws itself on SIGWINCH, so reflowing it is
        // both unnecessary and wrong (alt content has no logical-line history) —
        // there we fall back to the simple grid resize.
        if cols != old_cols && self.screen.saved_primary.is_none() {
            self.screen.reflow_resize(rows, cols);
        } else {
            self.screen.grid.resize(rows, cols);
            self.screen.row = self.screen.row.min(rows - 1);
            self.screen.col = self.screen.col.min(cols - 1);
            self.screen.resize_tab_stops(cols);
        }

        // A full-screen scroll region tracks the new height; a custom region is
        // clamped and reset if it no longer fits. The `region_was_full` capture
        // above (against the OLD height) is what catches a grow-resize: a region
        // that was full-screen before stays full-screen after, regardless of
        // whether the grid grew or shrank. We still also accept a region that is
        // full relative to the NEW height (covers a shrink that lands exactly on
        // the old bottom) and the degenerate `scroll_bottom == 0` case.
        if region_was_full
            || self.screen.scroll_bottom + 1 >= self.screen.grid.rows()
            || self.screen.scroll_bottom == 0
        {
            self.screen.scroll_top = 0;
            self.screen.scroll_bottom = rows - 1;
        } else {
            self.screen.clamp_scroll_region();
        }
    }
}

#[cfg(test)]
mod tests;
