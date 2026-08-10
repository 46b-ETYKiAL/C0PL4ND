//! Windowed GPU terminal shell: winit window + wgpu surface + glyphon text.
//!
//! Frameless by design — C0PL4ND draws its own brand title bar (wordmark +
//! min/max/close buttons) so the chrome matches the Retro-Future Anime OS
//! aesthetic instead of the OS default. Renders the live terminal grid from
//! `c0pl4nd-core` and forwards keyboard input to the PTY. Redraws on a light
//! poll so shell output appears promptly while an idle screen stays cheap.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use c0pl4nd_core::config::CursorStyle;
use c0pl4nd_core::layout::{
    Axis, Direction, Layout, LeafId, Preset, Rect as LRect, SplitOutcome, TabGroup,
};
use c0pl4nd_core::layout_persist::{self, LayoutSnapshot, LeafView};
use c0pl4nd_core::term::{
    ColorSet, DynamicColor, MouseButton as TermMouseButton, MouseEventKind, MouseMode,
    MouseModifiers,
};
use c0pl4nd_core::{theme::parse_hex, Config, Session, Theme};
use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};

use crate::drag::{classify_zone, DragState, DropZone};
use crate::pane_render::{
    cell_tabbar_text, chrome_quads, leaf_scissor, leaf_text_bounds, leaf_text_origin,
    ChromeRenderer, ColorRect, BORDER_PX,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{ResizeDirection, Window, WindowId};

const LINE_HEIGHT: f32 = 20.0;
const CELL_W: f32 = 9.0;
/// Translucent wash drawn over selected cells (mouse text selection). Blue, the
/// near-universal selection colour, at low alpha so the glyph stays readable.
const SELECTION_RGBA: [f32; 4] = [0.26, 0.45, 0.85, 0.35];
/// Height in physical pixels of a cell's nested-tab strip (one line). The strip
/// is drawn only when a cell holds >=2 tabs AND the cell is tall enough to spare
/// the line (auto-hide on short cells; reappears on hover via [`App::hover_leaf`]).
const CELL_TABBAR_H: f32 = LINE_HEIGHT;
/// A cell shorter than this (after borders) hides its tab strip to keep the
/// terminal grid usable; the strip reappears when the cursor hovers the cell.
const CELL_STRIP_MIN_H: i32 = (CELL_TABBAR_H as i32) + 3 * LINE_HEIGHT as i32;
/// Font-zoom clamp range (multiplier on the base grid cell size).
const FONT_SCALE_MIN: f32 = 0.5;
const FONT_SCALE_MAX: f32 = 3.0;

/// Grid cell width at a given font-zoom `scale`. Single source of truth so the
/// cols/rows, glyph positioning, and hit-testing all agree at any zoom.
#[inline]
fn scaled_cell_w(scale: f32) -> f32 {
    CELL_W * scale
}
/// Grid cell height at a given font-zoom `scale`.
#[inline]
fn scaled_cell_h(scale: f32) -> f32 {
    LINE_HEIGHT * scale
}
/// Height of the custom (frameless) title bar, in physical pixels.
const TITLEBAR_H: f32 = 30.0;
/// Each title-bar button occupies this many monospace cells. The button glyphs
/// are rendered in the chrome text buffer (CELL_W per cell), so the hit zones
/// are derived from the SAME geometry — keeping the visuals and click targets
/// aligned at any window width.
const BUTTON_CELLS: f32 = 5.0;
/// Total cells for the 3-button (min / max / close) cluster.
const BUTTONS_CELLS: f32 = BUTTON_CELLS * 3.0;
/// Gap in pixels between the close button and the window's right edge.
const BTN_RIGHT_MARGIN: f32 = 8.0;
/// Thickness in pixels of the invisible window-edge resize band (frameless).
/// 8px is the comfortable grab target for a decorations(false) window (a 6px
/// band is hard to hit, which read as "not resizable").
const RESIZE_BORDER: f64 = 8.0;
/// Left inset (px) of the chrome text buffer; used for column<->pixel mapping.
const CHROME_LEFT: f32 = 6.0;

/// Public entrypoint: open the windowed terminal.
pub fn run_gui(config: &Config) -> Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(config.clone());
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Write `text` to the OS clipboard without a third-party crate (offline build:
/// no new deps). Feeds the text via the child's stdin to the platform clipboard
/// tool: `clip` on Windows, `pbcopy` on macOS, `wl-copy`/`xclip`/`xsel` on Linux
/// (first available). Best-effort — failures are logged, never fatal. Used to
/// honour OSC 52 clipboard-write requests (E10 wiring).
fn write_os_clipboard(text: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let spawn = |mut cmd: Command| -> bool {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                // Dropping stdin closes it; wait so the tool flushes the clipboard.
                child.wait().map(|s| s.success()).unwrap_or(false)
            }
            Err(_) => false,
        }
    };
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("clip");
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        if !spawn(cmd) {
            tracing::warn!("clipboard write failed (clip)");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if !spawn(Command::new("pbcopy")) {
            tracing::warn!("clipboard write failed (pbcopy)");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let ok = [
            ("wl-copy", &[][..]),
            ("xclip", &["-selection", "clipboard"][..]),
            ("xsel", &["--clipboard", "--input"][..]),
        ]
        .iter()
        .any(|(tool, args)| {
            let mut cmd = Command::new(tool);
            cmd.args(*args);
            spawn(cmd)
        });
        if !ok {
            tracing::warn!("clipboard write failed (no wl-copy/xclip/xsel)");
        }
    }
}

/// Read the OS clipboard as UTF-8 text without a third-party crate (the build
/// environment cannot fetch new dependencies). Shells out to the platform's
/// standard clipboard tool: PowerShell `Get-Clipboard` on Windows, `pbpaste`
/// on macOS, and `wl-paste`/`xclip`/`xsel` on Linux (first available). Returns
/// `None` when no tool succeeds. CRLF is normalised to LF and a single trailing
/// newline that the tools append is trimmed.
fn read_os_clipboard() -> Option<String> {
    use std::process::Command;
    let out = {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            Command::new("powershell")
                .args(["-NoProfile", "-Command", "Get-Clipboard -Raw"])
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .output()
                .ok()
        }
        #[cfg(target_os = "macos")]
        {
            Command::new("pbpaste").output().ok()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            ["wl-paste", "xclip", "xsel"].iter().find_map(|tool| {
                let mut cmd = Command::new(tool);
                if *tool == "xclip" {
                    cmd.args(["-selection", "clipboard", "-o"]);
                } else if *tool == "xsel" {
                    cmd.args(["--clipboard", "--output"]);
                }
                cmd.output().ok().filter(|o| o.status.success())
            })
        }
    }?;
    if !out.status.success() {
        return None;
    }
    let mut s = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    if s.ends_with('\n') {
        s.pop();
    }
    Some(s)
}

/// Open a path with the OS default handler (editor for config.toml). Detached +
/// no console window; failures are non-fatal (the user can open it manually).
fn open_path(path: &std::path::Path) {
    use std::process::Command;
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(path).spawn();
    }
}

/// Format a dropped file path for insertion at the shell prompt. The path is
/// double-quoted when it contains whitespace or shell-significant characters
/// (so paths with spaces survive as a single argument), otherwise inserted raw.
/// A trailing space separates it from whatever the user types next. The path is
/// returned as TEXT to insert — the caller never appends a newline, so a dropped
/// file is never executed on the user's behalf.
fn format_dropped_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let needs_quote = s.is_empty()
        || s.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\''
                        | '&'
                        | '|'
                        | '('
                        | ')'
                        | '<'
                        | '>'
                        | '^'
                        | ';'
                        | '`'
                        | '$'
                        | '%'
                        | '!'
                )
        });
    if needs_quote {
        // Embedded double quotes are illegal in Windows filenames and rare
        // elsewhere; escape them defensively so the quoting can't be broken out of.
        let escaped = s.replace('"', "\\\"");
        format!("\"{escaped}\" ")
    } else {
        format!("{s} ")
    }
}

/// Keyboard shortcut shown next to a command-palette action, so the palette
/// teaches its own shortcuts (the standard discoverability bridge). Empty when
/// the action has no global shortcut. `Ctrl` is `Cmd` on macOS.
fn action_hint(action: &str) -> &'static str {
    match action {
        "New Tab" => "Ctrl+Shift+T",
        "Close Tab" => "Ctrl+Shift+W",
        "Next Tab" => "Ctrl+Shift+]",
        "Previous Tab" => "Ctrl+Shift+[",
        "Split Right" => "Ctrl+Shift+D",
        "Split Down" => "Ctrl+Shift+E",
        "Search" => "Ctrl+Shift+F",
        "Settings" => "Ctrl+,",
        _ => "",
    }
}

/// True when a saved window rect (physical px) keeps at least a usable strip
/// on-screen across the currently-connected monitors — the D2 multi-monitor
/// safety check. Requires ≥64px of the window to overlap some monitor so an
/// unplugged display / resolution change can't orphan the window off-screen.
fn geometry_on_screen(
    px: i32,
    py: i32,
    sw: u32,
    sh: u32,
    monitors: &[winit::monitor::MonitorHandle],
) -> bool {
    if monitors.is_empty() {
        // No monitor info (e.g. Wayland headless) — accept the size, decline to
        // assert position validity by treating it as on-screen (winit will
        // place it). Conservative: better than discarding a valid size.
        return true;
    }
    let win = LRect::new(px, py, sw as i32, sh as i32);
    monitors.iter().any(|m| {
        let mp = m.position();
        let ms = m.size();
        let mon = LRect::new(mp.x, mp.y, ms.width as i32, ms.height as i32);
        window_visible_on_monitor(win, mon)
    })
}

/// True when the `win` rect overlaps the `mon` rect by at least [`MIN_VISIBLE`]
/// px on BOTH axes (physical pixels).
///
/// Split out of [`geometry_on_screen`] so the multi-monitor safety math is unit
/// testable: `winit::monitor::MonitorHandle` wraps an opaque platform handle and
/// has no public constructor, so a test can only ever pass `geometry_on_screen`
/// an EMPTY monitor slice — the overlap rule itself would otherwise be
/// unreachable from a test.
fn window_visible_on_monitor(win: LRect, mon: LRect) -> bool {
    /// Minimum on-screen overlap (px, per axis) for a saved rect to be usable.
    const MIN_VISIBLE: i32 = 64;
    let ox = ((win.x + win.w).min(mon.x + mon.w) - win.x.max(mon.x)).max(0);
    let oy = ((win.y + win.h).min(mon.y + mon.h) - win.y.max(mon.y)).max(0);
    ox >= MIN_VISIBLE && oy >= MIN_VISIBLE
}

/// Find a URL spanning column `col` within a row of characters (E9). Expands
/// left/right from the click column over non-whitespace, then accepts the token
/// only if it begins with `http://` or `https://`. Trailing punctuation common
/// in prose (`.,;:!?` and closing brackets/quotes) is trimmed so a URL at the
/// end of a sentence opens cleanly. Returns `None` when no URL covers the column.
///
/// SECURITY: `file://` is deliberately NOT accepted. PTY output is fully
/// attacker-controlled, so auto-detecting a `file://host/share/evil.exe` URL
/// and feeding it to `open_path` (`cmd /C start`) would let one ctrl-click
/// launch an arbitrary local/UNC executable. The modern egui shell's
/// `hyperlink::find_urls` is http(s)-only for the same reason; this matches it.
/// The RFC-3986 URL character set used to bound a ctrl-click URL token, mirroring
/// the egui shell's `hyperlink::is_url_char` so both shells accept the same URL
/// shape (and reject the same shell-metacharacter smuggling).
fn is_url_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(ch)
}

fn find_url_in_line(chars: &[char], col: usize) -> Option<String> {
    if chars.is_empty() {
        return None;
    }
    let col = col.min(chars.len() - 1);
    // Bound the token to the RFC-3986 URL charset (matching the egui shell's
    // `hyperlink::is_url_char`), NOT merely "non-whitespace". This stops a
    // PTY-printed `http://x|whoami` / `http://x^...` / `` http://x`cmd` `` from
    // smuggling a cmd metacharacter into the `cmd /C start` opener: `|`, `^`,
    // `` ` ``, `<`, `>`, `"`, `{`, `}`, `\` are not URL chars, so they terminate
    // the token. (`&` IS a legitimate URL query char and stays; the stable-Rust
    // cmd-arg escaping from CVE-2024-24576 neutralizes it at spawn time anyway.)
    if !is_url_char(chars[col]) {
        return None;
    }
    let mut start = col;
    while start > 0 && is_url_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < chars.len() && is_url_char(chars[end + 1]) {
        end += 1;
    }
    let mut token: String = chars[start..=end].iter().collect();
    // Trim trailing punctuation that is rarely part of the URL.
    while let Some(last) = token.chars().last() {
        if matches!(
            last,
            '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\'' | '>'
        ) {
            token.pop();
        } else {
            break;
        }
    }
    let is_url = token.starts_with("http://") || token.starts_with("https://");
    if is_url && token.len() > "https://".len() {
        Some(token)
    } else {
        None
    }
}

/// Which title-bar button a point falls on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitlebarHit {
    None,
    Drag,
    Minimize,
    Maximize,
    Close,
}

/// A clickable region of the modern tab strip. `Tab(i)` is the chip for window
/// tab `i`; `NewTab` is the `+` affordance. Pixel x-ranges are recomputed each
/// frame from the chrome buffer's actual glyph layout (the rendered glyphs and
/// the click zones therefore always agree, regardless of font advance / DPI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabZone {
    Tab(usize),
    NewTab,
    Settings,
}

/// Pure title-bar hit-test: classify the physical-pixel point `(x, y)` against
/// a title bar on a `width`-px surface. Split out of [`App::hit_titlebar`] —
/// whose only impure step is reading `width` off the live wgpu surface — so the
/// caption-button / drag-strip routing is unit testable without a window/GPU.
///
/// Below the title bar → [`TitlebarHit::None`]; left of the button cluster →
/// [`TitlebarHit::Drag`]; otherwise the cluster slot the point falls in, with
/// anything past the third slot resolving to Close so the right-most pixels
/// (the corner the user throws the mouse at) are never dead.
fn titlebar_hit_at(x: f64, y: f64, width: f64) -> TitlebarHit {
    if y > TITLEBAR_H as f64 {
        return TitlebarHit::None;
    }
    let buttons_left = App::buttons_left_px(width as f32) as f64;
    if x < buttons_left {
        return TitlebarHit::Drag;
    }
    match ((x - buttons_left) / (BUTTON_CELLS as f64 * CELL_W as f64)) as i32 {
        0 => TitlebarHit::Minimize,
        1 => TitlebarHit::Maximize,
        _ => TitlebarHit::Close,
    }
}

/// Pure window-edge resize hit-test: classify `(x, y)` against the
/// [`RESIZE_BORDER`] band of a `w`×`h` surface. Split out of
/// [`App::hit_resize_edge`] — whose only impure step is reading the surface size
/// off the live GPU — so the frameless resize band is unit testable. A
/// degenerate (0-sized) surface yields `None`.
fn resize_edge_at(x: f64, y: f64, w: f64, h: f64) -> Option<ResizeDirection> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let b = RESIZE_BORDER;
    let (left, right, top, bottom) = (x <= b, x >= w - b, y <= b, y >= h - b);
    match (top, bottom, left, right) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (true, _, _, true) => Some(ResizeDirection::NorthEast),
        (_, true, true, _) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, ..) => Some(ResizeDirection::North),
        (_, true, ..) => Some(ResizeDirection::South),
        (_, _, true, _) => Some(ResizeDirection::West),
        (_, _, _, true) => Some(ResizeDirection::East),
        _ => None,
    }
}

/// Pure tab-strip hit-test: the zone whose pixel x-range `[x0, x1)` contains
/// `x`, if any. Split out from `App::hit_tab` so the click routing is unit
/// testable without a live window/GPU.
fn tab_zone_at(x: f32, zones: &[(TabZone, f32, f32)]) -> Option<TabZone> {
    zones
        .iter()
        .find(|&&(_, x0, x1)| x >= x0 && x < x1)
        .map(|&(z, _, _)| z)
}

/// Left x for a caption glyph so it sits horizontally centred in its
/// `cell_w`-wide backplate, given the cluster origin `cluster_left`, the
/// button's `idx` in the cluster, and the glyph's measured advance `glyph_w`.
/// Split out so the centring math is unit-testable independent of the GPU.
/// Clamps to the slot's left edge when a glyph is wider than its cell.
fn caption_glyph_left(cluster_left: f32, idx: usize, cell_w: f32, glyph_w: f32) -> f32 {
    let slot_x = cluster_left + idx as f32 * cell_w;
    slot_x + ((cell_w - glyph_w) / 2.0).max(0.0)
}

/// Pure settings-panel hit-test: map a click `(x, y)` and the current surface
/// width to a settings row index, mirroring the panel's render geometry (panel
/// at `left = (w*0.25).max(40)`, `top = TITLEBAR_H + 40`, a 2-line header, then
/// the rows at `LINE_HEIGHT` spacing). Split out so it is unit testable.
fn settings_row_index(x: f64, y: f64, surface_width: f64) -> Option<usize> {
    let panel_left = (surface_width * 0.25).max(40.0);
    if x < panel_left - 8.0 || x > panel_left + 440.0 {
        return None;
    }
    let panel_top = TITLEBAR_H as f64 + 40.0;
    let lh = LINE_HEIGHT as f64;
    let rel = y - (panel_top + 2.0 * lh);
    if rel < 0.0 {
        return None;
    }
    let i = (rel / lh) as usize;
    (i < SettingRow::ALL.len()).then_some(i)
}

/// Resolve the Ctrl+1..9 tab-jump shortcut (1-based; `9` always selects the
/// last tab) to a 0-based tab index, or `None` if out of range. Split out so
/// the keyboard tab nav is unit testable.
fn resolve_tab_number(n: u8, tab_count: usize) -> Option<usize> {
    if tab_count == 0 {
        return None;
    }
    let idx = if n == 9 {
        tab_count - 1
    } else {
        (n as usize).saturating_sub(1)
    };
    (idx < tab_count).then_some(idx)
}

/// Pick the surface composite-alpha mode. When transparency is NOT wanted, use
/// the surface's native (first) mode — Opaque on every desktop backend, exactly
/// as before. When transparency IS wanted, prefer PostMultiplied (the
/// compositor multiplies), then PreMultiplied, and gracefully fall back to the
/// native mode if the backend exposes neither — so the window simply stays
/// solid instead of failing. Split out for unit testing.
fn choose_alpha_mode(
    want_transparent: bool,
    modes: &[wgpu::CompositeAlphaMode],
) -> wgpu::CompositeAlphaMode {
    let native = modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Opaque);
    if !want_transparent {
        return native;
    }
    for pref in [
        wgpu::CompositeAlphaMode::PostMultiplied,
        wgpu::CompositeAlphaMode::PreMultiplied,
        // Inherit defers compositing to the OS/window: for a winit window
        // created `with_transparent(true)` (which is the only time we ask for
        // transparency) this lets the DWM acrylic backdrop show through on
        // GPUs that don't expose explicit pre/post-multiplied modes (e.g. the
        // Intel/Vulkan surface, which offers only [Opaque, Inherit]).
        wgpu::CompositeAlphaMode::Inherit,
    ] {
        if modes.contains(&pref) {
            return pref;
        }
    }
    native
}

struct Gpu {
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    text_renderer: TextRenderer,
    metrics: Metrics,
    /// One grid text buffer per visible leaf, keyed by `LeafId`. Recreated /
    /// resized as the layout changes; a single-pane window keeps one entry.
    leaf_buffers: HashMap<LeafId, Buffer>,
    /// One nested-tab-strip text buffer per visible cell that currently shows a
    /// strip (>=2 tabs, or a short cell under the cursor). Keyed by `LeafId`.
    tabbar_buffers: HashMap<LeafId, Buffer>,
    chrome_buffer: Buffer,
    /// The min/max/close caption glyphs — ONE buffer per glyph (no padding) so
    /// each can be independently centred (H+V) in its fixed backplate cell.
    /// Space-padding could not centre symbol glyphs (□ U+25A1, ✕ U+2715) whose
    /// font-fallback advance differs from the space advance, leaving them
    /// visibly off-centre in their hover squares. Order: [minimize, max, close].
    caption_buffers: [Buffer; 3],
    palette_buffer: Buffer,
    splash_buffer: Buffer,
    image_renderer: crate::image_render::ImageRenderer,
    chrome_renderer: ChromeRenderer,
    gpu_name: String,
    /// Clickable tab-strip regions (zone, x_start, x_end) in physical pixels,
    /// recomputed every frame from the chrome buffer's glyph layout. Read by
    /// `hit_tab` to route titlebar clicks to tab-switch / new-tab.
    tab_zones: Vec<(TabZone, f32, f32)>,
}

impl Gpu {
    /// Drop grid + tab-strip buffers for leaves no longer present in `live`.
    fn retain_leaf_buffers(&mut self, live: &[LeafId]) {
        self.leaf_buffers.retain(|id, _| live.contains(id));
        self.tabbar_buffers.retain(|id, _| live.contains(id));
    }
}

/// An in-progress or completed mouse text selection over one leaf's visible
/// grid. `anchor` is where the drag began, `head` is the current/last cell;
/// both are `(row, col)` in DISPLAY-grid coordinates (visible rows, 0-based).
/// `active` is true while the mouse button is held (drag extending).
#[derive(Debug, Clone, Copy)]
struct Selection {
    leaf: LeafId,
    anchor: (usize, usize),
    head: (usize, usize),
    active: bool,
}

impl Selection {
    /// The selection's (start, end) in reading order (top-left → bottom-right),
    /// normalising whichever of anchor/head comes first on screen. The render
    /// highlight and text extraction both build line-oriented ranges from this
    /// (first line start.col→EOL, middle lines full, last line BOL→end.col).
    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if (self.anchor.0, self.anchor.1) <= (self.head.0, self.head.1) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

struct App {
    config: Config,
    theme: Theme,
    /// One tab per entry; each tab holds one or more split panes.
    tabs: Vec<Tab>,
    active: usize,
    gpu: Option<Gpu>,
    next_poll: Instant,
    cursor: (f64, f64),
    /// Last cursor icon we asked winit for, so frameless edge/button hover only
    /// issues a `set_cursor` when the zone actually changes (avoids per-pixel churn).
    chrome_cursor: winit::window::CursorIcon,
    /// Timestamp of the last left-press on the titlebar drag area, so a second
    /// press within the double-click window toggles maximize (standard chrome).
    last_titlebar_click: Option<Instant>,
    /// Caption button currently under the cursor (drives the hover backplate).
    /// Only Minimize/Maximize/Close are meaningful; None otherwise.
    hovered_button: Option<TitlebarHit>,
    /// Tab-strip zone (tab chip / '+' / gear) currently under the cursor, for
    /// the modern hover backplate. None when not over the strip.
    hovered_tab: Option<TabZone>,
    /// Caption button currently pressed (mouse down on it), for the stronger
    /// active-state backplate. Cleared on mouse release.
    pressed_button: Option<TitlebarHit>,
    /// Debounce clock + dirty flag for window-geometry persistence (D2). A
    /// rapid interactive resize/move sets `geom_dirty`; the actual config write
    /// is throttled in `about_to_wait` so we never write per drag pixel.
    geom_dirty: bool,
    last_geom_save: Instant,
    /// Phase clock for the terminal cursor blink (E5/E7); a 530ms half-period
    /// toggle keyed off this start instant.
    blink_start: Instant,
    /// Last time a blink-driven redraw was issued, so an idle window with a
    /// blinking cursor still animates without redrawing every poll tick.
    last_blink_redraw: Instant,
    /// Leaf currently under the cursor (drives the short-cell tab-strip hover
    /// reveal in T4.2). `None` when the cursor is over chrome / no cell.
    hover_leaf: Option<LeafId>,
    modifiers: ModifiersState,
    search_mode: bool,
    search_query: String,
    search_matches: Vec<c0pl4nd_core::search::SearchMatch>,
    search_idx: usize,
    palette_mode: bool,
    palette_query: String,
    palette_idx: usize,
    /// neofetch-style startup splash (logo + system info), drawn as an overlay
    /// over the first tab until the first keypress. `None` once dismissed.
    splash: Option<String>,
    /// Latest pending surface size whose per-leaf PTY resize has not yet been
    /// applied. Set on every `Resized`; drained at ~30 Hz in `about_to_wait`
    /// so a rapid interactive resize issues at most ~30 PTY resizes/sec while
    /// the final size is always applied.
    pending_resize: Option<(u32, u32)>,
    /// Timestamp of the last applied per-leaf PTY resize (debounce clock).
    last_pty_resize: Instant,
    /// Mouse pane-drag state machine (Phase 5). `Idle` unless a `Ctrl+Shift`
    /// drag is in progress.
    drag: DragState,
    /// `true` while the OS reports a reduced-motion preference; disables the
    /// drag ghost / relayout glide animations (accessibility axis).
    reduced_motion: bool,
    /// Active modal workspace prompt (Phase 6): saving asks for a name,
    /// restoring lists saved workspaces. `None` when no prompt is open.
    workspace_prompt: Option<WorkspacePrompt>,
    /// Active in-app settings panel (D3). `None` when closed.
    settings: Option<SettingsPanel>,
    /// Mouse text selection over a leaf's grid. `Some` once a left-drag begins
    /// (in panes where the program has NOT enabled mouse reporting); cleared on
    /// PTY-bound keypress or a new click. `active` is true while dragging.
    selection: Option<Selection>,
    /// Clipboard text awaiting paste confirmation (paste-safety). `Some` while
    /// the multi-line-paste warning overlay is up; Enter commits, Esc cancels.
    pending_paste: Option<String>,
    /// Whether the window currently has OS keyboard focus. Drives focus
    /// reporting (DEC `?1004` → `ESC[I`/`ESC[O`) and the unfocused-notification
    /// taskbar flash. Starts `true` (a fresh window is focused).
    focused: bool,
    /// Live font-zoom factor. The grid cell size (`cell_w`/`cell_h`) and the
    /// grid glyph metrics both scale by this; chrome (titlebar) stays fixed.
    /// `1.0` = the configured font size. Changed by Ctrl +/−/0 and Ctrl+wheel.
    font_scale: f32,
}

/// A modal overlay for the Phase-6 workspace save / restore flow. Reuses the
/// command-palette text-input idiom (typed query + arrow selection) so the UX
/// is consistent and keyboard-complete (accessibility axis: no mouse path
/// required).
enum WorkspacePrompt {
    /// "Save Layout As…": the user types a workspace name; Enter writes it.
    Save { name: String },
    /// "Restore Layout": pick one of the discovered saved workspaces.
    Restore { names: Vec<String>, idx: usize },
}

/// The in-app settings panel (D3): a keyboard-driven overlay listing the most
/// useful settings. Up/Down move the selection, Left/Right adjust the focused
/// value, Esc closes. Every change is applied live where possible and written
/// back to the TOML config so the panel and the file stay consistent.
struct SettingsPanel {
    /// Currently selected row.
    idx: usize,
    /// Theme file stems discovered under the themes dir, for the theme cycler.
    themes: Vec<String>,
}

/// The ordered list of editable settings rows in the panel. Each maps to a
/// `Config` field; the render + key handler drive them generically.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingRow {
    FontSize,
    Theme,
    CursorStyle,
    CursorBlink,
    Scrollback,
    Opacity,
    StartupPanel,
}

impl SettingRow {
    /// Rows in display order.
    const ALL: [SettingRow; 7] = [
        SettingRow::FontSize,
        SettingRow::Theme,
        SettingRow::CursorStyle,
        SettingRow::CursorBlink,
        SettingRow::Scrollback,
        SettingRow::Opacity,
        SettingRow::StartupPanel,
    ];

    fn label(self) -> &'static str {
        match self {
            SettingRow::FontSize => "Font size",
            SettingRow::Theme => "Theme",
            SettingRow::CursorStyle => "Cursor style",
            SettingRow::CursorBlink => "Cursor blink",
            SettingRow::Scrollback => "Scrollback lines",
            SettingRow::Opacity => "Opacity",
            SettingRow::StartupPanel => "Startup panel",
        }
    }
}

/// Actions available from the command palette. The `Layout: …` entries apply a
/// quick-layout preset; "Save Layout As…" / "Restore Layout" open the workspace
/// prompts (Phase 6).
const PALETTE_ACTIONS: &[&str] = &[
    "New Tab",
    "Close Tab",
    "Next Tab",
    "Previous Tab",
    "New Cell Tab",
    "Next Cell Tab",
    "Previous Cell Tab",
    "Split Right",
    "Split Down",
    "Focus Next Pane",
    "Zoom Pane",
    "Equalize Panes",
    "Auto Arrange",
    "Layout: 1",
    "Layout: 1x2",
    "Layout: 2x1",
    "Layout: 1+2",
    "Layout: 2x2",
    "Layout: 1+3",
    "Layout: 2x3",
    "Save Layout As\u{2026}",
    "Restore Layout",
    "Search",
    "Scroll To Bottom",
    "Settings",
    "Open Config File",
    "Quit",
];

/// A grid cell (one leaf): a core [`TabGroup`] tracking the tab structure plus
/// the live `Session`s, one per tab, parallel to `group.tabs`. The visible
/// session is `sessions[group.active]`. A 1-tab cell renders no tab strip.
struct Cell {
    group: TabGroup,
    sessions: Vec<Session>,
}

impl Cell {
    /// A single-tab cell for `id` holding `session`.
    fn single(id: LeafId, session: Session) -> Self {
        Cell {
            group: TabGroup::new(id, 0),
            sessions: vec![session],
        }
    }

    /// The visible tab's session.
    fn active(&self) -> Option<&Session> {
        self.sessions.get(self.group.active)
    }

    /// The visible tab's session (mutable).
    fn active_mut(&mut self) -> Option<&mut Session> {
        self.sessions.get_mut(self.group.active)
    }

    /// Append `session` as a new tab and make it active.
    fn add(&mut self, session: Session) {
        self.group.add_tab(self.sessions.len() as u64);
        self.sessions.push(session);
    }

    /// Close the visible tab. Returns true when the cell is now empty (the
    /// caller must collapse the leaf out of the tree).
    fn close_active(&mut self) -> bool {
        let idx = self.group.active.min(self.sessions.len().saturating_sub(1));
        if idx < self.sessions.len() {
            self.sessions.remove(idx);
        }
        let (_slot, empty) = self.group.close_active();
        empty
    }

    /// Number of tabs in this cell.
    fn tab_count(&self) -> usize {
        self.sessions.len()
    }
}

/// A window tab holds a split-tree [`Layout`] and one [`Cell`] per leaf. Each
/// cell is a `TabGroup` that may hold multiple nested terminal tabs.
struct Tab {
    layout: Layout,
    cells: HashMap<LeafId, Cell>,
}

impl Tab {
    /// A fresh tab: a single leaf (the root) holding a single-tab `session`.
    fn single(session: Session) -> Self {
        let layout = Layout::new();
        let mut cells = HashMap::new();
        cells.insert(layout.focused, Cell::single(layout.focused, session));
        Tab { layout, cells }
    }

    /// The focused leaf's visible session.
    fn focused_session(&self) -> Option<&Session> {
        self.cells.get(&self.layout.focused).and_then(Cell::active)
    }

    /// The focused leaf's visible session (mutable).
    fn focused_session_mut(&mut self) -> Option<&mut Session> {
        let f = self.layout.focused;
        self.cells.get_mut(&f).and_then(Cell::active_mut)
    }
}

impl App {
    fn new(config: Config) -> Self {
        let theme = load_theme(&config.theme).unwrap_or_else(Theme::builtin_void);
        App {
            config,
            theme,
            tabs: Vec::new(),
            active: 0,
            gpu: None,
            next_poll: Instant::now(),
            cursor: (0.0, 0.0),
            chrome_cursor: winit::window::CursorIcon::Default,
            last_titlebar_click: None,
            hovered_button: None,
            hovered_tab: None,
            pressed_button: None,
            geom_dirty: false,
            last_geom_save: Instant::now(),
            blink_start: Instant::now(),
            last_blink_redraw: Instant::now(),
            hover_leaf: None,
            modifiers: ModifiersState::empty(),
            search_mode: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_idx: 0,
            palette_mode: false,
            palette_query: String::new(),
            palette_idx: 0,
            splash: None,
            pending_resize: None,
            last_pty_resize: Instant::now(),
            drag: DragState::Idle,
            // No portable winit query for reduced-motion; honour the env var
            // convention used by CI / accessibility tooling. Defaults to off.
            reduced_motion: std::env::var("C0PL4ND_REDUCED_MOTION")
                .map(|v| v != "0" && !v.is_empty())
                .unwrap_or(false),
            workspace_prompt: None,
            settings: None,
            selection: None,
            pending_paste: None,
            focused: true,
            font_scale: 1.0,
        }
    }

    fn enter_palette(&mut self) {
        self.palette_mode = true;
        self.palette_query.clear();
        self.palette_idx = 0;
    }

    fn palette_filtered(&self) -> Vec<&'static str> {
        c0pl4nd_core::fuzzy::filter_sorted(PALETTE_ACTIONS, &self.palette_query)
    }

    fn handle_palette_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) {
        if let Some(action) = self.palette_key_nav(key) {
            self.execute_palette_action(action, event_loop);
        }
        self.request_redraw();
    }

    /// Apply `key` to the palette's OWN state (open/closed, query, selection)
    /// and return the action `Enter` picked, if any.
    ///
    /// Split out of [`App::handle_palette_key`] so the palette's navigation,
    /// filtering, and text editing are unit testable: only the returned action's
    /// EXECUTION needs an `&ActiveEventLoop`, which exists solely inside a
    /// running winit event loop and cannot be constructed by a test.
    fn palette_key_nav(&mut self, key: &Key) -> Option<&'static str> {
        match key {
            Key::Named(NamedKey::Escape) => {
                self.palette_mode = false;
                None
            }
            Key::Named(NamedKey::ArrowDown) => {
                let n = self.palette_filtered().len().max(1);
                self.palette_idx = (self.palette_idx + 1) % n;
                None
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = self.palette_filtered().len().max(1);
                self.palette_idx = (self.palette_idx + n - 1) % n;
                None
            }
            Key::Named(NamedKey::Enter) => {
                let pick = self.palette_filtered().get(self.palette_idx).copied();
                self.palette_mode = false;
                pick
            }
            Key::Named(NamedKey::Backspace) => {
                self.palette_query.pop();
                self.palette_idx = 0;
                None
            }
            Key::Character(s) => {
                self.palette_query.push_str(s);
                self.palette_idx = 0;
                None
            }
            _ => None,
        }
    }

    fn execute_palette_action(&mut self, action: &str, event_loop: &ActiveEventLoop) {
        match action {
            "New Tab" => self.spawn_tab(),
            "Close Tab" => self.close_active_tab(event_loop),
            "Next Tab" => self.next_tab(),
            "Previous Tab" => self.prev_tab(),
            "New Cell Tab" => self.spawn_cell_tab(),
            "Next Cell Tab" => self.next_cell_tab(),
            "Previous Cell Tab" => self.prev_cell_tab(),
            "Split Right" => self.split_active(Axis::Horizontal),
            "Split Down" => self.split_active(Axis::Vertical),
            "Focus Next Pane" => self.focus_next_pane(),
            "Zoom Pane" => self.toggle_zoom(),
            "Equalize Panes" => self.equalize(),
            "Auto Arrange" => self.auto_balance(),
            "Layout: 1" => self.apply_preset(Preset::Single),
            "Layout: 1x2" => self.apply_preset(Preset::TwoColumns),
            "Layout: 2x1" => self.apply_preset(Preset::TwoRows),
            "Layout: 1+2" => self.apply_preset(Preset::MainLeftTwoStacked),
            "Layout: 2x2" => self.apply_preset(Preset::Grid2x2),
            "Layout: 1+3" => self.apply_preset(Preset::MainLeftThreeStacked),
            "Layout: 2x3" => self.apply_preset(Preset::Grid2x3),
            "Save Layout As\u{2026}" => self.open_save_prompt(),
            "Restore Layout" => self.open_restore_prompt(),
            "Search" => self.enter_search(),
            "Scroll To Bottom" => {
                if let Some(s) = self.active_session() {
                    if let Ok(mut t) = s.terminal().lock() {
                        t.scroll_to_bottom();
                    }
                }
            }
            "Settings" => self.open_settings_panel(),
            "Open Config File" => self.open_config_file(),
            "Quit" => event_loop.exit(),
            _ => {}
        }
    }

    /// Open the user config (`%APPDATA%\c0pl4nd\config.toml` / `~/.config/...`)
    /// in the OS default editor, creating a commented starter file if absent.
    /// The config is live-reloaded on save (see `Config::default_path`), so this
    /// is the discoverable Settings entry point until the in-app panel lands.
    fn open_config_file(&self) {
        let Some(path) = Config::default_path() else {
            return;
        };
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(
                &path,
                "# C0PL4ND configuration — see CONFIG.md for all options.\n\
                 # Edits are applied on save.\n\n\
                 [window]\n# cols = 100\n# rows = 30\n\n\
                 [font]\n# size = 14.0\n\n\
                 [theme]\n# name = \"wired-noir\"\n",
            );
        }
        open_path(&path);
    }

    /// Open the in-app settings panel (D3). Discovers the available theme
    /// stems so the theme row can cycle them.
    fn open_settings_panel(&mut self) {
        let themes = Self::discover_themes();
        self.settings = Some(SettingsPanel { idx: 0, themes });
        self.request_redraw();
    }

    /// Discover theme file stems from the bundled `assets/themes/` dir and the
    /// user's config `themes/` dir (deduped, sorted). Always includes the
    /// current config theme so the cycler can represent it even if the dir scan
    /// misses it.
    fn discover_themes() -> Vec<String> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        dirs.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("assets")
                .join("themes"),
        );
        if let Some(cfg) = Config::default_path() {
            if let Some(parent) = cfg.parent() {
                dirs.push(parent.join("themes"));
            }
        }
        let mut names: Vec<String> = Vec::new();
        for dir in dirs {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if let Some(stem) = p.file_name().and_then(|s| s.to_str()) {
                        if let Some(name) = stem.strip_suffix(".toml") {
                            if !names.iter().any(|n| n == name) {
                                names.push(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        names.sort();
        if names.is_empty() {
            names.push("itasha-corp".to_string());
        }
        names
    }

    /// Persist the current in-memory config to the user config file (D3). The
    /// settings panel calls this after every change so the file and the live
    /// state never drift. Errors are logged, never fatal.
    ///
    /// `save_to` now MERGES over the existing document, so a key this build has
    /// no field for survives. This shell does NOT carry the egui app's
    /// `config_unreadable` quarantine, though: if the startup load in `main.rs`
    /// failed, `self.config` is defaults and this write replaces the user's
    /// known-field values with them. That gap is accepted only because the
    /// legacy winit shell is behind the default-off `legacy-winit` feature and
    /// is a preserved comparison shell, not a shipped surface. Porting the
    /// quarantine means routing this through the same seam as
    /// `C0pl4ndApp::save_config_guarded`.
    fn persist_config(&self) {
        if let Some(path) = Config::default_path() {
            if let Err(e) = self.config.save_to(&path) {
                tracing::warn!("could not save config: {e}");
            }
        }
    }

    /// Handle a key while the settings panel is open (D3). Up/Down move the
    /// selection; Left/Right (and Enter for toggles) adjust the focused value
    /// with immediate live-apply + persist; Esc closes.
    fn handle_settings_key(&mut self, key: &Key) {
        let rows = SettingRow::ALL;
        // Read the panel state into locals so we never hold a borrow of
        // `self.settings` across a `self`-mutating call below.
        let Some((idx, themes)) = self.settings.as_ref().map(|p| (p.idx, p.themes.clone())) else {
            return;
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                self.settings = None;
                self.request_redraw();
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(p) = self.settings.as_mut() {
                    p.idx = (idx + 1) % rows.len();
                }
                self.request_redraw();
                return;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(p) = self.settings.as_mut() {
                    p.idx = (idx + rows.len() - 1) % rows.len();
                }
                self.request_redraw();
                return;
            }
            _ => {}
        }
        // Value-change keys: Left = decrement, Right/Enter/Space = increment
        // (Enter/Space toggle booleans / advance enums).
        let delta: i32 = match key {
            Key::Named(NamedKey::ArrowLeft) => -1,
            Key::Named(NamedKey::ArrowRight)
            | Key::Named(NamedKey::Enter)
            | Key::Named(NamedKey::Space) => 1,
            _ => return,
        };
        let row = rows[idx];
        self.adjust_setting(row, delta, &themes);
        self.persist_config();
        self.request_redraw();
    }

    /// Map a physical-pixel point to a settings-panel row index, when the panel
    /// is open. Mirrors the panel's render geometry: the panel is drawn at
    /// `left = (w*0.25).max(40)`, `top = TITLEBAR_H + 40`; a 2-line header
    /// ("settings" + blank) precedes the rows at `LINE_HEIGHT` spacing.
    fn settings_row_at(&self, x: f64, y: f64) -> Option<usize> {
        self.settings.as_ref()?;
        let w = self.gpu.as_ref()?.surface_config.width as f64;
        settings_row_index(x, y, w)
    }

    /// Click a settings-panel row: select it, or — if it is already selected —
    /// cycle its value forward (the GUI equivalent of selecting + pressing →).
    fn click_settings_row(&mut self, i: usize) {
        let already = self.settings.as_ref().map(|p| p.idx) == Some(i);
        if already {
            let themes = self
                .settings
                .as_ref()
                .map(|p| p.themes.clone())
                .unwrap_or_default();
            self.adjust_setting(SettingRow::ALL[i], 1, &themes);
            self.persist_config();
        } else if let Some(p) = self.settings.as_mut() {
            p.idx = i;
        }
        self.request_redraw();
    }

    /// Apply a single +/- step to a settings row, mutating `self.config` (and
    /// live state like `self.theme`) in place. The renderer reads `self.config`
    /// / `self.theme` each frame, so most changes are visible immediately;
    /// font size is baked into the GPU metrics at window creation, so it applies
    /// on the next launch (noted in the panel).
    fn adjust_setting(&mut self, row: SettingRow, delta: i32, themes: &[String]) {
        match row {
            SettingRow::FontSize => {
                let s = (self.config.font.size + delta as f32 * 0.5).clamp(6.0, 48.0);
                self.config.font.size = s;
            }
            SettingRow::Theme => {
                if !themes.is_empty() {
                    let cur = themes.iter().position(|t| *t == self.config.theme);
                    let len = themes.len() as i32;
                    let next = match cur {
                        Some(i) => (((i as i32) + delta).rem_euclid(len)) as usize,
                        None => 0,
                    };
                    self.config.theme = themes[next].clone();
                    // Live-apply the theme: the renderer reads self.theme.
                    if let Some(t) = load_theme(&self.config.theme) {
                        self.theme = t;
                    }
                }
            }
            SettingRow::CursorStyle => {
                self.config.cursor.style = match (self.config.cursor.style, delta) {
                    (CursorStyle::Block, d) if d > 0 => CursorStyle::Bar,
                    (CursorStyle::Bar, d) if d > 0 => CursorStyle::Underline,
                    (CursorStyle::Underline, d) if d > 0 => CursorStyle::Block,
                    (CursorStyle::Block, _) => CursorStyle::Underline,
                    (CursorStyle::Bar, _) => CursorStyle::Block,
                    (CursorStyle::Underline, _) => CursorStyle::Bar,
                };
            }
            SettingRow::CursorBlink => {
                self.config.cursor.blink = !self.config.cursor.blink;
            }
            SettingRow::Scrollback => {
                let step = 1000i64;
                let v = self.config.scrollback_lines as i64 + delta as i64 * step;
                self.config.scrollback_lines = v.clamp(0, 1_000_000) as usize;
            }
            SettingRow::Opacity => {
                let o = (self.config.opacity + delta as f32 * 0.05).clamp(0.1, 1.0);
                self.config.opacity = o;
            }
            SettingRow::StartupPanel => {
                self.config.startup_panel = !self.config.startup_panel;
            }
        }
    }

    /// Render text for the settings panel overlay (D3), or `None` when closed.
    fn settings_text(&self) -> Option<String> {
        let panel = self.settings.as_ref()?;
        let mut s = String::from("\u{2592} settings\n\n");
        for (i, row) in SettingRow::ALL.iter().enumerate() {
            let marker = if i == panel.idx { "\u{25b8} " } else { "  " };
            let value = match row {
                SettingRow::FontSize => {
                    format!("{:.1}  (applies next launch)", self.config.font.size)
                }
                SettingRow::Theme => self.config.theme.clone(),
                SettingRow::CursorStyle => match self.config.cursor.style {
                    CursorStyle::Block => "block".into(),
                    CursorStyle::Bar => "bar".into(),
                    CursorStyle::Underline => "underline".into(),
                },
                SettingRow::CursorBlink => bool_label(self.config.cursor.blink),
                SettingRow::Scrollback => self.config.scrollback_lines.to_string(),
                SettingRow::Opacity => format!("{:.2}", self.config.opacity),
                SettingRow::StartupPanel => bool_label(self.config.startup_panel),
            };
            s.push_str(&format!("{marker}{:<18}{}\n", row.label(), value));
        }
        s.push_str("\n  \u{2191}/\u{2193} select   \u{2190}/\u{2192} change   Esc close");
        Some(s)
    }

    fn enter_search(&mut self) {
        self.search_mode = true;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_idx = 0;
    }

    /// Paste the OS clipboard into the focused pane's PTY (E3). Honors
    /// bracketed-paste mode (`?2004`): when the running program requested it,
    /// the text is wrapped in `ESC[200~ … ESC[201~` and any embedded end
    /// sentinel is stripped first, so a pasted payload cannot smuggle control
    /// of the bracket (the canonical paste-injection guard). Falls back to a
    /// raw paste when bracketed mode is off.
    fn paste_clipboard(&mut self) {
        let Some(text) = read_os_clipboard() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        // Paste-safety: a multi-line paste can execute shell commands the instant
        // it lands (the embedded newline). When enabled (default), defer it to a
        // confirm overlay instead of pasting immediately.
        if self.config.paste_warn_multiline && (text.contains('\n') || text.contains('\r')) {
            self.pending_paste = Some(text);
            self.request_redraw();
            return;
        }
        self.do_paste(text);
    }

    /// Confirm a deferred multi-line paste (Enter in the paste-safety overlay).
    fn confirm_paste(&mut self) {
        if let Some(text) = self.pending_paste.take() {
            self.do_paste(text);
            self.request_redraw();
        }
    }

    /// Discard a deferred paste (Esc in the paste-safety overlay).
    fn cancel_paste(&mut self) {
        if self.pending_paste.take().is_some() {
            self.request_redraw();
        }
    }

    /// Write `text` to the active PTY as a paste, honouring bracketed-paste mode.
    fn do_paste(&mut self, text: String) {
        // Read the bracketed-paste flag under a short lock, dropped before the
        // mutable session borrow below.
        // Frame the paste through the SHARED core guard (strip embedded
        // `ESC[201~`, bracket-wrap iff `?2004`) so this legacy winit path and the
        // egui path cannot drift apart — the drift that left the egui path raw
        // (paste-injection) is exactly what this consolidation closes.
        let bytes: Vec<u8> = self
            .active_session()
            .and_then(|s| s.terminal().lock().ok().map(|t| t.frame_paste(&text)))
            .unwrap_or_default();
        if bytes.is_empty() {
            return;
        }
        if let Some(s) = self.active_session_mut() {
            if let Ok(mut term) = s.terminal().lock() {
                term.scroll_to_bottom();
            }
            let _ = s.write_input(&bytes);
        }
    }

    fn exit_search(&mut self) {
        self.search_mode = false;
        self.search_query.clear();
        self.search_matches.clear();
        if let Some(s) = self.active_session() {
            if let Ok(mut t) = s.terminal().lock() {
                t.scroll_to_bottom();
            }
        }
    }

    fn recompute_search(&mut self) {
        let lines = self
            .active_session()
            .and_then(|s| s.terminal().lock().ok().map(|t| t.all_lines()));
        if let Some(lines) = lines {
            self.search_matches =
                c0pl4nd_core::search::find(&lines, &self.search_query, Default::default());
            self.search_idx = 0;
            self.jump_to_current_match();
        }
    }

    fn next_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_idx = (self.search_idx + 1) % self.search_matches.len();
        self.jump_to_current_match();
    }

    /// Scroll the active terminal so the current match line is visible.
    fn jump_to_current_match(&self) {
        if let Some(m) = self.search_matches.get(self.search_idx) {
            if let Some(s) = self.active_session() {
                if let Ok(mut t) = s.terminal().lock() {
                    let offset = t.scrollback_len().saturating_sub(m.line);
                    t.set_view_offset(offset);
                }
            }
        }
    }

    /// Scroll the active terminal to the previous (`forward = false`) or next
    /// (`forward = true`) shell-prompt mark (OSC 133 ; A), relative to the line
    /// currently at the top of the viewport. No-op when no marks exist or none
    /// lie in the requested direction. Marks are captured for free by the
    /// existing OSC-133 handler; this is the consumer (Ctrl+Shift+PageUp/Down).
    fn jump_to_prompt(&mut self, forward: bool) {
        let mut moved = false;
        if let Some(s) = self.active_session() {
            if let Ok(mut t) = s.terminal().lock() {
                let scrollback = t.scrollback_len();
                // Absolute line currently at the top of the visible window.
                let top = scrollback.saturating_sub(t.view_offset());
                let target = {
                    let marks = t.prompt_marks();
                    if forward {
                        marks.iter().copied().filter(|&m| m > top).min()
                    } else {
                        marks.iter().copied().filter(|&m| m < top).max()
                    }
                };
                if let Some(line) = target {
                    t.set_view_offset(scrollback.saturating_sub(line));
                    moved = true;
                }
            }
        }
        if moved {
            self.request_redraw();
        }
    }

    /// Map a window pixel to the `(leaf, display-row, display-col)` under it, or
    /// `None` over chrome / a pane border / above-or-left of the text origin.
    /// Drives mouse text selection. Row/col are clamped non-negative; callers
    /// bound them against the actual grid when extracting text.
    fn cell_at_pixel(&self, px: f64, py: f64) -> Option<(LeafId, usize, usize)> {
        let leaf = self.leaf_at(px, py)?;
        let cell = self.leaf_rect(leaf)?;
        let border = self
            .active_tab()
            .map(|t| {
                if t.layout.leaf_count() > 1 {
                    BORDER_PX
                } else {
                    0
                }
            })
            .unwrap_or(0);
        let (ox, oy) = leaf_text_origin(cell, border, self.config.window.padding as f32, 2.0);
        if (px as f32) < ox || (py as f32) < oy {
            return None;
        }
        let col = ((px as f32 - ox) / self.cell_w()).floor().max(0.0) as usize;
        let row = ((py as f32 - oy) / self.cell_h()).floor().max(0.0) as usize;
        Some((leaf, row, col))
    }

    /// Extract the current selection's text from its leaf, as lines joined by
    /// `\n` with trailing whitespace trimmed per line (the standard terminal
    /// copy shape). `None` when there is no selection, the leaf is not in the
    /// active tab, or the result is empty.
    fn selection_text(&self) -> Option<String> {
        let sel = self.selection?;
        let s = self.active_tab()?.cells.get(&sel.leaf)?.active()?;
        let rows = s.terminal().lock().ok()?.display_rows();
        let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let (start, end) = sel.ordered();
        let mut out = String::new();
        for r in start.0..=end.0 {
            let Some(row) = rows.get(r) else { break };
            let lo = if r == start.0 { start.1 } else { 0 };
            let hi = if r == end.0 {
                end.1.min(width.saturating_sub(1))
            } else {
                width.saturating_sub(1)
            };
            let mut line = String::new();
            for c in lo..=hi {
                if let Some(cell) = row.get(c) {
                    line.push(cell.c);
                }
            }
            out.push_str(line.trim_end());
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

    /// Copy the current selection to the OS clipboard (write-only; reuses the
    /// dependency-free `write_os_clipboard`). No-op when nothing is selected.
    fn copy_selection(&mut self) {
        if let Some(text) = self.selection_text() {
            write_os_clipboard(&text);
        }
    }

    fn handle_search_key(&mut self, key: &Key) {
        match key {
            Key::Named(NamedKey::Escape) => self.exit_search(),
            Key::Named(NamedKey::Enter) => self.next_match(),
            Key::Named(NamedKey::Backspace) => {
                self.search_query.pop();
                self.recompute_search();
            }
            Key::Character(s) => {
                self.search_query.push_str(s);
                self.recompute_search();
            }
            _ => {}
        }
        self.request_redraw();
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn active_session(&self) -> Option<&Session> {
        self.tabs.get(self.active).and_then(|t| t.focused_session())
    }

    fn active_session_mut(&mut self) -> Option<&mut Session> {
        self.tabs
            .get_mut(self.active)
            .and_then(|t| t.focused_session_mut())
    }

    /// The window's content area below the title bar, in physical pixels.
    /// Current grid cell width (base × font-zoom). `font_scale == 1.0` returns
    /// exactly `CELL_W`, so the un-zoomed layout is byte-identical to before.
    fn cell_w(&self) -> f32 {
        scaled_cell_w(self.font_scale)
    }
    /// Current grid cell height (base × font-zoom).
    fn cell_h(&self) -> f32 {
        scaled_cell_h(self.font_scale)
    }

    /// Set the live font-zoom factor (clamped to [`FONT_SCALE_MIN`,
    /// [`FONT_SCALE_MAX`]). Drops the grid text buffers so they rebuild at the
    /// new metrics, then recomputes the active tab's cols/rows and resizes its
    /// PTYs for the new cell size.
    fn set_font_scale(&mut self, scale: f32) {
        let scale = scale.clamp(FONT_SCALE_MIN, FONT_SCALE_MAX);
        if (scale - self.font_scale).abs() < f32::EPSILON {
            return;
        }
        self.font_scale = scale;
        if let Some(gpu) = &mut self.gpu {
            gpu.leaf_buffers.clear();
            gpu.tabbar_buffers.clear();
        }
        let content = self.content_rect();
        self.relayout_active(content);
        self.request_redraw();
    }

    /// Zoom the grid font in/out by `delta` (Ctrl +/− and Ctrl+wheel).
    fn zoom_font(&mut self, delta: f32) {
        self.set_font_scale(self.font_scale + delta);
    }

    /// Reset the grid font zoom to 1.0 (Ctrl+0).
    fn reset_font_scale(&mut self) {
        self.set_font_scale(1.0);
    }

    fn content_rect(&self) -> LRect {
        match &self.gpu {
            Some(g) => {
                let w = g.surface_config.width as i32;
                let h = (g.surface_config.height as i32 - TITLEBAR_H as i32).max(1);
                LRect::new(0, TITLEBAR_H as i32, w, h)
            }
            None => LRect::new(
                0,
                TITLEBAR_H as i32,
                (self.config.window.cols as f32 * CELL_W) as i32,
                (self.config.window.rows as f32 * LINE_HEIGHT) as i32,
            ),
        }
    }

    /// The leaf whose cell contains the physical-pixel point `(x, y)`, or `None`
    /// when the point is over chrome / outside any cell. A linear scan over the
    /// cached <=6 leaf rects (cheap by the MAX_PANES guardrail).
    fn leaf_at(&self, x: f64, y: f64) -> Option<LeafId> {
        let content = self.content_rect();
        let tab = self.active_tab()?;
        let (px, py) = (x as i32, y as i32);
        tab.layout
            .cascade(content)
            .into_iter()
            .find(|(_, r)| r.contains_point(px, py))
            .map(|(id, _)| id)
    }

    /// The cell rect for `leaf` in the active tab's current cascade, if present.
    fn leaf_rect(&self, leaf: LeafId) -> Option<LRect> {
        let content = self.content_rect();
        self.active_tab()?
            .layout
            .cascade(content)
            .into_iter()
            .find(|(id, _)| *id == leaf)
            .map(|(_, r)| r)
    }

    /// Forward a mouse event to the focused pane's PTY when the running program
    /// enabled mouse reporting (E6). Returns `true` when the event was consumed
    /// as a mouse report (so the caller skips the normal scroll/selection path).
    /// `(px, py)` is the physical-pixel cursor position. Cells are 1-based per
    /// the xterm protocol; the column/row are derived from the focused leaf's
    /// grid origin (matching the render placement: border + 8.0/2.0 pad).
    fn forward_mouse(
        &mut self,
        button: TermMouseButton,
        kind: MouseEventKind,
        px: f64,
        py: f64,
    ) -> bool {
        let focused = self.active_tab().map(|t| t.layout.focused);
        let Some(leaf) = focused else { return false };
        let Some(cell) = self.leaf_rect(leaf) else {
            return false;
        };
        let border = self
            .active_tab()
            .map(|t| {
                if t.layout.leaf_count() > 1 {
                    BORDER_PX
                } else {
                    0
                }
            })
            .unwrap_or(0);
        let (ox, oy) = leaf_text_origin(cell, border, self.config.window.padding as f32, 2.0);
        // 1-based cell coordinates, clamped to the cell's grid extent.
        let col = (((px as f32 - ox) / self.cell_w()).floor() as i64).max(0) as usize + 1;
        let row = (((py as f32 - oy) / self.cell_h()).floor() as i64).max(0) as usize + 1;
        let mods = MouseModifiers {
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            control: self.modifiers.control_key(),
        };
        let Some(s) = self.active_session_mut() else {
            return false;
        };
        let term_arc = s.terminal();
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
                let _ = s.write_input(&b);
                true
            }
            None => false,
        }
    }

    /// The URL under the physical-pixel point `(px, py)`, if any (E9). Maps the
    /// point to the focused leaf's grid cell (same origin math as
    /// `forward_mouse`), reads that display row, and scans for an http(s)/file
    /// URL covering the column. Returns `None` over chrome / empty space / when
    /// the row holds no URL at that column.
    fn url_at(&self, px: f64, py: f64) -> Option<String> {
        let leaf = self.active_tab().map(|t| t.layout.focused)?;
        let cell = self.leaf_rect(leaf)?;
        let border = self
            .active_tab()
            .map(|t| {
                if t.layout.leaf_count() > 1 {
                    BORDER_PX
                } else {
                    0
                }
            })
            .unwrap_or(0);
        let (ox, oy) = leaf_text_origin(cell, border, self.config.window.padding as f32, 2.0);
        if (px as f32) < ox || (py as f32) < oy {
            return None;
        }
        let col = ((px as f32 - ox) / self.cell_w()).floor() as usize;
        let row = ((py as f32 - oy) / self.cell_h()).floor() as usize;
        let sess = self.active_tab()?.cells.get(&leaf)?.active()?;
        let term_arc = sess.terminal();
        let rows = term_arc.lock().ok()?.display_rows();
        let line = rows.get(row)?;
        let chars: Vec<char> = line.iter().map(|c| c.c).collect();
        find_url_in_line(&chars, col)
    }

    /// Cursor quad(s) for the focused pane's terminal cursor (E5/E7). Honors
    /// DECTCEM visibility (`?25`), DECSCUSR shape (block/bar/underline), and
    /// blink (530ms half-period). Returns empty when the cursor is hidden,
    /// blinked-off, or scrolled into scrollback. Block is drawn semi-transparent
    /// so the glyph beneath stays readable (the quad layer is behind the text).
    /// Must be called before the `&mut self.gpu` borrow (it locks the terminal).
    fn cursor_quads(
        &self,
        focused: LeafId,
        cells: &[(LeafId, LRect)],
        border: i32,
        laid_out_strip: &std::collections::HashSet<LeafId>,
        accent: GColor,
    ) -> Vec<ColorRect> {
        let Some(cell) = cells.iter().find(|(id, _)| *id == focused).map(|(_, r)| *r) else {
            return Vec::new();
        };
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        let Some(pane) = tab.cells.get(&focused).and_then(Cell::active) else {
            return Vec::new();
        };
        let term_arc = pane.terminal();
        let (row, col, shape, blink) = {
            let Ok(t) = term_arc.lock() else {
                return Vec::new();
            };
            if !t.is_cursor_visible() {
                return Vec::new();
            }
            match t.cursor_position() {
                Some((r, c)) => (r, c, t.cursor_shape(), t.cursor_blink()),
                None => return Vec::new(),
            }
        };
        // Blink: when enabled, suppress the cursor during the off half-period.
        if blink {
            let on = (self.blink_start.elapsed().as_millis() / 530).is_multiple_of(2);
            if !on {
                return Vec::new();
            }
        }
        let strip_top = if laid_out_strip.contains(&focused) {
            CELL_TABBAR_H
        } else {
            0.0
        };
        let (ox, oy) = leaf_text_origin(
            cell,
            border,
            self.config.window.padding as f32,
            2.0 + strip_top,
        );
        let x = (ox + col as f32 * self.cell_w()) as i32;
        let y = (oy + row as f32 * self.cell_h()) as i32;
        let cw = self.cell_w().ceil() as i32;
        let ch = self.cell_h().ceil() as i32;
        let a = accent;
        let (ar, ag, ab) = (
            a.r() as f32 / 255.0,
            a.g() as f32 / 255.0,
            a.b() as f32 / 255.0,
        );
        let rect = match shape {
            // Block: full cell, semi-transparent so the glyph shows through.
            c0pl4nd_core::term::CursorShape::Block => {
                ColorRect::new(x, y, cw, ch, [ar, ag, ab, 0.55])
            }
            // Bar: 2px vertical at the cell's left edge (opaque).
            c0pl4nd_core::term::CursorShape::Bar => ColorRect::new(x, y, 2, ch, [ar, ag, ab, 0.95]),
            // Underline: 2px horizontal at the cell's bottom (opaque).
            c0pl4nd_core::term::CursorShape::Underline => {
                ColorRect::new(x, (y + ch - 2).max(y), cw, 2, [ar, ag, ab, 0.95])
            }
        };
        vec![rect]
    }

    /// The drop zone under the cursor for the pane being dragged, or `None` when
    /// the cursor is not over a *different* pane than the source. Drives both the
    /// overlay highlight and the drop resolution.
    fn drag_target(&self) -> Option<(LeafId, DropZone)> {
        let source = self.drag.dragging_leaf()?;
        let (cx, cy) = self.drag.cursor()?;
        let target = self.leaf_at(cx, cy)?;
        if target == source {
            return None;
        }
        let rect = self.leaf_rect(target)?;
        Some((target, classify_zone(rect, cx as i32, cy as i32)))
    }

    /// Resolve a completed drag of `source` onto the pane/zone under the cursor.
    /// Edge zones move the source beside the target (a tree split); the center
    /// merges the source's nested tabs into the target's TabGroup. A drop onto
    /// the source itself, or onto no pane, is a no-op.
    fn resolve_drop(&mut self, source: LeafId) {
        let Some((target, zone)) = self.drag_target() else {
            return;
        };
        let content = self.content_rect();
        match zone.edge_split() {
            Some((axis, before)) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.layout.move_leaf(source, target, axis, before);
                }
                self.relayout_active(content);
            }
            None => self.merge_into(source, target),
        }
    }

    /// Build the drag overlay quads (in physical-pixel surface coords): a dim
    /// veil over the dragged source pane, a highlight over the pending drop
    /// zone's sub-rect, and a small ghost following the cursor. Empty when not
    /// dragging. The ghost lerp / glide is skipped under reduced-motion (the
    /// quads are static either way — only animated movement is suppressed).
    fn drag_overlay_quads(&self, accent: GColor) -> Vec<ColorRect> {
        let mut out = Vec::new();
        let Some(source) = self.drag.dragging_leaf() else {
            return out;
        };
        // Dim the source pane so it reads as "lifted".
        if let Some(src) = self.leaf_rect(source) {
            out.push(ColorRect::new(
                src.x,
                src.y,
                src.w,
                src.h,
                [0.0, 0.0, 0.0, 0.35],
            ));
        }
        let acc = [
            accent.r() as f32 / 255.0,
            accent.g() as f32 / 255.0,
            accent.b() as f32 / 255.0,
            0.35,
        ];
        // Highlight the pending drop zone's sub-rect on the target pane.
        if let Some((target, zone)) = self.drag_target() {
            if let Some(tr) = self.leaf_rect(target) {
                out.push(zone_highlight_rect(tr, zone, acc));
            }
        }
        // A ghost square at the cursor. With motion enabled, a larger, fainter
        // halo conveys movement; under reduced-motion, only the crisp square is
        // drawn (no motion-suggesting trail — accessibility axis).
        if let Some((cx, cy)) = self.drag.cursor() {
            let (cx, cy) = (cx as i32, cy as i32);
            if !self.reduced_motion {
                let halo = 40;
                out.push(ColorRect::new(
                    cx - halo / 2,
                    cy - halo / 2,
                    halo,
                    halo,
                    [acc[0], acc[1], acc[2], 0.18],
                ));
            }
            let g = 26;
            out.push(ColorRect::new(
                cx - g / 2,
                cy - g / 2,
                g,
                g,
                [acc[0], acc[1], acc[2], 0.6],
            ));
        }
        out
    }

    /// Center-zone drop: append every session/tab of `source`'s cell to
    /// `target`'s cell (as nested tabs), then remove the now-empty source leaf
    /// from the tree. Focus follows to the target. Core owns the structure;
    /// the app owns the sessions, so the merge lives here.
    fn merge_into(&mut self, source: LeafId, target: LeafId) {
        let content = self.content_rect();
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if source == target {
            return;
        }
        // Detach the source cell (its sessions) and fold them into target.
        let Some(src_cell) = tab.cells.remove(&source) else {
            return;
        };
        if let Some(dst) = tab.cells.get_mut(&target) {
            for s in src_cell.sessions {
                dst.add(s);
            }
        } else {
            // Target cell missing (shouldn't happen) — put the source back to
            // avoid losing live shells.
            tab.cells.insert(source, src_cell);
            return;
        }
        let _ = tab.layout.remove(source);
        tab.layout.focused = target;
        self.relayout_active(content);
    }

    /// Split the focused leaf along `axis` (Horizontal = side-by-side,
    /// Vertical = stacked), spawning a fresh shell into the new leaf. Rejected
    /// silently at `MAX_PANES` (the readability guardrail).
    fn split_active(&mut self, axis: Axis) {
        let content = self.content_rect();
        let target = match self.tabs.get(self.active) {
            Some(t) => t.layout.focused,
            None => return,
        };
        // Reserve the new leaf via the guarded action layer first so a spawn
        // failure cannot leave a leaf without a session.
        let new_leaf = match self.tabs.get_mut(self.active) {
            Some(t) => match t.layout.try_split(target, axis) {
                SplitOutcome::Split(id) => id,
                _ => return, // AtCapacity / NotFound — no-op.
            },
            None => return,
        };
        match Session::spawn_shell_with_term(
            self.config.shell.as_deref(),
            24,
            80,
            Some(self.config.term.as_str()),
        ) {
            Ok(s) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.cells.insert(new_leaf, Cell::single(new_leaf, s));
                }
                self.relayout_active(content);
            }
            Err(e) => {
                // Roll the split back so the tree never references a leaf with
                // no session.
                tracing::error!("failed to spawn split pane: {e}");
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    let _ = tab.layout.remove(new_leaf);
                }
            }
        }
    }

    /// Spawn a fresh nested tab inside the FOCUSED cell and make it active
    /// (`Ctrl+Shift+T`). Distinct from `spawn_tab`, which creates a window-level
    /// tab. The new shell is sized to the focused cell's current grid.
    fn spawn_cell_tab(&mut self) {
        let content = self.content_rect();
        let (rows, cols) = self.focused_cell_dims(content);
        match Session::spawn_shell_with_term(
            self.config.shell.as_deref(),
            rows,
            cols,
            Some(self.config.term.as_str()),
        ) {
            Ok(s) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    let f = tab.layout.focused;
                    if let Some(cell) = tab.cells.get_mut(&f) {
                        cell.add(s);
                    }
                }
                self.relayout_active(content);
            }
            Err(e) => tracing::error!("failed to spawn cell tab: {e}"),
        }
    }

    /// Switch to the next nested tab in the focused cell (`Ctrl+PageDown`).
    /// Acts ONLY on the focused cell (distinct from window-level `Ctrl+Tab` and
    /// from `Alt+Arrow` cell focus — resolves pre-mortem #4). Sizes the
    /// now-visible (previously background) tab lazily on activation.
    fn next_cell_tab(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let f = tab.layout.focused;
            if let Some(cell) = tab.cells.get_mut(&f) {
                cell.group.next_tab();
            }
        }
        self.relayout_active(self.content_rect());
    }

    /// Switch to the previous nested tab in the focused cell (`Ctrl+PageUp`).
    fn prev_cell_tab(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let f = tab.layout.focused;
            if let Some(cell) = tab.cells.get_mut(&f) {
                cell.group.prev_tab();
            }
        }
        self.relayout_active(self.content_rect());
    }

    /// The (rows, cols) of the focused cell's grid for the current cascade,
    /// accounting for the border and (when present) the nested-tab strip.
    fn focused_cell_dims(&self, content: LRect) -> (u16, u16) {
        let Some(tab) = self.active_tab() else {
            return (24, 80);
        };
        let f = tab.layout.focused;
        for (leaf, rect) in tab.layout.cascade(content) {
            if leaf != f {
                continue;
            }
            let iw = (rect.w - 2 * BORDER_PX).max(self.cell_w() as i32);
            let ih = (rect.h - 2 * BORDER_PX).max(self.cell_h() as i32);
            // A 2nd tab will make the strip appear once tab_count >= 2; reserve
            // its line up-front so the new shell starts at the right height.
            let strip = if (rect.h - 2 * BORDER_PX) >= CELL_STRIP_MIN_H {
                CELL_TABBAR_H as i32
            } else {
                0
            };
            let cols = (iw as f32 / self.cell_w()).floor().max(1.0) as u16;
            let rows = ((ih - strip) as f32 / self.cell_h()).floor().max(1.0) as u16;
            return (rows, cols);
        }
        (24, 80)
    }

    /// Cycle focus to the next leaf in DFS order (preserves the legacy
    /// `Ctrl+Shift+O` "focus next pane" behaviour on the tree model).
    fn focus_next_pane(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let leaves = tab.layout.leaves();
            if leaves.len() > 1 {
                let cur = leaves
                    .iter()
                    .position(|&id| id == tab.layout.focused)
                    .unwrap_or(0);
                tab.layout.focused = leaves[(cur + 1) % leaves.len()];
            }
        }
    }

    /// Move focus directionally (Phase 3 wires the chords; the tree walk is
    /// available now for the action layer and parity tests).
    fn focus_dir(&mut self, dir: Direction) {
        let content = self.content_rect();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.layout.focus_dir(dir, content);
        }
    }

    /// Toggle pane-zoom on the focused leaf. Zoom is a pure render override in
    /// core (`cascade` returns one full rect), so the renderer needs no special
    /// case; relayout resizes the zoomed PTY to the window and leaves hidden
    /// siblings at their last size.
    fn toggle_zoom(&mut self) {
        let content = self.content_rect();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.layout.toggle_zoom();
        }
        self.relayout_active(content);
    }

    /// Swap the focused pane with its neighbour in `dir` (keyboard fallback for
    /// drag-rearrange; the keyboard layer ships before mouse drag).
    fn swap_dir(&mut self, dir: Direction) {
        let content = self.content_rect();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.layout.swap_focused(dir, content);
        }
        self.relayout_active(content);
    }

    /// Grow (Right/Down) or shrink (Left/Up) the focused split by a fixed flex
    /// step. The core clamps so neither side falls below the minimum cell.
    fn resize_focused(&mut self, dir: Direction) {
        let content = self.content_rect();
        let (delta, axis_extent) = match dir {
            Direction::Right => (0.05_f32, content.w),
            Direction::Left => (-0.05_f32, content.w),
            Direction::Down => (0.05_f32, content.h),
            Direction::Up => (-0.05_f32, content.h),
        };
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let f = tab.layout.focused;
            tab.layout.resize(f, delta, axis_extent);
        }
        self.relayout_active(content);
    }

    /// Equalize every split ratio (the "balance panes" action).
    fn equalize(&mut self) {
        let content = self.content_rect();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.layout.equalize();
        }
        self.relayout_active(content);
    }

    /// Rebuild the layout as the squarest grid that holds the current leaves
    /// (the "auto-arrange" preset).
    fn auto_balance(&mut self) {
        let content = self.content_rect();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.layout.rebalance_squarest();
        }
        self.relayout_active(content);
    }

    // --- Phase 6: quick-layout presets + workspace save/restore -----------

    /// Apply a quick-layout `preset` to the active tab: build the preset tree,
    /// then materialise one fresh shell per leaf (re-homing the focused cell's
    /// current session into the first leaf so the user's active shell is kept).
    /// Spawn failures roll back the affected leaf so the tree never references
    /// a cell with no session.
    fn apply_preset(&mut self, preset: Preset) {
        let content = self.content_rect();
        let Some(active) = self.tabs.get_mut(self.active) else {
            return;
        };
        let layout = Layout::from_preset(preset);
        let leaf_ids = layout.leaves();

        // Re-home one existing session (the focused cell's visible tab) into the
        // first leaf so applying a preset never throws away the user's shell.
        let mut kept: Option<Session> =
            active
                .cells
                .remove(&active.layout.focused)
                .and_then(|mut c| {
                    if c.sessions.is_empty() {
                        None
                    } else {
                        Some(c.sessions.remove(c.group.active.min(c.sessions.len() - 1)))
                    }
                });

        let mut cells: HashMap<LeafId, Cell> = HashMap::new();
        let (rows, cols) = self.preset_cell_dims(content, &layout);
        for (i, &id) in leaf_ids.iter().enumerate() {
            if i == 0 {
                if let Some(s) = kept.take() {
                    cells.insert(id, Cell::single(id, s));
                    continue;
                }
            }
            match Session::spawn_shell_with_term(
                self.config.shell.as_deref(),
                rows,
                cols,
                Some(self.config.term.as_str()),
            ) {
                Ok(s) => {
                    cells.insert(id, Cell::single(id, s));
                }
                Err(e) => {
                    tracing::error!("preset {} pane spawn failed: {e}", preset.label());
                }
            }
        }

        // Drop any leaf whose session failed to spawn so the tree stays valid.
        let Some(active) = self.tabs.get_mut(self.active) else {
            return;
        };
        active.layout = layout;
        active.cells = cells;
        let missing: Vec<LeafId> = active
            .layout
            .leaves()
            .into_iter()
            .filter(|id| !active.cells.contains_key(id))
            .collect();
        for id in missing {
            let _ = active.layout.remove(id);
        }
        if !active.cells.contains_key(&active.layout.focused) {
            if let Some(first) = active.layout.leaves().first().copied() {
                active.layout.focused = first;
            }
        }
        self.relayout_active(content);
        self.request_redraw();
    }

    /// (rows, cols) for an average cell of `layout` over `content` — used to
    /// size freshly-spawned preset shells close to their final grid.
    fn preset_cell_dims(&self, content: LRect, layout: &Layout) -> (u16, u16) {
        let rects = layout.cascade(content);
        let (w, h) = rects
            .iter()
            .map(|(_, r)| (r.w, r.h))
            .min_by_key(|(w, h)| (*w as i64) * (*h as i64))
            .unwrap_or((content.w, content.h));
        let iw = (w - 2 * BORDER_PX).max(self.cell_w() as i32);
        let ih = (h - 2 * BORDER_PX).max(self.cell_h() as i32);
        let cols = (iw as f32 / self.cell_w()).floor().max(1.0) as u16;
        let rows = (ih as f32 / self.cell_h()).floor().max(1.0) as u16;
        (rows, cols)
    }

    /// Directory holding saved workspace layouts, next to the config file
    /// (`<config-dir>/workspaces/`). `None` when no per-user config dir exists.
    fn workspaces_dir() -> Option<std::path::PathBuf> {
        Config::default_path().and_then(|p| p.parent().map(|d| d.join("workspaces")))
    }

    /// Path of the named workspace file (`<workspaces>/<name>.layout.json`).
    /// `name` is sanitised to a safe file stem so a hostile name cannot escape
    /// the workspaces dir.
    fn workspace_path(name: &str) -> Option<std::path::PathBuf> {
        let safe = sanitize_workspace_name(name);
        Self::workspaces_dir().map(|d| d.join(format!("{safe}.layout.json")))
    }

    /// Capture one tab's layout as a persistence snapshot, recording each pane's
    /// tab count, active tab, shell profile, and current working directory
    /// (OSC-7) so a restored shell relaunches where the user left it.
    fn capture_tab_snapshot(&self, tab: &Tab) -> LayoutSnapshot {
        let profile = self.config.shell.clone();
        LayoutSnapshot::capture(&tab.layout, |id| match tab.cells.get(&id) {
            Some(c) => {
                let cwd = c.active().and_then(|s| {
                    s.terminal()
                        .lock()
                        .ok()
                        .and_then(|t| t.cwd().map(String::from))
                });
                layout_persist::leaf_view_for(&c.group, cwd, profile.clone())
            }
            None => LeafView::single(),
        })
    }

    /// Capture the active tab's layout (single-tab "Save Layout As…").
    fn capture_snapshot(&self) -> Option<LayoutSnapshot> {
        Some(self.capture_tab_snapshot(self.active_tab()?))
    }

    /// Capture ALL window tabs as a multi-tab workspace snapshot.
    fn capture_workspace(&self) -> layout_persist::WorkspaceSnapshot {
        let tabs = self
            .tabs
            .iter()
            .map(|t| self.capture_tab_snapshot(t))
            .collect();
        layout_persist::WorkspaceSnapshot::from_tabs(tabs, self.active)
    }

    /// Save the active tab's layout to the named workspace file. `"default"` is
    /// the workspace restored on the next launch.
    fn save_workspace(&self, name: &str) {
        let Some(snap) = self.capture_snapshot() else {
            return;
        };
        let Some(path) = Self::workspace_path(name) else {
            tracing::warn!("no config dir; cannot save workspace");
            return;
        };
        // Privacy (F2): a user-chosen workspace name can carry sensitive text
        // (e.g. a client/project name), and `path.display()` embeds the config
        // dir (which contains the username). Log neither the name nor the path —
        // success drops to `debug` (off by default); the error path keeps
        // error-level but logs only the error, not the name/path.
        match snap.save(&path) {
            Ok(()) => tracing::debug!("saved workspace ({} bytes path)", path.as_os_str().len()),
            Err(e) => tracing::error!("failed to save workspace: {e}"),
        }
    }

    /// Replace the active tab with the layout saved under `name`, spawning fresh
    /// shells per leaf. A missing/corrupt file degrades to a single pane (the
    /// `layout_persist::load` fallback) — never a crash.
    fn restore_workspace(&mut self, name: &str) {
        let Some(path) = Self::workspace_path(name) else {
            return;
        };
        if !path.exists() {
            // Privacy (F2): omit the user-chosen name and the username-bearing
            // path; a missing workspace is benign and needs no identifying text.
            tracing::debug!("requested workspace not found");
            return;
        }
        let restored = layout_persist::load(&path);
        self.materialize_restored(restored);
    }

    /// Build a live [`Tab`] from a restored layout: one fresh shell per leaf at
    /// its saved cwd, sized to `content`, dropping any leaf whose shell could
    /// not spawn. Returns `None` when nothing could be spawned. Live process
    /// state is not restored — by design.
    fn build_tab(&self, restored: layout_persist::RestoredLayout, content: LRect) -> Option<Tab> {
        let (rows, cols) = self.preset_cell_dims(content, &restored.layout);
        let mut cells: HashMap<LeafId, Cell> = HashMap::new();
        for (id, view) in &restored.leaves {
            match Session::spawn_shell_in_with_term(
                self.config.shell.as_deref(),
                rows,
                cols,
                view.cwd.as_deref(),
                Some(self.config.term.as_str()),
            ) {
                Ok(s) => {
                    let mut cell = Cell::single(*id, s);
                    cell.group.active = view.active.min(cell.group.len() - 1);
                    cells.insert(*id, cell);
                }
                Err(e) => tracing::error!("restore: pane spawn failed: {e}"),
            }
        }
        if cells.is_empty() {
            return None;
        }
        let mut layout = restored.layout;
        let missing: Vec<LeafId> = layout
            .leaves()
            .into_iter()
            .filter(|id| !cells.contains_key(id))
            .collect();
        for id in missing {
            let _ = layout.remove(id);
        }
        if !cells.contains_key(&layout.focused) {
            if let Some(first) = layout.leaves().first().copied() {
                layout.focused = first;
            }
        }
        Some(Tab { layout, cells })
    }

    /// Turn a [`layout_persist::RestoredLayout`] into the live active tab.
    fn materialize_restored(&mut self, restored: layout_persist::RestoredLayout) {
        let content = self.content_rect();
        let Some(tab) = self.build_tab(restored, content) else {
            return;
        };
        if let Some(active) = self.tabs.get_mut(self.active) {
            *active = tab;
        } else {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
        self.relayout_active(content);
        self.request_redraw();
    }

    /// Rebuild ALL window tabs from a restored multi-tab workspace (fresh shells
    /// per pane at their saved cwds). Replaces the current tab set; restores the
    /// previously-active tab index. Tabs that spawn nothing are skipped.
    fn materialize_workspace(&mut self, restored: layout_persist::RestoredWorkspace) {
        let content = self.content_rect();
        let active = restored.active;
        let tabs: Vec<Tab> = restored
            .tabs
            .into_iter()
            .filter_map(|rl| self.build_tab(rl, content))
            .collect();
        if tabs.is_empty() {
            return;
        }
        self.active = active.min(tabs.len() - 1);
        self.tabs = tabs;
        self.relayout_active(content);
        self.request_redraw();
    }

    /// On launch, restore the saved `default` workspace if present (fresh
    /// shells); otherwise leave the single pane `spawn_tab` created.
    ///
    /// Corruption guard (F4-2): the restore distinguishes the three states the
    /// user must perceive differently:
    ///
    /// * **Absent** — no saved file. Normal first-launch / never-saved case:
    ///   start fresh **silently** (the `spawn_tab` default is already correct).
    /// * **Corrupt** — a file exists but is unparseable / semantically invalid /
    ///   unreadable. Degrade gracefully to the fresh default tab, **surface a
    ///   "session restore failed — started fresh" notice**, AND quarantine the
    ///   corrupt file aside (`<file>.corrupt-<hash8>`) so it is neither silently
    ///   re-loaded on the next launch nor destroyed (preserved for debugging).
    /// * **Restored** — a valid file; re-materialise it when it carries a real
    ///   multi-pane / multi-tab / saved-cwd layout (a lone single-pane tab is
    ///   identical to the fresh tab we already have).
    fn restore_default_workspace_on_startup(&mut self) {
        let Some(path) = Self::workspace_path("default") else {
            return;
        };
        match layout_persist::WorkspaceSnapshot::load_outcome(&path) {
            // Normal: nothing saved yet — start fresh, no notice.
            layout_persist::RestoreOutcome::Absent => {}
            // Corrupt: degrade gracefully, tell the user, quarantine the file.
            layout_persist::RestoreOutcome::Corrupt { reason } => {
                self.handle_corrupt_workspace(&path, &reason);
            }
            layout_persist::RestoreOutcome::Restored(restored) => {
                // Restore when there's something to restore: more than one
                // window tab, any multi-pane tab, or any saved working
                // directory. A lone single-pane tab with no cwd is identical to
                // the fresh tab we already have.
                let interesting = restored.tabs.len() > 1
                    || restored.tabs.iter().any(|t| {
                        t.layout.leaf_count() > 1 || t.leaves.iter().any(|(_, v)| v.cwd.is_some())
                    });
                if interesting {
                    self.materialize_workspace(restored);
                }
            }
        }
    }

    /// Degrade a corrupt-restore gracefully: keep the fresh default session,
    /// surface a non-fatal "session restore failed — started fresh" notice on
    /// the startup overlay, and quarantine the corrupt file aside so it is not
    /// silently re-loaded next launch (but is preserved for debugging — never
    /// deleted). Best-effort: a quarantine IO failure is logged, never fatal.
    fn handle_corrupt_workspace(&mut self, path: &std::path::Path, reason: &str) {
        // Privacy (F2): the reason can embed the corrupt file's parse position
        // but never the user-chosen workspace name or the username-bearing path,
        // so it is safe to log at WARN for diagnosis.
        tracing::warn!("session restore failed ({reason}); starting fresh");
        match layout_persist::quarantine_corrupt(path) {
            Ok(dest) => tracing::info!(
                "corrupt workspace quarantined ({} bytes path)",
                dest.as_os_str().len()
            ),
            // Quarantine is best-effort — the app has already fallen back to a
            // fresh session, so a rename failure is non-fatal.
            Err(e) => tracing::warn!("could not quarantine corrupt workspace: {e}"),
        }
        self.arm_restore_failed_notice();
    }

    /// Arm the user-facing "session restore failed" notice, reusing the existing
    /// startup-splash overlay (dismissed on the first keypress — the same
    /// transient-notice surface as the neofetch panel). Prepended above any
    /// existing splash so the notice is the first thing the user reads; if no
    /// splash is armed (e.g. `startup_panel` disabled), the notice stands alone.
    fn arm_restore_failed_notice(&mut self) {
        const NOTICE: &str = "  ⚠  Couldn't restore your saved layout — started fresh\n  \
             (your previous layout was kept in case it can be recovered)\n";
        match self.splash.take() {
            Some(existing) => self.splash = Some(format!("{NOTICE}\n{existing}")),
            None => self.splash = Some(NOTICE.to_string()),
        }
        self.request_redraw();
    }

    /// Discover the saved workspace names (file stems under the workspaces dir).
    fn discover_workspaces() -> Vec<String> {
        let Some(dir) = Self::workspaces_dir() else {
            return Vec::new();
        };
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                // Files are `<name>.layout.json`; strip the doubled extension.
                if let Some(file) = p.file_name().and_then(|s| s.to_str()) {
                    if let Some(stem) = file.strip_suffix(".layout.json") {
                        names.push(stem.to_string());
                    }
                }
            }
        }
        names.sort();
        names
    }

    /// Open the "Save Layout As…" prompt (a name text-input overlay).
    fn open_save_prompt(&mut self) {
        self.workspace_prompt = Some(WorkspacePrompt::Save {
            name: String::new(),
        });
        self.request_redraw();
    }

    /// Open the "Restore Layout" prompt listing saved workspaces. No saved
    /// workspaces → a no-op (logged), so the prompt never opens empty.
    fn open_restore_prompt(&mut self) {
        let names = Self::discover_workspaces();
        if names.is_empty() {
            tracing::info!("no saved workspaces to restore");
            return;
        }
        self.workspace_prompt = Some(WorkspacePrompt::Restore { names, idx: 0 });
        self.request_redraw();
    }

    /// Handle a key while a workspace prompt is open. Mirrors the palette's
    /// Escape/Enter/arrow/text-edit idiom.
    fn handle_workspace_prompt_key(&mut self, key: &Key) {
        match self.workspace_prompt.as_mut() {
            Some(WorkspacePrompt::Save { name }) => match key {
                Key::Named(NamedKey::Escape) => self.workspace_prompt = None,
                Key::Named(NamedKey::Enter) => {
                    let n = if name.trim().is_empty() {
                        "default".to_string()
                    } else {
                        name.trim().to_string()
                    };
                    self.workspace_prompt = None;
                    self.save_workspace(&n);
                }
                Key::Named(NamedKey::Backspace) => {
                    name.pop();
                }
                Key::Character(s) => name.push_str(s),
                _ => {}
            },
            Some(WorkspacePrompt::Restore { names, idx }) => match key {
                Key::Named(NamedKey::Escape) => self.workspace_prompt = None,
                Key::Named(NamedKey::ArrowDown) => {
                    *idx = (*idx + 1) % names.len().max(1);
                }
                Key::Named(NamedKey::ArrowUp) => {
                    *idx = (*idx + names.len().saturating_sub(1)) % names.len().max(1);
                }
                Key::Named(NamedKey::Enter) => {
                    let pick = names.get(*idx).cloned();
                    self.workspace_prompt = None;
                    if let Some(name) = pick {
                        self.restore_workspace(&name);
                    }
                }
                _ => {}
            },
            None => {}
        }
        self.request_redraw();
    }

    /// Drain each live session's terminal-produced side outputs and act on them
    /// (E10/E11 app wiring): OSC query replies go back to that session's PTY;
    /// OSC 52 clipboard writes go to the OS clipboard; OSC 4/10/11/12 color sets
    /// update the live theme; OSC 9/777 notifications are logged. Runs every
    /// poll tick. Each terminal is locked only briefly to collect the queued
    /// items, then the lock is dropped before any PTY write / shell-out.
    fn pump_terminal_io(&mut self) {
        // Collect (session-address, pty_response) plus aggregated clipboard /
        // color / notification effects across all live sessions. Session
        // address is (tab_index, leaf, tab_slot) so we can write the PTY reply
        // back to the exact originating session afterwards.
        let mut responses: Vec<(usize, LeafId, usize, Vec<u8>)> = Vec::new();
        let mut clipboard: Vec<String> = Vec::new();
        let mut colors: Vec<ColorSet> = Vec::new();
        let mut redraw = false;
        let mut notified = false;
        for (ti, tab) in self.tabs.iter().enumerate() {
            for (leaf, cell) in tab.cells.iter() {
                for (slot, sess) in cell.sessions.iter().enumerate() {
                    let term_arc = sess.terminal();
                    let Ok(mut term) = term_arc.lock() else {
                        continue;
                    };
                    let resp = term.take_pty_response();
                    if !resp.is_empty() {
                        responses.push((ti, *leaf, slot, resp));
                    }
                    for mut cw in term.take_clipboard_writes() {
                        // `ClipboardWrite` zeroizes its buffer on drop; take the
                        // text out (leaving an empty buffer to drop) rather than
                        // moving the field out of the Drop type.
                        clipboard.push(std::mem::take(&mut cw.text));
                    }
                    let cs = term.take_color_sets();
                    if !cs.is_empty() {
                        colors.extend(cs);
                        redraw = true;
                    }
                    while let Some(n) = term.take_notification() {
                        // Privacy (F2): OSC 9/777 notification title/body is
                        // program-emitted content that can carry 2FA codes,
                        // message bodies, or secret URLs. Never log the text —
                        // and log only at `debug` (off by default) so the mere
                        // occurrence does not reach default-on stderr.
                        tracing::debug!("desktop notification received ({} bytes)", n.body.len());
                        notified = true;
                    }
                }
            }
        }
        // OSC 9/777 desktop notification while the window is unfocused → flash
        // the taskbar to draw attention (it stops on next focus). No-op when
        // focused or off Windows.
        if notified && !self.focused {
            self.flash_taskbar();
        }
        // PTY query replies: write to the exact originating session.
        for (ti, leaf, slot, bytes) in responses {
            if let Some(sess) = self
                .tabs
                .get_mut(ti)
                .and_then(|t| t.cells.get_mut(&leaf))
                .and_then(|c| c.sessions.get_mut(slot))
            {
                let _ = sess.write_input(&bytes);
            }
        }
        // OSC 52 → OS clipboard (write only; reads stay default-off in core).
        for text in clipboard {
            write_os_clipboard(&text);
        }
        // OSC 4/10/11/12 → live theme. Dynamic colors map to the theme's
        // foreground/background/cursor; indexed sets update the ANSI rows.
        for set in colors {
            self.apply_color_set(set);
        }
        if redraw {
            self.request_redraw();
        }
    }

    /// Apply an OSC color-set to the live theme so the change is visible on the
    /// next frame (E11 wiring).
    fn apply_color_set(&mut self, set: ColorSet) {
        let hex = |(r, g, b): (u8, u8, u8)| format!("#{r:02x}{g:02x}{b:02x}");
        match set {
            ColorSet::Dynamic { which, rgb } => match which {
                DynamicColor::Foreground => self.theme.foreground = hex(rgb),
                DynamicColor::Background => self.theme.background = hex(rgb),
                DynamicColor::Cursor => self.theme.cursor = hex(rgb),
            },
            ColorSet::Indexed { index, rgb } => {
                let row = if index < 8 {
                    &mut self.theme.normal
                } else if index < 16 {
                    &mut self.theme.bright
                } else {
                    // 256-color cube entries aren't represented in the 16-slot
                    // theme; ignore rather than misplace them.
                    return;
                };
                let slot = match index % 8 {
                    0 => &mut row.black,
                    1 => &mut row.red,
                    2 => &mut row.green,
                    3 => &mut row.yellow,
                    4 => &mut row.blue,
                    5 => &mut row.magenta,
                    6 => &mut row.cyan,
                    _ => &mut row.white,
                };
                *slot = hex(rgb);
            }
        }
    }

    /// Resize every visible leaf's PTY to its cell's cols/rows for the current
    /// cascade over `content`.
    fn relayout_active(&mut self, content: LRect) {
        let (cw, ch) = (self.cell_w(), self.cell_h());
        if let Some(tab) = self.tabs.get_mut(self.active) {
            for (leaf, cell) in tab.layout.cascade(content) {
                let inner_h = (cell.h - 2 * BORDER_PX).max(ch as i32);
                // The cell tab strip (when the cell has >=2 tabs and is tall
                // enough) steals one line from the terminal grid; account for it
                // so the visible shell's rows match its drawable area.
                let strip = if tab
                    .cells
                    .get(&leaf)
                    .is_some_and(|c| cell_strip_visible(c, cell))
                {
                    CELL_TABBAR_H as i32
                } else {
                    0
                };
                let inner_w = (cell.w - 2 * BORDER_PX).max(cw as i32);
                let cols = (inner_w as f32 / cw).floor().max(1.0) as u16;
                let rows = ((inner_h - strip) as f32 / ch).floor().max(1.0) as u16;
                if let Some(s) = tab.cells.get_mut(&leaf).and_then(Cell::active_mut) {
                    let _ = s.resize(rows, cols);
                }
            }
        }
    }

    /// Resize every visible leaf's PTY across ALL tabs to the cascade over a
    /// `w`×`h` surface. Called from the debounced window-resize path so a
    /// tab switch shows correct dimensions immediately.
    fn apply_pty_resize(&mut self, w: u32, h: u32) {
        let content = LRect::new(
            0,
            TITLEBAR_H as i32,
            w as i32,
            (h as i32 - TITLEBAR_H as i32).max(1),
        );
        let (cw, ch) = (self.cell_w(), self.cell_h());
        for ti in 0..self.tabs.len() {
            let cascade: Vec<(LeafId, LRect)> = self.tabs[ti].layout.cascade(content);
            let edits: Vec<(LeafId, u16, u16)> = cascade
                .into_iter()
                .map(|(leaf, cell)| {
                    let iw = (cell.w - 2 * BORDER_PX).max(cw as i32);
                    let ih = (cell.h - 2 * BORDER_PX).max(ch as i32);
                    let strip = if self.tabs[ti]
                        .cells
                        .get(&leaf)
                        .is_some_and(|c| cell_strip_visible(c, cell))
                    {
                        CELL_TABBAR_H as i32
                    } else {
                        0
                    };
                    let cols = (iw as f32 / cw).floor().max(1.0) as u16;
                    let rows = ((ih - strip) as f32 / ch).floor().max(1.0) as u16;
                    (leaf, rows, cols)
                })
                .collect();
            for (leaf, rows, cols) in edits {
                if let Some(s) = self.tabs[ti]
                    .cells
                    .get_mut(&leaf)
                    .and_then(Cell::active_mut)
                {
                    let _ = s.resize(rows, cols);
                }
            }
        }
    }

    /// Collect inline-image draw quads for `leaf`, offset into `cell` and
    /// positioned in that leaf's current viewport (skips out-of-view images).
    fn collect_leaf_image_quads(
        &self,
        leaf: LeafId,
        cell: LRect,
    ) -> Vec<crate::image_render::ImageQuad> {
        let mut out = Vec::new();
        let Some(tab) = self.active_tab() else {
            return out;
        };
        let Some(c) = tab.cells.get(&leaf) else {
            return out;
        };
        let Some(s) = c.active() else {
            return out;
        };
        let strip = if cell_strip_visible(c, cell) {
            CELL_TABBAR_H
        } else {
            0.0
        };
        let (ox, oy) = leaf_text_origin(
            cell,
            BORDER_PX,
            self.config.window.padding as f32,
            2.0 + strip,
        );
        if let Ok(t) = s.terminal().lock() {
            let rows = t.grid().rows();
            let window_start = t.scrollback_len().saturating_sub(t.view_offset());
            for img in t.images() {
                if img.line < window_start {
                    continue;
                }
                let vrow = img.line - window_start;
                if vrow >= rows {
                    continue;
                }
                out.push(crate::image_render::ImageQuad {
                    rgba: img.image.rgba.clone(),
                    width: img.image.width as u32,
                    height: img.image.height as u32,
                    x: ox + img.col as f32 * self.cell_w(),
                    y: oy + vrow as f32 * self.cell_h(),
                });
            }
        }
        out
    }

    /// Reorder one terminal row's `(char, color)` cells from logical order into
    /// visual order using the Unicode Bidirectional Algorithm (UAX #9), so
    /// right-to-left scripts (Hebrew, Arabic) display correctly. Returns `None`
    /// for the overwhelmingly common no-RTL row (pure-ASCII rows skip the
    /// algorithm entirely), letting the caller keep the logical slice with zero
    /// allocation. Per-cell background quads are NOT reordered — they stay in
    /// logical column order, so an explicit per-cell background highlight on RTL
    /// text is the one case where bg and fg can diverge; the common RTL line
    /// (default background) is unaffected. The cursor/selection remain in
    /// logical order (a documented limitation shared by most grid terminals).
    fn bidi_visual_order(cells: &[(char, GColor)]) -> Option<Vec<(char, GColor)>> {
        // Fast path: an all-ASCII row can never contain RTL — skip UBA entirely.
        if cells.iter().all(|(c, _)| c.is_ascii()) {
            return None;
        }
        let line: String = cells.iter().map(|(c, _)| *c).collect();
        let info = unicode_bidi::ParagraphBidiInfo::new(&line, None);
        if !info.has_rtl() {
            return None;
        }
        // Map each char-start byte offset to its logical index so a run's byte
        // range can recover per-char colors.
        let mut byte_to_ci = vec![0usize; line.len() + 1];
        for (ci, (b, _)) in line.char_indices().enumerate() {
            byte_to_ci[b] = ci;
        }
        let (levels, runs) = info.visual_runs(0..line.len());
        let mut out: Vec<(char, GColor)> = Vec::with_capacity(cells.len());
        for run in runs {
            // `runs` arrive in visual order; reverse chars within an RTL run.
            let rtl = levels[run.start].is_rtl();
            let mut seg: Vec<(char, GColor)> = line[run.clone()]
                .char_indices()
                .map(|(local_b, ch)| (ch, cells[byte_to_ci[run.start + local_b]].1))
                .collect();
            if rtl {
                seg.reverse();
            }
            out.extend(seg);
        }
        Some(out)
    }

    /// Snapshot one leaf's grid into BOTH foreground spans (for glyphon) and
    /// per-cell background paints (for the solid-quad layer). This is the single
    /// place the grid is read+damage-cleared per frame, so foreground and
    /// background stay consistent. Honors SGR inverse/reverse video by swapping
    /// the effective fg/bg per cell. The bg list holds `(row, col, rgba)` for
    /// every cell whose effective background differs from the window default;
    /// the caller turns these into `ColorRect`s at the leaf's pixel origin.
    #[allow(clippy::type_complexity)]
    fn leaf_render(
        &self,
        leaf: LeafId,
        fg: GColor,
    ) -> Option<(
        Vec<(String, GColor)>,
        Vec<(usize, usize, [f32; 4])>,
        Vec<(
            usize,
            usize,
            c0pl4nd_core::grid::UnderlineStyle,
            bool,
            [f32; 4],
        )>,
    )> {
        let tab = self.active_tab()?;
        let s = tab.cells.get(&leaf).and_then(Cell::active)?;
        let theme_fg = parse_hex(&self.theme.foreground).unwrap_or((240, 238, 245));
        let theme_bg = parse_hex(&self.theme.background).unwrap_or((8, 6, 13));
        let (rows, reverse_screen) = {
            // Bind the Arc<Mutex<…>> to a local so the guard does not outlive a
            // temporary (the `if let Ok(t) = s.terminal().lock()` form in
            // `collect_leaf_image_quads` extends the temporary; this `let` form
            // would otherwise drop it at the `;`).
            let term = s.terminal();
            let mut t = term.lock().ok()?;
            let rows = t.display_rows();
            let reverse = t.reverse_screen();
            t.grid_mut().clear_damage();
            (rows, reverse)
        };
        // DECSCNM (`?5`): reverse-video screen swaps the default fg/bg so cells
        // using the default colours render inverted.
        let (default_fg, default_bg) = if reverse_screen {
            (theme_bg, theme_fg)
        } else {
            (theme_fg, theme_bg)
        };
        let mut spans: Vec<(String, GColor)> = Vec::new();
        let mut bg_cells: Vec<(usize, usize, [f32; 4])> = Vec::new();
        // Per-cell text decorations (C20/C24): styled underline + strikeout, in
        // logical cell columns (drawn as lines by the renderer, not by glyphon).
        let mut decos: Vec<(
            usize,
            usize,
            c0pl4nd_core::grid::UnderlineStyle,
            bool,
            [f32; 4],
        )> = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            // Logical-order (char, fg) for the row. Background paints are pushed
            // here in logical cell columns (bidi reorders text only — see
            // `bidi_visual_order`).
            let mut logical: Vec<(char, GColor)> = Vec::with_capacity(row.len());
            for (c, cell) in row.iter().enumerate() {
                let (fg_rgb, bg_rgb) =
                    cell_render_colors(cell, &self.theme, default_fg, default_bg);
                if let Some(bg) = bg_rgb {
                    bg_cells.push((
                        r,
                        c,
                        [
                            bg.0 as f32 / 255.0,
                            bg.1 as f32 / 255.0,
                            bg.2 as f32 / 255.0,
                            1.0,
                        ],
                    ));
                }
                // Capture underline / strikeout for this cell (drawn as lines).
                // The underline inherits the cell's foreground unless an explicit
                // underline colour (SGR 58) was set.
                if cell.flags.underline_style != c0pl4nd_core::grid::UnderlineStyle::None
                    || cell.flags.strikeout
                {
                    let uc = cell
                        .underline_color
                        .map(|c| resolve_fg(c, &self.theme, default_fg))
                        .unwrap_or(fg_rgb);
                    decos.push((
                        r,
                        c,
                        cell.flags.underline_style,
                        cell.flags.strikeout,
                        [
                            uc.0 as f32 / 255.0,
                            uc.1 as f32 / 255.0,
                            uc.2 as f32 / 255.0,
                            1.0,
                        ],
                    ));
                }
                logical.push((cell.c, GColor::rgb(fg_rgb.0, fg_rgb.1, fg_rgb.2)));
            }
            // BiDi: reorder to visual order for RTL rows (None ⇒ keep logical).
            let visual = Self::bidi_visual_order(&logical);
            let ordered: &[(char, GColor)] = visual.as_deref().unwrap_or(&logical);
            // Group consecutive same-color chars into color runs.
            let mut run = String::new();
            let mut run_color: Option<GColor> = None;
            for (ch, gcol) in ordered {
                if run_color != Some(*gcol) {
                    if let Some(pc) = run_color {
                        spans.push((std::mem::take(&mut run), pc));
                    }
                    run_color = Some(*gcol);
                }
                run.push(*ch);
            }
            if let Some(pc) = run_color {
                spans.push((run, pc));
            }
            spans.push(("\n".to_string(), fg));
        }
        Some((spans, bg_cells, decos))
    }

    fn request_redraw(&self) {
        if let Some(g) = &self.gpu {
            g.window.request_redraw();
        }
    }

    /// Current grid dimensions from the GPU surface (cols, rows).
    fn grid_dims(&self) -> (u16, u16) {
        match &self.gpu {
            Some(g) => {
                let cols = (g.surface_config.width as f32 / self.cell_w())
                    .floor()
                    .max(1.0) as u16;
                let usable = (g.surface_config.height as f32 - TITLEBAR_H).max(self.cell_h());
                let rows = (usable / self.cell_h()).floor().max(1.0) as u16;
                (cols, rows)
            }
            None => (self.config.window.cols, self.config.window.rows),
        }
    }

    fn spawn_tab(&mut self) {
        let (cols, rows) = self.grid_dims();
        let first = self.tabs.is_empty();
        match Session::spawn_shell_with_term(
            self.config.shell.as_deref(),
            rows,
            cols,
            Some(self.config.term.as_str()),
        ) {
            Ok(s) => {
                self.tabs.push(Tab::single(s));
                self.active = self.tabs.len() - 1;
                // On the very first tab, arm the neofetch-style startup splash.
                // It is drawn as an app overlay (NOT injected into the PTY grid,
                // which Windows ConPTY clears on shell start) and dismissed on
                // the first keypress.
                if first && self.config.startup_panel && self.splash.is_none() {
                    let gpu = self.gpu.as_ref().map(|g| g.gpu_name.as_str());
                    let info = c0pl4nd_core::fetch::SystemInfo::gather(gpu);
                    let mut panel = c0pl4nd_core::fetch::render_panel(&info);
                    // First-run discoverability hint — the cheapest way to teach
                    // the command palette + the core shortcuts (any keypress
                    // dismisses the splash).
                    panel.push_str(
                        "\n  Ctrl+Shift+P  commands   ·   Ctrl+Shift+T  new tab\n  \
                         Ctrl+Shift+D / E  split   ·   Ctrl+,  settings\n",
                    );
                    self.splash = Some(panel);
                }
            }
            Err(e) => tracing::error!("failed to spawn tab: {e}"),
        }
    }

    fn close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        let content = self.content_rect();
        // Layered close, narrowest scope first:
        //   1. focused cell has >=2 nested tabs → close the active cell tab;
        //   2. else the window tab is split into >1 pane → close the focused leaf;
        //   3. else >1 window tab → close the window tab;
        //   4. else → quit.
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let focused = tab.layout.focused;
            if let Some(cell) = tab.cells.get_mut(&focused) {
                if cell.tab_count() > 1 {
                    cell.close_active();
                    self.relayout_active(content);
                    return;
                }
            }
            if tab.layout.leaf_count() > 1 {
                let closed = tab.layout.focused;
                // remove() moves focus to a surviving leaf; drop the cell.
                let _ = tab.layout.remove(closed);
                tab.cells.remove(&closed);
                self.relayout_active(content);
                return;
            }
        }
        if self.tabs.len() <= 1 {
            event_loop.exit();
            return;
        }
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    /// E16: close panes (and window tabs) whose shell has exited. Walks every
    /// window tab and every cell; a cell tab whose session is dead is closed,
    /// an emptied leaf is collapsed out of the split tree, and an emptied
    /// window tab is dropped. Exits the event loop when the last tab
    /// disappears. Relayout + redraw fire only when something actually changed.
    fn reap_dead_panes(&mut self, event_loop: &ActiveEventLoop) {
        let content = self.content_rect();
        let mut changed = false;

        let mut tab_idx = 0;
        while tab_idx < self.tabs.len() {
            // Scoped borrow of the tab so the window-tab drop below can mutate
            // `self.tabs`. Returns true when this tab has no cells left.
            let tab_emptied = {
                let tab = &mut self.tabs[tab_idx];
                for leaf in tab.layout.leaves() {
                    // Reap every dead tab within this cell, narrowest first.
                    while let Some(cell) = tab.cells.get_mut(&leaf) {
                        let Some(dead_idx) = cell.sessions.iter().position(|s| !s.is_alive())
                        else {
                            break;
                        };
                        // `sessions` and `group.tabs` are parallel arrays, so a
                        // sessions index is a valid active index for the group.
                        cell.group.active = dead_idx;
                        let emptied = cell.close_active();
                        changed = true;
                        if emptied {
                            tab.cells.remove(&leaf);
                            // Collapse the now-empty leaf. The last leaf cannot
                            // be removed — that case drops the whole tab below.
                            if tab.layout.leaf_count() > 1 {
                                let _ = tab.layout.remove(leaf);
                            }
                            break;
                        }
                    }
                }
                // Keep focus on a surviving leaf.
                if !tab.cells.is_empty() && !tab.layout.contains(tab.layout.focused) {
                    if let Some(first) = tab.layout.leaves().first().copied() {
                        tab.layout.focused = first;
                    }
                }
                tab.cells.is_empty()
            };

            if tab_emptied {
                self.tabs.remove(tab_idx);
                // The next tab shifted into this slot; do not advance.
            } else {
                tab_idx += 1;
            }
        }

        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }

        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }

        if changed {
            self.relayout_active(content);
            self.request_redraw();
        }
    }

    fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    /// Jump to window tab `n` (1-based). `9` always selects the last tab (the
    /// standard browser/terminal convention); other out-of-range numbers are
    /// ignored. Drives the Ctrl+1..9 shortcut (E14).
    fn select_tab_by_number(&mut self, n: u8) {
        if let Some(idx) = resolve_tab_number(n, self.tabs.len()) {
            self.active = idx;
            self.request_redraw();
        }
    }

    fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    /// Switch directly to window tab `i` (the modern tab-strip click target).
    fn activate_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.active = i;
            self.request_redraw();
        }
    }

    /// The clickable tab-strip zone under the physical-pixel point, if any.
    /// Reads the per-frame `tab_zones` computed from the chrome glyph layout.
    fn hit_tab(&self, x: f64, y: f64) -> Option<TabZone> {
        if y > TITLEBAR_H as f64 {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        tab_zone_at(x as f32, &gpu.tab_zones)
    }

    /// Handle a Ctrl/Cmd+Shift tab-control combo. Returns true if consumed.
    fn handle_tab_combo(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        let handled = match key {
            Key::Character(s) => match s.chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('t') => {
                    self.spawn_tab();
                    true
                }
                Some('w') => {
                    self.close_active_tab(event_loop);
                    true
                }
                Some(']') => {
                    self.next_tab();
                    true
                }
                Some('[') => {
                    self.prev_tab();
                    true
                }
                Some('f') => {
                    self.enter_search();
                    true
                }
                Some('v') => {
                    self.paste_clipboard();
                    true
                }
                Some('c') => {
                    // Ctrl/Cmd+Shift+C copies the current mouse selection
                    // (plain Ctrl+C stays SIGINT to the PTY).
                    self.copy_selection();
                    true
                }
                Some('p') => {
                    self.enter_palette();
                    true
                }
                Some('d') => {
                    // Legacy d = side-by-side split → Horizontal axis.
                    self.split_active(Axis::Horizontal);
                    true
                }
                Some('e') => {
                    // Legacy e = stacked split → Vertical axis.
                    self.split_active(Axis::Vertical);
                    true
                }
                Some('o') => {
                    self.focus_next_pane();
                    true
                }
                Some('=') | Some('+') => {
                    self.equalize();
                    true
                }
                Some('\\') => {
                    self.auto_balance();
                    true
                }
                _ => false,
            },
            Key::Named(NamedKey::Tab) => {
                self.next_tab();
                true
            }
            // Jump to previous/next shell prompt (OSC 133 marks).
            Key::Named(NamedKey::PageUp) => {
                self.jump_to_prompt(false);
                true
            }
            Key::Named(NamedKey::PageDown) => {
                self.jump_to_prompt(true);
                true
            }
            // Directional focus across the split-tree (Ctrl/Cmd+Shift+Arrow).
            Key::Named(NamedKey::ArrowLeft) => {
                self.focus_dir(Direction::Left);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.focus_dir(Direction::Right);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.focus_dir(Direction::Up);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.focus_dir(Direction::Down);
                true
            }
            // Pane zoom (toggle the focused leaf full-window).
            Key::Named(NamedKey::Enter) => {
                self.toggle_zoom();
                true
            }
            _ => false,
        };
        if handled {
            self.request_redraw();
        }
        handled
    }

    /// Handle an Alt-modified combo: `Alt+Arrow` swaps the focused pane with its
    /// neighbour, `Alt+Shift+Arrow` resizes the focused split. Returns true if
    /// consumed (so the arrow is not also forwarded to the PTY).
    fn handle_alt_combo(&mut self, key: &Key) -> bool {
        let dir = match key {
            Key::Named(NamedKey::ArrowLeft) => Direction::Left,
            Key::Named(NamedKey::ArrowRight) => Direction::Right,
            Key::Named(NamedKey::ArrowUp) => Direction::Up,
            Key::Named(NamedKey::ArrowDown) => Direction::Down,
            _ => return false,
        };
        if self.modifiers.shift_key() {
            self.resize_focused(dir);
        } else {
            self.swap_dir(dir);
        }
        self.request_redraw();
        true
    }

    /// Handle a Ctrl-only (no Shift/Alt) nested-cell-tab combo. Returns true if
    /// consumed (so the key is not forwarded to the PTY). `Ctrl+T` is the cell's
    /// new-tab chord; the window's new-tab chord stays `Ctrl+Shift+T`.
    fn handle_cell_tab_combo(&mut self, key: &Key) -> bool {
        let handled = match key {
            Key::Named(NamedKey::PageDown) => {
                self.next_cell_tab();
                true
            }
            Key::Named(NamedKey::PageUp) => {
                self.prev_cell_tab();
                true
            }
            Key::Character(s) if s.chars().next().map(|c| c.to_ascii_lowercase()) == Some('t') => {
                self.spawn_cell_tab();
                true
            }
            // Live font zoom: Ctrl + / = zoom in, Ctrl - / _ zoom out, Ctrl 0 reset.
            Key::Character(s) if matches!(s.chars().next(), Some('=') | Some('+')) => {
                self.zoom_font(0.1);
                true
            }
            Key::Character(s) if matches!(s.chars().next(), Some('-') | Some('_')) => {
                self.zoom_font(-0.1);
                true
            }
            Key::Character(s) if s.starts_with('0') => {
                self.reset_font_scale();
                true
            }
            _ => false,
        };
        if handled {
            self.request_redraw();
        }
        handled
    }

    fn bg_color(&self) -> wgpu::Color {
        let (r, g, b) = parse_hex(&self.theme.background).unwrap_or((8, 6, 13));
        let (mut lr, mut lg, mut lb) = (srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
        // When the surface is non-opaque (translucent / acrylic window) the
        // clear alpha is the configured opacity so the desktop / DWM backdrop
        // shows through. A pre-multiplied surface needs the colour channels
        // pre-scaled by alpha. Opaque surfaces (the default) keep a = 1.0.
        let mode = self.gpu.as_ref().map(|g| g.surface_config.alpha_mode);
        let a = match mode {
            Some(wgpu::CompositeAlphaMode::PreMultiplied)
            | Some(wgpu::CompositeAlphaMode::PostMultiplied)
            | Some(wgpu::CompositeAlphaMode::Inherit) => self.config.opacity.clamp(0.0, 1.0) as f64,
            _ => 1.0,
        };
        if matches!(mode, Some(wgpu::CompositeAlphaMode::PreMultiplied)) {
            lr *= a;
            lg *= a;
            lb *= a;
        }
        wgpu::Color {
            r: lr,
            g: lg,
            b: lb,
            a,
        }
    }

    fn fg_color(&self) -> GColor {
        let (r, g, b) = parse_hex(&self.theme.foreground).unwrap_or((240, 238, 245));
        GColor::rgb(r, g, b)
    }

    fn accent_color(&self) -> GColor {
        let (r, g, b) = parse_hex(&self.theme.cursor).unwrap_or((0, 229, 255));
        GColor::rgb(r, g, b)
    }

    /// Classify a physical-pixel point against the title bar. The only impure
    /// input is the surface width; the classification is [`titlebar_hit_at`].
    fn hit_titlebar(&self, x: f64, y: f64) -> TitlebarHit {
        let width = self
            .gpu
            .as_ref()
            .map(|g| g.surface_config.width as f64)
            .unwrap_or(0.0);
        titlebar_hit_at(x, y, width)
    }

    /// X pixel where the title-bar button cluster begins, for a given width.
    /// Shared by `hit_titlebar` and the chrome renderer so the rendered glyphs
    /// and the click zones use identical geometry.
    fn buttons_left_px(width: f32) -> f32 {
        width - BUTTONS_CELLS * CELL_W - BTN_RIGHT_MARGIN
    }

    /// D4 (Windows): install the custom-frame Aero-Snap subclass on `window`.
    /// Extracts the HWND via winit's raw-window-handle re-export and hands
    /// `win_snap` the chrome geometry (title-bar height, resize-border band, and
    /// caption-button cluster width) so the native hit-test matches what the
    /// renderer draws. Not compiled off Windows; a missing handle is non-fatal.
    #[cfg(windows)]
    fn install_snap(&self, window: &Window) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = window.window_handle() else {
            return;
        };
        if let RawWindowHandle::Win32(h) = handle.as_raw() {
            let hwnd: isize = h.hwnd.get();
            let buttons_w = (BUTTONS_CELLS * CELL_W + BTN_RIGHT_MARGIN).ceil() as i32;
            // SAFETY: hwnd is the live top-level window winit just created; we
            // install exactly once (resumed() early-returns once gpu exists).
            unsafe {
                crate::win_snap::install(hwnd, TITLEBAR_H as i32, RESIZE_BORDER as i32, buttons_w);
            }
        }
    }

    /// Flash the taskbar button to signal an OSC 9/777 desktop notification that
    /// arrived while the window was unfocused (it stops on the next focus). The
    /// lean, no-toast-infrastructure form of a notification. No-op off Windows.
    #[cfg(windows)]
    fn flash_taskbar(&self) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Some(gpu) = &self.gpu else {
            return;
        };
        let Ok(handle) = gpu.window.window_handle() else {
            return;
        };
        if let RawWindowHandle::Win32(h) = handle.as_raw() {
            // SAFETY: hwnd is the live top-level window handle.
            unsafe {
                crate::win_snap::flash_taskbar(h.hwnd.get());
            }
        }
    }

    /// Non-Windows: no taskbar flash.
    #[cfg(not(windows))]
    fn flash_taskbar(&self) {}

    /// Non-Windows: no custom frame to install.
    #[cfg(not(windows))]
    fn install_snap(&self, _window: &Window) {}

    /// Capture the live window geometry into `self.config.window` and persist
    /// it to the config file (D2). Best-effort: a failed `outer_position()`
    /// (e.g. Wayland) just skips the position. Only the geometry fields are
    /// written; every other config field the user set is preserved by
    /// `Config::persist_geometry`.
    fn save_window_geometry(&mut self) {
        let Some(g) = &self.gpu else { return };
        let win = &g.window;
        let size = win.inner_size();
        let maximized = win.is_maximized();
        // Don't capture the (transient) inner size while maximized — that would
        // overwrite the user's restored size with the maximized extent. Keep the
        // previously-saved size and only flip the maximized flag.
        if !maximized {
            self.config.window.size_w = Some(size.width.max(1));
            self.config.window.size_h = Some(size.height.max(1));
            if let Ok(pos) = win.outer_position() {
                self.config.window.pos_x = Some(pos.x);
                self.config.window.pos_y = Some(pos.y);
            }
        }
        self.config.window.maximized = Some(maximized);
        self.config.window.monitor = win.current_monitor().and_then(|m| m.name());
        let _ = Config::persist_geometry(self.config.window.clone());
    }

    /// Backplate quads for the caption buttons under hover / press (D1). A
    /// frameless window has no OS-drawn button highlights, so we paint a solid
    /// rect behind the hovered button (and a stronger one while pressed) using
    /// the SAME geometry as `hit_titlebar`/the chrome glyphs. Close hover is the
    /// danger red; minimize/maximize hover is a subtle foreground wash. The rect
    /// is drawn BEFORE the chrome text so the glyph stays legible on top.
    fn caption_backplates(&self, width: f32, fg_rgba: [f32; 4]) -> Vec<ColorRect> {
        let mut out = Vec::new();
        let btn_w = (BUTTON_CELLS * CELL_W) as i32;
        let left = Self::buttons_left_px(width);
        // (button, index-in-cluster)
        let slots = [
            (TitlebarHit::Minimize, 0),
            (TitlebarHit::Maximize, 1),
            (TitlebarHit::Close, 2),
        ];
        for (btn, idx) in slots {
            let pressed = self.pressed_button == Some(btn);
            let hovered = self.hovered_button == Some(btn);
            if !pressed && !hovered {
                continue;
            }
            let rgba = if btn == TitlebarHit::Close {
                // Danger red (matches the close glyph colour), Windows-style.
                let a = if pressed { 0.85 } else { 0.65 };
                [1.0, 0.0, 0.25, a]
            } else {
                // Subtle foreground wash for min/max.
                let a = if pressed { 0.22 } else { 0.14 };
                [fg_rgba[0], fg_rgba[1], fg_rgba[2], a]
            };
            let x = (left as i32) + idx * btn_w;
            out.push(ColorRect::new(x, 0, btn_w, TITLEBAR_H as i32, rgba));
        }
        out
    }

    /// Classify a point against the window's resize edges. A frameless window
    /// has no OS resize border, so we hit-test a thin band ourselves and ask
    /// winit to drive the native resize. The only impure input is the surface
    /// size; the band math is [`resize_edge_at`].
    fn hit_resize_edge(&self, x: f64, y: f64) -> Option<ResizeDirection> {
        let (w, h) = self
            .gpu
            .as_ref()
            .map(|g| {
                (
                    g.surface_config.width as f64,
                    g.surface_config.height as f64,
                )
            })
            .unwrap_or((0.0, 0.0));
        resize_edge_at(x, y, w, h)
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let cols = self.config.window.cols;
        let rows = self.config.window.rows;
        let width = (cols as f32 * CELL_W) as u32 + 16;
        let height = (rows as f32 * LINE_HEIGHT) as u32 + 16 + TITLEBAR_H as u32;

        // The window is created translucent when opacity < 1.0 so the desktop
        // shows through. Default (opacity 1.0) is a solid window. (The former
        // `acrylic` DWM-backdrop opt-in was removed with the multi-mode
        // transparency model in v0.4.21 — the single `opacity` slider is the whole
        // see-through control now.)
        let want_transparent = self.config.opacity < 1.0;
        let mut attrs = Window::default_attributes()
            .with_title(c0pl4nd_core::PRODUCT_NAME)
            .with_decorations(false)
            .with_transparent(want_transparent)
            .with_resizable(true);
        // D2: restore remembered geometry when it lands on a still-connected
        // monitor; otherwise fall back to the cols/rows-derived default size.
        // The on-screen validity check guards against a monitor being
        // unplugged or resolution-changed between runs (a saved position that
        // is now fully off-screen would orphan the window).
        let wc = &self.config.window;
        let restored = match (wc.size_w, wc.size_h) {
            (Some(sw), Some(sh)) if sw > 0 && sh > 0 => {
                let monitors: Vec<_> = event_loop.available_monitors().collect();
                let pos_ok = match (wc.pos_x, wc.pos_y) {
                    (Some(px), Some(py)) => geometry_on_screen(px, py, sw, sh, &monitors),
                    _ => false,
                };
                attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(sw, sh));
                if pos_ok {
                    attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(
                        wc.pos_x.unwrap_or(0),
                        wc.pos_y.unwrap_or(0),
                    ));
                }
                if wc.maximized == Some(true) {
                    attrs = attrs.with_maximized(true);
                }
                true
            }
            _ => false,
        };
        if !restored {
            attrs =
                attrs.with_inner_size(winit::dpi::LogicalSize::new(width as f64, height as f64));
        }
        // Audit UI-2: exit gracefully instead of panicking when the window
        // cannot be created (e.g. started with no display / headless session) —
        // a panic in the winit `resumed` callback aborts the process with a
        // backtrace; a clean `exit()` lets the event loop unwind normally.
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("failed to create the main window: {e}; exiting");
                event_loop.exit();
                return;
            }
        };

        // D4 (Windows): re-enable Aero Snap / maximize animations on the
        // frameless window by installing the custom-frame subclass. No-op off
        // Windows; a missing handle is non-fatal (the window just lacks snap).
        self.install_snap(&window);

        let gpu = match pollster::block_on(Gpu::new(
            window.clone(),
            self.config.font.size,
            want_transparent,
        )) {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("GPU init failed: {e}");
                event_loop.exit();
                return;
            }
        };

        self.gpu = Some(gpu);
        if self.tabs.is_empty() {
            self.spawn_tab();
            // Restore the saved `default` workspace on launch when it carries
            // a real multi-pane layout. Single-pane defaults are a no-op (same
            // shape as the fresh tab above). Without this call the
            // saved-on-exit default layout never re-materialises.
            self.restore_default_workspace_on_startup();
        }
        if let Some(g) = &self.gpu {
            g.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // D2: persist final geometry before we tear down.
                self.save_window_geometry();
                // Session restore: auto-save ALL window tabs (+ per-pane cwd) as
                // the "default" workspace via a crash-safe atomic write, which
                // `restore_default_workspace_on_startup` relaunches on the next
                // run. Fresh shells, restored working dirs.
                if let Some(path) = Self::workspace_path("default") {
                    if let Err(e) = self.capture_workspace().save_atomic(&path) {
                        tracing::error!("failed to save default workspace: {e}");
                    }
                }
                event_loop.exit();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                // Extend an in-progress mouse text selection (a plain left-drag;
                // the Ctrl+Shift pane-drag below is a separate gesture).
                if self.selection.map(|s| s.active).unwrap_or(false) {
                    if let Some((leaf, row, col)) = self.cell_at_pixel(position.x, position.y) {
                        if let Some(sel) = &mut self.selection {
                            if leaf == sel.leaf {
                                sel.head = (row, col);
                            }
                        }
                        self.request_redraw();
                    }
                    return;
                }
                // Feed the drag state machine; a press becomes a drag only past
                // the 6px threshold, so normal clicks are never eaten.
                let was_dragging = self.drag.is_dragging();
                let now_dragging = self.drag.cursor_moved((position.x, position.y)).is_some();
                if now_dragging {
                    if !was_dragging {
                        // First frame of a real drag — show the move cursor.
                        // The `.into()` is omitted here: winit::Window::set_cursor
                        // takes `impl Into<Cursor>` and CursorIcon implements that
                        // trivially. Adding `.into()` in this call site triggers
                        // E0283 (type-annotation needed) because the inference
                        // context cannot pick a unique Into target — the sibling
                        // call sites below succeed because their context resolves
                        // differently, but the safe form is the direct one.
                        if let Some(g) = &self.gpu {
                            g.window.set_cursor(winit::window::CursorIcon::Grabbing);
                        }
                    }
                    if let Some(g) = &self.gpu {
                        g.window.request_redraw();
                    }
                    return;
                }
                // Frameless resize/pointer cursor feedback. A decorations(false)
                // window has no OS resize cursor, so we drive it ourselves —
                // hovering an edge shows the directional resize cursor (the thing
                // that makes the window FEEL resizable) and hovering a caption
                // button shows the hand. Only fires on a zone change.
                {
                    use winit::window::{CursorIcon, ResizeDirection::*};
                    let edge = self.hit_resize_edge(position.x, position.y);
                    // Track which caption button (if any) the cursor is over, so
                    // the render pass can paint its hover backplate. An edge hit
                    // takes priority and clears the button hover.
                    let new_hover_btn = if edge.is_some() {
                        None
                    } else {
                        match self.hit_titlebar(position.x, position.y) {
                            h @ (TitlebarHit::Minimize
                            | TitlebarHit::Maximize
                            | TitlebarHit::Close) => Some(h),
                            _ => None,
                        }
                    };
                    if new_hover_btn != self.hovered_button {
                        self.hovered_button = new_hover_btn;
                        if let Some(g) = &self.gpu {
                            g.window.request_redraw();
                        }
                    }
                    // Track the hovered tab-strip zone for the hover backplate.
                    let new_hover_tab = if edge.is_some() {
                        None
                    } else {
                        self.hit_tab(position.x, position.y)
                    };
                    if new_hover_tab != self.hovered_tab {
                        self.hovered_tab = new_hover_tab;
                        if let Some(g) = &self.gpu {
                            g.window.request_redraw();
                        }
                    }
                    let icon = if let Some(d) = edge {
                        match d {
                            East | West => CursorIcon::EwResize,
                            North | South => CursorIcon::NsResize,
                            NorthWest | SouthEast => CursorIcon::NwseResize,
                            NorthEast | SouthWest => CursorIcon::NeswResize,
                        }
                    } else if new_hover_btn.is_some() {
                        CursorIcon::Pointer
                    } else {
                        CursorIcon::Default
                    };
                    if self.chrome_cursor != icon {
                        if let Some(g) = &self.gpu {
                            g.window.set_cursor(icon);
                        }
                        self.chrome_cursor = icon;
                    }
                }
                let hov = self.leaf_at(position.x, position.y);
                if hov != self.hover_leaf {
                    self.hover_leaf = hov;
                    // A hover change can reveal/hide a short cell's tab strip.
                    if let Some(g) = &self.gpu {
                        g.window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Settings overlay (GUI): while open it captures clicks — a
                // click on a row selects it (or cycles its value if already
                // selected); a click anywhere else dismisses the panel.
                if self.settings.is_some() {
                    if let Some(i) = self.settings_row_at(self.cursor.0, self.cursor.1) {
                        self.click_settings_row(i);
                    } else {
                        self.settings = None;
                        self.request_redraw();
                    }
                    return;
                }
                // Ctrl+Shift + press on a pane arms a pane-rearrange drag (the
                // Tabby model). Held first so it pre-empts titlebar/resize hits.
                let drag_mod = (self.modifiers.control_key() || self.modifiers.super_key())
                    && self.modifiers.shift_key();
                if drag_mod {
                    if let Some(leaf) = self.leaf_at(self.cursor.0, self.cursor.1) {
                        self.drag = DragState::press(leaf, self.cursor);
                        return;
                    }
                }
                // Frameless edge/corner resize takes priority over the titlebar.
                if let Some(dir) = self.hit_resize_edge(self.cursor.0, self.cursor.1) {
                    if let Some(gpu) = &self.gpu {
                        let _ = gpu.window.drag_resize_window(dir);
                    }
                    return;
                }
                // Modern tab strip: a click on a tab chip switches to it; the
                // '+' opens a new tab. Checked BEFORE the titlebar drag/forward
                // paths so the strip is clickable (it lives in the drag region).
                if let Some(zone) = self.hit_tab(self.cursor.0, self.cursor.1) {
                    match zone {
                        TabZone::Tab(i) => self.activate_tab(i),
                        TabZone::NewTab => self.spawn_tab(),
                        TabZone::Settings => self.open_settings_panel(),
                    }
                    return;
                }
                let hit = self.hit_titlebar(self.cursor.0, self.cursor.1);
                // E9: Ctrl(without Shift)+click on a URL in pane content opens
                // it via the OS default handler (http/https → browser, file →
                // OS). Checked before the E6 PTY forward so the click follows
                // the link instead of being reported. `url_at` returns None
                // over chrome/empty space, so titlebar clicks fall through.
                if self.modifiers.control_key() && !self.modifiers.shift_key() {
                    if let Some(url) = self.url_at(self.cursor.0, self.cursor.1) {
                        open_path(std::path::Path::new(&url));
                        return;
                    }
                }
                // E6: a press in pane content (not chrome) forwards to the PTY
                // when the program enabled mouse reporting (vim/tmux/htop). The
                // titlebar/resize paths above still win, so window controls work.
                if hit == TitlebarHit::None
                    && self.forward_mouse(
                        TermMouseButton::Left,
                        MouseEventKind::Press,
                        self.cursor.0,
                        self.cursor.1,
                    )
                {
                    return;
                }
                // Mouse text selection: a left-press in pane content (we only
                // reach here when E6 above did NOT consume it, i.e. the program
                // has mouse reporting OFF) begins a selection at that cell.
                if hit == TitlebarHit::None {
                    if let Some((leaf, row, col)) = self.cell_at_pixel(self.cursor.0, self.cursor.1)
                    {
                        self.selection = Some(Selection {
                            leaf,
                            anchor: (row, col),
                            head: (row, col),
                            active: true,
                        });
                        self.request_redraw();
                        return;
                    }
                    // A press on empty space clears any prior selection.
                    if self.selection.take().is_some() {
                        self.request_redraw();
                    }
                }
                // Double-click the drag area → toggle maximize (standard window
                // behaviour). Computed before borrowing gpu to avoid a borrow clash.
                let dbl_titlebar = if hit == TitlebarHit::Drag {
                    let now = Instant::now();
                    let dbl = self.last_titlebar_click.is_some_and(|t| {
                        now.duration_since(t) < std::time::Duration::from_millis(400)
                    });
                    self.last_titlebar_click = Some(now);
                    dbl
                } else {
                    false
                };
                // Record a press on a caption button for the active-state
                // backplate (cleared on release). Harmless for non-button hits.
                self.pressed_button = match hit {
                    TitlebarHit::Minimize | TitlebarHit::Maximize | TitlebarHit::Close => Some(hit),
                    _ => None,
                };
                if let Some(gpu) = &self.gpu {
                    match hit {
                        TitlebarHit::Close => event_loop.exit(),
                        TitlebarHit::Minimize => gpu.window.set_minimized(true),
                        TitlebarHit::Maximize => {
                            let max = gpu.window.is_maximized();
                            gpu.window.set_maximized(!max);
                        }
                        TitlebarHit::Drag => {
                            if dbl_titlebar {
                                let max = gpu.window.is_maximized();
                                gpu.window.set_maximized(!max);
                            } else {
                                let _ = gpu.window.drag_window();
                            }
                        }
                        TitlebarHit::None => {}
                    }
                    if self.pressed_button.is_some() {
                        gpu.window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                // Finalize an in-progress mouse text selection: stop extending
                // but keep the selection so it can be copied. A zero-area
                // selection (a plain click, no drag) is dropped.
                let mut finalized = false;
                if let Some(sel) = &mut self.selection {
                    if sel.active {
                        sel.active = false;
                        if sel.anchor == sel.head {
                            self.selection = None;
                        } else {
                            finalized = true;
                        }
                        self.request_redraw();
                    }
                }
                // X11-style copy-on-select (opt-in, default off): copy the moment
                // the drag ends. Write-only.
                if finalized && self.config.copy_on_select {
                    self.copy_selection();
                }
                // Clear any caption-button press state (drops the active-state
                // backplate back to hover/none).
                if self.pressed_button.take().is_some() {
                    if let Some(g) = &self.gpu {
                        g.window.request_redraw();
                    }
                }
                // E6: forward the release to the PTY when mouse reporting is on
                // and no pane-drag is in progress (a drag owns the release).
                if !self.drag.is_dragging()
                    && self.hit_titlebar(self.cursor.0, self.cursor.1) == TitlebarHit::None
                    && self.forward_mouse(
                        TermMouseButton::Left,
                        MouseEventKind::Release,
                        self.cursor.0,
                        self.cursor.1,
                    )
                {
                    return;
                }
                // End any pane drag; a real drag (past threshold) resolves a drop,
                // a sub-threshold press is just a click and is discarded.
                if let Some(source) = self.drag.release() {
                    self.resolve_drop(source);
                    if let Some(g) = &self.gpu {
                        g.window.set_cursor(winit::window::CursorIcon::Default);
                        g.window.request_redraw();
                    }
                }
            }
            WindowEvent::Resized(size) => {
                let w = size.width.max(1);
                let h = size.height.max(1);
                if self.gpu.is_some() {
                    // The surface reconfigure + redraw stay immediate for visual
                    // smoothness; the per-leaf PTY resize (which fires SIGWINCH
                    // into every shell) is debounced via `pending_resize`, drained
                    // at ~30 Hz in `about_to_wait`.
                    if let Some(gpu) = &mut self.gpu {
                        gpu.resize(w, h);
                    }
                    self.pending_resize = Some((w, h));
                    // D2: mark geometry dirty; the actual config write is
                    // throttled in `about_to_wait` so a drag doesn't write per
                    // pixel.
                    self.geom_dirty = true;
                    if let Some(g) = &self.gpu {
                        g.window.request_redraw();
                    }
                }
            }
            WindowEvent::Moved(_) => {
                // D2: window dragged to a new position — remember it (debounced).
                self.geom_dirty = true;
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                // C14: report focus in/out to every session whose program armed
                // DEC ?1004 (ESC[I on focus-in, ESC[O on focus-out). focus_report
                // is a no-op when ?1004 is off, so call it unconditionally.
                for tab in &self.tabs {
                    for cell in tab.cells.values() {
                        for sess in &cell.sessions {
                            if let Ok(mut t) = sess.terminal().lock() {
                                t.focus_report(focused);
                            }
                        }
                    }
                }
                // Drain the queued focus replies to the PTYs immediately.
                self.pump_terminal_io();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (up, lines) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => {
                        (y > 0.0, (y.abs().ceil() as usize).max(1))
                    }
                    winit::event::MouseScrollDelta::PixelDelta(p) => (
                        p.y > 0.0,
                        ((p.y.abs() / LINE_HEIGHT as f64).ceil() as usize).max(1),
                    ),
                };
                // Ctrl+wheel zooms the grid font instead of scrolling.
                if self.modifiers.control_key() || self.modifiers.super_key() {
                    self.zoom_font(if up { 0.1 } else { -0.1 });
                    return;
                }
                // E6: when the program enabled mouse reporting, the wheel is sent
                // to the PTY (one report per line) instead of scrolling the local
                // scrollback view — alt-screen apps (less/vim) drive their own.
                let wheel_btn = if up {
                    TermMouseButton::WheelUp
                } else {
                    TermMouseButton::WheelDown
                };
                let reporting = self
                    .active_session()
                    .and_then(|s| {
                        s.terminal()
                            .lock()
                            .ok()
                            .map(|t| t.mouse_mode() != MouseMode::Off)
                    })
                    .unwrap_or(false);
                if reporting {
                    for _ in 0..lines {
                        self.forward_mouse(
                            wheel_btn,
                            MouseEventKind::Press,
                            self.cursor.0,
                            self.cursor.1,
                        );
                    }
                    if let Some(g) = &self.gpu {
                        g.window.request_redraw();
                    }
                    return;
                }
                if let Some(s) = self.active_session() {
                    if let Ok(mut term) = s.terminal().lock() {
                        if up {
                            term.scroll_up_view(lines);
                        } else {
                            term.scroll_down_view(lines);
                        }
                    }
                }
                if let Some(g) = &self.gpu {
                    g.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                // Releasing the drag modifier mid-gesture cancels the drag (no
                // drop) so a pane is never moved by accident.
                let drag_mod = (self.modifiers.control_key() || self.modifiers.super_key())
                    && self.modifiers.shift_key();
                if !drag_mod && self.drag != DragState::Idle {
                    self.drag.cancel();
                    if let Some(g) = &self.gpu {
                        g.window.set_cursor(winit::window::CursorIcon::Default);
                        g.window.request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                // Any keypress dismisses the startup splash overlay.
                self.splash = None;
                // Paste-safety overlay captures Enter (paste) / Esc (cancel);
                // every other key is swallowed while the warning is up.
                if self.pending_paste.is_some() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Enter) => self.confirm_paste(),
                        Key::Named(NamedKey::Escape) => self.cancel_paste(),
                        _ => {}
                    }
                    return;
                }
                // Overlay modes capture keystrokes instead of the PTY.
                // The Save-/Restore-Layout prompts are routed FIRST because
                // they're opened via the palette ("Save Layout As…" /
                // "Restore Layout") and overlay on top of it; without this
                // route the prompt would render but every keystroke would
                // fall through to the PTY (P0 functional gap caught at QA).
                if self.settings.is_some() {
                    self.handle_settings_key(&event.logical_key);
                    return;
                }
                if self.workspace_prompt.is_some() {
                    self.handle_workspace_prompt_key(&event.logical_key);
                    return;
                }
                if self.palette_mode {
                    self.handle_palette_key(&event.logical_key, event_loop);
                    return;
                }
                if self.search_mode {
                    self.handle_search_key(&event.logical_key);
                    return;
                }
                // Font zoom — Ctrl with +/=/−/_/0, TOLERANT of Shift so that
                // BOTH Ctrl++ (which is physically Ctrl+Shift+= on a US layout)
                // and Ctrl+= zoom in, matching standard browser/editor UX. Placed
                // before the Ctrl[+Shift] chord families so the zoom keys are
                // never eaten by a tab combo. zoom_font/reset request their own
                // redraw via set_font_scale.
                let ctrl_zoom = (self.modifiers.control_key() || self.modifiers.super_key())
                    && !self.modifiers.alt_key();
                if ctrl_zoom {
                    if let Key::Character(c) = &event.logical_key {
                        match c.as_str() {
                            "+" | "=" => {
                                self.zoom_font(0.1);
                                return;
                            }
                            "-" | "_" => {
                                self.zoom_font(-0.1);
                                return;
                            }
                            "0" | ")" => {
                                self.reset_font_scale();
                                return;
                            }
                            _ => {}
                        }
                    }
                }
                // Nested-cell-tab combos on Ctrl WITHOUT Shift (distinct from the
                // window-level Ctrl+Shift+… family and the Alt cell-focus family,
                // so the three tab/focus levels never collide — pre-mortem #4):
                //   Ctrl+PageDown / Ctrl+PageUp  → next / prev tab in focused cell
                //   Ctrl+T                       → new tab in focused cell
                let ctrl_only = (self.modifiers.control_key() || self.modifiers.super_key())
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key();
                // Ctrl+, → open settings (the universal "preferences" shortcut);
                // Ctrl+1..9 → jump directly to window tab N (1-based; 9 = last).
                if ctrl_only {
                    if let Key::Character(c) = &event.logical_key {
                        match c.as_str() {
                            "," => {
                                // Ctrl+, opens the in-app settings OVERLAY (the
                                // discoverable preferences UI). The raw config
                                // file remains available via the palette's
                                // "Open Config File" action.
                                self.open_settings_panel();
                                return;
                            }
                            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && d != "0" => {
                                self.select_tab_by_number(d.as_bytes()[0] - b'0');
                                return;
                            }
                            _ => {}
                        }
                    }
                }
                if ctrl_only && self.handle_cell_tab_combo(&event.logical_key) {
                    return;
                }
                // Tab control combos (Ctrl+Shift+… ; on macOS Cmd+Shift+…).
                let mod_combo = (self.modifiers.control_key() || self.modifiers.super_key())
                    && self.modifiers.shift_key();
                if mod_combo && self.handle_tab_combo(&event.logical_key, event_loop) {
                    return;
                }
                // Pane move/resize family on Alt (Alt+Arrow = swap focused pane,
                // Alt+Shift+Arrow = resize focused split) — kept off the
                // Ctrl/Cmd+Shift+Arrow directional-focus chords.
                let alt_only = self.modifiers.alt_key()
                    && !self.modifiers.control_key()
                    && !self.modifiers.super_key();
                if alt_only && self.handle_alt_combo(&event.logical_key) {
                    return;
                }
                // DECCKM (application-cursor-keys) state, read before the mutable
                // session borrow so arrow/Home/End encode SS3 vs CSI correctly.
                let app_cursor = self
                    .active_session()
                    .and_then(|s| {
                        s.terminal()
                            .lock()
                            .ok()
                            .map(|t| t.application_cursor_keys())
                    })
                    .unwrap_or(false);
                if let Some(bytes) =
                    key_to_bytes(&event.logical_key, &event.text, app_cursor, self.modifiers)
                {
                    // Typing clears a stale mouse selection (the highlight would
                    // otherwise linger over scrolled/overwritten content).
                    if self.selection.take().is_some() {
                        self.request_redraw();
                    }
                    if let Some(s) = self.active_session_mut() {
                        if let Ok(mut term) = s.terminal().lock() {
                            term.scroll_to_bottom();
                        }
                        let _ = s.write_input(&bytes);
                    }
                }
            }
            WindowEvent::DroppedFile(path) => {
                // Insert the dropped file's path at the cursor (quoted when it
                // contains spaces/shell-significant chars). Inserted as TEXT
                // ONLY — no trailing newline is sent, so it is never executed on
                // the user's behalf. winit delivers one event per file, so
                // dropping several inserts each path separated by a space.
                let text = format_dropped_path(&path);
                if let Some(s) = self.active_session_mut() {
                    let _ = s.write_input(text.as_bytes());
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        // E10/E11: drain terminal-produced OSC side effects (query replies,
        // clipboard writes, color sets, notifications) for every live session.
        self.pump_terminal_io();
        // E16: close panes whose shell exited (may exit the app if it was last).
        self.reap_dead_panes(event_loop);
        // Drain a pending interactive-resize at ~30 Hz: a rapid window drag
        // issues at most ~30 per-leaf PTY resizes/sec, and because the pending
        // size persists until drained, the final size is always applied.
        if let Some((w, h)) = self.pending_resize {
            if now.duration_since(self.last_pty_resize) >= Duration::from_millis(33) {
                self.apply_pty_resize(w, h);
                self.pending_resize = None;
                self.last_pty_resize = now;
            }
        }
        // D2: debounced window-geometry persistence. A resize/move sets
        // `geom_dirty`; we write at most ~once/600ms so an interactive drag
        // doesn't thrash the config file.
        if self.geom_dirty && now.duration_since(self.last_geom_save) >= Duration::from_millis(600)
        {
            self.geom_dirty = false;
            self.last_geom_save = now;
            self.save_window_geometry();
        }
        if now >= self.next_poll {
            self.next_poll = now + Duration::from_millis(16);
            let damaged = self
                .active_tab()
                .map(|tab| {
                    // Only the visible (active) tab of each cell paints, so only
                    // its damage forces a redraw — background nested tabs do not.
                    tab.cells.values().filter_map(Cell::active).any(|p| {
                        p.terminal()
                            .lock()
                            .map(|t| t.grid().is_damaged())
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            // E5/E7: drive the cursor blink — once per 530ms half-period issue a
            // redraw so an idle window still animates the cursor. Cheap (<2 Hz)
            // and only when the focused pane's cursor actually blinks.
            let blink_due = now.duration_since(self.last_blink_redraw)
                >= Duration::from_millis(530)
                && self
                    .active_session()
                    .and_then(|s| {
                        s.terminal()
                            .lock()
                            .ok()
                            .map(|t| t.is_cursor_visible() && t.cursor_blink())
                    })
                    .unwrap_or(false);
            if blink_due {
                self.last_blink_redraw = now;
            }
            if damaged || blink_due {
                if let Some(g) = &self.gpu {
                    g.window.request_redraw();
                }
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_poll));
    }
}

impl App {
    fn render(&mut self) {
        let fg = self.fg_color();
        let accent = self.accent_color();
        let bg = self.bg_color();
        // Brand purple (ANSI 4 = #7700FF in the Itasha.Corp default) — the
        // 'Itasha' half of the two-tone corporate wordmark; the '.Corp' half
        // uses `accent` (brand green). Theme-driven so it adapts per theme.
        let brand_purple = {
            let (r, g, b) = self.theme.ansi(4);
            GColor::rgb(r, g, b)
        };
        // Muted chrome color (ANSI 8 / bright-black) for inactive tab chips.
        let muted = {
            let (r, g, b) = self.theme.ansi(8);
            GColor::rgb(r, g, b)
        };
        // Configurable grid content padding (captured before the &mut gpu borrow
        // so the render-loop call sites match the hit-test sites above).
        let content_pad = self.config.window.padding as f32;
        // Font-zoom: grid cell dims + scale captured before the &mut gpu borrow
        // so the render-loop call sites (after the borrow) match the hit-test
        // sites. At scale 1.0 these equal CELL_W / LINE_HEIGHT exactly.
        let grid_cw = self.cell_w();
        let grid_ch = self.cell_h();
        let grid_scale = self.font_scale;
        let signal_red = GColor::rgb(255, 0, 64);
        let to_rgba = |c: GColor| {
            [
                c.r() as f32 / 255.0,
                c.g() as f32 / 255.0,
                c.b() as f32 / 255.0,
                c.a() as f32 / 255.0,
            ]
        };

        // Cascade the focused tab into per-leaf cells over the content area.
        let content = self.content_rect();
        let cells: Vec<(LeafId, LRect)> = self
            .active_tab()
            .map(|t| t.layout.cascade(content))
            .unwrap_or_default();
        let focused = self
            .active_tab()
            .map(|t| t.layout.focused)
            .unwrap_or(LeafId(0));

        // Snapshot each VISIBLE leaf's grid into colour spans (clears damage)
        // and collect its inline-image quads, offset into the leaf's cell.
        type LeafSpan = (LeafId, LRect, Vec<(String, GColor)>);
        let mut leaf_spans: Vec<LeafSpan> = Vec::with_capacity(cells.len());
        // E1: per-cell background paints per leaf, as (row, col, rgba). Turned
        // into ColorRects below (before the gpu borrow) and drawn behind text.
        #[allow(clippy::type_complexity)]
        let mut leaf_bgs: Vec<(LeafId, LRect, Vec<(usize, usize, [f32; 4])>)> = Vec::new();
        #[allow(clippy::type_complexity)]
        let mut leaf_decos: Vec<(
            LeafId,
            LRect,
            Vec<(
                usize,
                usize,
                c0pl4nd_core::grid::UnderlineStyle,
                bool,
                [f32; 4],
            )>,
        )> = Vec::new();
        let mut image_quads: Vec<(LeafId, crate::image_render::ImageQuad)> = Vec::new();
        for (leaf, cell) in &cells {
            if let Some((spans, bgs, decos)) = self.leaf_render(*leaf, fg) {
                leaf_spans.push((*leaf, *cell, spans));
                if !bgs.is_empty() {
                    leaf_bgs.push((*leaf, *cell, bgs));
                }
                if !decos.is_empty() {
                    leaf_decos.push((*leaf, *cell, decos));
                }
            }
            for q in self.collect_leaf_image_quads(*leaf, *cell) {
                image_quads.push((*leaf, q));
            }
        }
        let live_leaves: Vec<LeafId> = cells.iter().map(|(id, _)| *id).collect();

        // Nested-tab strips for visible cells (>=2 tabs and tall enough, OR a
        // short cell currently hovered — the auto-hide/hover-reveal of T4.2).
        // Each entry: (leaf, cell rect, strip text). Built before borrowing gpu.
        // Each entry: (leaf, cell rect, strip text, laid_out). `laid_out` = the
        // strip steals a grid line (tall cell, matches PTY sizing); otherwise it
        // is a hover-only overlay on a short cell that does not change geometry.
        type StripInfo = (LeafId, LRect, String, bool);
        let mut strips: Vec<StripInfo> = Vec::new();
        if let Some(tab) = self.active_tab() {
            for (leaf, cell) in &cells {
                if let Some(c) = tab.cells.get(leaf) {
                    let tall_enough = (cell.h - 2 * BORDER_PX) >= CELL_STRIP_MIN_H;
                    let hovered = self.hover_leaf == Some(*leaf);
                    let show = c.tab_count() > 1 && (tall_enough || hovered);
                    if show {
                        let max_cols =
                            (((cell.w - 2 * BORDER_PX) as f32 / grid_cw).floor()).max(0.0) as usize;
                        let text = cell_tabbar_text(c.tab_count(), c.group.active, max_cols);
                        if !text.is_empty() {
                            strips.push((*leaf, *cell, text, tall_enough));
                        }
                    }
                }
            }
        }
        let laid_out_strip: std::collections::HashSet<LeafId> = strips
            .iter()
            .filter(|(_, _, _, laid)| *laid)
            .map(|(id, _, _, _)| *id)
            .collect();

        // Command-palette overlay text (built before borrowing gpu).
        let palette_text = if self.palette_mode {
            let items = self.palette_filtered();
            let mut s = format!(
                "\u{2592} command palette  \u{203a} {}\n\n",
                self.palette_query
            );
            if items.is_empty() {
                s.push_str("  (no matches)\n");
            }
            for (i, it) in items.iter().enumerate() {
                let marker = if i == self.palette_idx {
                    "\u{25b8} "
                } else {
                    "  "
                };
                s.push_str(marker);
                let hint = action_hint(it);
                if hint.is_empty() {
                    s.push_str(it);
                } else {
                    s.push_str(&format!("{it:<26}{hint}"));
                }
                s.push('\n');
            }
            Some(s)
        } else {
            None
        };

        // Settings panel overlay text (D3), built before borrowing gpu.
        let settings_text = self.settings_text();

        // The paste-safety overlay reuses the splash text layer (it takes
        // priority over the startup splash while a paste awaits confirmation).
        let splash_text = self.pending_paste.as_ref().map_or_else(
            || self.splash.clone(),
            |t| {
                let lines = t.lines().count().max(1);
                Some(format!(
                    "\u{26a0} Paste {} line{} ({} chars)?\n\n[Enter] paste     [Esc] cancel",
                    lines,
                    if lines == 1 { "" } else { "s" },
                    t.chars().count(),
                ))
            },
        );
        // Drag overlay quads (dim source + zone highlight + cursor ghost),
        // computed before the gpu borrow since they read the active tab.
        let drag_overlay = self.drag_overlay_quads(accent);
        // Caption-button hover/press backplates (D1). Computed here, before the
        // `&mut self.gpu` borrow below, so it can borrow `self` immutably; the
        // surface width is read from the (still-shared) gpu first.
        let caption_backplates = self
            .gpu
            .as_ref()
            .map(|g| self.caption_backplates(g.surface_config.width as f32, to_rgba(fg)))
            .unwrap_or_default();
        // No pane border/inset for a single leaf — visually identical to the
        // pre-split renderer; a 1px border only appears once the window splits.
        let border = if cells.len() > 1 { BORDER_PX } else { 0 };
        // Terminal cursor quads (E5/E7), computed before the gpu borrow since
        // they lock the focused terminal.
        let cursor_quads = self.cursor_quads(focused, &cells, border, &laid_out_strip, accent);

        // Programming-ligatures toggle. The grid is monospace by default
        // (`Shaping::Basic` — every char maps 1:1 to its own glyph, preserving
        // strict cell fidelity). When the user opts in via `ligatures = true`,
        // the grid runs `Shaping::Advanced` (rustybuzz), so a ligature font
        // (Fira Code / Cascadia Code) forms `->`, `=>`, `!=`, … Captured before
        // the `&mut self.gpu` borrow so `self.config` stays accessible.
        let grid_shaping = if self.config.ligatures {
            Shaping::Advanced
        } else {
            Shaping::Basic
        };

        let Some(gpu) = &mut self.gpu else { return };
        let width = gpu.surface_config.width as f32;

        // Fill one grid text buffer per visible leaf, sized to its cell, and
        // drop buffers for leaves that no longer exist. (Inlined rather than via
        // `Gpu::leaf_buffer` so `font_system` stays a disjoint field borrow
        // alongside the `leaf_buffers` entry.)
        gpu.retain_leaf_buffers(&live_leaves);
        // Grid glyph metrics scale with font-zoom; the base `gpu.metrics` (used
        // by the fixed-size chrome) is left untouched. At scale 1.0 this is the
        // base metrics unchanged.
        let metrics = Metrics::new(
            gpu.metrics.font_size * grid_scale,
            gpu.metrics.line_height * grid_scale,
        );
        let default_attrs = Attrs::new().family(Family::Monospace).color(fg);
        for (leaf, cell, spans) in &leaf_spans {
            // A laid-out strip steals one line from the grid's drawable height.
            let strip_h = if laid_out_strip.contains(leaf) {
                CELL_TABBAR_H
            } else {
                0.0
            };
            let cw = (cell.w - 2 * border).max(1) as f32;
            let ch = ((cell.h - 2 * border) as f32 - strip_h).max(1.0);
            let fs = &mut gpu.font_system;
            let buf = gpu
                .leaf_buffers
                .entry(*leaf)
                .or_insert_with(|| Buffer::new(fs, metrics));
            buf.set_size(fs, Some(cw), Some(ch));
            buf.set_rich_text(
                fs,
                spans.iter().map(|(s, col)| {
                    (
                        s.as_str(),
                        Attrs::new().family(Family::Monospace).color(*col),
                    )
                }),
                &default_attrs,
                grid_shaping,
                None,
            );
            buf.shape_until_scroll(fs, false);
        }

        // Nested-tab-strip buffers (one short line per visible strip cell).
        for (leaf, cell, text, _) in &strips {
            let cw = (cell.w - 2 * border).max(1) as f32;
            let fs = &mut gpu.font_system;
            let buf = gpu
                .tabbar_buffers
                .entry(*leaf)
                .or_insert_with(|| Buffer::new(fs, metrics));
            buf.set_size(fs, Some(cw), Some(CELL_TABBAR_H));
            buf.set_text(
                fs,
                text,
                &Attrs::new().family(Family::Monospace).color(accent),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(fs, false);
        }

        // Custom chrome: wordmark (accent) on the left, buttons flush right.
        // The button cluster begins at the SAME pixel `hit_titlebar` uses.
        let buttons_left = Self::buttons_left_px(width);
        // Modern tab strip: the wordmark, then one chip per tab (active chip in
        // accent, inactive chips muted), then a '+' new-tab affordance. Replaces
        // the old "[N/M]" counter. Click hit-testing is wired in `hit_titlebar`
        // off the SAME geometry. In search mode the strip yields to the search
        // status line.
        let mut chrome_spans: Vec<(String, GColor)> = Vec::new();
        // Byte ranges (into the concatenated chrome string) of each clickable
        // tab-strip zone, used below to map laid-out glyphs back to zones.
        let mut zone_bytes: Vec<(TabZone, usize, usize)> = Vec::new();
        let mut byte_off = 0usize;
        if self.search_mode {
            let n = self.search_matches.len();
            let cur = if n == 0 { 0 } else { self.search_idx + 1 };
            chrome_spans.push((
                format!(
                    " search /{}  [{cur}/{n}]  (esc to exit) ",
                    self.search_query
                ),
                accent,
            ));
        } else {
            let wm = format!(" {}   ", c0pl4nd_core::PRODUCT_NAME);
            byte_off += wm.len();
            chrome_spans.push((wm, accent));
            for i in 0..self.tabs.len() {
                let col = if i == self.active { accent } else { muted };
                let s = format!(" {} ", i + 1);
                zone_bytes.push((TabZone::Tab(i), byte_off, byte_off + s.len()));
                byte_off += s.len();
                chrome_spans.push((s, col));
            }
            let plus = "  +  ".to_string();
            zone_bytes.push((TabZone::NewTab, byte_off, byte_off + plus.len()));
            byte_off += plus.len();
            chrome_spans.push((plus, accent));
            // Clickable settings affordance (opens the in-app settings overlay).
            let gear = "  \u{2699}  ".to_string();
            zone_bytes.push((TabZone::Settings, byte_off, byte_off + gear.len()));
            chrome_spans.push((gear, muted));
        }
        gpu.chrome_buffer.set_rich_text(
            &mut gpu.font_system,
            chrome_spans.iter().map(|(s, col)| {
                (
                    s.as_str(),
                    Attrs::new().family(Family::Monospace).color(*col),
                )
            }),
            &Attrs::new().family(Family::Monospace).color(fg),
            Shaping::Advanced,
            None,
        );
        gpu.chrome_buffer
            .shape_until_scroll(&mut gpu.font_system, false);
        // Map laid-out glyphs back to clickable pixel x-ranges so the rendered
        // tab chips and their click targets always agree (independent of font
        // advance / DPI). x is buffer-relative; the buffer draws at CHROME_LEFT.
        let mut zones: Vec<(TabZone, f32, f32)> = zone_bytes
            .iter()
            .map(|&(z, _, _)| (z, f32::INFINITY, f32::NEG_INFINITY))
            .collect();
        for run in gpu.chrome_buffer.layout_runs() {
            for g in run.glyphs.iter() {
                for (idx, &(_, bs, be)) in zone_bytes.iter().enumerate() {
                    if g.start >= bs && g.start < be {
                        let x0 = CHROME_LEFT + g.x;
                        zones[idx].1 = zones[idx].1.min(x0);
                        zones[idx].2 = zones[idx].2.max(x0 + g.w);
                    }
                }
            }
        }
        gpu.tab_zones = zones.into_iter().filter(|&(_, x0, x1)| x1 > x0).collect();
        // Windows: hand the interactive tab-strip x-ranges to the native
        // hit-test so clicks on tabs / '+' / gear are HTCLIENT (reach winit)
        // instead of HTCAPTION (window drag). Without this the whole strip is a
        // drag region and the buttons appear dead. Physical px, surface-relative
        // — exactly the space win_snap's ScreenToClient produces.
        #[cfg(windows)]
        {
            let izones: Vec<(i32, i32)> = gpu
                .tab_zones
                .iter()
                .map(|&(_, x0, x1)| (x0.floor() as i32, x1.ceil() as i32))
                .collect();
            crate::win_snap::set_interactive_zones(izones);
        }
        // Caption buttons: each glyph in its OWN single-glyph buffer (no
        // padding) so it can be centred within its BUTTON_CELLS-wide backplate
        // in the render section below — independent of how the symbol font's
        // advance compares to the space advance. Order: [minimize, max, close].
        let caption_glyphs: [(&str, GColor); 3] = [
            ("\u{2014}", fg),         // minimize  —
            ("\u{25a1}", fg),         // maximize  □
            ("\u{2715}", signal_red), // close     ✕
        ];
        for (i, (glyph, col)) in caption_glyphs.iter().enumerate() {
            let fs = &mut gpu.font_system;
            let buf = &mut gpu.caption_buffers[i];
            buf.set_text(
                fs,
                glyph,
                &Attrs::new().family(Family::Monospace).color(*col),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(fs, false);
        }

        if let Some(pt) = palette_text.as_ref().or(settings_text.as_ref()) {
            gpu.palette_buffer.set_text(
                &mut gpu.font_system,
                pt,
                &Attrs::new().family(Family::Monospace).color(accent),
                Shaping::Advanced,
                None,
            );
            gpu.palette_buffer
                .shape_until_scroll(&mut gpu.font_system, false);
        }

        if let Some(st) = &splash_text {
            gpu.splash_buffer.set_size(
                &mut gpu.font_system,
                Some(gpu.surface_config.width as f32),
                Some(gpu.surface_config.height as f32),
            );
            // Splash body (accent), then the two-tone corporate wordmark
            // footer: 'Itasha' in brand purple, '.Corp' in brand green.
            let mono = || Attrs::new().family(Family::Monospace);
            gpu.splash_buffer.set_rich_text(
                &mut gpu.font_system,
                [
                    (st.as_str(), mono().color(accent)),
                    ("\n  an ", mono().color(fg)),
                    ("Itasha", mono().color(brand_purple)),
                    (".Corp", mono().color(accent)),
                    (" product", mono().color(fg)),
                ],
                &mono().color(accent),
                Shaping::Advanced,
                None,
            );
            gpu.splash_buffer
                .shape_until_scroll(&mut gpu.font_system, false);
        }

        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => {
                gpu.reconfigure();
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        gpu.viewport.update(
            &gpu.queue,
            Resolution {
                width: gpu.surface_config.width,
                height: gpu.surface_config.height,
            },
        );

        let w = gpu.surface_config.width as i32;
        let h = gpu.surface_config.height as i32;
        let mut areas = vec![
            // Title bar chrome (wordmark + tab strip), left-aligned.
            TextArea {
                buffer: &gpu.chrome_buffer,
                left: CHROME_LEFT,
                top: 6.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: TITLEBAR_H as i32,
                },
                default_color: fg,
                custom_glyphs: &[],
            },
        ];
        // Caption buttons (min / max / close): each glyph centred — H and V —
        // in its BUTTON_CELLS-wide backplate. The horizontal offset is measured
        // from the glyph's own laid-out advance (gw) so symbol-font fallback
        // (□/✕) can't push it off-centre; the vertical offset centres the line
        // box in the title bar. Slots are pinned at the ABSOLUTE `buttons_left`
        // pixel so they line up with the hit/backplate geometry at any width.
        {
            let cap_cell_w = BUTTON_CELLS * CELL_W;
            let cap_top = ((TITLEBAR_H - gpu.metrics.line_height) / 2.0).max(0.0);
            for i in 0..gpu.caption_buffers.len() {
                let buf = &gpu.caption_buffers[i];
                let mut gw = 0.0f32;
                for run in buf.layout_runs() {
                    for g in run.glyphs.iter() {
                        gw = gw.max(g.x + g.w);
                    }
                }
                let left = caption_glyph_left(buttons_left, i, cap_cell_w, gw);
                areas.push(TextArea {
                    buffer: buf,
                    left,
                    top: cap_top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: w,
                        bottom: TITLEBAR_H as i32,
                    },
                    default_color: fg,
                    custom_glyphs: &[],
                });
            }
        }
        // One terminal-grid TextArea per visible leaf, placed at its cell origin
        // and clipped to the cell. glyphon clips text via `bounds`, so no wgpu
        // scissor is needed for the grid layer.
        for (leaf, cell, _) in &leaf_spans {
            if let Some(buf) = gpu.leaf_buffers.get(leaf) {
                // Push the grid below a laid-out tab strip (tall cells); a
                // hover-only overlay strip leaves the grid origin unchanged.
                let strip_top = if laid_out_strip.contains(leaf) {
                    CELL_TABBAR_H
                } else {
                    0.0
                };
                let (lx, ly) = leaf_text_origin(*cell, border, content_pad, 2.0 + strip_top);
                areas.push(TextArea {
                    buffer: buf,
                    left: lx,
                    top: ly,
                    scale: 1.0,
                    bounds: leaf_text_bounds(*cell, border),
                    default_color: fg,
                    custom_glyphs: &[],
                });
            }
        }
        // Cell tab strips, drawn at the top of their cell (over the grid for a
        // hover-only overlay; above the pushed-down grid for a laid-out strip).
        for (leaf, cell, _, _) in &strips {
            if let Some(buf) = gpu.tabbar_buffers.get(leaf) {
                let left = cell.x as f32 + border as f32 + 4.0;
                let top = cell.y as f32 + border as f32 + 1.0;
                areas.push(TextArea {
                    buffer: buf,
                    left,
                    top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: cell.x + border,
                        top: cell.y + border,
                        right: (cell.x + cell.w - border).max(cell.x + border),
                        bottom: (cell.y + border + CELL_TABBAR_H as i32)
                            .min(cell.y + cell.h - border),
                    },
                    default_color: accent,
                    custom_glyphs: &[],
                });
            }
        }
        if splash_text.is_some() {
            // neofetch-style startup splash, drawn over the grid until the
            // first keypress dismisses it.
            areas.push(TextArea {
                buffer: &gpu.splash_buffer,
                left: 12.0,
                top: TITLEBAR_H + 10.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: TITLEBAR_H as i32,
                    right: w,
                    bottom: h,
                },
                default_color: accent,
                custom_glyphs: &[],
            });
        }
        if palette_text.is_some() || settings_text.is_some() {
            // Centered command-palette / settings overlay (mutually exclusive
            // modes share the palette buffer).
            areas.push(TextArea {
                buffer: &gpu.palette_buffer,
                left: (w as f32 * 0.25).max(40.0),
                top: TITLEBAR_H + 40.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: TITLEBAR_H as i32,
                    right: w,
                    bottom: h,
                },
                default_color: accent,
                custom_glyphs: &[],
            });
        }
        let prepared = gpu.text_renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut gpu.font_system,
            &mut gpu.atlas,
            &gpu.viewport,
            areas,
            &mut gpu.swash_cache,
        );
        if let Err(e) = prepared {
            tracing::error!("glyphon prepare failed: {e}");
            return;
        }

        // Pane chrome: a full-content gutter fill (= bg, harmless for a single
        // pane where chrome_quads returns empty) plus a 1px border per leaf,
        // accent on the focused leaf and muted on the rest.
        let mut chrome = chrome_quads(
            &cells,
            focused,
            to_rgba(accent),
            [0.30, 0.30, 0.34, 1.0],
            // `bg` is a wgpu::Color (f64 0..1 fields), distinct from glyphon::Color.
            [bg.r as f32, bg.g as f32, bg.b as f32, bg.a as f32],
            content,
        );
        // E1: per-cell background quads, drawn over the gutter fill but behind
        // the grid text. Uses the SAME origin math as the text-area placement
        // below so a cell's background aligns exactly under its glyph.
        for (leaf, cell, bgs) in &leaf_bgs {
            let strip_top = if laid_out_strip.contains(leaf) {
                CELL_TABBAR_H
            } else {
                0.0
            };
            let (ox, oy) = leaf_text_origin(*cell, border, content_pad, 2.0 + strip_top);
            let cw = grid_cw.ceil() as i32;
            let ch = grid_ch.ceil() as i32;
            for (r, c, rgba) in bgs {
                let x = (ox + *c as f32 * grid_cw) as i32;
                let y = (oy + *r as f32 * grid_ch) as i32;
                chrome.push(ColorRect::new(x, y, cw, ch, *rgba));
            }
        }
        // Styled underlines + strikeout (C20/C24), drawn as lines in the cell.
        // Curly is approximated as a single line; a true undercurl is a future
        // refinement. Underlines sit at the cell bottom; strikeout at mid-height.
        for (leaf, cell, decos) in &leaf_decos {
            let strip_top = if laid_out_strip.contains(leaf) {
                CELL_TABBAR_H
            } else {
                0.0
            };
            let (ox, oy) = leaf_text_origin(*cell, border, content_pad, 2.0 + strip_top);
            let cw = grid_cw.ceil() as i32;
            let ch = grid_ch.ceil() as i32;
            for (r, c, style, strikeout, rgba) in decos {
                use c0pl4nd_core::grid::UnderlineStyle as U;
                let x = (ox + *c as f32 * grid_cw) as i32;
                let y = (oy + *r as f32 * grid_ch) as i32;
                let uy = y + ch - 2;
                match style {
                    U::None => {}
                    U::Single => chrome.push(ColorRect::new(x, uy, cw, 1, *rgba)),
                    U::Curly => {
                        // Undercurl approximation: a 1px zigzag alternating
                        // between two adjacent rows every 2px (nvim LSP squiggle).
                        let mut dx = 0;
                        while dx < cw {
                            let w = 2.min(cw - dx);
                            let yo = if (dx / 2) % 2 == 0 { uy } else { uy - 1 };
                            chrome.push(ColorRect::new(x + dx, yo, w, 1, *rgba));
                            dx += 2;
                        }
                    }
                    U::Double => {
                        chrome.push(ColorRect::new(x, y + ch - 3, cw, 1, *rgba));
                        chrome.push(ColorRect::new(x, y + ch - 1, cw, 1, *rgba));
                    }
                    U::Dotted => {
                        let mut dx = 0;
                        while dx < cw {
                            chrome.push(ColorRect::new(x + dx, uy, 1, 1, *rgba));
                            dx += 2;
                        }
                    }
                    U::Dashed => {
                        let mut dx = 0;
                        while dx < cw {
                            let w = 3.min(cw - dx);
                            chrome.push(ColorRect::new(x + dx, uy, w, 1, *rgba));
                            dx += 5;
                        }
                    }
                }
                if *strikeout {
                    chrome.push(ColorRect::new(x, y + ch / 2, cw, 1, *rgba));
                }
            }
        }
        // Mouse text-selection highlight (over backgrounds, behind text+cursor).
        if let Some(sel) = self.selection {
            if let Some((_, cell)) = cells.iter().find(|(id, _)| *id == sel.leaf) {
                let strip_top = if laid_out_strip.contains(&sel.leaf) {
                    CELL_TABBAR_H
                } else {
                    0.0
                };
                let (ox, oy) = leaf_text_origin(*cell, border, content_pad, 2.0 + strip_top);
                let cw = grid_cw.ceil() as i32;
                let ch = grid_ch.ceil() as i32;
                // Grid column count derived from the cell width (full-line wash
                // for middle rows of a multi-row selection).
                let grid_cols =
                    (((cell.w - 2 * border) as f32 / grid_cw).floor()).max(1.0) as usize;
                let (start, end) = sel.ordered();
                for r in start.0..=end.0 {
                    let lo = if r == start.0 { start.1 } else { 0 };
                    let hi = if r == end.0 {
                        end.1
                    } else {
                        grid_cols.saturating_sub(1)
                    };
                    for c in lo..=hi.min(grid_cols.saturating_sub(1)) {
                        let x = (ox + c as f32 * grid_cw) as i32;
                        let y = (oy + r as f32 * grid_ch) as i32;
                        chrome.push(ColorRect::new(x, y, cw, ch, SELECTION_RGBA));
                    }
                }
            }
        }
        // Terminal cursor (E5/E7), behind the text so the glyph stays readable.
        chrome.extend(cursor_quads);
        // Caption-button hover/press backplates (D1), drawn before the chrome
        // text so the button glyph stays legible on top (computed above the gpu
        // borrow to avoid an aliasing conflict).
        chrome.extend(caption_backplates);
        // Active-tab backplate: a subtle accent-tinted fill behind the active
        // tab chip so it reads as "selected" like a modern tab bar. Uses the
        // same glyph-derived geometry as the click zones, so it always lines up.
        if !self.search_mode {
            if let Some(&(_, x0, x1)) = gpu
                .tab_zones
                .iter()
                .find(|&&(z, _, _)| z == TabZone::Tab(self.active))
            {
                let a = to_rgba(accent);
                chrome.push(ColorRect::new(
                    x0 as i32,
                    2,
                    (x1 - x0) as i32,
                    TITLEBAR_H as i32 - 4,
                    [a[0], a[1], a[2], 0.16],
                ));
            }
            // Hover backplate: a fainter foreground wash behind whatever
            // tab-strip zone the cursor is over (skipped on the active tab,
            // which already shows its accent backplate).
            if let Some(hz) = self.hovered_tab {
                if hz != TabZone::Tab(self.active) {
                    if let Some(&(_, x0, x1)) = gpu.tab_zones.iter().find(|&&(z, _, _)| z == hz) {
                        let f = to_rgba(fg);
                        chrome.push(ColorRect::new(
                            x0 as i32,
                            2,
                            (x1 - x0) as i32,
                            TITLEBAR_H as i32 - 4,
                            [f[0], f[1], f[2], 0.10],
                        ));
                    }
                }
            }
        }
        let prepared_chrome = gpu
            .chrome_renderer
            .prepare(&gpu.device, w as f32, h as f32, &chrome);
        // Drag overlay draws LAST (over text + images), so prepare it separately.
        let prepared_overlay =
            gpu.chrome_renderer
                .prepare(&gpu.device, w as f32, h as f32, &drag_overlay);

        // Prepare inline-image quads grouped by leaf, so each group can be
        // scissored to its cell — images cannot bleed across a pane border.
        let cell_of: std::collections::HashMap<LeafId, LRect> = cells.iter().copied().collect();
        let mut img_by_leaf: std::collections::HashMap<
            LeafId,
            Vec<crate::image_render::ImageQuad>,
        > = std::collections::HashMap::new();
        for (leaf, q) in image_quads {
            img_by_leaf.entry(leaf).or_default().push(q);
        }
        type ImageGroup = ((u32, u32, u32, u32), Vec<crate::image_render::Prepared>);
        let prepared_image_groups: Vec<ImageGroup> = img_by_leaf
            .into_iter()
            .filter_map(|(leaf, quads)| {
                let cell = *cell_of.get(&leaf)?;
                let scissor = leaf_scissor(
                    cell,
                    border,
                    gpu.surface_config.width,
                    gpu.surface_config.height,
                );
                let prepared =
                    gpu.image_renderer
                        .prepare(&gpu.device, &gpu.queue, w as f32, h as f32, &quads);
                Some((scissor, prepared))
            })
            .collect();

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("c0pl4nd"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("text"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(bg),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Pane chrome (gutter + borders) under the text.
            gpu.chrome_renderer.draw(&mut pass, &prepared_chrome);
            let _ = gpu
                .text_renderer
                .render(&gpu.atlas, &gpu.viewport, &mut pass);
            // Inline images, each scissored to its leaf's cell so an image
            // taller/wider than its pane cannot paint over a neighbour.
            for (scissor, prepared) in &prepared_image_groups {
                let (sx, sy, sw, sh) = *scissor;
                if sw > 0 && sh > 0 {
                    pass.set_scissor_rect(sx, sy, sw, sh);
                    gpu.image_renderer.draw(&mut pass, prepared);
                }
            }
            // Drag overlay on top of everything (reset the scissor first, as the
            // image loop may have left a per-cell scissor set).
            if !prepared_overlay.is_empty() {
                pass.set_scissor_rect(0, 0, gpu.surface_config.width, gpu.surface_config.height);
                gpu.chrome_renderer.draw(&mut pass, &prepared_overlay);
            }
        }
        gpu.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        gpu.atlas.trim();
    }
}

impl Gpu {
    async fn new(window: Arc<Window>, font_size: f32, want_transparent: bool) -> Result<Gpu> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("c0pl4nd-device"),
                ..Default::default()
            })
            .await?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer an sRGB format; else the surface's first reported format; else a
        // universal default. Audit UI-1: the old `caps.formats[0]` index panicked
        // if the surface reported zero formats — wgpu guarantees >=1 for a valid
        // surface/adapter pair (so this is unreachable), but a no-panic fallback
        // keeps a misbehaving backend from crashing the render init.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb);
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Prefer Mailbox (low-latency triple-buffer) over Fifo when
            // supported — saves up to ~1-2 frames of lag (research perf P0).
            present_mode: if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::Fifo
            },
            alpha_mode: choose_alpha_mode(want_transparent, &caps.alpha_modes),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let metrics = Metrics::new(font_size.max(8.0), LINE_HEIGHT.max(font_size + 2.0));
        let mut chrome_buffer = Buffer::new(&mut font_system, metrics);
        chrome_buffer.set_size(&mut font_system, Some(size.width as f32), Some(TITLEBAR_H));
        // One buffer per caption glyph (min/max/close), each sized to a single
        // backplate cell so the glyph can be centred within it (see render).
        let cap_cell_w = BUTTON_CELLS * CELL_W;
        let caption_buffers: [Buffer; 3] = std::array::from_fn(|_| {
            let mut b = Buffer::new(&mut font_system, metrics);
            b.set_size(&mut font_system, Some(cap_cell_w), Some(TITLEBAR_H));
            b
        });
        let mut palette_buffer = Buffer::new(&mut font_system, metrics);
        palette_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );
        let mut splash_buffer = Buffer::new(&mut font_system, metrics);
        splash_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );
        let image_renderer = crate::image_render::ImageRenderer::new(&device, format);
        let chrome_renderer = ChromeRenderer::new(&device, format);
        let gpu_name = adapter.get_info().name;

        Ok(Gpu {
            window,
            device,
            queue,
            surface,
            surface_config,
            font_system,
            swash_cache,
            atlas,
            viewport,
            text_renderer,
            metrics,
            leaf_buffers: HashMap::new(),
            tabbar_buffers: HashMap::new(),
            chrome_buffer,
            caption_buffers,
            palette_buffer,
            splash_buffer,
            image_renderer,
            chrome_renderer,
            gpu_name,
            tab_zones: Vec::new(),
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        // Per-leaf grid buffers are sized per-frame from the layout cascade in
        // `render`; only the fixed-height chrome buffer is resized here.
        self.chrome_buffer
            .set_size(&mut self.font_system, Some(width as f32), Some(TITLEBAR_H));
    }

    fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.surface_config);
    }
}

/// Whether `cell`'s nested-tab strip occupies a grid line for the cell drawn at
/// `rect`. The strip is laid out (stealing one terminal row) only when the cell
/// has >=2 tabs AND is tall enough ([`CELL_STRIP_MIN_H`]); on a short cell the
/// strip auto-hides and reappears as a render-only hover overlay
/// ([`App::hover_strip_leaf`]) without disturbing the grid geometry / PTY size.
fn cell_strip_visible(cell: &Cell, rect: LRect) -> bool {
    strip_laid_out(cell.tab_count(), rect)
}

/// Pure predicate for whether a tab strip is LAID OUT (steals a grid line) for a
/// cell with `tab_count` tabs drawn at `rect`. The render layer additionally
/// reveals a strip as a hover overlay on a short cell, but that path does not
/// change geometry, so PTY sizing keys off this stable predicate only.
fn strip_laid_out(tab_count: usize, rect: LRect) -> bool {
    tab_count > 1 && (rect.h - 2 * BORDER_PX) >= CELL_STRIP_MIN_H
}

/// The sub-rectangle of `target` a drop `zone` would fill, used for the drag
/// overlay highlight: an edge zone highlights that 1/3 band; the center
/// highlights the central region. Pure geometry.
fn zone_highlight_rect(target: LRect, zone: DropZone, rgba: [f32; 4]) -> ColorRect {
    let third_w = (target.w / 3).max(1);
    let third_h = (target.h / 3).max(1);
    let (x, y, w, h) = match zone {
        DropZone::Left => (target.x, target.y, third_w, target.h),
        DropZone::Right => (target.x + target.w - third_w, target.y, third_w, target.h),
        DropZone::Top => (target.x, target.y, target.w, third_h),
        DropZone::Bottom => (target.x, target.y + target.h - third_h, target.w, third_h),
        DropZone::Center => (
            target.x + third_w,
            target.y + third_h,
            target.w - 2 * third_w,
            target.h - 2 * third_h,
        ),
    };
    ColorRect::new(x, y, w.max(1), h.max(1), rgba)
}

/// Resolve a cell's foreground [`c0pl4nd_core::Color`] to an RGB triple.
/// Thin shim over the shared [`Theme::resolve_color`] so this winit shell and
/// the egui shell resolve colours through the SAME core path.
fn resolve_fg(
    color: c0pl4nd_core::Color,
    theme: &Theme,
    default_rgb: (u8, u8, u8),
) -> (u8, u8, u8) {
    theme.resolve_color(color, default_rgb)
}

/// Resolve a cell's effective `(foreground, Option<background>)` RGB, applying
/// SGR inverse/reverse video. The background is `None` when it should use the
/// window's default background (so the renderer can skip painting a quad for
/// the common case). For an inverse cell, the effective foreground is the
/// cell's background colour and vice-versa — matching every mainstream terminal
/// (selections, `\e[7m`, cursor-on-cell all rely on this).
#[allow(clippy::type_complexity)]
fn cell_render_colors(
    cell: &c0pl4nd_core::Cell,
    theme: &Theme,
    default_fg: (u8, u8, u8),
    default_bg: (u8, u8, u8),
) -> ((u8, u8, u8), Option<(u8, u8, u8)>) {
    // Delegate to the shared core helper so the inverse/reverse-video handling
    // is defined once and reused by the egui shell.
    theme.cell_colors(cell, default_fg, default_bg)
}

/// Reduce a user-supplied workspace name to a safe file stem.
///
/// Allowed: ASCII alphanumerics, `-`, `_`. Every other char (slash, dot, NUL,
/// control, Unicode) maps to `_`. Names longer than 64 chars truncate; an
/// empty name becomes `"default"`. The result contains no path separator and
/// no `..`, so `../../etc/passwd` is mapped to a flat stem with no path
/// escape possible. Workspace files are then stored as
/// `<workspaces-dir>/<stem>.layout.json` under the per-user config dir.
fn sanitize_workspace_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if s.is_empty() {
        "default".to_string()
    } else {
        s
    }
}

/// Render a boolean setting as a compact on/off label for the settings panel.
fn bool_label(b: bool) -> String {
    if b {
        "on".to_string()
    } else {
        "off".to_string()
    }
}

/// Approximate sRGB(0-255) → linear(0-1) for the wgpu clear color.
fn srgb_to_linear(c: u8) -> f64 {
    let s = c as f64 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Map a key press to the bytes to send to the PTY.
/// Encode a key press into the bytes a terminal program expects on the PTY.
///
/// Honours DECCKM (`app_cursor`): in application-cursor mode the arrow and
/// Home/End keys use SS3 (`ESC O x`) instead of CSI (`ESC [ x`), which is what
/// readline/vim expect. Encodes the function keys (F1–F12) and editing keys
/// (Home/End/Insert/Delete/PageUp/PageDown). Alt acts as Meta: an `Alt`-modified
/// text key is prefixed with `ESC` (the xterm convention behind Alt+B / Alt+F
/// word motion in bash).
fn key_to_bytes(
    key: &Key,
    text: &Option<winit::keyboard::SmolStr>,
    app_cursor: bool,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    use c0pl4nd_core::term::{encode_key, KeyModifiers, LogicalKey};
    // Map the winit key onto the engine-agnostic `LogicalKey`, then delegate to
    // the ONE canonical encoder in `c0pl4nd_core::term::keys` so the escape
    // sequences are not duplicated between this (winit) shell and the egui shell.
    let logical = match key {
        Key::Named(NamedKey::Enter) => LogicalKey::Enter,
        Key::Named(NamedKey::Backspace) => LogicalKey::Backspace,
        Key::Named(NamedKey::Tab) => LogicalKey::Tab,
        Key::Named(NamedKey::Escape) => LogicalKey::Escape,
        Key::Named(NamedKey::Space) => LogicalKey::Space,
        Key::Named(NamedKey::ArrowUp) => LogicalKey::ArrowUp,
        Key::Named(NamedKey::ArrowDown) => LogicalKey::ArrowDown,
        Key::Named(NamedKey::ArrowRight) => LogicalKey::ArrowRight,
        Key::Named(NamedKey::ArrowLeft) => LogicalKey::ArrowLeft,
        Key::Named(NamedKey::Home) => LogicalKey::Home,
        Key::Named(NamedKey::End) => LogicalKey::End,
        Key::Named(NamedKey::Insert) => LogicalKey::Insert,
        Key::Named(NamedKey::Delete) => LogicalKey::Delete,
        Key::Named(NamedKey::PageUp) => LogicalKey::PageUp,
        Key::Named(NamedKey::PageDown) => LogicalKey::PageDown,
        Key::Named(NamedKey::F1) => LogicalKey::Function(1),
        Key::Named(NamedKey::F2) => LogicalKey::Function(2),
        Key::Named(NamedKey::F3) => LogicalKey::Function(3),
        Key::Named(NamedKey::F4) => LogicalKey::Function(4),
        Key::Named(NamedKey::F5) => LogicalKey::Function(5),
        Key::Named(NamedKey::F6) => LogicalKey::Function(6),
        Key::Named(NamedKey::F7) => LogicalKey::Function(7),
        Key::Named(NamedKey::F8) => LogicalKey::Function(8),
        Key::Named(NamedKey::F9) => LogicalKey::Function(9),
        Key::Named(NamedKey::F10) => LogicalKey::Function(10),
        Key::Named(NamedKey::F11) => LogicalKey::Function(11),
        Key::Named(NamedKey::F12) => LogicalKey::Function(12),
        // Everything else: the composed text the platform delivered (printable
        // chars, IME output). `None` when there is no text → encodes nothing.
        _ => LogicalKey::Text(text.as_ref().map(|s| s.to_string()).unwrap_or_default()),
    };
    encode_key(
        &logical,
        app_cursor,
        KeyModifiers {
            ctrl: mods.control_key(),
            alt: mods.alt_key(),
            shift: mods.shift_key(),
            logo: mods.super_key(),
        },
    )
}

/// Load a theme file from the bundled themes directory (next to the binary or
/// in the source tree during development).
fn load_theme(name: &str) -> Option<Theme> {
    let mut candidates: Vec<std::path::PathBuf> =
        vec![std::path::PathBuf::from("assets/themes").join(format!("{name}.toml"))];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("assets/themes").join(format!("{name}.toml")));
        }
    }
    // Themes contributed by installed declarative plugins, matched by file stem.
    if let Some(dir) = c0pl4nd_core::plugin::default_plugins_dir() {
        let registry = c0pl4nd_core::plugin::PluginRegistry::load(&dir);
        for p in registry.contributed_theme_paths() {
            if p.file_stem().and_then(|s| s.to_str()) == Some(name) {
                candidates.push(p);
            }
        }
    }
    for c in candidates {
        if let Ok(t) = Theme::load_from(&c) {
            return Some(t);
        }
    }
    None
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
