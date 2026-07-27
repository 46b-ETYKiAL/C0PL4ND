//! Milestone 1 of the C0PL4ND egui chrome modernization (recon dossier
//! `.s4f3-data/recon-c0pl4nd-egui-modernization.md`, steps 1–4).
//!
//! This module is the modern `eframe`/`egui` application shell, shipped as a
//! SEPARATE binary (`c0pl4nd-egui`) so the existing winit-driven `c0pl4nd`
//! binary keeps building and shipping unchanged. The chrome (frameless
//! titlebar, two-tone wordmark, tab strip, caption buttons, status bar) and the
//! `egui_tiles` pane grid are real and clickable; each pane body hosts a live
//! PTY whose visible grid is drawn with egui's NATIVE coloured-text painter (see
//! [`paint_grid_native`]). An earlier milestone rendered the grid through a
//! glyphon GPU paint callback / offscreen texture, but that path composited
//! black inside `egui_tiles` panes on the real swapchain (while passing the wgpu
//! test harness); native text renders reliably everywhere and matches SCR1B3's
//! coloured-text approach, so the glyphon path was removed.
//!
//! eframe owns the event loop; no winit plumbing here.

pub mod bidi;
pub mod chrome;
pub mod chrome_toolbar;
mod crt;
pub mod fonts;
pub mod grid;
pub mod hyperlink;
pub mod job_object;
mod layout_state;
mod motion_fx;
pub mod pane_term;
mod search_ui;
pub mod settings;
/// Kick off the on-launch update CHECK on the shared in-app updater that drives
/// the notification banner + Settings → Updates page. Exposed for the binary
/// entry point (`egui_main`), which calls it exactly once at startup when the
/// persisted update mode opts in (`notify`/`auto`) and the interval throttle
/// says a check is due. Thin re-export of the settings-owned implementation so
/// the updater's private types never leak out of `egui_app`.
///
/// `#[allow(unused_imports)]`: this re-export exists for the `c0pl4nd` egui
/// BINARY (`egui_main`), which calls it once at startup. The `egui_kittest`
/// integration-test binaries `#[path]`-include this module but never launch the
/// on-launch check, so the re-export is (correctly) unused in those targets.
#[allow(unused_imports)]
pub use settings::start_launch_update_check;
pub mod shells;
mod theme;
pub(crate) use crt::*;
pub(crate) use motion_fx::*;
mod grid_interaction;
mod scrollbar;
mod taskbar;
pub(crate) use grid_interaction::*;
mod config_load;
pub(crate) use config_load::*;
mod window_effects;
pub(crate) use window_effects::*;
mod caption_close;
mod font_setup;
mod win_foreground;
pub(crate) use font_setup::*;
mod actions;
mod app_config;
mod app_report_ui;
mod app_search;

pub use actions::{Action, PaletteEntry};

use std::collections::{HashMap, HashSet};

use eframe::egui;

use c0pl4nd_core::term::ColorSet;
use grid::{count_panes, GridBehavior, Pane, PaneId, PaneIdAllocator};
use pane_term::{CellMetrics, ColorRun, PaneTerm};

mod glyph_cache;
pub(crate) use glyph_cache::*;

pub mod gpu_diag;

/// How many placeholder panes the shell opens with on first launch.
const INITIAL_PANES: usize = 1;

/// A window-level caption command issued by the titlebar buttons. Routed through
/// [`chrome::ChromeActions`] so [`C0pl4ndApp::frame_tick`] is the single site
/// that (a) issues the real `egui::ViewportCommand` to the OS and (b) records
/// the command in [`C0pl4ndApp::last_window_cmd`] so an interaction test can
/// assert that clicking the real button produced the real effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCmd {
    /// Minimize the window.
    Minimize,
    /// Toggle maximized/restored.
    ToggleMaximize,
    /// Close the window.
    Close,
}

/// The modern egui chrome application. Holds the tiling grid, the focused pane,
/// a settings-window toggle, and a transient status-bar toast.
pub struct C0pl4ndApp {
    /// Core config (loaded best-effort; defaults when absent). Kept so Milestone
    /// 2 can read font/cursor/keybinding settings without re-plumbing.
    ///
    /// `pub` (not `pub(crate)`) so the `tests/` suites can drive and observe it
    /// now that they link the lib target instead of `#[path]`-including a second
    /// copy of this module. This crate is an application lib — it is never
    /// published, and its only consumers are its own two binaries and its tests
    /// — so the wider visibility exposes nothing to a third party.
    pub config: c0pl4nd_core::Config,
    /// The active colour theme — glyph colours for the terminal grid come from
    /// here (NOT egui Visuals, which only style the chrome).
    pub theme: c0pl4nd_core::Theme,
    /// The tiling pane grid.
    pub grid_tree: egui_tiles::Tree<Pane>,
    /// Per-pane live terminal state (PTY + grid), keyed by pane id. A pane with
    /// no entry (or a failed spawn) renders an error/placeholder body.
    pub(crate) terms: HashMap<PaneId, PaneTerm>,
    /// Panes whose PTY is DEFERRED until their real pixel rect is known. The
    /// initial pane(s) are registered here at construction WITHOUT a PTY: if we
    /// spawned them at the 80×24 placeholder (the cmd-banner cursor-home bug
    /// #40) the first `resize_to_px` to the real width (e.g. a 200-col config)
    /// would reflow cmd's grid and snap its cursor back to (0,0), so typing
    /// overwrites the banner. Instead [`render_pane_body`] spawns each pending
    /// pane at the MEASURED `(cols, rows)` on the first frame its rect is known —
    /// exactly how a manually-opened terminal (`spawn_term`) already behaves —
    /// after which the debounced resize is a no-op and the cursor stays put.
    pub(crate) pending_spawn: HashSet<PaneId>,
    /// Working directories captured from a previous run's persisted layout
    /// snapshot, keyed by the restored pane id. Consumed (removed) by the
    /// deferred first-spawn in [`render_pane_body`]: a pane with an entry spawns
    /// its shell in that dir, a pane without one spawns in the default dir. Empty
    /// on a fresh launch and after every entry is consumed — so a pane the user
    /// later splits never inherits a stale restored cwd.
    pub(crate) restored_cwds: HashMap<PaneId, String>,
    /// Monotonic pane-id allocator.
    pub(crate) pane_alloc: PaneIdAllocator,
    /// The currently-focused pane (drives tab highlight + input routing).
    pub(crate) focused_pane: PaneId,
    /// Panes the user pinned: their tabs sort first and can't be closed via the
    /// tab × (must unpin first).
    pub(crate) pinned: HashSet<PaneId>,
    /// The focused pane's last-rendered size `(w, h)` in points. Drives the
    /// "+" button's split direction (split the longer axis to stay balanced).
    pub(crate) last_focused_size: Option<(f32, f32)>,
    /// Shells offered by the top-bar switcher, platform default first. Detected
    /// once at construction (`shells::detect_profiles`).
    pub(crate) shell_profiles: Vec<shells::ShellProfile>,
    /// Index into `shell_profiles` that the plain "+" button and new terminals
    /// use. Set when the user picks a shell from the top-bar ▾ menu.
    pub(crate) active_shell: usize,
    /// Whether the chrome fonts (incl. the `phosphor-fill` family used for a
    /// pinned tab's solid pin) have been installed on the egui context. Set in
    /// `new`; the first `frame_tick` installs them otherwise (e.g. headless
    /// tests built via `bootstrap()`), so referencing the `phosphor-fill` family
    /// can never hit an unregistered-family panic.
    pub(crate) fonts_installed: bool,
    /// The font-stack key (family + fallbacks folded into one string by
    /// [`font_apply_key`]) that was LAST installed into egui. Compared each frame
    /// against the live config so a Family/Fallback change in settings triggers a
    /// single live re-install of the font stack — and the (expensive) system-font
    /// load runs ONLY on an actual change, never per frame.
    pub(crate) applied_font_family: String,
    /// The UI scale (F2-3) currently applied to the egui context, tracked so
    /// `frame_tick` re-applies `set_zoom_factor` ONLY when the configured
    /// `ui_scale` actually changes (not every frame, and without fighting the
    /// transient Ctrl+/- keyboard zoom, which never writes `config.ui_scale`).
    /// Initialised to a sentinel `NaN` so the first frame always applies.
    pub(crate) applied_ui_scale: f32,
    /// Whether the settings window is open.
    pub(crate) settings_open: bool,
    /// The union bounding rect of whichever centered chrome panels (Settings
    /// window, command palette, multi-line-paste confirm) are open THIS frame,
    /// captured as each draws (before the whole-window motion-overlay block runs).
    /// The overlays paint AROUND this rect so a Motion setting previews live on the
    /// terminal WHILE the panel stays clean (no mesh/flicker washing over it).
    /// Reset to `None` each frame before the panels draw. `None` = no panel open.
    pub(crate) overlay_exclude_rect: Option<egui::Rect>,
    /// Recently-run commands, surfaced by the command palette for quick
    /// find/run. Captured best-effort from typed input (committed on Enter).
    pub(crate) cmd_history: c0pl4nd_core::command_history::CommandHistory,
    /// Accumulator for the line currently being typed in the focused pane.
    /// Committed to `cmd_history` on Enter, reset on focus change. Best-effort:
    /// it models printable text + Backspace, not full shell line-editing.
    pub(crate) input_line: String,
    /// A paste deferred for confirmation (paste-safety). Two gates park a paste
    /// here instead of executing it immediately — a MULTI-LINE paste (whose
    /// embedded newline would run a command the moment it lands) and an
    /// oversized SINGLE-line paste (`config.paste_warn_bytes`, the flood /
    /// hidden-tail half of the same footgun). The decision is
    /// [`c0pl4nd_core::paste_guard::paste_confirm_reason`] so both halves share
    /// one policy. Enter in the overlay sends it (through the paste-injection
    /// guard); Esc discards it.
    pub(crate) pending_paste: Option<String>,
    /// Which gate deferred [`Self::pending_paste`], so the confirm overlay can
    /// explain the actual hazard rather than always claiming "multiple lines".
    /// Set and cleared in lockstep with `pending_paste`.
    pub(crate) pending_paste_reason: Option<c0pl4nd_core::paste_guard::PasteConfirmReason>,
    /// Incognito session: when `true`, NO typed commands are recorded into
    /// command history (regardless of `config.history_capture_enabled`). Runtime
    /// only — never persisted, so it always starts off and resets each launch.
    pub(crate) incognito: bool,
    /// Whether the command palette overlay is open.
    pub(crate) palette_open: bool,
    /// Whether the command-history quick-run sidebar (`#21`) is open. A docked
    /// `egui::SidePanel` (side from `config.history_sidebar_side`) that lists the
    /// history newest-first with a filter box; clicking a row re-runs it in the
    /// focused pane via the SAME path as the command palette.
    pub(crate) history_open: bool,
    /// The history sidebar's filter query (substring/fuzzy over the history).
    pub(crate) history_filter: String,
    /// The palette's fuzzy-search query.
    pub(crate) palette_query: String,
    /// The palette's selected row (index into the filtered results).
    pub(crate) palette_sel: usize,
    /// The command most recently run FROM the palette (Enter or click). Set in
    /// [`Self::run_palette_selection`] so an interaction test can assert that
    /// driving the real palette ran the real command — the same observation
    /// pattern as [`Self::last_window_cmd`] (the PTY write itself is not
    /// observable in the headless harness).
    pub(crate) last_palette_run: Option<String>,
    /// The [`Action`] most recently dispatched FROM the command palette (Enter or
    /// click). Set in [`Self::run_palette_selection`] so an interaction test can
    /// assert that driving the REAL palette routed through the shared dispatch
    /// path — the action's own effect (pane count, `settings_open`, font size …)
    /// is asserted separately, so this is a routing witness, never the only
    /// evidence.
    pub(crate) last_palette_action: Option<Action>,
    /// The most recent URL a Ctrl-click opened (most-recent-wins), or `None` if
    /// none this session. Observable so an interaction test can assert that a
    /// Ctrl-click on a URL in the grid opened it — the OS-opener side effect
    /// (`ctx.open_url`) itself is not observable in the headless harness.
    pub(crate) last_opened_url: Option<String>,
    /// Whether the in-terminal find overlay is open.
    pub(crate) search_open: bool,
    /// The find overlay's search query.
    pub(crate) search_query: String,
    /// Whether the find query is treated as a regular expression.
    pub(crate) search_regex: bool,
    /// Whether find matching is case-SENSITIVE (the core option speaks
    /// `case_insensitive`, so this is its inverse — the UI label is "Case").
    pub(crate) search_case_sensitive: bool,
    /// The matches found this frame for `search_query` over the focused pane's
    /// grid text, recomputed by [`Self::recompute_search`] whenever the query or
    /// a toggle changes (and once on open). Kept on `self` so the cycle keys
    /// (Enter / F3 / Shift+F3) and the highlight pass both read the same set.
    pub(crate) search_matches: Vec<c0pl4nd_core::search::SearchMatch>,
    /// Index of the currently-selected match in `search_matches` (0-based).
    /// Meaningful only when `search_matches` is non-empty.
    pub(crate) search_sel: usize,
    /// TEST-ONLY corpus override for the find overlay. When `Some`, the matcher
    /// searches these lines instead of the live PTY grid. The live PTY's
    /// `grid_text()` is async + platform-dependent (a CI box may have no usable
    /// shell), so the headless find tests seed a KNOWN corpus here to assert the
    /// search wiring deterministically. `None` in the shipping binary — the real
    /// focused-pane grid text is searched. Set via `test_seed_focused_grid`.
    pub(crate) search_test_corpus: Option<String>,
    /// A transient status-bar message (e.g. "max 6 panes").
    pub(crate) toast: Option<String>,
    /// The `(font-family-key, size-bits, pixels-per-point-bits)` the grid glyph
    /// atlas was last PRE-WARMED for. When this differs from the live font stack
    /// (first frame, a system-font swap, a zoom, OR a DPI/`pixels_per_point`
    /// change — the last is why `ppp` is in the key: egui rasterises glyphs at
    /// `size × ppp`, so a 1.0→1.5 DPI settle re-rasterises the whole set), the
    /// atlas is re-warmed. Warming rasterises every glyph the grid draws up-front
    /// so the atlas reaches its FINAL size in one step, never growing mid-render —
    /// the growth that feeds the DX12 upload↔sample hazard (garbled/blank grid
    /// glyphs). `None` == never warmed yet.
    pub(crate) warmed_atlas: Option<(String, u32, u32)>,
    /// Frames remaining in the atlas WARMUP GATE. While > 0 the grid draws NO
    /// glyphs (empty panes) and `ui` blocks on `device.poll(Wait)` so the warmed
    /// atlas upload is guaranteed RESIDENT on the GPU before any glyph is sampled —
    /// the windowed-path equivalent of the offscreen render's implicit queue
    /// drain, which is why the offscreen path never garbles. Re-armed to a small
    /// count whenever the atlas is re-warmed. Startup/rare-only; zero steady-state
    /// cost (the grid content — the shell banner — has not arrived yet anyway).
    pub(crate) warmup_frames_left: u8,
    /// Frames elapsed while waiting for the off-thread custom font to swap in. Caps
    /// the font-load warmup gate (see `FONT_WAIT_GATE_CAP`) so a failed/slow font
    /// load can never hide the grid indefinitely.
    pub(crate) font_wait_frames: u32,
    /// Debounced font-size persistence deadline (egui `input.time`, seconds).
    /// Live Ctrl+wheel / Ctrl+/- zoom changes `config.font.size` every notch and
    /// applies it in-memory immediately, but writing the whole config file per
    /// notch (atomic temp-write + rename + perms) is wasteful under a fast scroll.
    /// Instead each zoom sets this to `now + debounce`; `frame_tick` flushes ONE
    /// save once the deadline passes with no further change. `None` == nothing
    /// pending. Shutdown also saves, so a pending zoom is never lost on close.
    pub(crate) pending_font_save_at: Option<f64>,
    /// Receiver for an opt-in launch update check spawned by the binary entry
    /// point (`egui_main`). The background thread sends a one-line "newer
    /// version available" notice exactly once; `frame_tick` polls this and
    /// surfaces it as a toast. `None` in the headless harness (tests never attach
    /// a check), so no network ever runs under test.
    pub(crate) update_rx: Option<std::sync::mpsc::Receiver<String>>,
    /// The most recent update notice surfaced (most-recent-wins), observable so
    /// an interaction test can assert the launch-check → toast wiring without a
    /// network call.
    pub(crate) last_update_notice: Option<String>,
    /// The most recent caption command issued (minimize/maximize/close). Set in
    /// [`Self::frame_tick`] alongside the real `ViewportCommand`, so interaction
    /// tests can assert that clicking a caption button had its real effect (the
    /// OS command itself is not observable in a headless harness).
    pub(crate) last_window_cmd: Option<WindowCmd>,
    /// Last known UN-maximized inner size (logical points). Updated every frame
    /// the window is not maximized, and used to drive an EXPLICIT restore size
    /// when the user un-maximizes: eframe's persisted window state can leave
    /// winit's own restore geometry equal to the maximized (monitor) size, so a
    /// plain un-maximize "restores" to a full-monitor window the user must then
    /// shrink by hand. `None` until the first un-maximized frame — the restore
    /// then falls back to the first-run default size.
    pub(crate) restore_size: Option<egui::Vec2>,
    /// Fading echoes of the focused terminal cursor's recent cell rects (screen
    /// coords) + their birth-times, feeding the optional cursor ghost-trail
    /// motion overlay ([`paint_cursor_trail`]). Bounded to a few dozen entries;
    /// pruned each frame once an echo outlives its fade. Empty (and unused) when
    /// the `cursor_trail` effect is off — never persisted.
    pub(crate) cursor_trail: std::collections::VecDeque<(egui::Rect, f64)>,
    /// The egui clock time (seconds) of the first rendered frame, captured once
    /// so the one-shot boot-glitch overlay measures its sweep from the first
    /// frame the user actually sees — not from context creation (which may
    /// predate the window by the atlas-warmup cost, hiding the sweep entirely).
    pub(crate) first_frame_time: Option<f64>,
    /// One-shot latch for the first-launch foreground raise. The window can open
    /// BEHIND other windows on Windows 11 (foreground-lock ignores the polite
    /// `with_active`/`Focus` request), so on the FIRST rendered frame of a real
    /// window we send `ViewportCommand::Focus` and run the `win_foreground`
    /// AttachThreadInput backstop — then set this so it NEVER runs again (raising
    /// on later frames would steal focus back from an app the user switched to).
    pub(crate) foreground_done: bool,
    /// The OS dark/light appearance observed on the previous `follow_os_theme_tick`
    /// (resolved, unknown → dark). `follow_os_theme_tick` re-applies the OS-derived
    /// theme ONLY when the live `ctx.system_theme()` differs from this — so a
    /// MANUAL theme pick sticks between OS-appearance changes (SCR1B3 parity).
    /// `None` when follow-OS is off / never observed. Never persisted.
    pub(crate) last_os_theme: Option<egui::Theme>,
    /// True on the first frame the Settings window opens (a closed→open edge),
    /// so `settings::show` FORCES the window to its saved-or-centered position
    /// that frame instead of trusting egui's `default_pos` (which read a
    /// not-yet-sized viewport on the open frame and parked the window top-left).
    /// Consumed (reset) after one frame so the window is freely movable after.
    pub(crate) settings_place_pending: bool,
    /// Previous frame's `settings_open`, used to detect the open edge above.
    pub(crate) settings_was_open: bool,
    /// True when running in a real eframe window (a wgpu render state exists),
    /// false in the headless `egui_kittest` harness. Drives the per-frame
    /// `request_repaint` pump so live PTY output animates without an input
    /// event — but NOT in headless tests, where an unconditional repaint would
    /// make `Harness::run` loop until `max_steps`.
    pub(crate) live_window: bool,
    /// Frameless terminal-only fullscreen (#36), toggled by F11 (and exited by
    /// F11 or Esc). TRANSIENT — never persisted to `Config`: F11 is a per-session
    /// view toggle, not a saved preference, so a relaunch is always windowed.
    /// While true, the titlebar + status panels (and the frameless resize bands)
    /// are not rendered, so only the grid fills the screen. The local mirror is
    /// the source of truth the panels read THIS frame (the OS-reported
    /// `i.viewport().fullscreen` lags a frame, which would flash the titlebar);
    /// it is reconciled from the OS value each frame to stay honest.
    pub(crate) fullscreen: bool,
    /// Whether the OS window held focus on the previous frame. Drives DEC
    /// `?1004` focus reporting: on a focus-in/out EDGE the focused pane's
    /// terminal is told (so vim/tmux see FocusGained/FocusLost). Initialised
    /// `true` so a window that starts focused does not emit a spurious report.
    pub(crate) was_focused: bool,
    /// The active mouse text selection over a pane's grid (None when nothing is
    /// selected). Drag selects; release copies (when `copy_on_select`); a plain
    /// click clears it. Ctrl/Cmd+Shift+C copies the live selection on demand.
    pub(crate) selection: Option<Selection>,
    /// When `Some`, render ONLY this pane full-size (siblings hidden) — the
    /// zoom-pane toggle (Ctrl/Cmd+Shift+Z). The grid tree is NOT mutated, so
    /// un-zooming restores the exact prior layout. Runtime-only (not persisted);
    /// cleared if the zoomed pane is closed.
    pub(crate) zoomed_pane: Option<PaneId>,
    /// Each pane's screen-space body rect, captured every frame during the grid
    /// render. Consumed by directional pane focus (Ctrl/Cmd+Shift+Arrow) to find
    /// the geometric neighbour in a direction. Rebuilt each frame, so it tracks
    /// the live layout (empty before the first render).
    pub(crate) pane_rects: HashMap<PaneId, egui::Rect>,
    /// The bytes `forward_input_to_focused` sent to the focused PTY on the most
    /// recent no-overlay frame. Kept so a test can assert that a consumed chord
    /// (e.g. Ctrl+Shift+D) leaked NOTHING to the shell — a regression where a
    /// chord's `events.retain` keeps the event would fire the action AND forward
    /// the control byte, which no action-only assertion would catch.
    #[allow(dead_code)]
    pub(crate) last_forwarded: Vec<u8>,
    /// Per-(pane,row) laid-out galley cache for [`paint_grid_native`] (audit #2).
    /// A row's galley is re-laid-out only when its content/style key changes, so
    /// an idle or partially-changed grid does not re-run text layout for every
    /// row every frame. Invalidated implicitly by the key (which folds font size,
    /// default fg, and the chromatic ghost params); cleared wholesale on a font
    /// re-install (family/fallback change). Bounded by per-pane row pruning.
    pub(crate) galley_cache: GalleyCache,
    /// GPU-texture cache for inline images (Sixel / Kitty graphics), pruned each
    /// frame so textures for off-screen images are released.
    pub(crate) image_textures: ImageTextureCache,
    /// Receiver for the off-thread system-font load (audit #3). When the default
    /// (or any custom) font config names a non-built-in family,
    /// `load_system_fonts()` (100s of ms) would block first paint; instead the
    /// first frame paints with the built-in mono and a worker thread enumerates
    /// the system font DB, sending the finished `FontDefinitions` here. `frame_tick`
    /// polls this and applies them via `set_fonts` when ready. `None` once applied
    /// (or when no system load is needed). Skipped entirely in the headless
    /// harness (no `live_window`), which keeps the synchronous path for
    /// deterministic tests.
    pub(crate) pending_fonts: Option<std::sync::mpsc::Receiver<egui::FontDefinitions>>,
    /// The in-progress IME pre-edit (composition) string for the focused pane,
    /// or `None` when no composition is active (F3-1). egui routes composed CJK /
    /// complex-script input through `Event::Ime` — the not-yet-committed
    /// candidate text arrives as `ImeEvent::Preedit` and is BUFFERED here for
    /// display only; it is NEVER sent to the PTY (only `ImeEvent::Commit` text
    /// reaches the shell). Painted underlined at the cursor by
    /// [`Self::render_pane_body`] so the user sees what they are composing before
    /// commit. Cleared on `ImeEvent::Enabled` / `Disabled` and on commit.
    pub(crate) ime_preedit: Option<String>,
    /// W1TN3SS per-launch crash-consent dialog state (opt-in, default-OFF). On
    /// launch [`Self::drain_crash_spool`] loads any spooled crash reports here
    /// when the crash stream's mode is `AskEachTime`; the dialog presents them
    /// one at a time with an editable preview + equal-weight Send / Don't-send.
    /// Empty (and touches no real config dir) when the user has not opted in.
    pub(crate) crash_consent: crate::reporting::CrashConsentState,
    /// W1TN3SS manual "Report an issue" dialog state (user-initiated, default
    /// CLOSED, diagnostics OFF). Opened from the titlebar script menu; builds a
    /// prefilled GitHub Issue-Form deep link (or clipboard / mailto fallback).
    pub(crate) issue_intake: crate::issue_intake::IssueIntakeState,
}

/// The PTY grid size used to spawn a pane before its real pixel rect is known.
/// The first `resize_to_px` corrects it to fit the allocated rect.
const SPAWN_COLS: u16 = 80;
/// See [`SPAWN_COLS`].
const SPAWN_ROWS: u16 = 24;

/// The `config.font.line_height` value (PIXELS) that maps to a row-pitch
/// multiplier of exactly `1.0` — i.e. "natural" spacing (the rendered glyph's
/// own galley height). The field is an absolute pixel line-height (default
/// 20.0; settings slider 12..=48 px); this anchor turns it into a multiplier
/// RELATIVE to the default so the default config reproduces the natural pitch
/// (`20.0 / 20.0 == 1.0`) and raising the slider opens the rows up
/// proportionally, lowering it tightens them — without breaking the existing
/// absolute-px config field or its settings slider.
const LINE_HEIGHT_ANCHOR_PX: f32 = 20.0;

/// Convert the configured `config.font.line_height` (absolute px, default 20.0)
/// into a row-pitch MULTIPLIER relative to the natural galley height. Pure +
/// GPU-free so the pitch wiring is unit-testable.
///
/// * `line_height_px == LINE_HEIGHT_ANCHOR_PX` (the 20.0 default) → `1.0`
///   (natural spacing).
/// * A larger configured line-height → a multiplier `> 1.0` (looser rows).
/// * A smaller one → a multiplier `< 1.0` (tighter rows).
///
/// Clamped to a sane `0.5..=4.0` band so a corrupt config can neither collapse
/// rows onto each other nor scatter them across the pane.
fn line_height_multiplier(line_height_px: f32) -> f32 {
    if !line_height_px.is_finite() || line_height_px <= 0.0 {
        return 1.0;
    }
    (line_height_px / LINE_HEIGHT_ANCHOR_PX).clamp(0.5, 4.0)
}

/// The effective terminal ROW PITCH (vertical advance per grid row) given the
/// natural per-row galley height `natural_line_h` and the configured
/// `line_height_px`. This single helper is the source of truth shared by the
/// glyph painter, the cursor, the search highlight, the hyperlink hit-test, and
/// the PTY `(cols, rows)` resize math, so every Y position stays aligned to the
/// SAME pitch (the bug class where the cursor drifts off the text when the row
/// pitch changes). Pure + GPU-free → unit-testable without an egui frame.
fn effective_row_pitch(natural_line_h: f32, line_height_px: f32) -> f32 {
    (natural_line_h * line_height_multiplier(line_height_px)).max(1.0)
}

impl C0pl4ndApp {
    /// Build the app inside eframe, applying the brand Visuals + window effect,
    /// and computing the terminal cell metrics from egui's monospace font (the
    /// font the grid is actually drawn with). Marks the app as a live window so
    /// the per-frame repaint pump runs.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Load persisted settings from disk so a user's saved theme / opacity /
        // font / cursor / update prefs take effect across launches. The headless
        // `bootstrap()` path keeps `Config::default()` for deterministic tests.
        // F5-2: load config AND capture any parse error, so a broken config file
        // surfaces as a visible toast instead of the silent fallback-to-defaults
        // that previously only `eprintln`'d (invisible to a GUI-launched user).
        let (cfg, config_error) = load_config_with_status();
        let mut app = Self::bootstrap_with(cfg);
        if let Some(err) = config_error {
            app.toast = Some(err);
        }
        // Restore the persisted split-pane layout + per-pane cwd from a previous
        // run (eframe `persistence` storage). A missing, unreadable, or
        // structurally-invalid snapshot is silently ignored — the default grid
        // built by `bootstrap_with` stands. Never a panic: a corrupt blob must not
        // brick launch. The headless `bootstrap()` path has no `cc`, so tests keep
        // the deterministic default grid.
        if let Some(storage) = cc.storage {
            if let Some(snapshot) = eframe::get_value::<layout_state::LayoutSnapshot>(
                storage,
                layout_state::LAYOUT_STORAGE_KEY,
            ) {
                app.apply_layout_snapshot(snapshot);
            }
        }
        // F5-3: first-run affordance. A fresh install has no config file yet —
        // and "zero-config is a first-class goal", so we deliberately do NOT
        // write one (that would defeat it). Surface a one-time welcome toast
        // pointing at Settings + the docs; it naturally stops once the user saves
        // any setting (which is what first writes the config file). Skipped when a
        // config-parse error already claimed the toast.
        if app.toast.is_none() && c0pl4nd_core::Config::default_path().is_some_and(|p| !p.exists())
        {
            app.toast = Some(
                "Welcome to C0PL4ND — open Settings (the gear) to customise; \
                 see TROUBLESHOOTING.md if anything looks off."
                    .to_string(),
            );
        }
        // Install the chrome icon fonts so the very first frame renders. When the
        // configured monospace family is a built-in choice this is the complete
        // install. When it names a SYSTEM family (the default config does —
        // "Monaspace Neon" / "Noto Sans JP"), the (100s-of-ms) system-font DB
        // load would block first paint, so instead we install the built-in base
        // immediately and enumerate the system DB on a worker thread (audit #3);
        // `frame_tick` swaps in the custom stack via `set_fonts` when it arrives.
        // Done after the config load so `app.config.font` is the source of truth.
        if system_font_load_needed(&app.config.font) {
            // Pass the UI font so the FIRST frame already paints the app UI in the
            // configured proportional font (default IBM Plex Mono) while the custom
            // terminal/monospace stack loads on the worker thread.
            install_base_fonts(&cc.egui_ctx, &app.config.font.ui_family);
            app.pending_fonts = Some(spawn_system_font_load(&app.config.font));
        } else {
            install_chrome_fonts(&cc.egui_ctx, &app.config.font);
        }
        app.applied_font_family = font_apply_key(&app.config.font);
        // The window is ALWAYS created transparent-capable (`with_transparent`) and
        // the single `opacity` slider drives the see-through level (v0.4.21). There
        // is no OS blur backdrop (acrylic / mica / vibrancy) and no uniform-dim
        // layered-window path anymore — those never composited on the hybrid-GPU
        // target — so the crash-loop recovery guard they needed is gone too. The
        // portable per-pixel transparent surface is the only effect.
        // The residual native MIN/MAX caption buttons winit leaves on the
        // undecorated window (winit #2754) are suppressed at WINDOW CREATION via
        // `ViewportBuilder::with_minimize_button(false)`/`with_maximize_button(false)`
        // (egui_main.rs). The native CLOSE button has no creation-time flag
        // (WS_SYSMENU is always in winit's base style), so prime the close-button
        // stripper with the real window handle; `ensure_close_button_stripped`
        // (run each frame in `ui`) clears ONLY WS_SYSMENU — leaving WS_CAPTION
        // intact so the frameless composition is never disturbed. Alt+F4 is
        // restored in-app (see `frame_tick`).
        #[cfg(windows)]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = cc.window_handle() {
                if let RawWindowHandle::Win32(w) = handle.as_raw() {
                    caption_close::set_main_hwnd(w.hwnd.get());
                    // Prime the first-launch foreground raise with the SAME main
                    // window handle; `frame_tick` fires it once on frame 1.
                    win_foreground::set_main_hwnd(w.hwnd.get());
                    // Prime the taskbar-progress consumer (OSC 9;4) with the SAME
                    // handle so `ITaskbarList3` drives THIS window's button.
                    taskbar::set_main_hwnd(w.hwnd.get());
                }
            }
        }
        // Apply Visuals DERIVED FROM the loaded terminal theme so the whole
        // chrome follows the active theme from the first frame (a light theme →
        // light UI, a dark theme → dark UI). Then fold the window `opacity` into
        // the resting chrome + background fills so the shell is see-through at low
        // opacity (only glyph text stays over the desktop at opacity 0). Done after
        // `bootstrap()` so `app.theme`/`app.config` are loaded; re-applied on every
        // theme/opacity change (see `settings_window` / `follow_os_theme_tick`).
        let mut visuals = theme::visuals_from_theme(&app.theme);
        window_effects::apply_window_opacity(&mut visuals, app.config.opacity);
        cc.egui_ctx.set_visuals(visuals);
        app.fonts_installed = true; // already installed above; skip the frame-tick install
                                    // A wgpu render state means a real window (also true under the wgpu test
                                    // harness, which drives frames explicitly with `step()`); headless tests
                                    // built via `bootstrap()` leave this false.
        app.live_window = cc.wgpu_render_state.is_some();
        // Cross-check instrumentation (pairs with the adapter-selector log written
        // during GPU init): record the adapter eframe ACTUALLY bound plus the
        // resolved opacity + clear-color alpha, so `gpu-diag.log` shows both the
        // per-adapter surface capabilities AND the final swapchain-facing pick.
        // This is the file the user hands back to diagnose "opaque black" (the
        // release binary is a GUI subsystem app, so stderr/tracing is lost). The
        // window is always transparent-capable now, so this always logs.
        if let Some(rs) = &cc.wgpu_render_state {
            let info = rs.adapter.get_info();
            let clear_alpha = window_clear_color()[3];
            gpu_diag::log_line(&format!(
                "RenderState bound: name='{}' type={} backend={:?} | \
                 opacity={:.2} with_transparent=true clear_alpha={:.3} pane_bg_alpha={}",
                info.name,
                gpu_diag::device_type_name(info.device_type),
                info.backend,
                app.config.opacity,
                clear_alpha,
                pane_bg_alpha(&app.config),
            ));
        }
        // W1TN3SS: drain the local crash-report spool per the user's opt-in
        // posture (production-only — never from `bootstrap`, so a unit test that
        // builds the app never reads/writes the real config dir's spool). A user
        // who has not opted in has an empty spool and this is a no-op.
        app.drain_crash_spool();
        app
    }

    /// Drain the local W1TN3SS crash-report spool per the user's opt-in posture.
    /// PRODUCTION-only (called from [`C0pl4ndApp::new`], never from `bootstrap`),
    /// so a unit test that builds the app never reads/writes the real config
    /// dir's spool. The spool is rooted at the per-user `config_dir`.
    ///
    /// Capture only ever spools when the user opted IN, so an `Off` user has an
    /// empty spool and nothing happens. `Always` auto-sends through the
    /// consent-gated path with no prompt; `AskEachTime` queues the consent dialog
    /// (rendered each frame). A `None` config dir means nowhere to spool — a no-op.
    fn drain_crash_spool(&mut self) {
        let Some(dir) = c0pl4nd_core::Config::config_dir() else {
            return;
        };
        match self.config.reporting.streams.crash_reports {
            crate::reporting::ReportingMode::Always => {
                crate::reporting::auto_send_spooled_crashes(&dir);
            }
            crate::reporting::ReportingMode::AskEachTime => {
                self.crash_consent.set_config_dir(Some(dir));
                self.crash_consent.load_from_spool();
            }
            crate::reporting::ReportingMode::Off => {}
        }
    }

    /// Construct the app state independent of eframe — used by `new` and by the
    /// headless `egui_kittest` tests (which run without a window). The initial
    /// pane(s) are registered in `pending_spawn` WITHOUT a PTY; each is spawned
    /// at its MEASURED size on the first frame its rect is known (bug #40), so
    /// the first pane behaves like a manually-opened one. A failed spawn degrades
    /// to an error label, never a panic.
    /// Default-config constructor used by the headless `egui_kittest` test
    /// binaries (the real app uses [`Self::new`], which loads the persisted
    /// config). `#[allow(dead_code)]` because it is unused in the shipping
    /// `c0pl4nd` binary itself — only the `#[path]`-including test bins call it.
    #[allow(dead_code)]
    pub fn bootstrap() -> Self {
        Self::bootstrap_with(c0pl4nd_core::Config::default())
    }

    /// Construct the app state from an EXPLICIT config — the shared body of
    /// [`Self::bootstrap`] (which passes `Config::default()`, used by the
    /// headless tests) and [`Self::new`] (which passes the config loaded from
    /// disk so persisted settings take effect across launches).
    pub fn bootstrap_with(config: c0pl4nd_core::Config) -> Self {
        let (theme, theme_notice) = load_terminal_theme(&config);
        let mut pane_alloc = PaneIdAllocator::default();
        let initial: Vec<PaneId> = (0..INITIAL_PANES).map(|_| pane_alloc.alloc()).collect();
        let focused_pane = initial[0];
        let grid_tree = grid::build_default_grid(&initial);
        // DEFER the initial pane PTYs: register them as pending and let
        // `render_pane_body` spawn each at the MEASURED `(cols, rows)` on the
        // first frame its rect is known (bug #40). Spawning here at the 80×24
        // placeholder is exactly what desynced cmd's cursor when the first
        // `resize_to_px` reflowed it to the real (e.g. 200-col) width.
        let terms: HashMap<PaneId, PaneTerm> = HashMap::new();
        let pending_spawn: HashSet<PaneId> = initial.iter().copied().collect();
        Self {
            config,
            theme,
            grid_tree,
            terms,
            pending_spawn,
            restored_cwds: HashMap::new(),
            pane_alloc,
            focused_pane,
            pinned: HashSet::new(),
            last_focused_size: None,
            shell_profiles: shells::detect_profiles(),
            active_shell: 0,
            fonts_installed: false,
            applied_font_family: String::new(),
            applied_ui_scale: f32::NAN,
            settings_open: false,
            overlay_exclude_rect: None,
            cmd_history: c0pl4nd_core::command_history::CommandHistory::default(),
            input_line: String::new(),
            pending_paste: None,
            pending_paste_reason: None,
            incognito: false,
            palette_open: false,
            history_open: false,
            history_filter: String::new(),
            palette_query: String::new(),
            palette_sel: 0,
            last_palette_run: None,
            last_palette_action: None,
            last_opened_url: None,
            search_open: false,
            search_query: String::new(),
            search_regex: false,
            search_case_sensitive: false,
            search_matches: Vec::new(),
            search_sel: 0,
            search_test_corpus: None,
            toast: theme_notice,
            warmed_atlas: None,
            warmup_frames_left: 0,
            font_wait_frames: 0,
            pending_font_save_at: None,
            update_rx: None,
            last_update_notice: None,
            last_window_cmd: None,
            restore_size: None,
            cursor_trail: std::collections::VecDeque::new(),
            first_frame_time: None,
            foreground_done: false,
            last_os_theme: None,
            settings_place_pending: false,
            settings_was_open: false,
            live_window: false,
            fullscreen: false,
            was_focused: true,
            selection: None,
            zoomed_pane: None,
            pane_rects: HashMap::new(),
            last_forwarded: Vec::new(),
            galley_cache: GalleyCache::default(),
            image_textures: ImageTextureCache::default(),
            pending_fonts: None,
            ime_preedit: None,
            crash_consent: crate::reporting::CrashConsentState::default(),
            issue_intake: crate::issue_intake::IssueIntakeState::default(),
        }
    }

    /// Spawn a fresh live terminal for `pid` running the active shell profile,
    /// and register it. Used by `split`. The default profile (program `None`,
    /// index 0) uses the platform default shell; a named profile launches its
    /// explicit program + args. A failed spawn degrades to an error pane.
    /// Replace the default grid with a restored layout snapshot, IF it is
    /// structurally usable. The panes are registered as DEFERRED (`pending_spawn`)
    /// exactly like the default initial pane, so each spawns at its MEASURED size
    /// on the first frame its rect is known (bug #40) — and consults
    /// `restored_cwds` so it opens in its saved working directory. An out-of-range
    /// pane count (empty or over the cap) leaves the default grid untouched. The
    /// allocator resumes past every restored id so a fresh split can never collide
    /// with a restored pane.
    fn apply_layout_snapshot(&mut self, snapshot: layout_state::LayoutSnapshot) {
        let panes = grid::panes_in_visual_order(&snapshot.tree);
        if !layout_state::snapshot_is_restorable(panes.len()) {
            return;
        }
        self.grid_tree = snapshot.tree;
        // The default grid's panes were deferred (never spawned), but clear any
        // live terms defensively so a restore can never leak a stale pane.
        self.terms.clear();
        self.pending_spawn = panes.iter().copied().collect();
        self.restored_cwds = snapshot
            .cwds
            .into_iter()
            .filter(|(pid, _)| panes.contains(pid))
            .collect();
        self.focused_pane = layout_state::restored_focus(&panes, snapshot.focused);
        self.pinned = snapshot
            .pinned
            .into_iter()
            .filter(|pid| panes.contains(pid))
            .collect();
        self.pane_alloc =
            PaneIdAllocator::seeded(layout_state::restored_next_id(&panes, snapshot.next_id));
    }

    /// Capture the current layout into a snapshot for persistence: the tiling
    /// tree, each live pane's reported cwd (OSC 7), the focused pane, the pinned
    /// set, and the allocator's next id. Panes that never reported a cwd simply
    /// have no entry (they re-spawn in the default dir on restore).
    fn capture_layout(&self) -> layout_state::LayoutSnapshot {
        let mut cwds = HashMap::new();
        for pid in grid::panes_in_visual_order(&self.grid_tree) {
            if let Some(cwd) = self.terms.get(&pid).and_then(PaneTerm::cwd) {
                cwds.insert(pid, cwd);
            }
        }
        layout_state::LayoutSnapshot {
            tree: self.grid_tree.clone(),
            cwds,
            focused: self.focused_pane,
            pinned: self.pinned.iter().copied().collect(),
            next_id: self.pane_alloc.peek_next(),
        }
    }

    fn spawn_term(&mut self, pid: PaneId) {
        let theme = self.theme.clone();
        let profile = self.shell_profiles.get(self.active_shell);
        let term = match profile.and_then(|p| p.program.clone()) {
            Some(program) => {
                let args: Vec<String> = profile.map(|p| p.args.clone()).unwrap_or_default();
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                PaneTerm::spawn_program(theme, &program, &arg_refs, SPAWN_COLS, SPAWN_ROWS)
            }
            None => PaneTerm::spawn_with_term(
                theme,
                SPAWN_COLS,
                SPAWN_ROWS,
                Some(self.config.term.as_str()),
            ),
        };
        self.terms.insert(pid, term);
    }

    // ---- public observation surface (production accessors, NOT test-only) ----
    //
    // These are real accessors that the `egui_kittest` interaction tests use to
    // assert observable outcomes after driving the REAL `frame_tick`. They are
    // deliberately not `#[cfg(test)]` so the test exercises the exact production
    // path (no test-only mirror that could drift from the real frame loop — that
    // drift is how "clicking does nothing" ships). `allow(dead_code)` because the
    // shipping binary does not yet call every accessor (the test crate, compiled
    // separately via `#[path]`, is the current consumer); they are a deliberate
    // public observation API, not dead code.
    #[allow(dead_code)]
    /// Number of open panes in the grid.
    pub fn pane_count(&self) -> usize {
        count_panes(&self.grid_tree)
    }

    /// Whether the settings window is currently open.
    #[allow(dead_code)]
    pub fn settings_is_open(&self) -> bool {
        self.settings_open
    }

    /// The current pane shell layout (`Grid` or `Tabs`) from the live config —
    /// the value the titlebar view-toggle button flips and that `grid_ui` reads
    /// each frame to decide whether to render the egui_tiles tree or a single
    /// full-size pane. Observation accessor for the view-toggle interaction test.
    #[allow(dead_code)]
    pub fn view_mode(&self) -> c0pl4nd_core::config::ViewMode {
        self.config.view_mode
    }

    /// Number of live terminal sessions currently held. Observation accessor for
    /// the fast-shutdown test: after [`prepare_shutdown`](Self::prepare_shutdown)
    /// this MUST be zero (every `PaneTerm` dropped → every PTY child killed, no
    /// orphans).
    #[allow(dead_code)]
    pub fn term_count(&self) -> usize {
        self.terms.len()
    }

    /// The currently-focused pane id.
    #[allow(dead_code)]
    pub fn focused_pane(&self) -> PaneId {
        self.focused_pane
    }

    /// Whether `pane_id` is currently pinned (tab sorts first, × hidden).
    #[allow(dead_code)]
    pub fn is_pinned(&self, pane_id: PaneId) -> bool {
        self.pinned.contains(&pane_id)
    }

    /// The pane's UNIQUE accessible tab label — the same string
    /// [`chrome`](super::chrome) sets as the tab's accessible name AND the base
    /// of its `pin`/`close` button labels, so an interaction test can look up a
    /// tab by `get_by_label(label)` without hardcoding a value the shell's
    /// window-title escape would change. The label is dynamic precisely because
    /// the title feature makes it so — a real shell sets its own title, so a
    /// fixed `"pane 0"` literal is no longer a stable lookup key.
    #[allow(dead_code)]
    pub fn tab_label_for_pane(&self, pane_id: PaneId) -> Option<String> {
        self.pane_titles()
            .into_iter()
            .find(|(id, _)| *id == pane_id)
            .map(|(id, label)| Self::tab_a11y_label(id, &label))
    }

    /// A pane's UNIQUE accessible tab label, derived from its displayed tab text.
    ///
    /// The VISIBLE tab text is just the title (or the `pane {id}` fallback), but
    /// two shells launched in the same directory routinely set the SAME OSC
    /// window title — so the visible text alone is NOT unique. An ambiguous
    /// accessible name is a real defect: a screen reader cannot distinguish the
    /// two tabs, and the accessibility tree has two nodes with one name (which
    /// also makes `get_by_label` lookups ambiguous). This stable-by-construction
    /// label fixes that by anchoring every label on the unique `pane {id}`:
    ///
    /// - untitled pane → `pane {id}` (already unique; no redundant suffix)
    /// - titled pane   → `{title} (pane {id})` (title for context + id for
    ///   uniqueness; the title is kept first so WCAG 2.5.3 "Label in Name" holds
    ///   against the visible text)
    fn tab_a11y_label(pane_id: PaneId, display: &str) -> String {
        let fallback = format!("pane {}", pane_id.raw());
        if display == fallback {
            fallback
        } else {
            format!("{display} (pane {})", pane_id.raw())
        }
    }

    /// The most recent caption command the user issued (min/max/close), or
    /// `None` if no caption button has been clicked this session.
    #[allow(dead_code)]
    pub fn last_window_cmd(&self) -> Option<WindowCmd> {
        self.last_window_cmd
    }

    /// First-run default window size (logical points), used as the restore
    /// target before the user has ever un-maximized. Mirrors the
    /// `with_inner_size` seed in `egui_main.rs`.
    const DEFAULT_INNER_SIZE: egui::Vec2 = egui::vec2(1100.0, 720.0);

    /// Toggle the OS maximize state for the single app window. When RESTORING
    /// (currently maximized), drive the restore EXPLICITLY: return to the last
    /// un-maximized size (or the first-run default) and re-center on the monitor,
    /// rather than trusting winit's own restore geometry — which eframe's
    /// persisted window state can leave equal to the maximized (monitor) size, so
    /// a plain un-maximize yanks the window back to full-monitor and the user has
    /// to shrink it by hand (the reported bug). Maximizing is the plain command.
    fn toggle_maximize(&self, ctx: &egui::Context, is_max: bool) {
        if !is_max {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            return;
        }
        let target = self.restore_size.unwrap_or(Self::DEFAULT_INNER_SIZE);
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target));
        // Re-center on the monitor so the restored window lands in the middle,
        // not at the monitor's top-left. `monitor_size` is in logical points.
        if let Some(mon) = ctx.input(|i| i.viewport().monitor_size) {
            let pos = ((mon - target) * 0.5).max(egui::vec2(0.0, 0.0));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos.to_pos2()));
        }
    }

    /// The visible grid text of a pane's terminal, or `None` if the pane has no
    /// live terminal. Used by interaction tests to assert that PTY output landed
    /// on screen (the load-bearing type→PTY→grid round-trip).
    #[allow(dead_code)]
    pub fn pane_grid_text(&self, pane_id: PaneId) -> Option<String> {
        self.terms.get(&pane_id).and_then(PaneTerm::grid_text)
    }

    /// The focused pane's visible grid text. Convenience over
    /// [`Self::pane_grid_text`] for the common test assertion.
    #[allow(dead_code)]
    pub fn focused_grid_text(&self) -> Option<String> {
        self.pane_grid_text(self.focused_pane)
    }

    /// A pane's PTY grid size `(cols, rows)`, or `None` if it has no terminal.
    /// Used by the resize→PTY interaction test.
    #[allow(dead_code)]
    pub fn pane_size(&self, pane_id: PaneId) -> Option<(u16, u16)> {
        self.terms.get(&pane_id).map(PaneTerm::size)
    }

    /// The ids of every pane with a live terminal, in unspecified order. Used by
    /// tests to enumerate panes for focus routing assertions.
    #[allow(dead_code)]
    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.pane_titles().into_iter().map(|(id, _)| id).collect()
    }

    /// Maximum displayed length of an OSC-derived tab title before it is
    /// truncated with an ellipsis. A program can set an arbitrarily long title
    /// (e.g. a full `user@host: /deep/path` string); the tab strip caps it so
    /// one verbose pane cannot blow out the whole strip.
    const MAX_TAB_TITLE: usize = 32;

    /// `(pane_id, title)` for every pane in the grid, in STABLE visual order
    /// (left→right, top→bottom). Built by walking the tree from the root via
    /// [`grid::panes_in_visual_order`] — NOT by iterating the `ahash::HashMap`
    /// storage, whose order changes every process launch (the "tab order
    /// reshuffles between launches" bug). The tab strip and every consumer of
    /// this list therefore stay in a fixed, on-screen-matching order.
    ///
    /// Each tab label is the running program's live OSC 0/2 title (trimmed and
    /// capped to [`Self::MAX_TAB_TITLE`] chars, with a `…` suffix when longer)
    /// when the program has set one — like every real terminal. Panes that have
    /// no title yet (a fresh shell, or one whose program never set a title) fall
    /// back to the generic `pane {id}` label, so untitled panes read identically
    /// to before.
    fn pane_titles(&self) -> Vec<(PaneId, String)> {
        grid::panes_in_visual_order(&self.grid_tree)
            .into_iter()
            .map(|pane_id| {
                let label = self
                    .terms
                    .get(&pane_id)
                    .and_then(PaneTerm::title)
                    .map(|t| Self::cap_tab_title(&t))
                    .unwrap_or_else(|| format!("pane {}", pane_id.raw()));
                (pane_id, label)
            })
            .collect()
    }

    /// Trim a raw OSC title and cap it to [`Self::MAX_TAB_TITLE`] CHARACTERS
    /// (not bytes — a multi-byte glyph is never split), appending `…` when the
    /// title was actually shortened. The raw title is first run through
    /// [`scrub_display_text`] so a hostile program/SSH host cannot inject bidi,
    /// zero-width, or control characters into the tab label.
    fn cap_tab_title(raw: &str) -> String {
        let scrubbed = scrub_display_text(raw);
        let trimmed = scrubbed.trim();
        if trimmed.chars().count() <= Self::MAX_TAB_TITLE {
            trimmed.to_string()
        } else {
            let kept: String = trimmed.chars().take(Self::MAX_TAB_TITLE).collect();
            format!("{kept}…")
        }
    }

    /// Split the focused pane, allocating a fresh placeholder pane. Refused (with
    /// a toast) at the 6-pane cap.
    fn split(&mut self, dir: egui_tiles::LinearDir) {
        if count_panes(&self.grid_tree) >= grid::MAX_PANES {
            self.toast = Some(format!(
                "You've reached the maximum of {} panes. Close one to open another.",
                grid::MAX_PANES
            ));
            return;
        }
        let new_pane = self.pane_alloc.alloc();
        if grid::split_focused(&mut self.grid_tree, self.focused_pane, new_pane, dir) {
            self.spawn_term(new_pane);
            self.focused_pane = new_pane;
            self.toast = None;
        }
    }

    /// Open a new terminal (the single "+" button). Splits the focused pane
    /// along its LONGER axis so panes stay balanced: a wide pane splits
    /// left|right, a tall pane splits top/bottom. This gives a "logical" grid
    /// expansion without asking the user to pick a direction.
    ///
    /// `pub` so the headless interaction tests can drive the split path directly
    /// (the same path the "+" button triggers) — the blank-pane-on-split
    /// regression test exercises this.
    pub fn new_terminal(&mut self) {
        let (w, h) = self.last_focused_size.unwrap_or((16.0, 9.0));
        let dir = if w >= h {
            egui_tiles::LinearDir::Horizontal // wide → side-by-side
        } else {
            egui_tiles::LinearDir::Vertical // tall → stacked
        };
        self.split(dir);
    }

    /// Make shell profile `idx` active and open a new terminal running it (the
    /// top-bar ▾ menu path). Subsequent plain "+" presses then use the same
    /// shell, mirroring the Windows-Terminal "+ ▾" profile behaviour. An
    /// out-of-range index is ignored (defensive — the menu only emits valid
    /// indices).
    fn open_shell(&mut self, idx: usize) {
        if idx < self.shell_profiles.len() {
            self.active_shell = idx;
            self.new_terminal();
        }
    }

    /// The shell profiles offered by the top-bar switcher (platform default
    /// first). Used by the chrome to render the ▾ menu.
    pub fn shell_profiles(&self) -> &[shells::ShellProfile] {
        &self.shell_profiles
    }

    /// The label of the currently-active shell profile (what new terminals run).
    /// Used by the chrome's hover text and by interaction tests.
    pub fn active_shell_label(&self) -> &str {
        self.shell_profiles
            .get(self.active_shell)
            .map(|p| p.label.as_str())
            .unwrap_or("Default shell")
    }

    /// Paint one terminal pane's body and wire its per-frame interaction:
    ///
    /// 1. Allocate the pane rect and paint the theme background quad + focus ring
    ///    behind the glyphs (so text never blends directly against the acrylic).
    /// 2. Compute the physical-pixel size and DEBOUNCED-resize the PTY to fit.
    /// 3. DISPLAY the visible grid with egui's native coloured-text painter via
    ///    [`paint_grid_native`].
    /// 4. Report click (refocus) + drag-start (egui_tiles).
    ///
    /// A failed-spawn pane paints an error label instead of a grid — never a
    /// panic. This is a FREE function (not `&mut self`) so the `grid_ui` closure
    /// can borrow `terms`/`theme` disjointly from `self.grid_tree` (which
    /// `tree.ui` borrows mutably) — the classic egui_tiles borrow split.
    #[allow(clippy::too_many_arguments)]
    fn render_pane_body(
        ui: &mut egui::Ui,
        pane_id: PaneId,
        focused: bool,
        terms: &mut HashMap<PaneId, PaneTerm>,
        pending_spawn: &mut HashSet<PaneId>,
        // Per-pane working dirs from a restored layout snapshot; consumed here so
        // a deferred first-spawn opens in its saved cwd (disjoint borrow, like
        // `pending_spawn`).
        restored_cwds: &mut HashMap<PaneId, String>,
        galley_cache: &mut GalleyCache,
        image_textures: &mut ImageTextureCache,
        theme: &c0pl4nd_core::Theme,
        // The configured `TERM` advertised to a deferred-first-spawn pane, so the
        // initial pane's child PTY sees the same `TERM` as every later pane.
        term: &str,
        font_size: f32,
        line_height_px: f32,
        cursor_cfg: c0pl4nd_core::config::CursorConfig,
        effects: c0pl4nd_core::config::EffectsConfig,
        padding: f32,
        bg_alpha: u8,
        search: Option<SearchHighlight<'_>>,
        links: &[(CellSpan, String)],
        // True while Ctrl/Cmd is held: enables the whole-pane link underline +
        // click-to-open (the hover underline shows regardless). Gating click on
        // this — not on `links` being non-empty — is what lets links be detected
        // every frame for the hover affordance without a plain click opening one.
        link_modifier: bool,
        // The focused pane's in-progress IME pre-edit (composition) string, for
        // display at the cursor (F3-1). `None` for non-focused panes and when no
        // composition is active. Never sent to the PTY — display only.
        ime_preedit: Option<&str>,
        // The app-wide mouse text selection state, updated here on drag and read
        // by the selection painter below. Threaded as `&mut` (a separate field
        // from `terms`/`theme`) so the egui_tiles disjoint-borrow split holds.
        selection: &mut Option<Selection>,
        // While the atlas-warmup gate is open, skip painting the grid glyphs (the
        // pane still lays out, spawns, sizes, and paints its background) so no
        // glyph is sampled before the warmed atlas is uploaded + resident.
        warming: bool,
    ) -> PaneBodyOutcome {
        let (rect, resp) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());

        // Right-click context menu (table-stakes terminal gesture). Copy +
        // Clear-scrollback run INLINE here (they only touch `terms`); split /
        // new / close need `&mut self`, so they are QUEUED into the outcome and
        // applied by the caller after the egui_tiles closure releases its
        // borrows. Paste is intentionally disabled: egui exposes no clipboard
        // READ, so paste can only arrive via the OS Ctrl/Cmd+Shift+V event.
        let mut context_menu_action: Option<ContextMenuAction> = None;
        resp.context_menu(|ui| {
            let has_selection = selection
                .as_ref()
                .is_some_and(|s| s.pane == pane_id && s.anchor != s.head);
            if ui
                .add_enabled(has_selection, egui::Button::new("Copy"))
                .clicked()
            {
                if let Some(sel) = *selection {
                    if sel.pane == pane_id {
                        if let Some(term) = terms.get(&pane_id) {
                            // Selection anchors are ABSOLUTE scrollback lines; map
                            // them to the CURRENT display rows before extracting
                            // (the view may be scrolled back), exactly like the
                            // drag-release and Ctrl/Cmd+Shift+C copy paths. Passing
                            // the absolute coords straight to `selection_text`
                            // (which takes DISPLAY coords) would copy the wrong
                            // rows whenever the pane is scrolled up.
                            let rows = term.size().1 as usize;
                            let ws = term.window_start().unwrap_or(0);
                            if let Some((a, b)) =
                                selection_visible_rows(sel.anchor, sel.head, ws, rows)
                            {
                                let block = sel.mode == SelectionMode::Block;
                                if let Some(text) = term.selection_text(a, b, block) {
                                    ui.ctx().copy_text(text);
                                }
                            }
                        }
                    }
                }
                ui.close_kind(egui::UiKind::Menu);
            }
            // Copy the WHOLE buffer (scrollback + screen) — the no-selection
            // companion to Copy, always available. The mouse-selection Copy above
            // is display-window-bound; this copies the entire retained buffer.
            if ui.button("Copy all").clicked() {
                if let Some(t) = terms.get(&pane_id) {
                    if let Some(text) = t.buffer_text() {
                        ui.ctx().copy_text(text);
                    }
                }
                ui.close_kind(egui::UiKind::Menu);
            }
            ui.add_enabled(false, egui::Button::new("Paste"))
                .on_hover_text("Paste with the keyboard shortcut (Ctrl/Cmd+Shift+V)");
            ui.separator();
            if ui.button("Clear scrollback").clicked() {
                if let Some(t) = terms.get_mut(&pane_id) {
                    t.clear_scrollback();
                }
                ui.close_kind(egui::UiKind::Menu);
            }
            ui.separator();
            if ui.button("Split right").clicked() {
                context_menu_action = Some(ContextMenuAction::SplitRight);
                ui.close_kind(egui::UiKind::Menu);
            }
            if ui.button("Split down").clicked() {
                context_menu_action = Some(ContextMenuAction::SplitDown);
                ui.close_kind(egui::UiKind::Menu);
            }
            if ui.button("New tab").clicked() {
                context_menu_action = Some(ContextMenuAction::NewTerminal);
                ui.close_kind(egui::UiKind::Menu);
            }
            ui.separator();
            if ui.button("Close pane").clicked() {
                context_menu_action = Some(ContextMenuAction::ClosePane(pane_id));
                ui.close_kind(egui::UiKind::Menu);
            }
        });

        let ppp = ui.ctx().pixels_per_point();
        let painter = ui.painter_at(rect);
        // Cell metrics from the SAME monospace font the grid is drawn with, so
        // the PTY's `(cols, rows)` match the rendered glyph size. Measured via a
        // probe galley (`Painter::layout_job`); ppp scales points → physical px
        // to match the `rect * ppp` resize math below. The configured
        // Line-height folds into the row pitch here so the PTY reflows to the
        // SAME pitch the painter draws at.
        let cell_metrics = monospace_cell_metrics(&painter, font_size, ppp, line_height_px);

        // --- deferred first-spawn at the MEASURED size (bug #40) ---
        // The configurable inner padding (points) insets the text on every edge,
        // so the grid area is the rect minus 2×padding per axis. We need it both
        // for the deferred spawn (below) and the debounced resize (further down),
        // so compute it ONCE here.
        let pad = padding.max(0.0);
        let px_w = (rect.width() - 2.0 * pad).max(0.0) * ppp;
        let px_h = (rect.height() - 2.0 * pad).max(0.0) * ppp;
        // A pane whose PTY was deferred (the initial pane) is spawned HERE, at the
        // real `(cols, rows)` derived from its measured rect — exactly the size
        // `resize_to_px` would otherwise reflow it to a frame later. Spawning at
        // the correct size up front means the subsequent debounced `resize_to_px`
        // is a no-op, so cmd's banner/prompt cursor never snaps home to (0,0).
        if pending_spawn.remove(&pane_id) {
            let (cols, rows) = cell_metrics.cols_rows(px_w, px_h);
            // A restored pane opens in its saved cwd; a fresh pane (no restore
            // entry) opens in the default dir. `remove` consumes the entry so a
            // later re-use of the id never inherits a stale cwd.
            let pane_term = match restored_cwds.remove(&pane_id) {
                Some(cwd) => {
                    PaneTerm::spawn_in_with_term(theme.clone(), cols, rows, Some(term), Some(&cwd))
                }
                None => PaneTerm::spawn_with_term(theme.clone(), cols, rows, Some(term)),
            };
            terms.insert(pane_id, pane_term);
        }

        // --- background quad (theme bg) + focus ring ---
        // SINGLE-BACKDROP RULE (opacity linearity): the terminal background is
        // painted EXACTLY ONCE — by the `CentralPanel` `central_fill` behind the
        // whole tiling grid (see `ui`), which already carries the opacity-folded
        // `pane_bg_alpha` AND backs the gaps between panes (so an opaque window
        // stays solid, no desktop leak in the 4px seams). This per-pane body used
        // to ALSO fill the pane rect at the same `bg_alpha`, so the two identical
        // theme-bg layers COMPOUNDED (`opacity` over `opacity` ≈ `opacity²` — at
        // 0.7 → ~0.91 effective), which read as a heavy haze that never went clear
        // like SCR1B3 (whose editor paints its background once). Dropping this
        // second fill makes the opacity slider LINEAR — one alpha over the desktop.
        // The tint (background layer, behind `central_fill`) still reaches the pane
        // through the single translucent backing, in exactly one pass. `bg_alpha`
        // stays in use below to fold the bezel/focus-ring stroke.
        //
        // Focus ring + bezel follow the active theme (accent on focus, bezel
        // otherwise) so the grid chrome matches the rest of the themed UI. Both
        // fold the window-transparency alpha (`bg_alpha`) so the border is as
        // translucent as the pane it frames — a full-alpha border over a
        // see-through window read as a hard opaque line "unaffected by tint or
        // transparency" (the reported divider bug). At an opaque window
        // (`bg_alpha == 255`) `fold_alpha` returns the colour unchanged.
        let pane_colors = theme::ChromeColors::from_theme(theme);
        let stroke = if focused {
            // Focus is SEMANTIC (which pane has keyboard focus), so floor its
            // folded alpha: it still tints/fades with the window, but never drops
            // below a legible strength even at very low opacity.
            const FOCUS_RING_ALPHA_FLOOR: u8 = 150;
            let a = bg_alpha.max(FOCUS_RING_ALPHA_FLOOR);
            egui::Stroke::new(2.0f32, window_effects::fold_alpha(pane_colors.accent, a))
        } else {
            // The unfocused bezel is pure definition — let it fully fade into
            // negative space as the window goes see-through.
            egui::Stroke::new(
                1.0f32,
                window_effects::fold_alpha(pane_colors.bezel, bg_alpha),
            )
        };
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(4),
            stroke,
            egui::StrokeKind::Inside,
        );

        // --- resize the PTY to fit this rect (debounced) ---
        // The configurable inner padding (points) insets the text on every edge,
        // so the area available for the terminal grid is the rect minus 2×padding
        // on each axis. Subtract it BEFORE the px conversion so the computed
        // (cols, rows) match what `paint_grid_native` actually draws inside the
        // padded origin — otherwise a large padding would size the PTY for the
        // full rect and clip the last row/column.
        let pad = padding.max(0.0);
        let px_w = (rect.width() - 2.0 * pad).max(0.0) * ppp;
        let px_h = (rect.height() - 2.0 * pad).max(0.0) * ppp;
        if let Some(term) = terms.get_mut(&pane_id) {
            term.resize_to_px(px_w, px_h, cell_metrics);
        }

        // --- display the grid ---
        match terms.get(&pane_id) {
            Some(term) if term.error().is_none() => {
                // Single native render path for BOTH the live window and headless
                // snapshots (see `paint_grid_native`). egui's own glyph painter
                // draws the coloured grid reliably on the real swapchain — the
                // glyphon GPU paths (callback + offscreen texture) composited
                // black inside `egui_tiles` panes live while passing the wgpu
                // test harness.
                if !warming {
                    paint_grid_native(
                        &painter,
                        rect,
                        term,
                        galley_cache,
                        font_size,
                        line_height_px,
                        theme,
                        focused,
                        cursor_cfg,
                        effects,
                        pad,
                    );
                }
                // Find-overlay highlight: tint every match span (and outline the
                // active one) over the rendered grid. Only the focused pane while
                // the overlay is open carries a `SearchHighlight`.
                if let Some(hl) = search {
                    paint_search_highlight(
                        &painter,
                        rect,
                        font_size,
                        line_height_px,
                        pad,
                        &pane_colors,
                        // The current match takes the theme's CURSOR colour — the
                        // one palette entry that already means "where you are" —
                        // so it is hue-distinct from the accent tint the other
                        // matches share, in every theme.
                        c0pl4nd_core::theme::parse_hex(&theme.cursor)
                            .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
                            .unwrap_or(pane_colors.accent),
                        hl,
                    );
                }
            }
            Some(term) => {
                // Failed spawn: show the error, never panic.
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    term.error().unwrap_or(
                        "This pane couldn't open a terminal. Close it and open a new pane.",
                    ),
                    egui::FontId::monospace(14.0),
                    pane_colors.fg,
                );
            }
            None => {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "This pane is empty. Open a new pane to start a terminal.",
                    egui::FontId::monospace(14.0),
                    pane_colors.fg,
                );
            }
        }

        // Hyperlinks. `links` holds the detected URL spans for the FOCUSED pane
        // (empty for the others), computed EVERY frame. `link_modifier` is true
        // while Ctrl/Cmd is held. Affordances:
        //   - HOVER (always, no modifier): underline the URL under the pointer +
        //     show the hand cursor, so a link is discoverable; the hand signals
        //     "Ctrl/Cmd+click to open".
        //   - Ctrl/Cmd HELD: underline EVERY URL (the whole-pane click affordance)
        //     and OPEN the one a click lands on.
        // The pixel→cell mapping ([`cell_at_pos`]) and the span hit test are pure
        // + unit-tested; only this thin wiring + the OS-opener side effect live
        // here.
        let mut opened_url = None;
        if !links.is_empty() {
            let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
            let origin = grid_text_origin(rect, pad);
            if link_modifier {
                paint_link_underlines(&painter, origin, cw, ch, &pane_colors, links);
            }
            if let Some(hover) = resp.hover_pos() {
                if let Some((r, c)) = cell_at_pos(hover, origin, cw, ch) {
                    if let Some(span) = link_span_at_cell(links, r, c) {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        // Without the modifier, underline JUST the hovered link
                        // (with it, every link is already underlined above).
                        if !link_modifier {
                            paint_one_link_underline(&painter, origin, cw, ch, &pane_colors, span);
                        }
                    }
                }
            }
            if link_modifier && resp.clicked() {
                if let Some(click) = resp.interact_pointer_pos() {
                    if let Some((r, c)) = cell_at_pos(click, origin, cw, ch) {
                        if let Some(url) = link_url_at_cell(links, r, c) {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                            opened_url = Some(url.to_string());
                        }
                    }
                }
            }
        }

        // --- mouse reporting (E6) + local wheel scrollback ---
        // When the program in this pane has grabbed the mouse (?1000/?1002/?1003)
        // translate pointer gestures into `encode_mouse` reports written to its
        // PTY (mouse in vim/tmux/htop/less). Otherwise the wheel scrolls this
        // pane's local scrollback. Two conventional overrides force LOCAL
        // handling even while a program grabs the mouse: holding Shift (the
        // standard "let me select/scroll" escape) and Ctrl-with-a-link-under-the-
        // pointer (`links` is non-empty only then — that click opens the URL).
        // Without this the canonical egui binary reported NO mouse at all and
        // could not scroll back through history; the legacy winit shell did both.
        let mut mouse_captured = false;
        let mut copy_selection: Option<String> = None;
        {
            use c0pl4nd_core::term::{MouseButton, MouseEventKind, MouseMode, MouseModifiers};
            let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
            let origin = grid_text_origin(rect, pad);
            // The pane's grid size (cols, rows), so a reported mouse cell can be
            // clamped to the grid — `cell_at_pos` only guards the LOW edge.
            let pane_size = terms.get(&pane_id).map(PaneTerm::size);
            // The absolute line at the top of the visible window THIS frame, so a
            // mouse selection is anchored to absolute scrollback lines (not the
            // display row, which changes as the view scrolls).
            let window_start = terms
                .get(&pane_id)
                .and_then(PaneTerm::window_start)
                .unwrap_or(0);
            // 1-based (col, row) of a screen-space point over the grid, if any.
            // Clamped to the grid's high edge too: a point over the trailing
            // padding or a fractional last cell must not encode an out-of-range
            // cell into the SGR/X10 mouse report (a conformant terminal clamps
            // reported cells to the grid bounds; oversized values make TUIs like
            // vim/tmux mis-parse the report).
            let cell_of = |pos: egui::Pos2| -> Option<(usize, usize)> {
                let (r, c) = cell_at_pos(pos, origin, cw, ch)?;
                let (cols, rows) = pane_size?;
                if cols == 0 || rows == 0 {
                    return None;
                }
                Some(((c + 1).min(cols as usize), (r + 1).min(rows as usize)))
            };
            let m = ui.input(|i| i.modifiers);
            let mods = MouseModifiers {
                shift: m.shift,
                alt: m.alt,
                control: m.ctrl,
            };
            let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
            let mode = terms
                .get(&pane_id)
                .map(PaneTerm::mouse_mode)
                .unwrap_or(MouseMode::Off);
            // Report to the program only when it grabbed the mouse, Shift is not
            // forcing local selection, and the link modifier is not held (a
            // Ctrl/Cmd+click opens a hovered link instead of being reported).
            let report = mode != MouseMode::Off && !m.shift && !link_modifier;
            if report {
                // Button press/release at the interacted cell.
                let buttons = [
                    (egui::PointerButton::Primary, MouseButton::Left),
                    (egui::PointerButton::Middle, MouseButton::Middle),
                    (egui::PointerButton::Secondary, MouseButton::Right),
                ];
                let pos = resp
                    .interact_pointer_pos()
                    .or(resp.hover_pos())
                    .or_else(|| ui.input(|i| i.pointer.latest_pos()));
                if let Some(pos) = pos {
                    if let Some((col, row)) = cell_of(pos) {
                        if let Some(term) = terms.get_mut(&pane_id) {
                            for (egui_btn, term_btn) in buttons {
                                if ui.input(|i| i.pointer.button_pressed(egui_btn)) {
                                    mouse_captured |= term.report_mouse(
                                        term_btn,
                                        mods,
                                        col,
                                        row,
                                        MouseEventKind::Press,
                                    );
                                }
                                if ui.input(|i| i.pointer.button_released(egui_btn)) {
                                    term.report_mouse(
                                        term_btn,
                                        mods,
                                        col,
                                        row,
                                        MouseEventKind::Release,
                                    );
                                }
                            }
                            // Motion: ?1002 reports drag (button held), ?1003 any
                            // motion. encode_mouse gates by mode, so a bare hover
                            // under ?1002 yields nothing.
                            if resp.dragged() || resp.hovered() {
                                let held = if ui.input(|i| i.pointer.primary_down()) {
                                    MouseButton::Left
                                } else if ui.input(|i| i.pointer.secondary_down()) {
                                    MouseButton::Right
                                } else if ui.input(|i| i.pointer.middle_down()) {
                                    MouseButton::Middle
                                } else {
                                    MouseButton::None
                                };
                                if term.report_mouse(held, mods, col, row, MouseEventKind::Motion) {
                                    mouse_captured = true;
                                }
                            }
                        }
                    }
                }
                // Wheel → buttons 64/65 (one report per ~cell of travel, capped).
                if scroll_y.abs() > f32::EPSILON {
                    let pos = resp
                        .hover_pos()
                        .or_else(|| ui.input(|i| i.pointer.latest_pos()));
                    if let (Some(pos), Some(term)) = (pos, terms.get_mut(&pane_id)) {
                        if let Some((col, row)) = cell_of(pos) {
                            let btn = if scroll_y > 0.0 {
                                MouseButton::WheelUp
                            } else {
                                MouseButton::WheelDown
                            };
                            let ticks = ((scroll_y.abs() / ch.max(1.0)).round() as i32).clamp(1, 8);
                            for _ in 0..ticks {
                                term.report_mouse(btn, mods, col, row, MouseEventKind::Press);
                            }
                            mouse_captured = true;
                        }
                    }
                }
            } else {
                // Local gesture (the program has NOT grabbed the mouse, or Shift
                // forces local): a primary-drag selects grid text and copies it on
                // release; a plain click clears any selection; the wheel scrolls
                // this pane's scrollback. This is the mouse text-selection the egui
                // shell lacked entirely (the legacy shell had it).
                // NOTE (selection lifetime): this path deliberately does NOT fall
                // back to the global `pointer.latest_pos()` the mouse-REPORTING
                // branch above uses. That fallback made a primary press ANYWHERE —
                // including inside the floating right-click context menu, which
                // always drops down-RIGHT over the pane and so maps to a real grid
                // cell — reset `selection` to an empty `anchor == head`. Since the
                // menu's "Copy" item is gated on `anchor != head` and `clicked()`
                // fires on RELEASE, the press that reached for Copy disabled Copy.
                // `interact_pointer_pos()` still tracks a drag that leaves the pane,
                // so nothing is lost.
                let pos = resp.interact_pointer_pos().or(resp.hover_pos());
                // Hit cell as an ABSOLUTE (line, col): display row + window_start.
                let cell0 = pos
                    .and_then(|p| cell_at_pos(p, origin, cw, ch))
                    .map(|(r, c)| (window_start + r, c));
                // ...and the press itself is gated on the pointer being over THIS
                // pane's body with nothing floating above it. `contains_pointer`
                // resolves through the layer stack, so an open context menu / popup
                // / modal over the pane makes it false — a click on a menu item can
                // no longer clobber the selection that item is about to copy.
                let press_over_body = resp.contains_pointer();
                if press_over_body
                    && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
                {
                    if let Some((line, c)) = cell0 {
                        // Alt-drag selects a rectangular BLOCK; a plain drag is
                        // line-wise. The mode is fixed at press and carried for the
                        // whole drag.
                        let mode = if ui.input(|i| i.modifiers.alt) {
                            SelectionMode::Block
                        } else {
                            SelectionMode::Linewise
                        };
                        *selection = Some(Selection {
                            pane: pane_id,
                            anchor: (line, c),
                            head: (line, c),
                            mode,
                        });
                        mouse_captured = true;
                    }
                }
                // Double-click selects the WORD under the cursor; triple-click
                // selects the whole LINE. Both set an absolute-coords selection
                // AND copy it immediately (so even a single-char word copies),
                // the table-stakes terminal gesture the egui shell lacked. The
                // release-clear below is suppressed on these frames so it cannot
                // wipe the fresh selection.
                let multi_click = resp.double_clicked() || resp.triple_clicked();
                if multi_click {
                    if let Some((line, c)) = cell0 {
                        let r = line.saturating_sub(window_start);
                        let (start, end) = if resp.triple_clicked() {
                            let cols = pane_size.map(|(cc, _)| cc as usize).unwrap_or(0);
                            (0, cols.saturating_sub(1))
                        } else {
                            let row = terms
                                .get(&pane_id)
                                .map(|t| t.display_row_chars(r))
                                .unwrap_or_default();
                            word_bounds(&row, c)
                        };
                        *selection = Some(Selection {
                            pane: pane_id,
                            anchor: (line, start),
                            head: (line, end),
                            mode: SelectionMode::Linewise,
                        });
                        if let Some(term) = terms.get(&pane_id) {
                            copy_selection = term.selection_text((r, start), (r, end), false);
                        }
                        mouse_captured = true;
                    }
                }
                if resp.dragged() {
                    if let (Some(sel), Some((line, c))) = (selection.as_mut(), cell0) {
                        if sel.pane == pane_id {
                            sel.head = (line, c);
                            mouse_captured = true;
                        }
                    }
                }
                if !multi_click
                    && ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary))
                {
                    if let Some(sel) = *selection {
                        if sel.pane == pane_id {
                            if sel.anchor == sel.head {
                                // A plain click (no drag) clears any selection.
                                *selection = None;
                            } else if let Some(term) = terms.get(&pane_id) {
                                // Map the absolute selection to current display
                                // rows (it may have scrolled since press); copy
                                // the visible portion.
                                let rows = pane_size.map(|(_, r)| r as usize).unwrap_or(0);
                                if let Some((a, b)) =
                                    selection_visible_rows(sel.anchor, sel.head, window_start, rows)
                                {
                                    copy_selection =
                                        term.selection_text(a, b, sel.mode == SelectionMode::Block);
                                }
                            }
                        }
                    }
                }
                // Local scrollback: wheel up (positive y) goes BACK into history.
                // A Ctrl/Cmd-held wheel is reserved for font zoom (frame_tick):
                // egui reroutes it into `zoom_delta` and zeroes `smooth_scroll_delta`
                // (so `scroll_y` is already 0 here during a zoom), and this `command`
                // guard is a belt-and-suspenders skip regardless.
                if scroll_y.abs() > f32::EPSILON && resp.hovered() && !m.command {
                    if let Some(term) = terms.get_mut(&pane_id) {
                        let lines = (scroll_y / ch.max(1.0)).round() as i32;
                        if lines != 0 {
                            term.scroll_view(lines);
                        }
                    }
                }
            }
        }

        // --- inline images (Sixel / Kitty graphics), paint AFTER the grid text
        // so the image covers the placeholder cells. Each visible image is drawn
        // at native pixel size (ppp-corrected to physical pixels), anchored at
        // its grid cell; the GPU texture is cached + pruned per frame. Core
        // decodes + exposes these via Terminal::images(); the egui shell
        // previously dropped them silently (the legacy winit shell rendered
        // them). Clipped to the pane by `painter` (a painter_at(rect)).
        {
            let metas = terms
                .get(&pane_id)
                .map(PaneTerm::visible_image_metas)
                .unwrap_or_default();
            if !metas.is_empty() {
                let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
                let origin = grid_text_origin(rect, pad);
                let ppp = ui.ctx().pixels_per_point().max(0.01);
                for m in metas {
                    // `display_row` may be NEGATIVE when a tall image's top has
                    // scrolled above the window top; the image's visible remainder
                    // must still draw (the painter clips the off-top portion).
                    let min = origin + egui::vec2(m.col as f32 * cw, m.display_row as f32 * ch);
                    // Native pixel size in points (physical px / ppp).
                    let size = egui::vec2(m.width as f32 / ppp, m.height as f32 / ppp);
                    // Skip an image whose BOTTOM edge is at/above the grid top —
                    // it's fully scrolled off, so don't even upload its texture
                    // (a partial top is kept and clipped by `painter_at(rect)`).
                    if min.y + size.y <= origin.y {
                        continue;
                    }
                    let key: ImageKey = (pane_id, m.line, m.col, m.width, m.height);
                    // Pixels are fetched+cloned ONLY on a cache miss (the closure
                    // runs only then); an already-uploaded texture just returns
                    // its id.
                    let tex_id = image_textures.get_or_upload(ui.ctx(), key, || {
                        terms
                            .get(&pane_id)
                            .and_then(|t| t.image_rgba(m.line, m.col))
                    });
                    if let Some(tex_id) = tex_id {
                        painter.image(
                            tex_id,
                            egui::Rect::from_min_size(min, size),
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                }
            }
        }

        // --- selection wash (paint AFTER the grid so the translucent highlight
        // sits ON TOP of the text, the standard selection look) ---
        if let Some(sel) = *selection {
            if sel.pane == pane_id && sel.anchor != sel.head {
                let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
                let origin = grid_text_origin(rect, pad);
                let (cols, rows) = terms
                    .get(&pane_id)
                    .map(|t| {
                        let (c, r) = t.size();
                        (c as usize, r as usize)
                    })
                    .unwrap_or((0, 0));
                // Map the absolute selection to display rows for THIS frame's view
                // — the wash tracks the selected content as the view scrolls.
                let ws = terms
                    .get(&pane_id)
                    .and_then(PaneTerm::window_start)
                    .unwrap_or(0);
                if let Some((start, end)) = selection_visible_rows(sel.anchor, sel.head, ws, rows) {
                    // The selection wash comes from the ACTIVE THEME
                    // (`selection_background`), not a hard-coded steel blue: a
                    // fixed `#6080c0` ignored every theme the user picked and
                    // clashed with any palette that was not blue-ish. The theme's
                    // own colour is opaque, so it is applied at the wash alpha
                    // that keeps the glyphs beneath legible; the builtin fallback
                    // preserves the previous look for a theme with no selection
                    // colour set.
                    let sel_bg = c0pl4nd_core::theme::parse_hex(&theme.selection_background)
                        .unwrap_or((0x60, 0x80, 0xc0));
                    let wash = egui::Color32::from_rgba_unmultiplied(
                        sel_bg.0,
                        sel_bg.1,
                        sel_bg.2,
                        SELECTION_WASH_ALPHA,
                    );
                    let block = sel.mode == SelectionMode::Block;
                    // Block mode: every row shares the same column range; the wash
                    // must paint the SAME rectangle each row so it matches the
                    // block-mode copy (anchored to the endpoint columns).
                    let (block_lo, block_hi) = (
                        sel.anchor.1.min(sel.head.1),
                        sel.anchor.1.max(sel.head.1).min(cols.saturating_sub(1)),
                    );
                    for r in start.0..=end.0 {
                        let lo = if block {
                            block_lo
                        } else if r == start.0 {
                            start.1
                        } else {
                            0
                        };
                        // `end.1` may be usize::MAX (selection end scrolled below
                        // the bottom → to line end); clamp to the last column.
                        let hi_raw = if block {
                            block_hi
                        } else if r == end.0 {
                            end.1
                        } else {
                            cols.saturating_sub(1)
                        };
                        let hi = hi_raw.min(cols.saturating_sub(1));
                        if cols == 0 || hi < lo {
                            continue;
                        }
                        let x0 = origin.x + lo as f32 * cw;
                        let x1 = origin.x + (hi as f32 + 1.0) * cw;
                        let y0 = origin.y + r as f32 * ch;
                        let sel_rect =
                            egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y0 + ch));
                        painter.rect_filled(sel_rect, 0.0, wash);
                        // Honour `selection_foreground` too: re-draw the selected
                        // glyphs in the theme's selection text colour ON TOP of
                        // the wash. Without this the wash alone tints whatever
                        // colour the text already had, so a dark-on-dark or
                        // low-contrast pairing stayed unreadable while selected —
                        // the theme declares a selection foreground precisely to
                        // guarantee contrast, and it was being ignored.
                        // `parse_hex` returns Result, not Option — a malformed
                        // selection_foreground simply leaves the wash to tint the
                        // existing glyphs rather than failing the frame.
                        if let Ok(sel_fg) =
                            c0pl4nd_core::theme::parse_hex(&theme.selection_foreground)
                        {
                            let fg32 = egui::Color32::from_rgb(sel_fg.0, sel_fg.1, sel_fg.2);
                            let font = egui::FontId::monospace(font_size);
                            if let Some(rows) = terms.get(&pane_id).and_then(PaneTerm::grid_rows) {
                                if let Some(runs) = rows.get(r) {
                                    for (c, _, col_cells) in pane_term::row_glyph_cells(runs) {
                                        if col_cells < lo || col_cells > hi {
                                            continue;
                                        }
                                        painter.text(
                                            egui::pos2(
                                                origin.x + col_cells as f32 * cw,
                                                origin.y + r as f32 * ch,
                                            ),
                                            egui::Align2::LEFT_TOP,
                                            c,
                                            font.clone(),
                                            fg32,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // --- accessibility (F2-1): expose the grid text to screen readers ---
        // The terminal grid is custom-painted, so without an explicit AccessKit
        // node a screen reader perceives only an empty interactive region — the
        // terminal's actual content is invisible to assistive tech. Attach the
        // visible grid text as the pane's accessible value, marking the focused
        // pane active. egui invokes this closure LAZILY and ONLY while building an
        // AccessKit tree (i.e. when a screen reader / `egui_kittest` is attached),
        // so the full-grid `grid_text()` snapshot costs nothing in the common
        // no-assistive-tech case.
        resp.widget_info(|| {
            let text = terms
                .get(&pane_id)
                .and_then(PaneTerm::grid_text)
                .unwrap_or_default();
            egui::WidgetInfo::labeled(egui::WidgetType::Label, focused, text)
        });

        // --- IME composition (F3-1): cursor rect + pre-edit display ---
        // Compute the focused pane's terminal-cursor cell rect in screen space
        // using the SAME geometry the glyph painter, cursor, and link hit-test
        // share (`origin + (col*cw, row*ch)`). The caller hands this rect to
        // `ctx.output_mut(|o| o.ime = Some(IMEOutput {..}))` so winit's
        // `set_ime_cursor_area` places the OS candidate window AT the caret.
        // Only the focused pane reports a rect (the OS tracks a single caret).
        let mut ime_cursor_rect = None;
        if focused {
            if let Some((row, col)) = terms.get(&pane_id).and_then(PaneTerm::cursor_cell) {
                let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
                let origin = grid_text_origin(rect, pad);
                let cell_min = origin + egui::vec2(col as f32 * cw, row as f32 * ch);
                ime_cursor_rect = Some(egui::Rect::from_min_size(cell_min, egui::vec2(cw, ch)));

                // Paint the in-progress pre-edit string at the cursor, underlined
                // and in the theme fg, so the user sees what they are composing
                // before commit. The candidate window (positioned via the rect
                // above) shows the IME's own suggestion list; this is the inline
                // composition echo at the caret. The pre-edit is DISPLAY-ONLY —
                // it is never forwarded to the PTY (only `ImeEvent::Commit` is).
                if let Some(pre) = ime_preedit.filter(|s| !s.is_empty()) {
                    let fg = theme::ChromeColors::from_theme(theme).fg;
                    let font = egui::FontId::monospace(font_size);
                    let galley = painter.layout_no_wrap(pre.to_string(), font, fg);
                    let text_pos = origin + egui::vec2(col as f32 * cw, row as f32 * ch);
                    let galley_w = galley.size().x;
                    painter.galley(text_pos, galley, fg);
                    // Underline the composition span (the conventional pre-edit
                    // affordance), one device-px line at the cell baseline.
                    let underline = egui::Rect::from_min_size(
                        text_pos + egui::vec2(0.0, ch - 1.0),
                        egui::vec2(galley_w, 1.0),
                    );
                    painter.rect_filled(underline, 0.0, fg);
                }
            }
        }

        // --- right-side scrollbar (overlay; auto-hides when everything fits) ---
        // The terminal grid is custom-painted (no `egui::ScrollArea`), so the bar
        // owns its rect + hit-testing. It reflects the scrollback position and
        // viewport size, is draggable to scrub, click-in-trough pages, and marks
        // the focused pane's search hits. Painted LAST so it sits over the grid.
        let mut scrollbar_grabbed = false;
        {
            let (scrollback_len, view_offset, rows) = terms
                .get(&pane_id)
                .map(|t| (t.scrollback_len(), t.view_offset(), t.size().1 as usize))
                .unwrap_or((0, 0, 0));
            let metrics = scrollbar::ScrollMetrics {
                scrollback_len,
                view_offset,
                rows,
            };
            if metrics.scrollable() {
                let track = scrollbar::track_rect(rect);
                let sb_resp = ui.interact(
                    track,
                    egui::Id::new(("c0pl4nd_scrollbar", pane_id.raw())),
                    egui::Sense::click_and_drag(),
                );
                // Grabbing the bar must NOT also start an egui_tiles pane-rearrange.
                scrollbar_grabbed = sb_resp.dragged() || sb_resp.drag_started();
                let thumb = scrollbar::thumb_rect(&metrics, track);
                // A drag scrubs (thumb centres on the pointer); a trough click
                // above/below the thumb pages by a viewport.
                let mut target: Option<usize> = None;
                if sb_resp.dragged() {
                    if let Some(p) = sb_resp.interact_pointer_pos() {
                        target = Some(scrollbar::view_offset_for_pointer_y(&metrics, track, p.y));
                    }
                } else if sb_resp.clicked() {
                    if let Some(p) = sb_resp.interact_pointer_pos() {
                        if p.y < thumb.top() {
                            target = Some((view_offset + rows).min(scrollback_len));
                        } else if p.y > thumb.bottom() {
                            target = Some(view_offset.saturating_sub(rows));
                        }
                    }
                }
                if let Some(off) = target {
                    if let Some(t) = terms.get_mut(&pane_id) {
                        // `scroll_view(+n)` goes BACK into history (more offset).
                        let delta = off as i32 - view_offset as i32;
                        if delta != 0 {
                            t.scroll_view(delta);
                        }
                    }
                    // The mutated view repaints next frame — request it so a click
                    // (which does not hold the pointer) still redraws immediately.
                    ui.ctx().request_repaint();
                }
                // Marks: the focused pane's search hits, mapped from their visible
                // display row to an absolute content line via `window_start`.
                let mut marks: Vec<scrollbar::ScrollMark> = Vec::new();
                if let Some(hl) = search {
                    let ws = metrics.window_start();
                    let last = metrics.total().saturating_sub(1);
                    for (i, span) in hl.spans.iter().enumerate() {
                        marks.push(scrollbar::ScrollMark {
                            abs_line: (ws + span.line).min(last),
                            selected: i == hl.selected,
                        });
                    }
                }
                let cursor_color = c0pl4nd_core::theme::parse_hex(&theme.cursor)
                    .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
                    .unwrap_or(pane_colors.accent);
                let active = sb_resp.hovered() || sb_resp.dragged();
                scrollbar::paint(
                    &painter,
                    track,
                    &metrics,
                    &pane_colors,
                    cursor_color,
                    active,
                    &marks,
                );
            }
        }

        PaneBodyOutcome {
            // A body-drag normally tells egui_tiles to REARRANGE the pane. When a
            // program grabbed the mouse and we reported the drag to its PTY, the
            // gesture belongs to the program — never rearrange panes underneath it.
            // A scrollbar grab is likewise NOT a pane-rearrange.
            drag_started: resp.drag_started() && !mouse_captured && !scrollbar_grabbed,
            clicked: resp.clicked(),
            size: rect.size(),
            opened_url,
            ime_cursor_rect,
            copy_selection,
            context_menu_action,
            body_rect: rect,
        }
    }

    /// Forward this frame's keyboard + paste events to the FOCUSED pane's PTY,
    /// using the SHARED core key encoder. Consumes Tab/arrows so egui does not
    /// steal them for widget navigation (recon dossier §5.1). Called once per
    /// frame. Returns the bytes forwarded (for tests that drive the real input
    /// path and assert what reached the PTY).
    fn forward_input_to_focused(&mut self, ctx: &egui::Context) -> Vec<u8> {
        use c0pl4nd_core::term::{KeyEventKind, KeyModifiers, LogicalKey};

        // When the focused program negotiated the kitty keyboard protocol with
        // REPORT-EVENT-TYPES (bit2), ALSO forward key RELEASE and REPEAT events;
        // otherwise keep the legacy press-only behavior. Read the flag once.
        let report_event_types = self
            .terms
            .get(&self.focused_pane)
            .map(|t| t.kitty_reports_event_types())
            .unwrap_or(false);

        // Collect input events under the immutable input borrow first, THEN
        // mutate the PTY (egui forbids re-entrant input borrows).
        let mut keys: Vec<(LogicalKey, KeyModifiers, KeyEventKind)> = Vec::new();
        let mut pastes: Vec<String> = Vec::new();
        // The pre-edit (composition) string to store on `self` after the input
        // borrow closes. `Some(Some(s))` = set/replace the preedit; `Some(None)`
        // = clear it; `None` = no IME event this frame, leave it as-is (F3-1).
        let mut ime_update: Option<Option<String>> = None;
        // Ctrl/Cmd+Shift+Arrow requests a directional pane-focus move; captured
        // here and applied after the forward loop so the arrow is NOT also sent to
        // the PTY as a cursor sequence.
        let mut dir_focus: Option<Direction> = None;
        ctx.input(|i| {
            let mods = KeyModifiers {
                ctrl: i.modifiers.ctrl,
                alt: i.modifiers.alt,
                shift: i.modifiers.shift,
                logo: i.modifiers.command || i.modifiers.mac_cmd,
            };
            for ev in &i.events {
                match ev {
                    // Composed text (printable chars, IME). Skip when Ctrl/logo
                    // is held so a shortcut chord (Ctrl+C etc.) is handled by the
                    // Key event below, not double-sent as raw text.
                    egui::Event::Text(t) if !mods.ctrl && !mods.logo => {
                        keys.push((LogicalKey::Text(t.clone()), mods, KeyEventKind::Press));
                    }
                    // IME composition (F3-1). When an IME (CJK / complex-script)
                    // is active, egui routes composed text through `Event::Ime`
                    // INSTEAD of `Event::Text`, so without this arm CJK input is
                    // impossible. The OS candidate-window position is set
                    // separately each frame via `ctx.output_mut(|o| o.ime = ...)`
                    // in `render_pane_body` (so the popup tracks the caret).
                    egui::Event::Ime(ime) => match ime {
                        // Final composed result: send it to the PTY exactly as
                        // ordinary `Event::Text` would, and clear the pre-edit.
                        // Commit text is final and MUST reach the shell
                        // regardless of modifier state (an IME commit is not a
                        // shortcut chord), so — unlike `Event::Text` above — it
                        // is forwarded even while Ctrl/logo is held.
                        egui::ImeEvent::Commit(text) => {
                            if !text.is_empty() {
                                keys.push((
                                    LogicalKey::Text(text.clone()),
                                    mods,
                                    KeyEventKind::Press,
                                ));
                            }
                            ime_update = Some(None);
                        }
                        // In-progress candidate text: buffer for DISPLAY only —
                        // never sent to the PTY. An empty pre-edit ends the
                        // current composition without committing.
                        egui::ImeEvent::Preedit(text) => {
                            ime_update = Some(if text.is_empty() {
                                None
                            } else {
                                Some(text.clone())
                            });
                        }
                        // Composition session boundaries: clear any stale
                        // pre-edit so a cancelled composition leaves nothing
                        // painted at the cursor.
                        egui::ImeEvent::Enabled | egui::ImeEvent::Disabled => {
                            ime_update = Some(None);
                        }
                    },
                    egui::Event::Paste(s) => pastes.push(s.clone()),
                    egui::Event::Key {
                        key,
                        pressed,
                        repeat,
                        modifiers,
                        ..
                    } => {
                        // Press-only by default; with REPORT-EVENT-TYPES also
                        // forward releases and distinguish repeats.
                        if !*pressed && !report_event_types {
                            continue;
                        }
                        let kind = if !*pressed {
                            KeyEventKind::Release
                        } else if *repeat {
                            KeyEventKind::Repeat
                        } else {
                            KeyEventKind::Press
                        };
                        // Ctrl/Cmd+Shift+Arrow moves keyboard focus to the
                        // adjacent pane instead of sending a cursor sequence to
                        // the PTY. Capture the direction and skip forwarding the
                        // arrow (the ctrl-OR-command discipline used everywhere).
                        if *pressed
                            && (modifiers.ctrl || modifiers.command)
                            && modifiers.shift
                            && !modifiers.alt
                        {
                            let d = match key {
                                egui::Key::ArrowLeft => Some(Direction::Left),
                                egui::Key::ArrowRight => Some(Direction::Right),
                                egui::Key::ArrowUp => Some(Direction::Up),
                                egui::Key::ArrowDown => Some(Direction::Down),
                                _ => None,
                            };
                            if let Some(d) = d {
                                dir_focus = Some(d);
                                continue;
                            }
                        }
                        let m = KeyModifiers {
                            ctrl: modifiers.ctrl,
                            alt: modifiers.alt,
                            shift: modifiers.shift,
                            logo: modifiers.command || modifiers.mac_cmd,
                        };
                        if let Some(lk) = egui_key_to_logical(*key, m) {
                            keys.push((lk, m, kind));
                        }
                    }
                    _ => {}
                }
            }
        });

        // Apply the buffered IME pre-edit change now the input borrow is closed
        // (F3-1). `None` means no IME event this frame — leave the pre-edit as-is
        // so a composition spanning multiple frames is not dropped.
        if let Some(new_preedit) = ime_update {
            self.ime_preedit = new_preedit;
        }

        // Tab/arrows must reach the PTY, not drive egui focus — consume them so
        // egui's built-in navigation does not also act on them.
        ctx.input_mut(|i| {
            for key in [
                egui::Key::Tab,
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
                egui::Key::ArrowLeft,
                egui::Key::ArrowRight,
            ] {
                while i.consume_key(egui::Modifiers::NONE, key) {}
            }
        });

        let mut forwarded: Vec<u8> = Vec::new();
        if let Some(term) = self.terms.get_mut(&self.focused_pane) {
            for (lk, m, kind) in &keys {
                // The common press path goes through the stable `forward_key`
                // wrapper; repeats/releases (kitty REPORT-EVENT-TYPES) take the
                // full event form.
                forwarded.extend(if *kind == KeyEventKind::Press {
                    term.forward_key(lk, *m)
                } else {
                    term.forward_key_event(lk, *m, *kind)
                });
            }
        }

        // Apply a directional pane-focus move AFTER forwarding this frame's other
        // keys (so they reach the previously-focused pane). Uses the pane rects
        // captured during the last grid render (the layout is stable frame to
        // frame); a no-op before the first render or with no neighbour.
        if let Some(dir) = dir_focus {
            self.focus_directional(dir);
        }

        // Paste handling — SECURITY: every paste goes through the core paste-
        // injection guard (`PaneTerm::write_paste` → `Terminal::frame_paste`),
        // NEVER raw `write_bytes`. Two hazards DEFER a paste to the confirm
        // overlay (`pending_paste`) instead of pasting immediately: a MULTI-LINE
        // paste (it executes the instant its embedded newline lands) and an
        // oversized SINGLE-line paste (`paste_warn_bytes` — a hidden-tail
        // command or an accidental whole-file flood, which the newline gate
        // cannot see). Both are decided by ONE core policy function so the two
        // halves can never drift apart. The config read / `pending_paste` set /
        // `terms` borrow are sequential statements so they never alias `self`.
        for s in &pastes {
            if let Some(reason) = c0pl4nd_core::paste_guard::paste_confirm_reason(&self.config, s) {
                self.pending_paste = Some(s.clone());
                self.pending_paste_reason = Some(reason);
            } else if let Some(term) = self.terms.get_mut(&self.focused_pane) {
                term.write_paste(s);
            }
        }

        // Best-effort capture of the line being typed, for the command-palette
        // history (see `c0pl4nd_core::command_history`). Printable text accrues,
        // Backspace pops one char, and Enter commits the line then clears the
        // accumulator. This models printable input + Backspace, NOT full shell
        // line-editing (cursor motion, kill-line) — exactly the contract the
        // `command_history` module documents. Only runs when typing reaches the
        // PTY (the palette routes its own keys away from here), so the history is
        // a record of what the user actually ran, not what they searched for.
        // Ordinary printable characters (incl. Space) arrive as `LogicalKey::Text`
        // (egui delivers them via `Event::Text`); only the special keys below are
        // `LogicalKey` variants, so this captures the full typed line.
        for (lk, _m, kind) in &keys {
            // Releases never accrue typed-line content (a released Enter must not
            // re-commit the line). Presses and repeats do.
            if *kind == KeyEventKind::Release {
                continue;
            }
            match lk {
                LogicalKey::Text(t) => {
                    // Ctrl-letter chords arrive here as a single C0 control byte
                    // (Ctrl+C = 0x03, Ctrl+U = 0x15, …), NOT printable line
                    // content. Ctrl+C / Ctrl+U abort the current line in a shell,
                    // so mirror that by clearing the accumulator; other control
                    // bytes are ignored. Printable text (incl. Space) accrues.
                    if t.chars().all(|c| !c.is_control()) {
                        self.input_line.push_str(t);
                    } else if t == "\u{3}" || t == "\u{15}" {
                        self.input_line.clear();
                    }
                }
                LogicalKey::Backspace => {
                    self.input_line.pop();
                }
                LogicalKey::Enter => {
                    let line = std::mem::take(&mut self.input_line);
                    if self.should_record_history(&line) {
                        // `record` redacts inline secrets (--password=…, API_KEY=…).
                        self.cmd_history.record(line);
                    }
                }
                _ => {}
            }
        }
        forwarded
    }

    /// Render the egui_tiles grid (live terminal panes) + enforce the 6-pane cap
    /// (clone-and-snap-back). The terminal bodies are painted by the FREE
    /// [`Self::render_pane_body`] so the closure can borrow `self.terms`/`theme`
    /// disjointly from `self.grid_tree` (which `tree.ui` borrows mutably).
    fn grid_ui(&mut self, ui: &mut egui::Ui) {
        // Linked dividers (opt-in): hold every split at equal shares so the panes
        // stay the same size ("move together"). Applied BEFORE the tree renders,
        // so a divider drag from the previous frame is reset before it is shown —
        // the panes never visibly drift from equal while the toggle is on. A no-op
        // (and no repaint) when there is no split to equalise.
        if self.config.link_pane_dividers {
            grid::equalize_pane_shares(&mut self.grid_tree);
        }
        let titles = self.pane_titles();
        let mut closes: Vec<PaneId> = Vec::new();
        let focused = self.focused_pane;
        let mut clicked: Option<PaneId> = None;
        let mut pending_ctx_action: Option<ContextMenuAction> = None;
        let mut frame_pane_rects: HashMap<PaneId, egui::Rect> = HashMap::new();
        let mut focused_size: Option<(f32, f32)> = None;
        let mut opened_url: Option<String> = None;
        // The focused pane's IME cursor rect, captured from the render closure
        // and fed into `ctx.output_mut(|o| o.ime = ...)` AFTER the disjoint-
        // borrow block so the OS candidate window tracks the caret (F3-1).
        let mut ime_cursor_rect: Option<egui::Rect> = None;

        // The find overlay highlights the FOCUSED pane only, and only while open.
        // Build the cell spans HERE (before the disjoint-borrow block takes
        // `&mut self.terms`), since `cell_spans_for_search` reads `self` via
        // `focused_grid_text`. Owned `Vec` + a copied index, so the render
        // closure borrows them disjointly from `self.grid_tree`.
        // Snapshot the focused pane's grid text ONCE for both the find-highlight
        // and the hyperlink spans below (perf, audit #2): each used to clone the
        // whole grid into a fresh `Vec<String>` independently, so with the find
        // overlay open AND Ctrl held the grid was cloned twice per frame. Compute
        // it a single time only when at least one consumer needs it.
        let link_modifier = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
        // The focused grid text, snapshotted ONCE per frame for both the find
        // highlight and the hyperlink spans. Computed every frame now (not only
        // when the overlay is open or Ctrl is held) because the hyperlink HOVER
        // affordance must detect a link under the pointer without the modifier.
        let search_lines: Vec<String> = self.focused_search_lines();
        let search_spans: Vec<CellSpan> = if self.search_open {
            self.cell_spans_for_search(&search_lines)
        } else {
            Vec::new()
        };
        let search_sel = self.search_sel;

        // Detected URL spans in the focused pane's visible grid, computed EVERY
        // frame (like the search spans) so a plain HOVER can underline the link
        // under the pointer (the discoverability affordance). Whether a click
        // OPENS a link is gated separately by `link_modifier` (Ctrl/Cmd held), so
        // detecting links every frame does not make a plain click open one.
        // `find_urls` reads the focused grid via `focused_search_lines`.
        let link_spans: Vec<(CellSpan, String)> = self.cell_spans_for_hyperlinks(&search_lines);

        // The active pane shell layout (#30), read LIVE so the titlebar toggle
        // takes effect this frame. Captured before the disjoint-borrow block
        // (which takes `&mut self.terms`).
        let view_mode = self.config.view_mode;
        // The zoom-pane override, captured before the disjoint-borrow block (like
        // `view_mode`): when `Some` and the pane still exists, only that pane is
        // rendered full-size this frame.
        let zoomed_pane = self
            .zoomed_pane
            .filter(|z| grid::tile_of_pane(&self.grid_tree, *z).is_some());

        // Snapshot BEFORE the frame so we can revert a drag that exceeds the cap.
        // (Kept unconditional: the Tabs-view path below reads it as a non-`self`
        // view of the tree while `terms` is mutably borrowed, so it cannot be
        // replaced by a `self.grid_tree` borrow; the clone is a small ~pane-count
        // structure and this runs only on an on-demand repaint.)
        let pre = self.grid_tree.clone();
        {
            // Disjoint borrows: the closure touches these fields, NOT grid_tree.
            let terms = &mut self.terms;
            // Deferred first-spawn set (bug #40): disjoint field borrow, passed
            // through so `render_pane_body` can spawn a pending pane at the
            // MEASURED `(cols, rows)` on the first frame its rect is known.
            let pending_spawn = &mut self.pending_spawn;
            // Restored per-pane cwds: a separate field, disjoint from `terms` and
            // `grid_tree`, threaded so a deferred first-spawn opens in its saved
            // working directory.
            let restored_cwds = &mut self.restored_cwds;
            // The per-row galley cache is a separate field, so it borrows
            // disjointly from `terms` AND from `grid_tree` (audit #2).
            let galley_cache = &mut self.galley_cache;
            // Inline-image GPU-texture cache: a separate field, disjoint borrow.
            let image_textures = &mut self.image_textures;
            // Mouse text selection state: a separate field, disjoint from
            // `terms`/`grid_tree`, threaded so a drag updates it and the painter
            // reads it (Wave G — selection was entirely absent from the egui shell).
            let selection = &mut self.selection;
            // Auto-copy a completed selection to the OS clipboard only when the
            // user opted into copy-on-select (else the selection is visible and
            // Ctrl/Cmd+Shift+C copies it on demand — handled in frame_tick).
            let copy_on_select = self.config.copy_on_select;
            let theme = &self.theme;
            // The configured TERM, read alongside the other LIVE config reads so a
            // deferred-first-spawn pane advertises the same `TERM` as later panes.
            let term = self.config.term.as_str();
            let font_size = self.config.font.size;
            // Read the line-height LIVE from the config so a Settings change
            // reflows the row pitch (and the PTY rows/cursor/highlight) without a
            // relaunch. Folded into the row pitch by [`effective_row_pitch`].
            let line_height_px = self.config.font.line_height;
            let cursor_cfg = self.config.cursor;
            // CRT scanlines + chromatic aberration, read LIVE so toggling them in
            // Settings takes effect this frame; both are zero-cost when off/zero.
            let effects = self.config.effects;
            // While the atlas-warmup gate is open, render panes WITHOUT their grid
            // glyphs (captured as a plain `bool` here so the disjoint-borrow
            // closure need not touch `self`). This holds every glyph draw off until
            // the warmed atlas is uploaded + GPU-resident (see `warmup_frames_left`
            // + the `ui` poll), closing the DX12 upload↔sample race.
            let warming = self.warmup_frames_left > 0;
            // Pane background alpha: full when opaque, opacity-folded when the
            // window is effectively translucent — painting the pane fill
            // non-opaque is what lets the OS blur / desktop show through (the
            // transparency fix). Read LIVE so the opacity slider applies without
            // a relaunch.
            let bg_alpha = pane_bg_alpha(&self.config);
            // Read the inner padding LIVE from the config so a Settings change
            // moves the grid inset without a relaunch (it was a hardcoded 4px
            // before). `u16` config → f32 points for the painter.
            let padding = f32::from(self.config.window.padding);
            let search_spans = &search_spans;
            let link_spans = &link_spans;
            let empty_links: &[(CellSpan, String)] = &[];
            // The active IME pre-edit, borrowed for the focused pane only (F3-1).
            // A `&str` borrow of `self.ime_preedit` is disjoint from the field
            // borrows above and from `grid_tree`, so it joins the closure cleanly.
            let ime_preedit = self.ime_preedit.as_deref();
            let ime_rect_out = &mut ime_cursor_rect;
            let mut render_body = |ui: &mut egui::Ui, pid: PaneId| -> bool {
                let search = if pid == focused && !search_spans.is_empty() {
                    Some(SearchHighlight {
                        spans: search_spans,
                        selected: search_sel,
                    })
                } else {
                    None
                };
                // Hyperlinks are interactive on the FOCUSED pane only (the others
                // get an empty slice → no underline, no hit test).
                let links: &[(CellSpan, String)] = if pid == focused {
                    link_spans
                } else {
                    empty_links
                };
                let outcome = Self::render_pane_body(
                    ui,
                    pid,
                    pid == focused,
                    terms,
                    pending_spawn,
                    restored_cwds,
                    galley_cache,
                    image_textures,
                    theme,
                    term,
                    font_size,
                    line_height_px,
                    cursor_cfg,
                    effects,
                    padding,
                    bg_alpha,
                    search,
                    links,
                    // Link click/all-underline gated to the focused pane with the
                    // modifier held; the hover underline shows regardless (but
                    // only the focused pane has a non-empty `links` slice).
                    link_modifier && pid == focused,
                    if pid == focused { ime_preedit } else { None },
                    selection,
                    warming,
                );
                if outcome.clicked {
                    clicked = Some(pid);
                }
                if let Some(url) = outcome.opened_url {
                    opened_url = Some(url);
                }
                if let Some(act) = outcome.context_menu_action {
                    pending_ctx_action = Some(act);
                }
                frame_pane_rects.insert(pid, outcome.body_rect);
                // Copy-on-select: a just-completed selection goes to the OS
                // clipboard only when the user enabled it.
                if let Some(text) = outcome.copy_selection {
                    if copy_on_select {
                        ui.ctx().copy_text(text);
                    }
                }
                if pid == focused {
                    focused_size = Some((outcome.size.x, outcome.size.y));
                    // The focused pane's caret rect drives IME candidate-window
                    // placement (set on the context after this block closes).
                    *ime_rect_out = outcome.ime_cursor_rect;
                }
                outcome.drag_started
            };
            // Pane shell layout (#30):
            // - Grid: drive the egui_tiles tree → every pane visible.
            // - Tabs: render ONLY the focused pane, full-size, in the content
            //   area. The tab strip (in the titlebar) stays the pane switcher;
            //   the multi-pane egui_tiles layout is skipped entirely this frame.
            //   The grid tree is NOT mutated, so flipping back to Grid restores
            //   the exact prior layout.
            if let Some(zoomed) = zoomed_pane {
                // Zoom-pane (Ctrl/Cmd+Shift+Z): render ONLY the zoomed pane
                // full-size (siblings hidden), like Tabs mode but for the explicit
                // single-pane zoom toggle. The grid tree is NOT mutated, so
                // un-zooming restores the exact prior layout. `zoomed_pane` was
                // already filtered to a still-live pane above.
                render_body(ui, zoomed);
            } else if view_mode == c0pl4nd_core::config::ViewMode::Tabs {
                // The focused pane must exist in the tree; if it somehow does not
                // (defensive — focus is always re-anchored to a live pane), fall
                // back to the first pane so the content area is never blank.
                let show = if grid::tile_of_pane(&pre, focused).is_some() {
                    focused
                } else {
                    titles.first().map(|(id, _)| *id).unwrap_or(focused)
                };
                render_body(ui, show);
            } else {
                let mut behavior = GridBehavior {
                    titles: &titles,
                    render_body: &mut render_body,
                    close_requests: &mut closes,
                };
                self.grid_tree.ui(&mut behavior, ui);
            }
        }
        // Prune galley-cache rows not painted this frame (rows that scrolled off,
        // a pane switched away from in Tabs view, or a closed pane) so the cache
        // tracks only the live grid and cannot grow without bound (audit #2).
        self.galley_cache.prune_unseen();
        self.image_textures.prune_unseen();
        if let Some(s) = focused_size {
            self.last_focused_size = Some(s);
        }
        // Tell the OS where the IME candidate window should appear (F3-1): the
        // focused pane's terminal-cursor cell. Without this, `output.ime` stays
        // `None` (the grid is a custom-painted region, not an egui `TextEdit`,
        // so egui never sets it for us) and the candidate window anchors at the
        // screen origin or fails to appear. Setting `rect` (the cell) and
        // `cursor_rect` (the caret) drives winit's `set_ime_cursor_area`.
        if let Some(cursor_rect) = ime_cursor_rect {
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    rect: cursor_rect,
                    cursor_rect,
                });
            });
            // Feed the cursor ghost-trail motion overlay: push a new echo only
            // when the focused cursor CELL actually moved (so a blinking-but-still
            // cursor doesn't stack dozens of coincident echoes), and only while the
            // effect is enabled AND motion is not reduced. Bounded to 24 echoes;
            // stale ones are pruned at the paint site each frame. The `else` clears
            // the deque the instant the effect (or motion) is turned off, so no
            // stale echoes linger to pop back on re-enable.
            if self.config.effects.animations_enabled
                && self.config.effects.cursor_trail
                && !c0pl4nd_core::reduced_motion::reduced_motion()
            {
                let now = ui.ctx().input(|i| i.time);
                let moved = self
                    .cursor_trail
                    .back()
                    .is_none_or(|(r, _)| r.min.distance(cursor_rect.min) > 0.5);
                if moved {
                    self.cursor_trail.push_back((cursor_rect, now));
                    while self.cursor_trail.len() > 24 {
                        self.cursor_trail.pop_front();
                    }
                }
            } else if !self.cursor_trail.is_empty() {
                self.cursor_trail.clear();
            }
        }
        // Record a Ctrl-clicked URL (the browser open already fired in-render);
        // most-recent-wins, observable for the interaction test.
        if let Some(url) = opened_url {
            self.last_opened_url = Some(url);
        }
        // Record this frame's per-pane body rects for directional pane focus.
        // In Tabs / zoom mode only the single visible pane is captured (so
        // directional focus finds no neighbour — correct, there is only one).
        self.pane_rects = frame_pane_rects;

        // Enforce the cap: a drag-to-split that pushed us over 6 reverts.
        if count_panes(&self.grid_tree) > grid::MAX_PANES {
            self.grid_tree = pre;
            self.toast = Some(format!(
                "You've reached the maximum of {} panes. Close one to open another.",
                grid::MAX_PANES
            ));
        }

        if let Some(pid) = clicked {
            if pid != self.focused_pane {
                self.input_line.clear(); // the typed-line accumulator is per-pane
            }
            self.focused_pane = pid;
        }

        // Apply a queued right-click context-menu action (split / new / close)
        // now that the egui_tiles render closure has released its borrows and
        // `self` is available again (Copy + Clear ran inline in the menu).
        if let Some(action) = pending_ctx_action {
            self.apply_context_menu_action(action);
        }

        // Apply close requests; keep at least one pane alive. Drop the closed
        // pane's terminal (PTY + reader thread) so it does not leak.
        for pid in closes {
            self.close_pane(pid);
        }
    }

    /// Toggle zoom on the focused pane: when off, zoom the focused pane (render
    /// it full-size, siblings hidden); when on, un-zoom (restore the full
    /// layout). The grid tree is never mutated — zoom is a pure render override —
    /// so un-zooming restores the exact prior layout.
    fn toggle_zoom_pane(&mut self) {
        self.zoomed_pane = if self.zoomed_pane.is_some() {
            None
        } else {
            Some(self.focused_pane)
        };
    }

    /// The currently zoomed pane, if any (Ctrl/Cmd+Shift+Z). Exposed so the
    /// interaction test can assert the toggle's observable state.
    #[allow(dead_code)]
    pub fn zoomed_pane(&self) -> Option<PaneId> {
        self.zoomed_pane
    }

    /// The active mouse selection as `(anchor, head, is_block)`, if any. Exposed
    /// so an interaction test can assert a drag produced a (block-or-line)
    /// selection.
    #[allow(dead_code)]
    pub fn test_selection(&self) -> Option<TestSelection> {
        self.selection
            .map(|s| (s.anchor, s.head, s.mode == SelectionMode::Block))
    }

    /// The bytes forwarded to the focused PTY on the most recent no-overlay
    /// frame. Exposed so a test can assert a consumed chord leaked nothing.
    #[allow(dead_code)]
    pub fn test_last_forwarded(&self) -> &[u8] {
        &self.last_forwarded
    }

    /// The pane geometrically adjacent to `focus` in `dir`, using the body rects
    /// captured during the last grid render. Delegates to the pure
    /// [`neighbor_in_rects`] (unit-tested against synthetic layouts).
    fn neighbor_pane(&self, focus: PaneId, dir: Direction) -> Option<PaneId> {
        neighbor_in_rects(&self.pane_rects, focus, dir)
    }

    /// Move keyboard focus to the pane adjacent to the focused pane in `dir`
    /// (Ctrl/Cmd+Shift+Arrow). A no-op when there is no neighbour in that
    /// direction. Clears the per-pane typed-line accumulator on a real move.
    fn focus_directional(&mut self, dir: Direction) {
        if let Some(neighbor) = self.neighbor_pane(self.focused_pane, dir) {
            if neighbor != self.focused_pane {
                self.input_line.clear();
                self.focused_pane = neighbor;
            }
        }
    }

    /// Apply a right-click context-menu action that needs `&mut self` (it mutates
    /// the tiles tree): split the focused pane, open a new tab, or close a pane.
    /// Copy + Clear-scrollback are NOT routed here — they run inline in the menu
    /// closure (they only touch `terms`). Extracted from `frame_tick` so the
    /// action→effect mapping is unit-testable without driving the egui menu UI.
    fn apply_context_menu_action(&mut self, action: ContextMenuAction) {
        match action {
            ContextMenuAction::SplitRight => self.split(egui_tiles::LinearDir::Horizontal),
            ContextMenuAction::SplitDown => self.split(egui_tiles::LinearDir::Vertical),
            ContextMenuAction::NewTerminal => self.new_terminal(),
            ContextMenuAction::ClosePane(pid) => self.close_pane(pid),
        }
    }

    /// Close one pane: remove its tile + terminal (PTY + reader thread), drop its
    /// pinned state, and re-anchor focus if the focused pane was the one closed.
    /// Keeps at least one pane alive — the last pane is never closed. Shared by
    /// the egui_tiles close button (via `grid_ui`) and the tab-bar × (via
    /// `frame_tick`).
    fn close_pane(&mut self, pid: PaneId) {
        if count_panes(&self.grid_tree) <= 1 {
            return;
        }
        let Some(tile) = grid::tile_of_pane(&self.grid_tree, pid) else {
            return;
        };
        self.grid_tree.tiles.remove(tile);
        self.grid_tree.simplify_children_of_tile(
            self.grid_tree.root.unwrap_or(tile),
            &egui_tiles::SimplificationOptions::default(),
        );
        self.terms.remove(&pid);
        self.pinned.remove(&pid);
        // A selection holds grid coordinates of a now-removed pane; drop it so it
        // cannot paint against a different pane after the focus re-anchors below.
        self.selection = None;
        // Drop a stale zoom on the closed pane so the next frame does not try to
        // render a pane that no longer exists (it would fall through to the grid).
        if self.zoomed_pane == Some(pid) {
            self.zoomed_pane = None;
        }
        // Re-anchor focus if the focused pane was closed.
        if grid::tile_of_pane(&self.grid_tree, self.focused_pane).is_none() {
            if let Some((p, _)) = self.pane_titles().first() {
                self.focused_pane = *p;
            }
        }
    }

    /// The settings window (Milestone 2): a grouped, well-spaced, searchable
    /// two-pane window matching the sibling SCR1B3 editor's layout. Delegates the
    /// whole UI to the [`settings`] module (a free function so it never fights
    /// `self`'s borrow), then live-applies any change: persist the config to
    /// disk, reload the terminal color theme when the theme stem changed (so the
    /// live panes repaint, not just the chrome), and re-apply the egui Visuals.
    /// Union `r` into [`Self::overlay_exclude_rect`] — the bounding rect of the
    /// open centered chrome panels this frame, which the whole-window motion
    /// overlays paint AROUND. Reset to `None` each frame before the panels draw;
    /// each open panel calls this after it renders. No-op when `r` is `None`.
    fn note_overlay_rect(&mut self, r: Option<egui::Rect>) {
        if let Some(r) = r {
            self.overlay_exclude_rect = Some(match self.overlay_exclude_rect {
                Some(existing) => existing.union(r),
                None => r,
            });
        }
    }

    /// When `follow_os_theme` is on, swap between the default dark (`itasha-corp`)
    /// and light (`ghost-paper`) themes to match the OS appearance — but ONLY on
    /// an actual OS-appearance CHANGE since the last observed frame, NOT every
    /// frame. Between OS changes a MANUAL theme pick (combo / arrows / name field)
    /// therefore STICKS until the OS theme actually flips.
    ///
    /// This is the SCR1B3-parity behaviour (`frame_tick.rs`: re-apply only when
    /// `Some(os_theme) != last_os_theme`). The previous C0PL4ND implementation
    /// reasserted the OS-derived theme on every frame whenever it differed, so a
    /// manual pick reverted on the very next frame — a divergence from SCR1B3 and
    /// a confusing UX. egui reports the OS appearance via `ctx.system_theme()`; an
    /// unknown value resolves to the dark default (the app default), so the first
    /// observation still applies. Toggling the switch OFF forgets the tracked
    /// appearance so re-enabling re-applies on the next observed frame.
    fn follow_os_theme_tick(&mut self, ctx: &egui::Context) {
        if !self.config.follow_os_theme {
            // Forget the tracked OS appearance so a later re-enable re-applies the
            // OS theme on its next observation instead of being suppressed by a
            // stale match.
            self.last_os_theme = None;
            return;
        }
        // Resolve the OS appearance to a concrete dark/light (unknown → dark, the
        // app default). Re-apply ONLY when it CHANGED since the last observation;
        // the `Some(..)` wrap makes the first observation (`last_os_theme == None`)
        // count as a change so the initial OS theme is applied.
        let os_theme = ctx.system_theme().unwrap_or(egui::Theme::Dark);
        if Some(os_theme) == self.last_os_theme {
            return;
        }
        self.last_os_theme = Some(os_theme);
        let desired = match os_theme {
            egui::Theme::Light => "ghost-paper",
            egui::Theme::Dark => "itasha-corp",
        };
        if self.config.theme == desired {
            return;
        }
        self.config.theme = desired.to_string();
        let (theme, notice) = load_terminal_theme(&self.config);
        self.theme = theme;
        if let Some(notice) = notice {
            self.toast = Some(notice);
        }
        for term in self.terms.values_mut() {
            term.set_theme(self.theme.clone());
        }
        let mut visuals = theme::visuals_from_theme(&self.theme);
        window_effects::apply_window_opacity(&mut visuals, self.config.opacity);
        ctx.set_visuals(visuals);
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        // Theme-derived palette so the settings window fill + headings follow
        // the active theme along with the rest of the chrome.
        let colors = theme::ChromeColors::from_theme(&self.theme);
        // Consume the place-on-open flag: `show` force-positions the window this
        // one frame, then it is freely movable.
        let place_now = self.settings_place_pending;
        self.settings_place_pending = false;
        let outcome = settings::show(
            ctx,
            &mut self.config,
            &mut open,
            colors,
            self.incognito,
            place_now,
        );
        self.settings_open = open;
        // Record the window rect so the whole-window motion overlays exclude it
        // this frame — a live Motion-setting preview shows on the terminal without
        // washing over the settings panel (the overlay block reads this below).
        self.note_overlay_rect(outcome.window_rect);

        // Privacy-section actions (runtime, not config): handle before the
        // config-changed persistence below.
        if outcome.clear_history {
            self.clear_command_history();
        }
        if let Some(on) = outcome.set_incognito {
            self.set_incognito(on);
        }

        if outcome.changed {
            // Reload the terminal grid's color theme so a theme change shows in
            // the live PTY panes immediately (the chrome Visuals are re-applied
            // below; the grid glyph colours come from this `Theme`, not Visuals).
            if outcome.theme_changed {
                let (theme, theme_notice) = load_terminal_theme(&self.config);
                self.theme = theme;
                if let Some(notice) = theme_notice {
                    // A user-authored theme file existed but failed to parse —
                    // surface it instead of silently showing fallback colours.
                    self.toast = Some(notice);
                }
                // Propagate to the LIVE panes: each PaneTerm holds its own theme
                // clone (glyph + background colours resolve from it), so without
                // this the picker would change `self.theme` but no visible pane.
                for term in self.terms.values_mut() {
                    term.set_theme(self.theme.clone());
                }
            }
            // Re-apply the chrome Visuals DERIVED FROM the (possibly changed)
            // terminal theme so the WHOLE app UI — titlebar, tabs, status bar,
            // settings window, panel fills — follows the picked theme (a light
            // theme flips the chrome light, a dark one dark) without waiting for
            // a relaunch. `self.theme` was reloaded just above on a theme change.
            let mut visuals = theme::visuals_from_theme(&self.theme);
            window_effects::apply_window_opacity(&mut visuals, self.config.opacity);
            ctx.set_visuals(visuals);
            // Live-apply the always-on-top window level so flipping the toggle
            // takes effect without a relaunch (mirrors the opacity live-apply
            // just above). Setting the level to its current value is idempotent,
            // so re-sending it on any settings change is harmless.
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                if self.config.always_on_top {
                    egui::WindowLevel::AlwaysOnTop
                } else {
                    egui::WindowLevel::Normal
                },
            ));
            // Persist to the platform config file so the change survives a
            // relaunch — but ONLY in a real window. The headless `egui_kittest`
            // harness sets `live_window == false`; persisting there would write
            // the user's real `%APPDATA%\c0pl4nd\config.toml` from a test run
            // (test pollution). The live in-memory apply above is what the tests
            // observe; the disk write is a real-window-only side effect.
            // Best-effort: a write failure (e.g. read-only config dir) never
            // blocks the live in-memory apply.
            if self.live_window {
                if let Some(path) = c0pl4nd_core::Config::default_path() {
                    // Surface a persist failure (read-only %APPDATA%, full disk,
                    // permission error) instead of silently dropping the user's
                    // settings change — mirrors the legacy shell (window.rs). A
                    // GUI user never sees stderr, so a visible toast (the same
                    // channel the config-LOAD error uses) is the real surface.
                    if let Err(e) = self.config.save_to(&path) {
                        self.toast = Some(crate::user_error::config_save_failed(
                            e,
                            "Your settings change",
                        ));
                    }
                }
            }
        }
    }

    // ---- settings observation surface (production accessors, NOT test-only) ----
    //
    // Real accessors the `egui_kittest` settings tests use to assert observable
    // Config/theme changes after driving the REAL `settings::show` through
    // `frame_tick`. Deliberately not `#[cfg(test)]` so the test exercises the
    // exact production path (the same observation-accessor discipline the other
    // public accessors above follow).

    /// Whether a paste is currently awaiting confirmation. (Test / observation
    /// API for the paste-safety overlay.)
    #[allow(dead_code)]
    pub fn has_pending_paste(&self) -> bool {
        self.pending_paste.is_some()
    }

    /// Which gate deferred the pending paste (multi-line vs oversized), or
    /// `None` when nothing is pending. Observation API so a test can assert the
    /// SIZE gate fired rather than merely that *something* was deferred.
    #[allow(dead_code)]
    pub fn pending_paste_reason(&self) -> Option<c0pl4nd_core::paste_guard::PasteConfirmReason> {
        self.pending_paste_reason
    }

    /// Send the deferred paste to the focused pane through the core
    /// paste-injection guard, then clear it. Returns the text that was sent (for
    /// tests; `None` if nothing was pending). The OS side effect aside, this is
    /// the same path a non-deferred paste takes.
    pub fn confirm_pending_paste(&mut self) -> Option<String> {
        let text = self.pending_paste.take()?;
        self.pending_paste_reason = None;
        if let Some(term) = self.terms.get_mut(&self.focused_pane) {
            term.write_paste(&text);
        }
        Some(text)
    }

    /// Discard the deferred paste without sending it.
    pub fn cancel_pending_paste(&mut self) {
        self.pending_paste = None;
        self.pending_paste_reason = None;
    }

    /// Whether a just-typed line should be recorded in command history. PRIVACY:
    /// the history feeds the palette + sidebar and must never capture secrets the
    /// user never meant to store. Two guards:
    ///
    /// 1. **Leading-space opt-out** (the HISTCONTROL=ignorespace convention): a
    ///    line the user prefixed with a space/tab is intentionally excluded.
    /// 2. **Password-prompt suppression** (the load-bearing one): a password
    ///    typed at `sudo` / `ssh` / `mysql -p` is NOT echoed by the tty, so its
    ///    characters never reach the grid. We record a line only if a short
    ///    prefix of it was ECHOED into the focused pane's visible text. A
    ///    non-echoed line (zero echo = password) is dropped. The prefix (the
    ///    earliest-typed chars, which have had the most time to round-trip
    ///    through the PTY) tolerates trailing-echo lag while still catching a
    ///    fully-unechoed secret. Privacy-conservative: when in doubt, drop —
    ///    losing a history entry is acceptable; storing a password is not.
    ///
    /// Inline secrets that ARE echoed (`--password=…`, `API_KEY=…`) are redacted
    /// downstream by [`c0pl4nd_core::command_history::redact_secrets`].
    fn should_record_history(&self, line: &str) -> bool {
        // Privacy controls: capture disabled in settings, or an incognito session.
        if !self.config.history_capture_enabled || self.incognito {
            return false;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return false;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return false;
        }
        const PROBE_LEN: usize = 4;
        let probe: String = trimmed.chars().take(PROBE_LEN).collect();
        self.focused_grid_text()
            .is_some_and(|grid| grid.contains(&probe))
    }

    /// The multi-line-paste confirm overlay: a small centred modal showing how
    /// many lines the paste is and a preview, with Send / Cancel. Defends against
    /// the "paste a multi-line command that runs on the embedded newline" footgun
    /// — the paste does not reach the PTY until the user confirms. Enter = send,
    /// Esc = cancel (also handled here so the modal is keyboard-drivable).
    fn paste_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(text) = self.pending_paste.clone() else {
            return;
        };
        // Keyboard: Esc cancels, Enter (or Ctrl+Enter) sends.
        let (send, cancel) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if cancel {
            self.cancel_pending_paste();
            return;
        }
        if send {
            self.confirm_pending_paste();
            return;
        }

        let line_count = text.lines().count().max(1);
        // A short, control-stripped preview so the modal itself can't be used to
        // smuggle escape sequences into the chrome.
        let preview: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(400)
            .collect();

        let mut do_send = false;
        let mut do_cancel = false;
        let win = egui::Window::new("Paste multiple lines?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "This paste contains {line_count} lines and may run commands as soon as it lands."
                ));
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&preview).monospace())
                            .wrap(),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Send paste (Enter)").clicked() {
                        do_send = true;
                    }
                    if ui.button("Cancel (Esc)").clicked() {
                        do_cancel = true;
                    }
                });
            });
        // Exclude the confirm modal from the whole-window motion overlays this frame.
        self.note_overlay_rect(win.map(|w| w.response.rect));
        if do_send {
            self.confirm_pending_paste();
        } else if do_cancel {
            self.cancel_pending_paste();
        }
    }

    /// The current inner window padding (points) from the live config — the
    /// value `grid_ui` threads into the grid paint each frame, so it reflects
    /// what the terminal grid is actually inset by. Observation accessor for the
    /// padding live-apply interaction test.
    #[allow(dead_code)]
    pub fn config_window_padding(&self) -> u16 {
        self.config.window.padding
    }

    /// The grid text origin a focused pane WOULD draw at for a given body
    /// `rect`, using the LIVE config padding — exercising the exact
    /// [`grid_text_origin`] helper the production paint path uses. Lets the
    /// interaction test prove a Padding change moves the rendered origin (not
    /// just the stored config value), with no GPU. Pure read of live state.
    #[allow(dead_code)]
    pub fn grid_text_origin_for(&self, rect: egui::Rect) -> egui::Pos2 {
        grid_text_origin(rect, f32::from(self.config.window.padding))
    }

    // ---- command palette (quick find/run previously-run commands) ----
    //
    // The palette surfaces `cmd_history` (commands the user typed + ran in any
    // pane this session) and lets them fuzzy-search and re-run one with Enter.
    // It is opened with Ctrl+Shift+P (handled in `frame_tick`). These methods are
    // the production logic the frame loop calls — the interaction tests drive
    // them through the real frame loop, NOT as a test-only mirror.

    /// Toggle the command palette. Opening it resets the query, selection, and
    /// the in-flight typed-line accumulator (so a half-typed line is not later
    /// recorded as if it had been run after the palette closes).
    fn toggle_palette(&mut self) {
        self.palette_open = !self.palette_open;
        if self.palette_open {
            self.palette_query.clear();
            self.palette_sel = 0;
            self.input_line.clear();
        }
    }

    /// The palette's filtered rows for the current query.
    ///
    /// The palette lists BOTH previously-run shell commands and the shell's own
    /// [`Action`]s, so it can actually DO things (new tab, split, settings, font
    /// size, theme-independent view flip …) rather than only re-run history:
    ///
    /// - a query starting with `>` filters to ACTIONS ONLY (the VS Code
    ///   convention), so the action list is one keystroke away no matter how
    ///   long the history is;
    /// - otherwise the history matches come first (the palette's original job,
    ///   most-recent-first / fuzzy-filtered) followed by the matching actions,
    ///   so a fresh session with no history opens straight onto the actions.
    fn palette_results(&self) -> Vec<PaletteEntry> {
        if let Some(rest) = self.palette_query.trim_start().strip_prefix('>') {
            return Self::matching_actions(rest.trim_start())
                .into_iter()
                .map(PaletteEntry::Action)
                .collect();
        }
        let mut rows: Vec<PaletteEntry> = self
            .cmd_history
            .search(&self.palette_query)
            .into_iter()
            .map(PaletteEntry::History)
            .collect();
        rows.extend(
            Self::matching_actions(&self.palette_query)
                .into_iter()
                .map(PaletteEntry::Action),
        );
        rows
    }

    /// The actions whose labels fuzzy-match `query`, best-scoring first with
    /// declaration order as the stable tiebreak. An empty query scores every
    /// action 0, so the list keeps [`Action::ALL`] order.
    fn matching_actions(query: &str) -> Vec<Action> {
        let mut scored: Vec<(i32, usize, Action)> = Action::ALL
            .iter()
            .enumerate()
            .filter_map(|(i, a)| c0pl4nd_core::fuzzy::score(a.label(), query).map(|s| (s, i, *a)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, a)| a).collect()
    }

    /// Move the palette selection by `delta` rows, clamped to the result range.
    /// A no-op when there are no results.
    fn palette_move(&mut self, delta: i64) {
        let n = self.palette_results().len();
        if n == 0 {
            self.palette_sel = 0;
            return;
        }
        let max = n as i64 - 1;
        let cur = self.palette_sel as i64;
        self.palette_sel = (cur + delta).clamp(0, max) as usize;
    }

    /// Run the currently-selected history entry in the focused pane: write it to
    /// the PTY followed by a carriage return (what the shell sees for Enter),
    /// move it to the front of the history, and close the palette. Returns the
    /// command run (for tests). Closes the palette with no command when the
    /// result set is empty.
    fn run_palette_selection(&mut self, ctx: &egui::Context) -> Option<String> {
        let entry = self.palette_results().get(self.palette_sel).cloned();
        let ran = match &entry {
            Some(PaletteEntry::History(cmd)) => {
                self.run_command_in_focused(cmd);
                self.last_palette_run = Some(cmd.clone());
                Some(cmd.clone())
            }
            Some(PaletteEntry::Action(action)) => {
                // The SAME dispatch path the keybinding dispatcher uses — the
                // palette is a second surface onto one action layer, never a
                // second implementation of it.
                self.dispatch_action(*action, ctx);
                self.last_palette_run = None;
                self.last_palette_action = Some(*action);
                Some(action.label().to_string())
            }
            None => {
                self.last_palette_run = None;
                None
            }
        };
        // Closing AFTER the dispatch means the "Command palette" action itself
        // (which toggles the flag) still ends with the palette closed.
        self.palette_open = false;
        ran
    }

    /// Write `cmd` followed by a carriage return (what the shell sees for Enter)
    /// to the focused pane's PTY, and move `cmd` to the front of the history (no
    /// duplicate). The single run path shared by the command palette
    /// ([`Self::run_palette_selection`]) and the history sidebar
    /// ([`Self::run_history_command`]) so both surfaces re-run a command
    /// identically.
    fn run_command_in_focused(&mut self, cmd: &str) {
        if let Some(term) = self.terms.get_mut(&self.focused_pane) {
            term.write_bytes(cmd.as_bytes());
            term.write_bytes(b"\r");
        }
        // Re-running moves the command to the front (no duplicate).
        self.cmd_history.record(cmd.to_string());
    }

    /// Open a native file picker (#35) and, on a pick, RUN the chosen script in
    /// the focused pane by feeding its PATH to the shell as a command — the
    /// shell then executes it via its own shebang/interpreter dispatch. Reading
    /// and injecting the file's lines instead would bypass the shebang, mangle
    /// multi-line scripts, and flood the shell's line history. The path is
    /// quoted for the active shell ([`quote_path_for_shell`]: PowerShell's call
    /// operator `& "…"`, else a `'…'`/`"…"`-quoted path). The blocking
    /// `pick_file()` is fine here — it is called from the post-panel action
    /// block (every panel has already closed) and the OS dialog runs its own
    /// modal loop, so no egui borrow is held and no animation is in flight.
    fn open_script_file(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Run a script in the focused terminal")
            .add_filter(
                "Scripts",
                &["sh", "ps1", "bat", "cmd", "py", "js", "rb", "fish", "zsh"],
            )
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            let quoted = quote_path_for_shell(&path, self.active_shell_label());
            self.run_command_in_focused(&quoted);
        }
    }

    // ---- command-history quick-run sidebar (#21) -------------------------
    //
    // A toggleable docked `egui::SidePanel` (side from `config.history_sidebar_
    // side`) listing the command history newest-first with a filter box. Clicking
    // a row re-runs it in the focused pane via the SAME `run_command_in_focused`
    // path the command palette uses. Opened/closed with Ctrl+Shift+H (handled in
    // `frame_tick`, with the chord filtered out of the PTY input stream).

    /// Toggle the command-history sidebar. Opening it clears the stale filter so
    /// the full history shows first.
    fn toggle_history_sidebar(&mut self) {
        self.history_open = !self.history_open;
        if self.history_open {
            self.history_filter.clear();
        }
    }

    /// Run `cmd` from the history sidebar: the same focused-pane run + history
    /// re-order path the palette uses, recorded in `last_palette_run` so an
    /// interaction test can assert the click ran the real command (reusing the
    /// palette's observable). Closes the sidebar after a run.
    fn run_history_command(&mut self, cmd: &str) {
        self.run_command_in_focused(cmd);
        self.last_palette_run = Some(cmd.to_string());
        self.history_open = false;
    }

    /// Whether the command-history sidebar is currently open. Observation
    /// accessor for the toggle interaction test.
    #[allow(dead_code)]
    pub fn history_sidebar_open(&self) -> bool {
        self.history_open
    }

    /// Which side the history sidebar docks to (from the live config).
    /// Observation accessor for the side-preference test.
    #[allow(dead_code)]
    pub fn history_sidebar_side(&self) -> c0pl4nd_core::config::PanelSide {
        self.config.history_sidebar_side
    }

    /// The history rows the sidebar would show for the current filter — every
    /// entry (most-recent-first) when the filter is empty, fuzzy-filtered
    /// otherwise. Pure read shared by the render and the click test.
    fn history_sidebar_rows(&self) -> Vec<String> {
        let f = self.history_filter.trim();
        if f.is_empty() {
            self.cmd_history.entries().map(str::to_string).collect()
        } else {
            self.cmd_history.search(f)
        }
    }

    /// Render the command-history quick-run sidebar as a docked, resizable
    /// `egui::SidePanel` on the configured side. Only called when `history_open`
    /// — a closed sidebar is NOT `.show`n, so the central terminal reflows to the
    /// full width (the "true popout" behaviour). A filter box sits at the top;
    /// below it the history is listed newest-first as clickable rows (failed
    /// vs ok styling is out of scope — the history holds commands, not exit
    /// codes). Clicking a row runs it via [`Self::run_history_command`].
    // egui 0.34 deprecated the top-level `SidePanel::show(ctx, …)` form in favour
    // of `show_inside(ui, …)`, but this frameless app shows its panels straight
    // from the `ctx` in `frame_tick` (there is no parent `&mut Ui` at this level
    // — same rationale as the titlebar/status TopBottomPanels). Allow it here as
    // those panels do.
    #[allow(deprecated)]
    fn history_sidebar(&mut self, ctx: &egui::Context, colors: theme::ChromeColors) {
        let rows = self.history_sidebar_rows();
        let history_empty = self.cmd_history.is_empty();
        let mut clicked: Option<String> = None;
        let mut close_requested = false;

        let mut body = |ui: &mut egui::Ui, filter: &mut String| {
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("History").color(colors.fg));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(egui::RichText::new(egui_phosphor::thin::X).size(14.0))
                        .on_hover_text("Close history (Ctrl+Shift+H)")
                        .clicked()
                    {
                        close_requested = true;
                    }
                });
            });
            ui.add(
                egui::TextEdit::singleline(filter)
                    .hint_text("filter…")
                    .desired_width(f32::INFINITY),
            );
            ui.separator();
            if rows.is_empty() {
                ui.weak(if history_empty {
                    "No commands run yet."
                } else {
                    "No matches."
                });
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for cmd in &rows {
                            // A full-width clickable row in the chosen font; click
                            // re-runs it in the focused pane.
                            let resp = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(cmd)
                                        .color(colors.fg)
                                        .family(egui::FontFamily::Monospace),
                                )
                                .sense(egui::Sense::click())
                                .wrap(),
                            );
                            if resp.clicked() {
                                clicked = Some(cmd.clone());
                            }
                            resp.on_hover_text("Run in the focused pane");
                        }
                    });
            }
            ui.add_space(4.0);
            ui.weak("Ctrl+Shift+H toggles · click a row to run");
        };

        let frame = egui::Frame::new().fill(colors.panel).inner_margin(8.0);
        // Snapshot the filter into a local so the panel closure's `&mut filter`
        // (the TextEdit) does not collide with the immutable `rows`/`self` reads.
        let mut filter = std::mem::take(&mut self.history_filter);
        match self.config.history_sidebar_side {
            c0pl4nd_core::config::PanelSide::Left => {
                egui::SidePanel::left("c0pl4nd_history")
                    .resizable(true)
                    .default_width(260.0)
                    .frame(frame)
                    .show(ctx, |ui| body(ui, &mut filter));
            }
            c0pl4nd_core::config::PanelSide::Right => {
                egui::SidePanel::right("c0pl4nd_history")
                    .resizable(true)
                    .default_width(260.0)
                    .frame(frame)
                    .show(ctx, |ui| body(ui, &mut filter));
            }
        }
        self.history_filter = filter;

        if close_requested {
            self.history_open = false;
        }
        if let Some(cmd) = clicked {
            self.run_history_command(&cmd);
        }
    }

    /// Run the history entry at `index` (newest-first) exactly as a real click
    /// does — through [`Self::run_history_command`]. `pub` for the
    /// `#[path]`-included interaction test (which seeds the history then drives a
    /// row "click"); inert in the shipping binary, which runs rows via real
    /// pointer clicks. Returns the command run, or `None` when `index` is out of
    /// range.
    #[allow(dead_code)]
    pub fn test_run_history_row(&mut self, index: usize) -> Option<String> {
        let cmd = self.history_sidebar_rows().get(index).cloned()?;
        self.run_history_command(&cmd);
        Some(cmd)
    }

    /// Whether the command palette is currently open. Observation accessor for
    /// the interaction tests (asserts Ctrl+Shift+P toggled it through the real
    /// frame loop).
    #[allow(dead_code)]
    pub fn palette_open(&self) -> bool {
        self.palette_open
    }

    /// Whether the app is in frameless terminal-only fullscreen (#36).
    /// Observation accessor for the interaction test (asserts an F11 press
    /// toggled it through the real frame loop, hiding the chrome panels).
    #[allow(dead_code)]
    pub fn fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// The recorded command history, most-recent-first. Observation accessor for
    /// the interaction tests (asserts typed-then-Enter lines were captured).
    #[allow(dead_code)]
    pub fn command_history_entries(&self) -> Vec<String> {
        self.cmd_history.entries().map(str::to_string).collect()
    }

    /// Clear all recorded command history now (the buffers are zeroized). Wired
    /// to the Privacy settings "Clear command history" button.
    pub fn clear_command_history(&mut self) {
        self.cmd_history.clear();
    }

    /// Whether this session is in incognito mode (no command-history capture).
    #[allow(dead_code)]
    pub fn is_incognito(&self) -> bool {
        self.incognito
    }

    /// Toggle incognito (no-history) for this session. Runtime-only; never
    /// persisted. Entering incognito also clears any already-recorded history so
    /// the switch is a clean break.
    pub fn set_incognito(&mut self, on: bool) {
        self.incognito = on;
        if on {
            self.cmd_history.clear();
        }
    }

    /// The command most recently run from the palette, if any. Observation
    /// accessor for the interaction test (asserts Enter on a selection ran the
    /// real command through the real frame loop).
    #[allow(dead_code)]
    pub fn last_palette_run(&self) -> Option<String> {
        self.last_palette_run.clone()
    }

    /// The action most recently dispatched from the palette, if any. Observation
    /// accessor for the palette-dispatch interaction tests.
    #[allow(dead_code)]
    pub fn last_palette_action(&self) -> Option<Action> {
        self.last_palette_action
    }

    /// The command-palette rows for the current query, as display strings.
    /// Observation accessor for the interaction tests (asserts the `>` filter and
    /// the history/action ordering through the real result builder).
    #[allow(dead_code)]
    pub fn palette_row_labels(&self) -> Vec<String> {
        self.palette_results()
            .iter()
            .map(|e| e.display(&self.config.keybindings))
            .collect()
    }

    /// The most recent URL a Ctrl-click opened, or `None`. Observable accessor
    /// for the hyperlink interaction test.
    #[allow(dead_code)]
    pub fn last_opened_url(&self) -> Option<String> {
        self.last_opened_url.clone()
    }

    /// Activate the URL (if any) at grid cell `(row, col)` exactly as a real
    /// Ctrl-click does — record it in [`Self::last_opened_url`] and return it.
    /// This shares the SAME span-build + hit-test path the renderer uses
    /// ([`Self::cell_spans_for_hyperlinks`] + [`link_url_at_cell`]); only the
    /// pixel→cell mapping (unit-tested separately via [`cell_at_pos`]) and the
    /// `ctx.open_url` OS side effect are omitted, neither of which is observable
    /// in the headless harness. `pub` for the `#[path]`-included test binary;
    /// inert in the shipping binary (which never calls it).
    #[allow(dead_code)]
    pub fn test_open_url_at_cell(&mut self, row: usize, col: usize) -> Option<String> {
        let lines = self.focused_search_lines();
        let links = self.cell_spans_for_hyperlinks(&lines);
        let url = link_url_at_cell(&links, row, col)?.to_string();
        self.last_opened_url = Some(url.clone());
        Some(url)
    }

    /// `(check_on_launch, channel)` from the loaded config — read by the binary
    /// entry point to decide whether to spawn the opt-in launch update check and
    /// which release channel to query. Kept here so the check lives outside the
    /// `egui_app` module (whose update *logic* dependency the test binaries do
    /// not carry) — the entry point owns the network call.
    #[allow(dead_code)]
    pub fn update_check_config(&self) -> (bool, String) {
        (
            self.config.update.check_on_launch,
            self.config.update.channel.clone(),
        )
    }

    /// Attach the receiver for a background launch update check. The entry point
    /// spawns the check (the only network surface) and hands the app the channel;
    /// [`Self::frame_tick`] polls it and surfaces a found update as a toast.
    #[allow(dead_code)]
    pub fn attach_update_check(&mut self, rx: std::sync::mpsc::Receiver<String>) {
        self.update_rx = Some(rx);
    }

    /// Surface an update notice: show it as a transient toast and record it
    /// (most-recent-wins) for the interaction test. Shared by the launch-check
    /// poll and the test, so both exercise one path.
    fn apply_update_notice(&mut self, notice: String) {
        self.toast = Some(notice.clone());
        self.last_update_notice = Some(notice);
    }

    /// The most recent update notice surfaced, or `None`. Observable accessor for
    /// the launch-check interaction test.
    #[allow(dead_code)]
    pub fn last_update_notice(&self) -> Option<String> {
        self.last_update_notice.clone()
    }

    /// Poll the launch-check channel (if attached) and surface a received notice
    /// as a toast. Non-blocking; the background thread sends at most one notice.
    fn poll_update_check(&mut self) {
        if let Some(rx) = &self.update_rx {
            if let Ok(notice) = rx.try_recv() {
                self.apply_update_notice(notice);
                self.update_rx = None; // one-shot: stop polling after the notice
            }
        }
    }

    /// Render the command-palette overlay: a centred window with an auto-focused
    /// fuzzy-search box over the command history and a selectable result list.
    /// Clicking a row runs it (same path as Enter). Navigation (↑/↓/Enter/Esc)
    /// is handled in [`Self::frame_tick`] before this renders, so the list here
    /// only needs to display the current query + selection and report a click.
    ///
    /// Immutable state is snapshotted into locals before the window closure so
    /// the closure's `&mut palette_query` (for the `TextEdit`) does not collide
    /// with reads of `palette_sel` / `cmd_history` on the same `self`.
    fn command_palette_window(&mut self, ctx: &egui::Context) {
        let results = self.palette_results();
        // Clamp selection if the result set shrank since the last frame.
        if self.palette_sel >= results.len() {
            self.palette_sel = results.len().saturating_sub(1);
        }
        let sel = self.palette_sel;
        let history_empty = self.cmd_history.is_empty();
        // Render text is resolved BEFORE the window closure so the closure's
        // `&mut palette_query` does not collide with the `keybindings` read.
        let rows: Vec<String> = results
            .iter()
            .map(|e| e.display(&self.config.keybindings))
            .collect();
        let query = &mut self.palette_query;
        let mut clicked: Option<usize> = None;

        let win = egui::Window::new("Command palette")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 80.0))
            .default_width(540.0)
            .show(ctx, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(query)
                        .hint_text("Search commands and actions — type > for actions only…")
                        .desired_width(f32::INFINITY),
                );
                // Keep the search box focused for the palette's whole lifetime so
                // typed characters always populate the query, never the PTY.
                resp.request_focus();
                ui.separator();
                if rows.is_empty() {
                    ui.weak(if history_empty {
                        "No matches — type > to list every action."
                    } else {
                        "No matches."
                    });
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (i, row) in rows.iter().enumerate() {
                                if ui.selectable_label(i == sel, row).clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                }
                ui.separator();
                ui.weak("Up/Down select · Enter run · Esc close · > actions only");
            });
        // Exclude the palette from the whole-window motion overlays this frame.
        self.note_overlay_rect(win.map(|w| w.response.rect));

        if let Some(i) = clicked {
            self.palette_sel = i;
            self.run_palette_selection(ctx);
        }
    }
}

impl eframe::App for C0pl4ndApp {
    /// Frameless window clear color: unconditionally fully transparent
    /// `[0,0,0,0]`. The window is always created transparent-capable, so the
    /// rounded corners and (below opacity 1.0) the desktop show through; the
    /// `opacity` slider is folded into the PANEL fills ([`pane_bg_alpha`]) +
    /// resting chrome ([`window_effects::apply_window_opacity`]), never the clear.
    /// At opacity 1.0 the opaque panels cover the transparent clear (solid look).
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        window_clear_color()
    }

    /// Do NOT persist egui `Memory` to disk (privacy F1).
    ///
    /// eframe's default `App::persist_egui_memory()` is `true`, which serializes
    /// the entire egui [`egui::Memory`] — including every widget's
    /// `TextEditState` and its `Undoer<(CCursorRange, String)>` undo stack — into
    /// `app.ron` under the `with_app_id` storage folder
    /// (`%APPDATA%\com.itashacorp.c0pl4nd\data\app.ron` on Windows;
    /// `~/.local/share/com.itashacorp.c0pl4nd/app.ron` on Linux). The undo stack
    /// stores the ACTUAL typed text, so fragments of the find overlay, the command
    /// palette, and the settings search — all of which are substrings of the
    /// user's scrollback — would land on disk in plaintext RON. Returning `false`
    /// keeps that typed-text undo history entirely in memory.
    ///
    /// Window geometry (position + size) is NOT lost by this: it is persisted
    /// independently via [`c0pl4nd_core::Config::persist_geometry`] into the
    /// config TOML AND by eframe's own `persist_window` native-window state, both
    /// of which are unaffected by `persist_egui_memory`.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    /// Persist the split-pane layout + per-pane cwd so the next launch restores
    /// the user's panes/splits and working directories ([`apply_layout_snapshot`]
    /// reads it back in [`Self::new`]). eframe fires this on a debounced interval
    /// and on exit (the `persistence` feature). Only structural layout state +
    /// already-OSC-7-reported cwds are written — never typed text or scrollback,
    /// so this is consistent with the privacy `persist_egui_memory() == false`
    /// policy. RON, in the app's `with_app_id` data folder (local-only).
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            layout_state::LAYOUT_STORAGE_KEY,
            &self.capture_layout(),
        );
    }

    /// Run the shutdown side effects on EVERY exit path, including an OS-initiated
    /// window close (titlebar ×, Alt+F4). The in-app quit button calls
    /// `prepare_shutdown` itself before `process::exit`, but an OS close skipped
    /// it — so the best-effort config save was lost on that path (only the layout
    /// RON persisted via `save()`). `prepare_shutdown` is idempotent (saving
    /// config twice / clearing already-cleared panes is harmless) and never calls
    /// `process::exit`, so it is safe to invoke here.
    fn on_exit(&mut self) {
        self.prepare_shutdown();
    }

    /// eframe 0.34's `App` main entry is `ui(&mut self, &mut Ui, &mut Frame)`;
    /// the top-level panels are driven through the (deprecated-but-functional)
    /// `Panel::show(ctx, …)` path via a cloned `ctx`, matching the reference
    /// egui app. The work lives in [`frame_tick`](Self::frame_tick) so the
    /// headless tests can drive it without an `eframe::Frame`.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Strip the residual native close (WS_SYSMENU) button each frame so DWM
        // stops drawing a second "×" over our custom titlebar close (self-heals if
        // winit re-asserts the bit on resize/restore; near-zero cost once cleared).
        // No-op on non-Windows and before priming. Min/max are already suppressed
        // at creation (see `new`); this is the only runtime caption touch.
        caption_close::ensure_close_button_stripped();
        // Remember the last UN-maximized inner size so the restore button can
        // return to it explicitly (see `toggle_maximize`). Skip while maximized so
        // the monitor-sized maximized rect is never captured as the restore size;
        // a floor guards against a transient tiny/zero read during a resize.
        {
            let (maxed, inner) = ctx.input(|i| {
                (
                    i.viewport().maximized.unwrap_or(false),
                    i.viewport().inner_rect,
                )
            });
            if !maxed {
                if let Some(r) = inner {
                    let sz = r.size();
                    if sz.x >= 400.0 && sz.y >= 300.0 {
                        self.restore_size = Some(sz);
                    }
                }
            }
        }
        // Atlas-warmup GPU fence: while the warmup gate is open, BLOCK until every
        // previously-submitted GPU op — crucially the prior frame's font-atlas
        // texture upload — is complete before this frame samples the atlas. This
        // reproduces, on the real windowed present path, the queue drain that makes
        // the offscreen render path race-free (the DX12 `write_texture`→sample
        // hazard that garbles the grid). Startup/rare-only; no steady-state cost.
        if self.warmup_frames_left > 0 {
            if let Some(render_state) = frame.wgpu_render_state() {
                let _ = render_state
                    .device
                    .poll(eframe::wgpu::PollType::wait_indefinitely());
            }
        }
        self.frame_tick(&ctx);
        // Advance the warmup gate AFTER this frame drew (so this frame saw the
        // pre-decrement value) and keep repainting until it closes.
        if self.warmup_frames_left > 0 {
            self.warmup_frames_left -= 1;
            ctx.request_repaint();
        }
        // The grid is painted with egui's native text painter inside
        // `render_pane_body` during `frame_tick`; there is no post-frame GPU pass.
    }
}

impl C0pl4ndApp {
    /// Run the necessary on-close cleanup, synchronously and fast, so the Close
    /// handler can immediately `std::process::exit(0)` afterwards instead of
    /// waiting on eframe/wgpu's slow graceful GPU-device + swapchain +
    /// winit-window-destroy teardown (the actual source of the slow-to-close
    /// latency — the PTY teardown is ~2ms and is not the bottleneck).
    ///
    /// Two side effects, in order:
    ///
    /// 1. **Persist config** — the same best-effort `config.save_to(default_path)`
    ///    write the settings-change handler performs, gated on `live_window` so a
    ///    headless test never writes the user's real `%APPDATA%\c0pl4nd\config.toml`
    ///    (test pollution). This is the save-on-close that MUST still happen before
    ///    a fast exit.
    /// 2. **Kill every live shell** — one pass of `PaneTerm::kill_child` over
    ///    every pane (`TerminateProcess`, non-blocking) so all N children
    ///    terminate in parallel. This is the no-orphan guarantee: after this call
    ///    no `cmd.exe` (or other shell) is left running. It deliberately does NOT
    ///    drop the panes (`self.terms.clear()`) — dropping runs the per-pane
    ///    `ClosePseudoConsole` that BLOCKS until each child exits, sequentially,
    ///    which was the slow-to-close latency. `process::exit(0)` runs no
    ///    destructors, so skipping the drop skips that block entirely while the
    ///    kill above still reaps every child.
    ///
    /// Kept separate from the `process::exit(0)` call so tests can exercise the
    /// cleanup (save + child reaping) WITHOUT terminating the test runner.
    /// Persist the live config to the platform config file, best-effort and
    /// real-window-only. Shared by the runtime config mutations that happen
    /// OUTSIDE the settings window (e.g. the Ctrl+wheel / Ctrl+/- font zoom) so
    /// they survive a relaunch exactly like a settings-page change. The headless
    /// `egui_kittest` harness has `live_window == false`, so a test never writes
    /// the user's real `%APPDATA%\c0pl4nd\config.toml` (test pollution). A write
    /// failure surfaces as a toast (the same channel the settings save uses) and
    /// never blocks the live in-memory apply. `what` names the change for the
    /// toast (e.g. "The font size").
    fn persist_config_change(&mut self, what: &str) {
        if !self.live_window {
            return;
        }
        if let Some(path) = c0pl4nd_core::Config::default_path() {
            if let Err(e) = self.config.save_to(&path) {
                self.toast = Some(crate::user_error::config_save_failed(e, what));
            }
        }
    }

    pub fn prepare_shutdown(&mut self) {
        // 1) Persist config — real-window-only, best-effort (a write failure must
        //    never wedge the close path). Mirrors the settings-handler save.
        if self.live_window {
            if let Some(path) = c0pl4nd_core::Config::default_path() {
                // Surface a persist failure instead of silently dropping the
                // user's settings change — mirrors the legacy shell (window.rs).
                if let Err(e) = self.config.save_to(&path) {
                    tracing::warn!("could not save config: {e}");
                }
            }
        }
        // 2) Kill every pane's shell FIRST, in one pass, so all N children
        //    terminate in PARALLEL. This replaces the old `self.terms.clear()`,
        //    which dropped each PaneTerm inline → per-pane `ClosePseudoConsole`
        //    that BLOCKS until that child exits, run SEQUENTIALLY for all N panes
        //    (the "takes a while to close" latency: N × block). Killing every
        //    child up-front means:
        //      * the fast-exit callers (`WindowCmd::Close`, OS close-requested)
        //        then `std::process::exit(0)`, which runs NO destructors — so the
        //        blocking `ClosePseudoConsole` never fires at all, yet no shell is
        //        orphaned because it was just killed here; and
        //      * the graceful `on_exit` path (eframe then drops the app, dropping
        //        `self.terms`) finds every child already gone, so each
        //        `ClosePseudoConsole` returns promptly instead of blocking.
        //    `TerminateProcess` is effectively non-blocking (it requests
        //    termination and returns), so this whole pass is fast regardless of N.
        for term in self.terms.values_mut() {
            term.kill_child();
        }
    }

    /// One per-frame tick of the chrome + grid. Separated from `eframe::App::ui`
    /// so `egui_kittest` can drive it through a `Context` without a `Frame`.
    ///
    /// egui 0.34 deprecated the top-level `Panel::show(ctx, …)` form in favour
    /// of `show_inside(ui, …)`, but `show_inside` needs a parent `&mut Ui` that
    /// the top-level entry does not provide; `show(ctx)` remains the working
    /// top-level path (same compromise the reference app documents).
    #[allow(deprecated)]
    pub fn frame_tick(&mut self, ctx: &egui::Context) {
        // Fast close for an OS-initiated window-close (Alt+F4, taskbar → Close,
        // the system menu). The in-app caption-× already takes the fast path
        // (`WindowCmd::Close` → `prepare_shutdown` + `process::exit(0)`); without
        // this, an OS close falls through to eframe/wgpu's slow graceful
        // GPU-device + swapchain + winit-window teardown — the real source of the
        // slow-to-close latency (the PTY teardown is ~2ms). Mirror the fast path.
        // Gated on `live_window` so the headless egui_kittest harness, which has
        // no real viewport, never calls `process::exit` mid-test.
        if self.live_window && ctx.input(|i| i.viewport().close_requested()) {
            self.prepare_shutdown();
            std::process::exit(0);
        }
        // Alt+F4 close, restored in-app. Removing WS_SYSMENU (to kill the doubled
        // native close button — see `caption_close`) means DefWindowProc no longer
        // translates Alt+F4 into a WM_CLOSE, so egui/winit still delivers the key
        // event but the OS never turns it into a close_requested. Handle it here
        // and take the same fast-exit path as the caption-× / OS close. Gated on
        // `live_window` so the headless harness never calls `process::exit`.
        if self.live_window && ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::F4)) {
            self.prepare_shutdown();
            std::process::exit(0);
        }
        // Follow-OS dark/light (SCR1B3 parity): when enabled, track the OS
        // appearance and swap between the default dark/light themes to match.
        self.follow_os_theme_tick(ctx);
        // Motion master switch (SCR1B3 parity): scale egui's global animation time
        // by the configured UI-transition speed, or zero it for a fully static UI
        // when animations are disabled OR the user requested reduced motion (env or
        // OS). Cheap; applied every frame so a live Settings change (Motion → Enable
        // animations / UI transition speed) takes effect at once. `1.0/12.0` is
        // egui's stock `Style::animation_time`, so the default (enabled, intensity
        // 1.0, no reduced-motion) reproduces the shipped feel exactly, while a
        // reduced-motion preference makes egui's own chrome fades instant too. The
        // `animation_intensity` factor now governs ONLY these chrome transitions —
        // the retro overlays each carry their own per-effect drift-speed multiplier
        // (see the overlay-painting block and the scanline painter below) — so the
        // whole 0..=2 band applies straight to chrome (0 = instant, 2 = double).
        {
            const EGUI_DEFAULT_ANIMATION_TIME: f32 = 1.0 / 12.0;
            let fx = &self.config.effects;
            let anim = if fx.animations_enabled && !c0pl4nd_core::reduced_motion::reduced_motion() {
                EGUI_DEFAULT_ANIMATION_TIME * fx.clamped_animation_intensity()
            } else {
                0.0
            };
            ctx.style_mut(|s| s.animation_time = anim);
        }
        // Capture the first rendered-frame clock so the one-shot boot-glitch
        // overlay measures its sweep from the first frame the user actually sees,
        // not from context creation (which predates the window by the atlas-warmup
        // cost and would hide the sweep).
        if self.first_frame_time.is_none() {
            self.first_frame_time = Some(ctx.input(|i| i.time));
        }
        // FIRST-LAUNCH FOREGROUND (once). A freshly-launched window can open
        // BEHIND other windows on Windows 11: the OS foreground-lock ignores the
        // polite `with_active(true)` (egui_main.rs) request. On the first frame of
        // a REAL window we (a) ask egui/winit to focus us and (b) run the
        // `win_foreground` AttachThreadInput backstop that beats the lock. Gated on
        // `live_window` so the headless/offscreen test harnesses never issue it,
        // and latched by `foreground_done` so it runs EXACTLY once — raising on
        // later frames would yank focus back from an app the user switched to.
        if self.live_window && !self.foreground_done {
            self.foreground_done = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            win_foreground::force_foreground_main();
        }
        // Ensure the chrome fonts (incl. the `phosphor-fill` family) are
        // installed before any widget references them — `new()` does this for
        // the real app; headless tests built via `bootstrap()` install here on
        // frame 1 (otherwise the pinned tab's `FontFamily::Name("phosphor-fill")`
        // would panic on an unregistered family).
        if !self.fonts_installed {
            install_chrome_fonts(ctx, &self.config.font);
            self.applied_font_family = font_apply_key(&self.config.font);
            self.fonts_installed = true;
        }
        // Pre-warm the grid glyph atlas whenever the live font stack (family,
        // size, OR DPI/pixels_per_point) differs from what it was last warmed for
        // — first frame, a system-font swap, a live zoom, or a DPI settle. This
        // rasterises the full grid glyph set at the FINAL scale in one step so the
        // atlas reaches its final size immediately, and ARMS the warmup gate so the
        // grid holds its glyphs off (and `ui` GPU-fences) until that atlas is
        // uploaded + resident — closing the DX12 upload↔sample race (see
        // `prewarm_grid_atlas` + `warmup_frames_left`).
        let atlas_key = (
            self.applied_font_family.clone(),
            self.config.font.size.to_bits(),
            ctx.pixels_per_point().to_bits(),
        );
        if self.warmed_atlas.as_ref() != Some(&atlas_key) {
            prewarm_grid_atlas(ctx, self.config.font.size);
            self.warmed_atlas = Some(atlas_key);
            // The warmup GATE (grid-glyphs-off + `ui` GPU-fence) only matters on
            // the real swapchain, where the DX12 upload↔sample race lives. A
            // headless render (`live_window == false`) is offscreen/serialized —
            // it never races — so keep the gate closed there so tests see grid
            // text immediately (and never block on a poll).
            if self.live_window {
                self.warmup_frames_left = ATLAS_WARMUP_GATE_FRAMES;
                ctx.request_repaint(); // drive the warmup frames without waiting on input
            }
        }
        // Hold the grid gated (and `ui` GPU-fencing) until the OFF-THREAD custom
        // font has actually swapped in. The system-font stack loads on a worker;
        // until it arrives the grid would draw with the built-in mono, then the
        // swap RESETS the atlas — and that reset↔redraw transition is a prime
        // window for the DX12 upload↔sample race that garbles the grid. Keeping
        // the grid hidden (glyphs off) through the whole font-load period means
        // its FIRST real draw happens once the final font is warmed + resident.
        // Capped by `FONT_WAIT_GATE_CAP` so a font-load failure can never hide the
        // grid forever. Live-window only (headless never races and has no worker).
        if self.live_window && self.pending_fonts.is_some() {
            self.font_wait_frames = self.font_wait_frames.saturating_add(1);
            if self.font_wait_frames < FONT_WAIT_GATE_CAP {
                self.warmup_frames_left = self.warmup_frames_left.max(1);
                ctx.request_repaint();
            }
        }
        // Flush a DEBOUNCED font-zoom save once its deadline has passed with no
        // further change (see `FONT_SAVE_DEBOUNCE_SECS`): coalesces a fast
        // Ctrl+wheel zoom into a SINGLE config write instead of one per notch.
        if let Some(deadline) = self.pending_font_save_at {
            if ctx.input(|i| i.time) >= deadline {
                self.pending_font_save_at = None;
                self.persist_config_change("The font size");
            }
        }
        // Zoom↔focus reconcile: a zoom (Ctrl+Shift+Z) renders ONLY the zoomed
        // pane, but focus can move to a DIFFERENT pane while zoomed (switching
        // tabs, or Ctrl+Shift+T opening a new tab). Without this, the screen would
        // keep showing the old zoomed pane while keystrokes route to the now-
        // focused (hidden) pane — a silent display/input mismatch. Drop the zoom
        // whenever focus diverges from the zoomed pane, so the focused pane is
        // always the one on screen. (Focus is applied at the END of the previous
        // frame, so this start-of-frame check corrects before this frame renders.)
        if self.zoomed_pane.is_some_and(|z| z != self.focused_pane) {
            self.zoomed_pane = None;
        }
        // F2-3: apply the persisted UI scale (accessibility zoom) to the whole
        // egui context — ONLY when the configured value changed since last
        // applied, so it is a no-op on steady-state frames and never overrides
        // the transient Ctrl+/- keyboard zoom (which does not write
        // `config.ui_scale`). The NaN-initialised `applied_ui_scale` guarantees
        // the first frame applies; `set_zoom_factor` is itself a no-op when the
        // value is unchanged.
        let ui_scale = self.config.effective_ui_scale();
        if ui_scale != self.applied_ui_scale {
            ctx.set_zoom_factor(ui_scale);
            self.applied_ui_scale = ui_scale;
        }
        // Apply the configured scrollback line cap to every live pane each frame.
        // This is what makes `scrollback_lines` actually take effect — previously
        // the value was persisted and shown in Settings but every pane stayed at
        // the hard-coded default (a dead setting). Ungated because deferred /
        // restored / split panes are created at different times (some during
        // `grid_ui`, after this point), and a per-pane lock-and-set is trivially
        // cheap (≤ MAX_PANES panes; the render path already locks each pane many
        // times per frame) and idempotent. Clamped to the Settings slider range.
        let scrollback = self.config.scrollback_lines.clamp(100, 1_000_000);
        for term in self.terms.values() {
            term.set_max_scrollback(scrollback);
        }
        // Wire each live pane's UI-wake callback (once) so live PTY output wakes
        // the render loop — the other half of the damage-tracked-redraw scheme
        // whose idle side lives in `idle_repaint_interval`. Real window only:
        // a wake that calls `request_repaint` would make headless `Harness::run`
        // loop until `max_steps`.
        // Drain each pane's terminal-owed effects every frame (runs headless too,
        // so interaction tests can assert a query reply reached the PTY): PTY
        // query replies are written back to their own pane inside
        // `pump_host_effects`; the host-global effects (clipboard / live theme /
        // notification) are applied by `pump_pane_effects`.
        self.pump_pane_effects(ctx);
        if self.live_window {
            self.wire_pane_wakes(ctx);
        }
        // Live font apply runs in BOTH the live window AND headless — it must NOT
        // be gated on `live_window`. Previously this lived in the `else` of the
        // `if self.live_window` above, so a Family/Fallback change was INERT in the
        // real window (`live_window == true` took the `wire_pane_wakes` arm and
        // never the font apply); only headless tests ever exercised it. That made
        // the font dropdown a no-op in production. It is now an unconditional call.
        self.apply_live_font_change(ctx);
        // Off-thread startup font load (audit #3): when the worker thread that
        // enumerated the system font DB has finished, swap in the custom stack.
        // Until then the window painted with the built-in mono. `try_recv` is
        // non-blocking so the frame never stalls; a disconnected channel (the
        // worker failed to spawn or panicked) just drops the pending state and
        // keeps the built-in mono.
        if let Some(rx) = &self.pending_fonts {
            match rx.try_recv() {
                Ok(defs) => {
                    ctx.set_fonts(defs);
                    self.galley_cache.clear();
                    self.pending_fonts = None;
                    // `set_fonts` RESET the glyph atlas. Invalidate the warm key so
                    // the next frame's warm-check re-warms it (at the live ppp) and
                    // re-arms the warmup gate, holding the grid's glyphs off until
                    // the fresh atlas is uploaded + resident — no garble flash on
                    // the system-font swap.
                    self.warmed_atlas = None;
                    ctx.request_repaint();
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_fonts = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        // Surface an opt-in launch update check result (if one arrived) as a
        // toast. No-op when no check was attached (every headless test).
        self.poll_update_check();
        // 0a) KEYBOARD SHORTCUTS — the single, config-driven dispatcher.
        //
        //     Every shortcut the shell has is resolved HERE from the live
        //     `config.keybindings` (see `egui_app::actions`): the chord strings
        //     are parsed by the SAME `Chord` code `Keybindings::validate` uses,
        //     matched EXACTLY on modifiers, consumed out of the event stream (so
        //     a bound chord never also reaches the PTY as a control byte), and
        //     dispatched through `dispatch_action` — the same entry point the
        //     command palette uses. Rebinding an action in `config.toml` really
        //     moves its chord because nothing else opens/closes/splits anything.
        //
        //     This replaced ~8 hand-rolled `events.retain` blocks that hard-wired
        //     Ctrl+Shift+{P,F,H,T,W,D,E,Z,K,A}, Ctrl+{,}, F11, Ctrl+{+,-,0} and
        //     Ctrl+Shift+{Home,End}; the defaults reproduce every one of them.
        let fired_actions = self.dispatch_keybindings(ctx);

        // 0a') frameless fullscreen (#36): the `fullscreen` binding (F11 by
        //      default) toggles borderless OS fullscreen through the dispatcher
        //      above — the window is already `decorations: false`, so
        //      `Fullscreen` (not `Maximized`) is the right call: it covers the
        //      monitor with no border and keeps DWM compositing so the
        //      acrylic/mica backdrop still composites.
        //
        //      Esc ALSO exits fullscreen, but ONLY when no overlay owns Esc (the
        //      palette + find consume Esc to close themselves; handling it here
        //      too would fight them), and is left in the stream otherwise so
        //      those overlays still see it. Esc-exit stays here rather than
        //      becoming a binding precisely because it is conditional on that
        //      overlay state.
        let esc_exit_fullscreen = self.fullscreen
            && !self.palette_open
            && !self.search_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc_exit_fullscreen {
            self.set_fullscreen(ctx, false);
        } else if !fired_actions.contains(&Action::ToggleFullscreen) {
            // Reconcile the local mirror from the OS each frame so a fullscreen
            // toggled via another path (e.g. a window-manager shortcut) stays
            // honest — but NOT on a frame we just commanded a change, because
            // `i.viewport().fullscreen` still reports the OLD state until the OS
            // reports back next frame (that stale read would undo the toggle).
            if let Some(os) = ctx.input(|i| i.viewport().fullscreen) {
                self.fullscreen = os;
            }
        }

        // 0a'') font zoom (E-parity): the increase/decrease/reset FONT bindings
        //       (Ctrl/Cmd with +/=/- and 0 by default) run through the
        //       dispatcher above. What stays here is the part that is NOT a key
        //       chord: Ctrl/Cmd + wheel (and trackpad pinch) live zoom.
        //
        //       egui reroutes a zoom-modifier wheel into `zoom_delta()` (a
        //       MULTIPLICATIVE factor) and ZEROES `smooth_scroll_delta` for that
        //       frame, so the zoom MUST be read from `zoom_delta` — a
        //       scroll-delta read never fires under a held Ctrl/Cmd. It is 1.0
        //       with no zoom, > 1.0 zooming in (wheel up), < 1.0 out. Map that
        //       onto an ADDITIVE point step so it feeds the SAME clamp + debounced
        //       persist as the keyboard zoom (`nudge_font_size`): ~one wheel notch
        //       ≈ ±1pt, clamped so a fast pinch cannot jump size in one frame.
        //       The pane's local wheel scrollback skips a Ctrl-held wheel (see
        //       `render_pane_body`) so a Ctrl+wheel only zooms.
        {
            let zoom = ctx.input(|i| i.zoom_delta());
            if (zoom - 1.0).abs() > f32::EPSILON {
                let dz = ((zoom - 1.0) * 4.0).clamp(-3.0, 3.0);
                self.nudge_font_size(ctx, dz);
            }
        }

        // 0a''''') drag-and-drop (E-parity): insert each dropped file's
        //          shell-quoted path at the focused prompt as TEXT — never
        //          executed (no trailing newline beyond a separating space),
        //          matching the legacy shell. Routed through write_paste (the
        //          pastejacking-safe path).
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            let label = self.active_shell_label().to_string();
            let text: String = dropped
                .iter()
                .map(|p| format!("{} ", quote_path_for_shell(p, &label)))
                .collect();
            if let Some(term) = self.terms.get_mut(&self.focused_pane) {
                term.write_paste(&text);
            }
        }

        // 0a'''''') jump-to-prompt (E-parity): Ctrl+Shift+PageUp/PageDown scrolls
        //           the scrollback to the previous/next OSC 133 prompt mark. The
        //           chord is removed from the event stream so PageUp/Down don't
        //           also reach the PTY. The ctrl-OR-command match is done
        //           explicitly via events.retain (NOT consume_key): consume_key
        //           only matches when the `command` modifier bool is set, which
        //           real winit on Windows/Linux (ctrl) and synthetic test events
        //           do not set — the same cross-platform discipline the
        //           palette/find/history chords above use.
        let jump = ctx.input_mut(|i| {
            let mut dir: Option<bool> = None;
            i.events.retain(|ev| {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                {
                    let cmd = modifiers.ctrl || modifiers.command;
                    if cmd && modifiers.shift && !modifiers.alt {
                        if *key == egui::Key::PageUp {
                            dir = Some(false); // backward → older prompt
                            return false;
                        } else if *key == egui::Key::PageDown {
                            dir = Some(true); // forward → newer prompt
                            return false;
                        }
                    }
                }
                true
            });
            dir
        });
        if let Some(forward) = jump {
            if let Some(term) = self.terms.get_mut(&self.focused_pane) {
                if term.jump_to_prompt(forward) {
                    ctx.request_repaint();
                }
            }
        }

        // 0a''''''') DEC ?1004 focus reporting (E-parity): on a window focus-in/out
        //            EDGE, tell the focused pane's program (so vim/tmux see
        //            FocusGained/FocusLost). report_focus is a no-op unless the
        //            program armed ?1004; the reply is drained by pump_host_effects.
        let focused_now = ctx.input(|i| i.viewport().focused.unwrap_or(self.was_focused));
        if focused_now != self.was_focused {
            if let Some(term) = self.terms.get_mut(&self.focused_pane) {
                term.report_focus(focused_now);
            }
            self.was_focused = focused_now;
        }

        // 0a''''''''') CLIPBOARD CHORDS — `Event::Copy` / `Event::Cut`, NOT `Event::Key`.
        //
        // `egui-winit` intercepts the clipboard chords in its window-event
        // dispatcher and RETURNS EARLY, so it never emits an `Event::Key` for
        // them (egui-winit-0.34.3/src/lib.rs:1016-1027). Its predicate ignores
        // Shift — `is_copy_command` is `modifiers.command && key == C` (:1311) —
        // and on Windows/Linux `modifiers.command` IS `ctrl` (:473). So BOTH
        // `Ctrl+C` and `Ctrl+Shift+C` (and `Ctrl+Insert`) collapse into a single
        // `egui::Event::Copy`, and `Ctrl+X` into `egui::Event::Cut`.
        //
        // Matching `Event::Key { key: C, .. }` here — as this handler used to —
        // is therefore DEAD CODE: copy silently did nothing, and, far worse,
        // `Ctrl+C` never reached the PTY, so a running command could not be
        // interrupted. We recover the chord from the frame's modifier snapshot
        // (`InputState::modifiers`, which egui copies verbatim from
        // `RawInput::modifiers` — egui-0.34.3/src/input_state/mod.rs:485 — and
        // which egui-winit keeps current from `ModifiersChanged`) and:
        //
        //   Ctrl+Shift+C          → copy the selection (the terminal copy chord).
        //   Ctrl+C  WITH selection → copy the selection AND CLEAR it (Windows
        //                            Terminal's behaviour). Clearing is what keeps
        //                            SIGINT reachable: the very next Ctrl+C has no
        //                            selection and therefore interrupts.
        //   Ctrl+C  NO selection   → restore the swallowed key so the normal PTY
        //                            forwarder encodes it — 0x03, SIGINT.
        //   Ctrl+X (any selection) → always restore the key (0x18 / the readline
        //                            `C-x` prefix); a terminal cannot "cut" its
        //                            scrollback, so cut must never eat the chord.
        //
        // macOS: `command` is Super there, so `Ctrl+C`/`Ctrl+X` still arrive as
        // real `Event::Key`s and are untouched by this block; a `Cmd+C`/`Cmd+X`
        // reaches us with `ctrl == false` and always means COPY, never interrupt.
        //
        // Restoring the key (rather than writing 0x03 directly) keeps ONE PTY
        // encoding path: the kitty keyboard protocol, REPORT-EVENT-TYPES, and
        // `forward_key` all still apply, exactly as if egui-winit had not
        // swallowed the chord.
        let selection_live = self
            .selection
            .is_some_and(|s| s.anchor != s.head && self.terms.contains_key(&s.pane));
        let mut copy_sel = false;
        let mut clear_after_copy = false;
        ctx.input_mut(|i| {
            let m = i.modifiers;
            let mut kept: Vec<egui::Event> = Vec::with_capacity(i.events.len());
            for ev in i.events.drain(..) {
                match ev {
                    egui::Event::Copy => {
                        if m.shift || !m.ctrl {
                            copy_sel = true;
                        } else if selection_live {
                            copy_sel = true;
                            clear_after_copy = true;
                        } else {
                            kept.push(restored_chord_key(egui::Key::C, m));
                        }
                    }
                    egui::Event::Cut => {
                        if m.ctrl {
                            kept.push(restored_chord_key(egui::Key::X, m));
                        } else {
                            // macOS `Cmd+X`: no cut semantics in a terminal grid —
                            // treat it as a copy rather than dropping it.
                            copy_sel = true;
                        }
                    }
                    other => kept.push(other),
                }
            }
            i.events = kept;
        });
        if copy_sel {
            if let Some(sel) = self.selection {
                if sel.anchor != sel.head {
                    if let Some(term) = self.terms.get(&sel.pane) {
                        // Selection anchors are ABSOLUTE lines; map to the current
                        // display window before extracting (it may have scrolled
                        // since the selection was made).
                        let rows = term.size().1 as usize;
                        let ws = term.window_start().unwrap_or(0);
                        if let Some((a, b)) = selection_visible_rows(sel.anchor, sel.head, ws, rows)
                        {
                            if let Some(text) =
                                term.selection_text(a, b, sel.mode == SelectionMode::Block)
                            {
                                ctx.copy_text(text);
                            }
                        }
                    }
                }
            }
        }
        if clear_after_copy {
            // Windows Terminal semantics: a bare `Ctrl+C` that copied a selection
            // also DISMISSES it, so the chord is not permanently hijacked — the
            // next `Ctrl+C` finds no selection and sends SIGINT.
            self.selection = None;
        }

        // 0b) route this frame's input. When the palette is open, its navigation
        //     keys (↑/↓/Enter/Esc) are consumed here and the typed query is
        //     captured by the palette's focused TextEdit — NOT forwarded to the
        //     PTY. Otherwise keyboard/paste goes to the FOCUSED pane's PTY BEFORE
        //     the panels, so the keystrokes reach the PTY whose grid this same
        //     frame then snapshots (the load-bearing "typing reaches the PTY and
        //     the grid updates" round-trip).
        if self.pending_paste.is_some() {
            // A multi-line paste is awaiting confirmation: the confirm overlay is
            // modal, so DO NOT forward this frame's keystrokes to the PTY (else
            // the Enter that confirms the paste would also send a bare newline to
            // the shell). The overlay itself reads Enter/Esc in `paste_confirm_window`.
        } else if self.palette_open {
            let (up, down, enter, esc) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                )
            });
            if up {
                self.palette_move(-1);
            }
            if down {
                self.palette_move(1);
            }
            if esc {
                self.palette_open = false;
            }
            if enter {
                self.run_palette_selection(ctx);
            }
        } else if self.search_open {
            // The find overlay owns input while open: its TextEdit captures the
            // typed query (it auto-focuses each frame), and the navigation keys
            // (Enter / F3 / Shift+F3 cycle, Esc close) are consumed HERE so they
            // never reach the PTY. F3 is consumed in BOTH shift states so the
            // shell never sees the F3 escape sequence while finding. Typed text
            // is deliberately NOT forwarded to the PTY this branch — the overlay
            // is modal over keyboard input, like the palette.
            // Consume Shift+F3 BEFORE plain F3: `consume_key` matches the most
            // specific modifier set, and consuming the SHIFT variant first means
            // a Shift+F3 press cannot also satisfy the bare-F3 consume (which
            // would step forward instead of back).
            let (enter, esc, f3_shift, f3) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                    i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::F3),
                )
            });
            if esc {
                self.search_open = false;
            } else {
                // Enter and F3 step to the next match; Shift+F3 steps to the
                // previous. The match set is recomputed each frame below so a
                // scrolling PTY keeps the cycle honest.
                if enter || f3 {
                    self.search_cycle(1);
                }
                if f3_shift {
                    self.search_cycle(-1);
                }
                // The live grid scrolls under the overlay, so refresh the match
                // set every open frame (cheap: a substring/regex scan of the
                // visible rows) before the highlight pass reads it.
                self.recompute_search();
            }
        } else {
            self.last_forwarded = self.forward_input_to_focused(ctx);
        }

        // 0c) Frameless window edge/corner RESIZE (#24). The decorations are off,
        //     so the OS gives no resize border — we synthesize one: hint the
        //     matching resize cursor over an edge band, and on a primary press
        //     there drive a MANUAL resize (per-frame `InnerSize` from the pointer
        //     delta), NOT the OS `BeginResize` modal loop — which needs the
        //     stripped `WS_SYSMENU` and hung the window. Run early, BEFORE the
        //     panels, so an edge grab wins. Skipped in fullscreen (#36): there is
        //     no window edge to resize.
        if !self.fullscreen {
            handle_frameless_resize(ctx);
        }

        // Theme-derived chrome surface palette — the titlebar / tab strip /
        // status bar / central pane / settings window all follow the active
        // terminal theme through these (a light theme flips the whole chrome
        // light, a dark one dark). The wordmark keeps its fixed brand accent.
        let colors = theme::ChromeColors::from_theme(&self.theme);
        // Whether the active theme is dark — picks the hover-veil polarity for the
        // FLAT chrome buttons (white veil on dark, black on light) so the hover
        // reads over whatever shows through a translucent bar.
        let dark = !theme::is_light(colors.bg);

        // Chrome panel fill (titlebar + status bar): fold in the SAME opacity alpha
        // the panes + central fill use when the window is effectively translucent,
        // so the WHOLE app window is see-through — top bar + status bar included —
        // not just the pane backgrounds (an opaque `colors.panel` here left the top
        // bar solid over an otherwise-transparent window). Fully opaque otherwise.
        // The SETTINGS window deliberately keeps its own opaque `colors.panel` fill
        // so it stays solid + readable regardless of window transparency.
        let panel_alpha = pane_bg_alpha(&self.config);
        let panel_fill = egui::Color32::from_rgba_unmultiplied(
            colors.panel.r(),
            colors.panel.g(),
            colors.panel.b(),
            panel_alpha,
        );
        // The window tint is a single wash painted on the BACKGROUND layer HERE —
        // before any panel — so it sits behind every translucent background fill
        // (panes, gaps, titlebar, status) and shows through them UNIFORMLY at any
        // opacity, while the glyph text + the Settings window (painted later /
        // higher) are never tinted. See `paint_background_tint`.
        window_effects::paint_background_tint(ctx, &self.config);
        // The software "frosted glass" wash, on the SAME background layer, over the
        // tint and behind the panes/glyphs. Independent of the opacity slider (its
        // own `frost_amount`), so it adds an adjustable diffuse frost that shows
        // through the see-through glass at any opacity < 1 without fading. See
        // `paint_frost`.
        window_effects::paint_frost(ctx, &self.config, &self.theme);

        // 1) custom titlebar + tab strip. Fixed height so the drag region below
        //    is exactly the bar (not the whole remaining column), and so the
        //    caption-cluster geometry is stable. In fullscreen (#36) the titlebar
        //    + status panels are NOT rendered so only the grid fills the screen;
        //    `actions` falls back to the empty default for that frame (no chrome
        //    means no chrome actions). The floating overlays (settings / palette /
        //    find / history) can still open over the grid while fullscreen.
        let actions = if self.fullscreen {
            chrome::ChromeActions::default()
        } else {
            egui::TopBottomPanel::top("titlebar")
                .exact_height(40.0)
                .frame(egui::Frame::new().fill(panel_fill).inner_margin(6.0))
                .show(ctx, |ui| {
                    // Frameless-window move: dragging any EMPTY part of the
                    // titlebar moves the window; double-click toggles maximize.
                    // Added FIRST so it sits behind the tabs/buttons (egui gives
                    // later widgets the click), so only the empty bar area
                    // initiates a drag.
                    let bar = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("c0pl4nd_titlebar_drag"),
                        egui::Sense::click_and_drag(),
                    );
                    if bar.drag_started_by(egui::PointerButton::Primary) {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if bar.double_clicked() {
                        let is_max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                        self.toggle_maximize(ui.ctx(), is_max);
                    }
                    // Flat chrome buttons: no idle background, fill only on hover —
                    // so the controls read as part of the (translucent) bar instead
                    // of floating opaque chips. Must run BEFORE the buttons draw.
                    window_effects::flatten_chrome_buttons(ui, dark);
                    self.titlebar_and_tabs(ui, colors)
                })
                .inner
        };

        // 1b) persistent, dismissible UPDATE NOTIFICATION BANNER. Rendered right
        //     below the titlebar so a newer release surfaces a one-click "Update
        //     now" strip that runs the WHOLE verified flow (download → verify →
        //     silent self-replace → relaunch) inline — never leaving the app. It
        //     shares the SAME `Updater` the Settings → Updates page drives, and it
        //     polls that updater every frame so the on-launch check advances even
        //     while Settings is closed. The panel only appears when an update is
        //     actionable (or an apply is in flight) and not dismissed, so it costs
        //     nothing in the common up-to-date case.
        settings::update_banner(ctx);

        // 2) status bar (hidden in fullscreen — see the titlebar gate above — and
        //    hidden when the user turns it off in Settings, reclaiming the row for
        //    the terminal grid).
        if !self.fullscreen && self.config.show_status_bar {
            egui::TopBottomPanel::bottom("status")
                .frame(egui::Frame::new().fill(panel_fill).inner_margin(4.0))
                .show(ctx, |ui| {
                    window_effects::flatten_chrome_buttons(ui, dark);
                    self.status_bar(ui, colors);
                });
        }

        // 2b) command-history quick-run sidebar (#21), if open. Rendered as a
        //     docked SidePanel BEFORE the CentralPanel so the terminal grid
        //     reflows around it (and reclaims the full width when it closes — the
        //     panel is simply NOT shown when `history_open == false`).
        if self.history_open {
            self.history_sidebar(ctx, colors);
        }

        // 3) the pane grid (egui_tiles) — LIVE terminal panes (Milestone 2). This
        //    CentralPanel fill is the SINGLE terminal backdrop (see the single-
        //    backdrop rule in `render_pane_body`): it carries the opacity-folded
        //    `pane_bg_alpha` and backs BOTH the panes AND the gaps between them, so
        //    a translucent window reveals the desktop uniformly and an opaque one
        //    stays solid (no seam leak). The per-pane body no longer paints its own
        //    fill, so this alpha is applied EXACTLY ONCE — the opacity slider is
        //    linear (no `opacity²` compounding haze). The backing colour is the
        //    focused terminal's own theme background (identical to the chrome bg,
        //    but semantically the TERMINAL surface, not the chrome).
        let central_alpha = pane_bg_alpha(&self.config);
        let backing = self
            .terms
            .get(&self.focused_pane)
            .map(PaneTerm::background_rgb)
            .unwrap_or((colors.bg.r(), colors.bg.g(), colors.bg.b()));
        let central_fill =
            egui::Color32::from_rgba_unmultiplied(backing.0, backing.1, backing.2, central_alpha);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(central_fill))
            .show(ctx, |ui| self.grid_ui(ui));

        // Apply chrome actions AFTER the panels close (no mid-borrow mutation).
        if let Some(pid) = actions.focus_tab {
            if pid != self.focused_pane {
                self.input_line.clear(); // the typed-line accumulator is per-pane
            }
            self.focused_pane = pid;
        }
        if let Some(pid) = actions.pin_tab {
            // Toggle pinned state.
            if !self.pinned.remove(&pid) {
                self.pinned.insert(pid);
            }
        }
        if let Some(pid) = actions.close_tab {
            self.close_pane(pid);
        }
        if actions.new_terminal {
            self.new_terminal();
        }
        if let Some(idx) = actions.open_shell {
            self.open_shell(idx);
        }
        if actions.toggle_settings {
            self.settings_open = !self.settings_open;
        }
        // Script menu (#35), applied AFTER the panels close so neither the
        // `&mut self` run path nor the BLOCKING native file picker fires
        // mid-panel-borrow. A history re-run goes first; the "Open…" picker
        // (which blocks on its own OS modal loop) runs last.
        if let Some(cmd) = actions.rerun_command {
            self.run_command_in_focused(&cmd);
        }
        if actions.open_script_file {
            self.open_script_file();
        }
        // W1TN3SS manual issue intake: open the prefilled-GitHub-issue dialog
        // (user-initiated; nothing transmits until the user submits in-browser).
        if actions.report_issue {
            self.issue_intake.open_fresh();
        }
        // View-mode toggle (#30): the chrome button and the `toggle_view_mode`
        // action/binding share ONE method, so the flip + its persist behave
        // identically however the user reached it.
        if actions.toggle_view_mode {
            self.toggle_view_mode();
        }
        // One-shot "make panes symmetrical": rebuild the layout as a UNIFORM grid
        // so all panes are equal-sized regardless of the prior (possibly nested /
        // asymmetric) split structure — the fix for "clicked symmetrical but the
        // panes stayed uneven". Preserves pane order + every attached terminal
        // (panes carry only their id). No-op for a 0/1-pane tree.
        if actions.equalize_panes {
            // Shared with the `equalize_panes` action/binding — one method.
            self.equalize_panes(ctx);
        }
        // Caption command: issue the REAL OS viewport command AND record it so an
        // interaction test can assert the click had its effect.
        if let Some(cmd) = actions.window_cmd {
            self.last_window_cmd = Some(cmd);
            let is_max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            match cmd {
                WindowCmd::Minimize => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                WindowCmd::ToggleMaximize => {
                    self.toggle_maximize(ctx, is_max);
                }
                WindowCmd::Close => {
                    // Fast clean shutdown: run the necessary cleanup (persist
                    // config + reap every PTY child so none orphan), then exit
                    // immediately. This skips eframe/wgpu's slow graceful
                    // GPU-device + swapchain + winit-window-destroy teardown —
                    // the OS reclaims the GPU/window handles instantly — which is
                    // what made the window slow to close. `prepare_shutdown` does
                    // the load-bearing work; `process::exit(0)` is safe under
                    // `#![forbid(unsafe_code)]`.
                    self.prepare_shutdown();
                    std::process::exit(0);
                }
            }
        }

        // Reset the motion-overlay exclude rect each frame; each centered panel
        // drawn below records its rect (via `note_overlay_rect`) so the overlay
        // block further down paints AROUND whatever is open this frame. A closed
        // panel therefore clears the exclusion automatically.
        self.overlay_exclude_rect = None;

        // 4) the (opaque) settings window, if open. Detect the closed→open edge so
        //    `settings_window` can force the window to its saved-or-centered
        //    position on that first frame.
        if self.settings_open {
            if !self.settings_was_open {
                self.settings_place_pending = true;
            }
            self.settings_window(ctx);
        }
        self.settings_was_open = self.settings_open;

        // 5) the command palette overlay, if open (rendered last so it floats
        //    above the chrome + grid; its nav keys were handled in step 0b).
        if self.palette_open {
            self.command_palette_window(ctx);
        }

        // 5b) the find overlay, if open (floats above the grid; its nav keys
        //     were handled in step 0b). Recomputes matches on a query/toggle edit
        //     inside the window closure.
        if self.search_open {
            self.search_window(ctx);
        }

        // 5c) the multi-line paste confirm overlay, if a paste is pending. Floats
        //     above everything; Enter sends it through the injection guard, Esc
        //     discards it. Rendered before the tint so the wash sits over it too.
        if self.pending_paste.is_some() {
            self.paste_confirm_window(ctx);
        }

        // 5d) W1TN3SS opt-in reporting dialogs (float above the chrome). The
        //     crash-consent dialog presents any spooled crash report drained on
        //     launch (only when the user opted into AskEachTime); the manual
        //     "Report an issue" dialog opens from the titlebar script menu. Both
        //     are no-ops unless there is something to present / the dialog is
        //     open, so the default (opted-out) experience is untouched.
        self.render_crash_consent(ctx);
        self.render_report_issue(ctx);

        // Whole-window motion overlays (SCR1B3 parity): flicker / VHS-tracking /
        // wired-mesh ambient / cursor ghost-trail / boot-glitch, each painted once
        // per frame at the Context layer (Background for the ambient mesh so it
        // sits BEHIND the panes, Foreground for the rest so they wash OVER the
        // composited view). Gated behind the master `animations_enabled` switch AND
        // each per-effect toggle, suppressed under reduced-motion (env or OS) and
        // in the headless harness (`live_window == false`) so tests stay
        // deterministic. Any active animated overlay drives a per-frame repaint so
        // its motion keeps advancing without a free-running timer.
        // Exclude whichever centered chrome surface is open (Settings window,
        // command palette, multi-line-paste confirm) from the whole-window overlays,
        // rather than suppressing them entirely: the Foreground effects otherwise
        // wash OVER those opaque panels — the reported "the mesh overlays the settings
        // menu" — but suppressing them meant a Motion setting only took visible effect
        // AFTER Settings closed ("made me think they weren't working"). Painting the
        // effects everywhere EXCEPT the panel rect gives a live terminal-area preview
        // while keeping the panel clean. The panel rect was captured earlier THIS
        // frame (`overlay_exclude_rect`, set as each panel drew above) and is padded
        // so the effect doesn't crowd the panel edge.
        let exclude = self
            .overlay_exclude_rect
            .map(|r| r.expand(8.0).intersect(ctx.content_rect()));
        if self.live_window
            && self.config.effects.animations_enabled
            && !c0pl4nd_core::reduced_motion::reduced_motion()
        {
            let fx = self.config.effects;
            let t = ctx.input(|i| i.time);
            // Ambient motion effects (mesh / VHS / flicker) are INDEPENDENT of the
            // window Opacity slider: their visibility is driven ONLY by their own
            // Motion settings (mesh brightness/density/speed, VHS/flicker intensity),
            // so dragging Opacity never changes how strong the node mesh reads.
            // (Opacity, Tint, Frost, and Motion are four independent controls.)
            // Each continuous drift overlay now carries its OWN speed multiplier
            // (SCR1B3 parity), decoupled from the UI-transition-speed slider
            // (`animation_intensity`, which governs only egui's chrome fades). Each
            // per-effect clock is `t * clamped_<effect>_speed`, so the default 1.0
            // reproduces the shipped drift EXACTLY and higher values run that ONE
            // effect faster without touching the others or the UI fades. The
            // event-driven cursor trail and the one-shot boot sweep keep the REAL
            // clock (their timing anchors to cursor movement / the first frame, not
            // a drift phase, so scaling would corrupt them).
            let mut animating = false;
            if fx.wired_ambient {
                // The mesh colour follows the theme accent UNLESS the user pinned an
                // explicit override in Settings (`effects.mesh_color`, a `#rrggbb`).
                // "Reset to theme" clears the override back to None → accent again.
                let accent = self
                    .config
                    .effects
                    .mesh_color
                    .map(|[r, g, b]| egui::Color32::from_rgb(r, g, b))
                    .unwrap_or_else(|| theme::ChromeColors::from_theme(&self.theme).accent);
                // The Motion → Mesh-drift-speed slider (`mesh_speed`) scales the
                // mesh's own drift clock: the node lattice can hold a static frame
                // (0) or drift briskly (2) independently of the other effects.
                // `t * mesh_move` = the per-mesh clock; at 0 the nodes stop moving.
                let mesh_move = fx.clamped_mesh_speed() as f64;
                paint_wired_mesh(
                    ctx,
                    fx.clamped_mesh_density(),
                    fx.clamped_mesh_brightness(),
                    accent,
                    t * mesh_move,
                    exclude,
                );
                animating |= mesh_move > 0.0;
            }
            if fx.vhs_tracking {
                paint_vhs_tracking(
                    ctx,
                    t * fx.clamped_vhs_speed() as f64,
                    fx.clamped_vhs_intensity(),
                    exclude,
                );
                animating = true;
            }
            if fx.flicker {
                paint_flicker(
                    ctx,
                    fx.clamped_flicker_strength(),
                    t * fx.clamped_flicker_speed() as f64,
                    exclude,
                );
                animating = true;
            }
            if fx.cursor_trail {
                // The trail intensity scales BOTH opacity and lifetime; prune with
                // the SAME lifetime the painter fades over so the deque can't grow
                // while the cursor sits still (the fresh-echo push only happens on
                // cursor movement).
                let trail_intensity = fx.clamped_cursor_trail_intensity();
                let life = cursor_trail_life(trail_intensity);
                while self
                    .cursor_trail
                    .front()
                    .is_some_and(|(_, born)| t - born > life)
                {
                    self.cursor_trail.pop_front();
                }
                let accent = theme::ChromeColors::from_theme(&self.theme).accent;
                paint_cursor_trail(ctx, &self.cursor_trail, accent, t, trail_intensity, exclude);
                if !self.cursor_trail.is_empty() {
                    animating = true;
                }
            }
            if fx.boot_glitch {
                if let Some(t0) = self.first_frame_time {
                    let elapsed = t - t0;
                    paint_boot_glitch(ctx, elapsed);
                    if (0.0..=0.55).contains(&elapsed) {
                        animating = true;
                    }
                }
            }
            if animating {
                ctx.request_repaint();
            }
        }

        // Window color-tint recap: it is a SINGLE background-layer wash
        // (`paint_background_tint`, painted early above), behind every translucent
        // panel/pane fill — so it colours the app background uniformly WITHOUT
        // discolouring the terminal text or the opaque Settings window, and never a
        // flat film painted over the chrome. The top-bar/status buttons carry it the
        // same way the panes do: the wash shows through their translucent bar, and
        // the buttons themselves are FLAT (`flatten_chrome_buttons`), so no opaque
        // chip floats over the see-through bar.

        // Live terminals: schedule the IDLE repaint fallback — but ONLY in the
        // real window (`live_window`). PTY output now wakes the UI instantly via
        // each pane's Session wake callback (wired in `wire_pane_wakes`), and
        // real input repaints natively, so we no longer free-run at the monitor
        // refresh rate: an idle terminal drops from 60–144 fps to ~1–2 fps,
        // cutting idle GPU/CPU. `request_repaint_after` only sets a ceiling on
        // staleness (egui repaints at the SOONEST of all requests). In the
        // headless `egui_kittest` harness an unconditional repaint would make
        // `Harness::run` loop until `max_steps`, so the pump stays off there.
        if self.live_window {
            ctx.request_repaint_after(self.idle_repaint_interval());
        }
    }

    /// Wire every live pane's UI-wake callback exactly once. The reader thread
    /// invokes it after each chunk of PTY output so [`Self::idle_repaint_interval`]
    /// can let the UI sleep when idle while still repainting the instant output
    /// arrives. Idempotent per pane (see [`PaneTerm::wire_wake`]); cheap to call
    /// every frame. Only invoked for the real window.
    fn wire_pane_wakes(&mut self, ctx: &egui::Context) {
        for pane in self.terms.values_mut() {
            pane.wire_wake(|| {
                let ctx = ctx.clone();
                std::sync::Arc::new(move || ctx.request_repaint())
            });
        }
    }

    /// Re-install the configured font stack when the user changes the Family (or a
    /// Fallback) in settings, so the new typeface shows THIS frame without a
    /// relaunch. The `applied_font_family` key folds the family + fallbacks into
    /// one string so the (expensive) re-install runs ONLY on an actual change,
    /// never every frame. A re-install changes the font atlas, so the cached
    /// galleys (which reference the old atlas) must be dropped (audit #2).
    ///
    /// This MUST run in both the live window AND headless: it previously lived in
    /// the `else` of `if self.live_window`, so in the real window the
    /// `live_window == true` arm took `wire_pane_wakes` and the font apply never
    /// ran — the font dropdown was a silent no-op in production, exercised only by
    /// headless tests. It is now called unconditionally.
    fn apply_live_font_change(&mut self, ctx: &egui::Context) {
        let want = font_apply_key(&self.config.font);
        if want != self.applied_font_family {
            install_chrome_fonts(ctx, &self.config.font);
            self.applied_font_family = want;
            self.galley_cache.clear();
            // A settings re-install supersedes any in-flight startup load.
            self.pending_fonts = None;
        }
    }

    /// Drain every live pane's terminal-owed effects once per frame.
    ///
    /// PTY query replies (device attributes, cursor-position reports, OSC color
    /// queries, focus reports) are written straight back to their originating
    /// pane inside [`PaneTerm::pump_host_effects`]. The host-global effects it
    /// returns are applied here:
    /// - OSC 52 clipboard writes → the OS clipboard (`ctx.copy_text`).
    /// - OSC 4/10/11/12/104 color sets → the live [`Self::theme`], re-pushed to
    ///   every pane so the new palette shows the same frame.
    /// - OSC 9/777 notifications → a taskbar attention request while unfocused.
    ///
    /// Without this the canonical egui binary silently dropped every reply AND
    /// let the unread queues grow unbounded; the legacy winit shell drained them
    /// each frame. Runs in the headless harness too so interaction tests can
    /// assert a query reply reached the PTY.
    fn pump_pane_effects(&mut self, ctx: &egui::Context) {
        let mut clipboard: Vec<String> = Vec::new();
        let mut colors: Vec<ColorSet> = Vec::new();
        let mut notified = false;
        let mut progress: Vec<c0pl4nd_core::term::osc::Progress> = Vec::new();
        for pane in self.terms.values_mut() {
            let fx = pane.pump_host_effects();
            clipboard.extend(fx.clipboard_writes);
            colors.extend(fx.color_sets);
            notified |= fx.notified;
            progress.extend(fx.progress);
        }
        // OSC 52 → OS clipboard (write only; reads stay default-off in core).
        for text in clipboard {
            ctx.copy_text(text);
        }
        // OSC 4/10/11/12/104 → live theme, then repaint so the new palette shows.
        if !colors.is_empty() {
            for set in colors {
                self.apply_color_set(set);
            }
            for term in self.terms.values_mut() {
                term.set_theme(self.theme.clone());
            }
            ctx.request_repaint();
        }
        // OSC 9/777 desktop notification while the window is unfocused → request
        // user attention (taskbar flash). The notification TEXT is never read
        // here (privacy: it can carry a 2FA code / secret URL — never log it).
        // The focused-suppression predicate lives in `taskbar` so it is unit-
        // testable without a live window (`focused == None` at startup is
        // treated as focused, so a startup notification does not flash).
        let focused = ctx.input(|i| i.viewport().focused);
        if taskbar::should_request_attention(notified, focused) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        // OSC 9;4 taskbar progress → the Windows taskbar button's progress
        // segment. Only the LAST report of the frame is visible on a single
        // button, so `latest_progress` collapses the frame's stream to one
        // apply; an empty drain leaves the button untouched (no needless COM
        // call every frame).
        if let Some(latest) = taskbar::latest_progress(&progress) {
            taskbar::apply_progress(taskbar::map_progress_state(latest.state), latest.percent);
        }
    }

    /// Apply one drained [`ColorSet`] (OSC 4/10/11/12/104) to the live theme.
    /// Mirrors the legacy winit shell's mapping exactly: dynamic fg/bg/cursor
    /// update the theme's three core colors; indexed entries 0-15 update the
    /// 16-slot ANSI palette; 256-cube entries (index ≥ 16) have no theme slot
    /// and are ignored rather than misplaced.
    fn apply_color_set(&mut self, set: ColorSet) {
        use c0pl4nd_core::term::DynamicColor;
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

    /// The current [`FramePolicy`](c0pl4nd_renderer::FramePolicy): `Continuous`
    /// (redraw every vsync) only while the CRT scanline animation is enabled AND
    /// reduced-motion is off; otherwise `OnDamage` (redraw on PTY output / input /
    /// the bounded idle cadence). This is the typed expression of the shell's
    /// frame-scheduling contract — the single biggest perceived-latency / battery
    /// lever — shared with the renderer crate.
    fn frame_policy(&self) -> c0pl4nd_renderer::FramePolicy {
        // Continuous redraw only while the scanline drift is actually MOVING: the
        // effect is on, reduced-motion is off, and the master animation switch is
        // on. The scanline roll rate is the dedicated `scanline_speed` multiplier
        // (clamped to a 0.25 floor, so an enabled+animating scanline always
        // drifts); when any gate is false the bands are a static texture and
        // OnDamage keeps the terminal off the vsync treadmill (battery /
        // perceived-latency lever).
        let fx = &self.config.effects;
        let scanline_animating = fx.crt_scanlines
            && fx.animations_enabled
            && !c0pl4nd_core::reduced_motion::reduced_motion();
        if scanline_animating {
            c0pl4nd_renderer::FramePolicy::Continuous
        } else {
            c0pl4nd_renderer::FramePolicy::OnDamage
        }
    }

    /// The longest the live UI may wait before an *unforced* repaint. PTY output
    /// and user input repaint immediately; this only bounds idle staleness so an
    /// otherwise-quiescent terminal stops redrawing at the monitor refresh rate.
    fn idle_repaint_interval(&self) -> std::time::Duration {
        use std::time::Duration;
        // The CRT scanline post-effect is a continuous animation — keep it smooth
        // by ticking every frame while it is enabled (the scanline painter also
        // self-requests a repaint, so this just matches that cadence). F2-2: under
        // reduced-motion the roll band is frozen and does not self-request, so do
        // NOT pump the animation here either — fall through to the idle cadence.
        if self.frame_policy() == c0pl4nd_renderer::FramePolicy::Continuous {
            return Duration::ZERO; // == request_repaint(): animate at display rate
        }
        // A blink-enabled cursor must keep blinking on an otherwise-idle screen;
        // tick at the blink half-period so the caret toggles. The cursor painter
        // reads wall-clock time and does NOT self-request, so without this tick a
        // fully-idle screen would freeze the blink.
        if self.config_cursor_blink() {
            return Duration::from_millis(CURSOR_BLINK_HALF_PERIOD_MS);
        }
        // Fully quiescent: a 1 s safety-net tick bounds worst-case staleness if
        // any animation path forgot to self-request, while still cutting the idle
        // repaint rate ~60–140×. Output and input always repaint immediately.
        Duration::from_secs(1)
    }
}

/// Cursor-blink half-period, in milliseconds (the on/off toggle interval). Used
/// to schedule the idle repaint tick so a blinking caret keeps animating on an
/// otherwise-quiescent screen. Matches the 530 ms cadence the cursor painter and
/// the legacy winit shell use.
const CURSOR_BLINK_HALF_PERIOD_MS: u64 = 530;

/// Frames the atlas-warmup gate holds the grid's glyphs off (and GPU-fences in
/// `ui`) after a (re)warm, so the warmed atlas is uploaded + resident before any
/// glyph is sampled. Two frames: one to submit the warmed-atlas upload, one whose
/// `poll(Wait)` guarantees it resident before the grid first draws. Invisible —
/// the shell banner has not arrived this early anyway.
const ATLAS_WARMUP_GATE_FRAMES: u8 = 2;

/// Max frames the grid stays gated waiting for the off-thread custom font to swap
/// in (~4 s at 60 fps). A safety cap: past it the grid renders regardless, so a
/// failed/hung font load can never hide the terminal forever.
const FONT_WAIT_GATE_CAP: u32 = 240;

/// Pre-warm the monospace glyph atlas: rasterise every glyph the terminal grid
/// commonly draws — printable ASCII plus the Unicode box-drawing block — at the
/// grid font size, so egui's font-atlas TEXTURE is fully populated and uploaded
/// BEFORE the first banner/prompt frame draws.
///
/// This is the fix for the intermittently garbled / blank grid glyphs: egui grows
/// the atlas lazily as new glyphs appear and re-uploads the texture, and on some
/// GPUs a drawn frame can sample that texture WHILE the next frame's upload of a
/// grown atlas is still in flight (an upload↔draw race that a low present latency
/// makes worse — see the `desired_maximum_frame_latency` note in `egui_main`).
/// Once the atlas is complete and STABLE, a steady-state frame never modifies the
/// texture a previous frame is reading, so the race cannot occur. `layout_no_wrap`
/// forces each glyph into the atlas as a side effect; the returned galley is
/// discarded. Cheap (a few hundred glyphs, once per font-stack change).
fn prewarm_grid_atlas(ctx: &egui::Context, font_size: f32) {
    let font = egui::FontId::monospace(font_size);
    // Laying out the glyphs ALLOCATES each in the texture atlas as a side effect
    // (the same population the real grid draw triggers). Use `fonts_mut` — the
    // atlas-mutating accessor — since layout takes `&mut FontsView` in egui 0.34.
    // Printable ASCII (banners / prompts / output) + the Unicode box-drawing block
    // (TUI borders / rules).
    let mut glyphs = String::new();
    glyphs.extend((0x20u8..=0x7e).map(char::from));
    glyphs.extend((0x2500u32..=0x257f).filter_map(char::from_u32));
    ctx.fonts_mut(|fonts| {
        let _ = fonts.layout_no_wrap(glyphs, font, egui::Color32::WHITE);
    });
}

/// Debounce window (seconds) for persisting a live font-zoom change. A fast
/// Ctrl+wheel emits many notches; coalescing to ONE config write this long after
/// the last change avoids a temp-write + rename + perms per notch. Short enough
/// that the size is durably saved almost immediately once the user stops zooming.
const FONT_SAVE_DEBOUNCE_SECS: f64 = 0.6;

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
fn handle_frameless_resize(ctx: &egui::Context) {
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

/// Cell metrics (physical px) derived from egui's monospace font at `font_size`
/// — the same font [`paint_grid_native`] draws the grid with — so the PTY's
/// `(cols, rows)` match the rendered glyph size. Width is the advance of `'M'`;
/// height is the EFFECTIVE row pitch ([`effective_row_pitch`] of the font's
/// natural row height and the configured `line_height_px`), so a Line-height
/// change reflows the PTY to the SAME pitch the glyph painter draws at — rows
/// never overlap or leave a gap the resize math is unaware of. Both are scaled
/// to physical pixels by the context's `pixels_per_point`.
fn monospace_cell_metrics(
    painter: &egui::Painter,
    font_size: f32,
    ppp: f32,
    line_height_px: f32,
) -> CellMetrics {
    let probe = egui::text::LayoutJob::single_section(
        "M".to_string(),
        egui::text::TextFormat {
            font_id: egui::FontId::monospace(font_size.max(6.0)),
            ..Default::default()
        },
    );
    let size = painter.layout_job(probe).size();
    let pitch = effective_row_pitch(size.y, line_height_px);
    CellMetrics {
        advance_w: (size.x * ppp).max(1.0),
        line_h: (pitch * ppp).max(1.0),
    }
}

/// Outcome of painting one terminal pane's body for a frame.
/// A pane action requested from the right-click context menu that needs
/// `&mut self` (it mutates the tiles tree), so it cannot run inside the
/// egui_tiles render closure — it is queued in [`PaneBodyOutcome`] and applied
/// by the caller in `frame_tick` after the closure releases its borrows. Copy
/// and Clear-scrollback run INLINE in the menu closure (they only touch
/// `terms`) and never reach this enum.
#[derive(Debug, Clone, Copy)]
enum ContextMenuAction {
    SplitRight,
    SplitDown,
    NewTerminal,
    ClosePane(PaneId),
}

/// A spatial direction for directional pane focus (Ctrl/Cmd+Shift+Arrow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

struct PaneBodyOutcome {
    /// Whether the pane reported it wants to begin an egui_tiles drag.
    drag_started: bool,
    /// True when the pane body was clicked (a refocus request).
    clicked: bool,
    /// The pane's body size (points) this frame — used to pick the "+" split
    /// direction for the focused pane.
    size: egui::Vec2,
    /// A URL the user Ctrl-clicked in this pane's grid this frame, if any. The
    /// caller records it in [`C0pl4ndApp::last_opened_url`]; the OS-opener call
    /// (`ctx.open_url`) already happened inside the render.
    opened_url: Option<String>,
    /// The screen-space rect of the terminal cursor cell for this pane, in
    /// points (F3-1). `Some` only for the FOCUSED pane that has a live cursor;
    /// the caller feeds it into `ctx.output_mut(|o| o.ime = Some(IMEOutput {..}))`
    /// so the OS IME candidate window tracks the caret instead of anchoring at
    /// the screen origin.
    ime_cursor_rect: Option<egui::Rect>,
    /// The text of a mouse selection that was just COMPLETED (drag released)
    /// in this pane this frame, if non-empty. The caller copies it to the OS
    /// clipboard when `config.copy_on_select` is enabled; an explicit
    /// Ctrl/Cmd+Shift+C copies the live selection regardless.
    copy_selection: Option<String>,
    /// A right-click context-menu action (split / new / close) requested this
    /// frame that needs `&mut self`; applied by the caller after the egui_tiles
    /// render closure releases its borrows. `None` when no such item was chosen.
    context_menu_action: Option<ContextMenuAction>,
    /// This pane's screen-space body rect this frame. The caller records it in
    /// [`C0pl4ndApp::pane_rects`] for directional pane focus (geometry).
    body_rect: egui::Rect,
}

/// The theme's default foreground as an `(r,g,b)` triple — the glyph colour for
/// runs with no explicit SGR colour, and the egui-painter fallback colour.
fn term_default_fg(theme: &c0pl4nd_core::Theme) -> (u8, u8, u8) {
    c0pl4nd_core::theme::parse_hex(&theme.foreground).unwrap_or((232, 230, 240))
}

/// The top-left point at which a pane's terminal grid text is drawn, given the
/// pane's body `rect` and the configurable inner `padding` (points). Pure +
/// GPU-free so the padding live-apply wiring is unit-testable: the origin must
/// move with the padding (a larger padding insets the grid further from the
/// pane's top-left corner). Negative paddings are clamped to zero so a bad
/// config can never push the origin outside the pane.
fn grid_text_origin(rect: egui::Rect, padding: f32) -> egui::Pos2 {
    let p = padding.max(0.0);
    rect.left_top() + egui::vec2(p, p)
}

/// Alpha the theme's opaque `selection_background` is washed over the grid at.
///
/// The theme colour is an opaque RGB; painting it solid would hide the text
/// underneath (the wash is drawn AFTER the glyphs). This alpha is the previous
/// hard-coded wash's alpha, so the selection reads exactly as before while now
/// taking the ACTIVE THEME's hue instead of a fixed steel blue.
const SELECTION_WASH_ALPHA: u8 = 0x60;

/// Snap a POINT coordinate to the physical-pixel grid at `ppp`.
///
/// Background quads must tile without seams: two adjacent cells with the same
/// background are painted as separate rectangles whose shared edge lands on a
/// fractional pixel at most DPI scalings. Rounding that edge to a whole physical
/// pixel makes the left quad's right edge and the right quad's left edge the
/// SAME value, so they abut exactly — no bright hairline where the window
/// background shows through, and no double-blended overlap.
fn snap_to_physical(v: f32, ppp: f32) -> f32 {
    if ppp > 0.0 {
        (v * ppp).round() / ppp
    } else {
        v
    }
}

/// Draw one span's underline in `style`, from `x0` to `x1` with its top at `y`.
///
/// Every variant is drawn ANALYTICALLY from the span geometry (no glyph, no
/// texture), so all of them stay crisp and correctly-proportioned at any
/// `pixels_per_point` — the requirement that rules out rendering the curly
/// variant as a repeated `~`-like glyph, which aliases into mush on HiDPI.
// Geometry primitive: endpoints, thickness, colour, style and pixels-per-point
// are all independent painting parameters. A struct would not reduce the count,
// only rename it — the same rationale as `glyph_button`'s existing allow.
#[allow(clippy::too_many_arguments)]
fn paint_underline(
    painter: &egui::Painter,
    x0: f32,
    x1: f32,
    y: f32,
    thickness: f32,
    color: egui::Color32,
    style: c0pl4nd_core::grid::UnderlineStyle,
    ppp: f32,
) {
    use c0pl4nd_core::grid::UnderlineStyle as U;
    if x1 <= x0 {
        return;
    }
    // A solid horizontal bar from `a` to `b`, snapped so it is exactly the
    // requested thickness in physical pixels.
    let bar = |a: f32, b: f32, top: f32| {
        let ty = snap_to_physical(top, ppp);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(a, ty),
                egui::pos2(b, ty + thickness.max(1.0 / ppp.max(0.01))),
            ),
            0.0,
            color,
        );
    };
    match style {
        U::None => {}
        U::Single => bar(x0, x1, y),
        // Two hairlines with a gap of one thickness between them.
        U::Double => {
            bar(x0, x1, y - thickness);
            bar(x0, x1, y + thickness);
        }
        // Dot on / dot off at a 2x period.
        U::Dotted => {
            let step = (thickness * 2.0).max(1.0);
            let mut x = x0;
            while x < x1 {
                bar(x, (x + thickness).min(x1), y);
                x += step;
            }
        }
        // Longer dashes at a 7x period — visually distinct from dotted.
        U::Dashed => {
            let dash = (thickness * 4.0).max(2.0);
            let step = (thickness * 7.0).max(3.0);
            let mut x = x0;
            while x < x1 {
                bar(x, (x + dash).min(x1), y);
                x += step;
            }
        }
        // Undercurl (nvim LSP diagnostics): a sine sampled at ~1 physical pixel
        // so the wave has the same shape and amplitude in PHYSICAL terms on a
        // 1x and a 2x display.
        U::Curly => {
            let amplitude = thickness * 1.5;
            let period = (thickness * 6.0).max(4.0);
            let sample = (1.0 / ppp.max(0.01)).max(0.25);
            let mid = y + thickness * 0.5;
            let mut pts: Vec<egui::Pos2> = Vec::new();
            let mut x = x0;
            while x < x1 {
                let phase = (x - x0) / period * std::f32::consts::TAU;
                pts.push(egui::pos2(x, mid + phase.sin() * amplitude));
                x += sample;
            }
            // Always close on the span's right edge so the curl spans the full
            // run regardless of where the sampling loop happened to stop.
            let phase = (x1 - x0) / period * std::f32::consts::TAU;
            pts.push(egui::pos2(x1, mid + phase.sin() * amplitude));
            if pts.len() >= 2 {
                painter.add(egui::Shape::line(
                    pts,
                    egui::Stroke::new(thickness.max(1.0 / ppp.max(0.01)), color),
                ));
            }
        }
    }
}

/// Paint a pane's visible grid with egui's NATIVE text painter, using the
/// per-row colour runs from [`PaneTerm::grid_rows`]. This is the single,
/// engine-agnostic render path for BOTH the live window and the headless
/// snapshot tests — identical code, so a passing test faithfully proves the
/// live render. It deliberately uses egui's own glyph rasteriser (the same one
/// that draws the chrome, and the same approach SCR1B3 uses for coloured code)
/// rather than a glyphon GPU paint callback: the glyphon paint (in-pass
/// callback AND offscreen texture) composited black inside `egui_tiles` panes
/// on the real eframe/winit swapchain — a class of defect the wgpu test harness
/// could not reproduce — whereas native text renders reliably everywhere.
///
/// Rows are NOT wrapped (`max_width = INFINITY`): each terminal row stays one
/// visual line and is clipped at the pane edge by the caller's `painter_at`
/// clip rect, so row alignment is preserved.
///
/// The argument list is a bundle of per-frame render inputs (font size,
/// line-height, theme, focus, cursor config, effects, padding) threaded from the
/// single call site in [`C0pl4ndApp::render_pane_body`]; the
/// `too_many_arguments` allow matches that sibling free function for the same
/// reason.
///
/// Rows are painted ONE GALLEY PER ROW at the effective row pitch
/// ([`effective_row_pitch`] of the natural galley height and the configured
/// `line_height_px`) rather than as a single multi-row galley — egui's combined
/// galley uses the font's own line spacing, which the Line-height setting could
/// not influence. Per-row positioning makes the row pitch the live, configurable
/// thing the cursor / search / hit-test all share.
#[allow(clippy::too_many_arguments)]
fn paint_grid_native(
    painter: &egui::Painter,
    rect: egui::Rect,
    term: &PaneTerm,
    galley_cache: &mut GalleyCache,
    font_size: f32,
    line_height_px: f32,
    theme: &c0pl4nd_core::Theme,
    focused: bool,
    cursor_cfg: c0pl4nd_core::config::CursorConfig,
    effects: c0pl4nd_core::config::EffectsConfig,
    padding: f32,
) {
    let default_fg = term_default_fg(theme);
    let font = egui::FontId::monospace(font_size);
    // Inset the grid by the configurable window padding (points), read live from
    // `config.window.padding` each frame (threaded down from `grid_ui`). Pure
    // [`grid_text_origin`] helper so the live-apply wiring is unit-testable.
    let origin = grid_text_origin(rect, padding);
    // Cell size in POINTS: `M` advance for width, the effective row pitch for the
    // vertical advance per grid row. This is the SAME `(cw, ch)` the cursor,
    // search highlight, and hyperlink hit-test use, so every Y stays aligned.
    let (cw, ch) = monospace_cell_points(painter, font_size, line_height_px);

    // Each row becomes one galley, painted at `origin.y + row_idx * ch`.
    // Damage-gated, already grouped per-row by [`PaneTerm::grid_rows`] (an `Rc`
    // clone on the idle/blinking-cursor path — no per-frame grid clone, run
    // rebuild, or newline-split). The fallback (dead session mid-frame) wraps the
    // mono text in the same `Rc` shape so the paint loop below is uniform.
    let rows: std::rc::Rc<Vec<Vec<ColorRun>>> = match term.grid_rows() {
        Some(rows) if !rows.is_empty() => rows,
        _ => {
            // No colour runs (e.g. dead session mid-frame): mono fallback so the
            // pane is never blank. One row per text line, all in the default fg.
            std::rc::Rc::new(
                term.grid_text()
                    .unwrap_or_default()
                    .lines()
                    .map(|line| vec![(line.to_string(), pane_term::RunStyle::plain(default_fg))])
                    .collect(),
            )
        }
    };

    // The effective chromatic intensity (gated by the explicit enable toggle),
    // resolved to a PHYSICAL-px-aware ghost offset so the fringe clears the glyph
    // on HiDPI panels (issue #28). Zero-cost when off (offset == 0 ⇒ skipped).
    let ppp = painter.ctx().pixels_per_point();
    let chroma = effects.effective_chromatic();
    let ghost_offset = chromatic_offset(chroma, ppp);
    let ghost_alpha = chromatic_ghost_alpha(chroma);
    // Style bits shared by every glyph this frame (font size + the fallback fg).
    // Folded into each glyph's cache key so a font-size or theme change relays
    // them (a font FAMILY change clears the whole cache via `clear()`).
    let style_key = row_style_key(font_size, default_fg);
    let default_fg32 = egui::Color32::from_rgb(default_fg.0, default_fg.1, default_fg.2);
    // Paint each grid CELL's glyph at its exact cell origin `origin + (col*cw,
    // row*ch)`. Positions are COMPUTED from the cell column, never accumulated
    // from glyph advances, so the layout is font-advance-independent: a wide
    // (CJK/emoji) or fallback glyph occupies its own cell(s) and can NEVER shift
    // another cell — and there is no proportional-font scatter (the failure mode
    // that reverted the per-run approach). `grid_rows` already split wide glyphs
    // into their own runs and skipped the continuation spacer, so `col_cells`
    // (advanced by each glyph's cell width) is the true grid column. Blank cells
    // are skipped (the background is already painted); this also bounds the glyph
    // count to the non-blank glyphs actually on screen.
    // --- PASS 1: per-cell BACKGROUNDS -------------------------------------
    // Every cell whose resolved background is NOT the window default gets a
    // filled quad, painted BEFORE any glyph so the text sits on top of it. This
    // is what makes `grep --color`, `ls` directory colours, `git diff`, fzf's
    // selected row, starship segments and every TUI's selected row show their
    // coloured block — the runs used to carry only a foreground, so all of that
    // rendered as plain text on the window background. It is also what makes
    // reverse video (SGR `7`) visible at all: with no quad, an inverse cell drew
    // its BACKGROUND colour as text onto an unchanged background.
    //
    // `row_cell_spans` merges neighbouring same-style runs, so a highlighted
    // region is ONE quad rather than one per glyph, and the quads are snapped to
    // the physical pixel grid: adjacent spans share a snapped boundary, so they
    // tile exactly with no hairline seam and no overlap at any DPI.
    let bold_font = egui::FontId::new(font_size, fonts::bold_monospace_family());
    let faux_bold = !fonts::bold_face_available();
    for (row_idx, runs) in rows.iter().enumerate() {
        let row_y = origin.y + row_idx as f32 * ch;
        let y0 = snap_to_physical(row_y, ppp);
        let y1 = snap_to_physical(row_y + ch, ppp);
        for span in pane_term::row_cell_spans(runs) {
            let Some(bg) = span.style.bg else {
                continue; // window default — the common case, no quad needed
            };
            let x0 = snap_to_physical(origin.x + span.col as f32 * cw, ppp);
            let x1 = snap_to_physical(origin.x + (span.col + span.width) as f32 * cw, ppp);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)),
                0.0,
                egui::Color32::from_rgb(bg.0, bg.1, bg.2),
            );
        }
    }

    // --- PASS 2: glyphs ----------------------------------------------------
    for (row_idx, runs) in rows.iter().enumerate() {
        let row_y = origin.y + row_idx as f32 * ch;
        // `row_glyph_cells` is the single source of truth for per-cell X: each
        // painted glyph paired with its grid cell column (wide glyphs advance 2,
        // blanks skipped). Positions are COMPUTED from the cell column, never
        // accumulated from glyph advances — see its doc + unit tests.
        for (c, style, col_cells) in pane_term::row_glyph_cells(runs) {
            let rgb = style.fg;
            let attrs = glyph_cache::GlyphAttrs {
                bold: style.bold,
                italic: style.italic,
            };
            // A bold cell is drawn with the dedicated bold FAMILY (egui's FontId
            // selects a family, not a weight). When the machine has no bold cut
            // that family mirrors the regular stack, so the glyph is additionally
            // double-struck a half physical pixel to the right — faux bold, the
            // same fallback every terminal uses rather than drawing SGR-1 thin.
            let glyph_font = if style.bold { &bold_font } else { &font };
            let cell_origin = egui::pos2(origin.x + col_cells as f32 * cw, row_y);
            // --- chromatic aberration (CRT effect, off by default): pure-
            // channel ghosts at ±offset BEHIND the crisp glyph (red left,
            // blue right), edge-weighted by the row's vertical position.
            if ghost_offset > 0.0 {
                let off =
                    chromatic_edge_weighted_offset(ghost_offset, row_y, rect.top(), rect.bottom());
                let red = egui::Color32::from_rgba_unmultiplied(255, 0, 0, ghost_alpha);
                let red_g = galley_cache.glyph(
                    painter,
                    glyph_cache_key(c, (ghost_alpha, 0, 1), RowPass::GhostRed, style_key, attrs),
                    || build_glyph_job(c, glyph_font, red, attrs),
                );
                painter.galley(cell_origin + egui::vec2(-off, 0.0), red_g, default_fg32);
                let blue = egui::Color32::from_rgba_unmultiplied(0, 0, 255, ghost_alpha);
                let blue_g = galley_cache.glyph(
                    painter,
                    glyph_cache_key(c, (ghost_alpha, 0, 2), RowPass::GhostBlue, style_key, attrs),
                    || build_glyph_job(c, glyph_font, blue, attrs),
                );
                painter.galley(cell_origin + egui::vec2(off, 0.0), blue_g, default_fg32);
            }
            // Crisp main pass in the cell's real colour, on top of ghosts.
            let color = egui::Color32::from_rgb(rgb.0, rgb.1, rgb.2);
            let main_g = galley_cache.glyph(
                painter,
                glyph_cache_key(c, rgb, RowPass::Main, style_key, attrs),
                || build_glyph_job(c, glyph_font, color, attrs),
            );
            // Faux bold: no bold cut is installed, so double-strike the SAME
            // galley half a PHYSICAL pixel right, UNDER the crisp pass. That
            // thickens the stem at any DPI without shifting the cell (the offset
            // is sub-cell), which is how a terminal shows SGR-1 on a font that
            // ships only one weight.
            if style.bold && faux_bold {
                painter.galley(
                    cell_origin + egui::vec2(0.5 / ppp.max(0.01), 0.0),
                    std::sync::Arc::clone(&main_g),
                    default_fg32,
                );
            }
            painter.galley(cell_origin, main_g, default_fg32);
        }
    }

    // --- PASS 3: line decorations -----------------------------------------
    // Underlines (including the `4:0..5` styled variants and the SGR 58/59
    // underline colour) and strikethrough are drawn ANALYTICALLY per contiguous
    // span rather than baked into each glyph's galley. Per-span is what makes an
    // underline continuous across a word instead of one dash per glyph, and
    // analytic is what makes the curly variant survive HiDPI — a sampled sine
    // scales with `pixels_per_point`, a pre-rendered squiggle glyph does not.
    for (row_idx, runs) in rows.iter().enumerate() {
        let row_y = origin.y + row_idx as f32 * ch;
        for span in pane_term::row_cell_spans(runs) {
            if !span.style.has_decoration() {
                continue;
            }
            let x0 = origin.x + span.col as f32 * cw;
            let x1 = origin.x + (span.col + span.width) as f32 * cw;
            // At least one PHYSICAL pixel, so a decoration is never sub-pixel and
            // invisible on a low-DPI display.
            let thickness = (ch * 0.06).max(1.0 / ppp.max(0.01));
            let deco_rgb = span.style.underline_color.unwrap_or(span.style.fg);
            let deco = egui::Color32::from_rgb(deco_rgb.0, deco_rgb.1, deco_rgb.2);
            if span.style.underline != c0pl4nd_core::grid::UnderlineStyle::None {
                paint_underline(
                    painter,
                    x0,
                    x1,
                    row_y + ch - thickness * 2.0,
                    thickness,
                    deco,
                    span.style.underline,
                    ppp,
                );
            }
            if span.style.strikeout {
                // Strikethrough always takes the TEXT colour: SGR 58 scopes the
                // custom colour to the underline only.
                let fg = span.style.fg;
                let y = snap_to_physical(row_y + ch * 0.55, ppp);
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, y),
                        egui::pos2(x1, y + thickness.max(1.0 / ppp.max(0.01))),
                    ),
                    0.0,
                    egui::Color32::from_rgb(fg.0, fg.1, fg.2),
                );
            }
            if span.style.overline {
                // SGR 53: a line along the TOP of the cell, in the TEXT colour
                // (like strikeout, the custom SGR-58 colour is underline-scoped).
                let fg = span.style.fg;
                let y = snap_to_physical(row_y, ppp);
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, y),
                        egui::pos2(x1, y + thickness.max(1.0 / ppp.max(0.01))),
                    ),
                    0.0,
                    egui::Color32::from_rgb(fg.0, fg.1, fg.2),
                );
            }
        }
    }

    // --- terminal cursor ---
    if let Some((row, col)) = term.cursor_cell() {
        let cell_min = origin + egui::vec2(col as f32 * cw, row as f32 * ch);
        let cell = egui::Rect::from_min_size(cell_min, egui::vec2(cw, ch));
        let cur = c0pl4nd_core::theme::parse_hex(&theme.cursor).unwrap_or((0, 255, 144));
        let col32 = egui::Color32::from_rgb(cur.0, cur.1, cur.2);
        // Blink only on the focused pane (and only if configured). The live
        // window repaints every frame, so the phase animates without an explicit
        // repaint request; headless tests see a steady ON frame.
        let on = if cursor_cfg.blink && focused {
            (painter.ctx().input(|i| i.time) / 1.06).fract() < 0.5
        } else {
            true
        };
        if on {
            match cursor_cfg.style {
                c0pl4nd_core::config::CursorStyle::Block => {
                    if focused {
                        // Semi-transparent fill so the glyph beneath stays legible.
                        painter.rect_filled(cell, 1.0, col32.gamma_multiply(0.55));
                    } else {
                        painter.rect_stroke(
                            cell,
                            1.0,
                            egui::Stroke::new(1.0f32, col32),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                c0pl4nd_core::config::CursorStyle::Bar => {
                    let bar = egui::Rect::from_min_size(cell_min, egui::vec2(2.0, ch));
                    painter.rect_filled(bar, 0.0, col32);
                }
                c0pl4nd_core::config::CursorStyle::Underline => {
                    let under = egui::Rect::from_min_size(
                        cell_min + egui::vec2(0.0f32, ch - 2.0),
                        egui::vec2(cw, 2.0),
                    );
                    painter.rect_filled(under, 0.0, col32);
                }
            }
        }
    }

    // --- CRT scanlines (research §1): the LAST thing painted over this pane's
    // grid, so the dark bands dim the glyphs + cursor uniformly. Filled dark
    // bands at a physical-px-anchored period + an animated rolling scan band.
    // Drawn only when the setting is on (strictly zero-cost otherwise); the
    // repaint request keeps the roll animating without an explicit timer.
    if effects.crt_scanlines {
        // F2-2: honour the user's reduced-motion preference (env override OR the
        // OS accessibility setting). When reduced motion is requested, FREEZE the
        // rolling scan band (`t = 0` → a static frame; the dark scan-line bands
        // are a texture, not motion, so they remain) and STOP the per-frame
        // animation repaint. This makes the "Auto-disabled under reduced-motion"
        // promise the settings UI already shows actually true.
        let reduce = c0pl4nd_core::reduced_motion::reduced_motion();
        // Freeze the scanline drift (static texture, bands still painted) under
        // reduced-motion OR when the master animation switch is off; otherwise the
        // drift clock is scaled by the dedicated `scanline_speed` multiplier (its
        // own Motion → Scanline-drift-speed slider), so the scan bands roll at
        // their configured rate independently of the other overlays and the
        // UI-transition-speed slider. Default 1.0 reproduces the shipped roll.
        let speed = effects.clamped_scanline_speed();
        let animate = !reduce && effects.animations_enabled;
        let t = if animate {
            painter.ctx().input(|i| i.time) as f32 * speed
        } else {
            0.0
        };
        paint_crt_scanlines(painter, rect, ppp, t, effects.scanline_darkness);
        if animate {
            painter.ctx().request_repaint();
        }
    }
}

/// Fold the per-frame style bits (font size + fallback fg) into a stable seed for
/// a glyph's cache key. Font SIZE is captured here (so a size change relays the
/// glyphs); a font FAMILY/fallback change instead clears the whole cache (the
/// galleys reference the old atlas). Pure → unit-testable.
fn row_style_key(font_size: f32, default_fg: (u8, u8, u8)) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    font_size.to_bits().hash(&mut h);
    default_fg.hash(&mut h);
    h.finish()
}

/// Quote a script `path` as a command line for the active shell (#35), so the
/// shell EXECUTES the file (via its shebang/interpreter) rather than the app
/// reading + injecting its lines. The form depends on the shell named by
/// `shell_label`:
///
/// * **PowerShell** (`PowerShell 7` / `Windows PowerShell`): the call operator
///   `& "<path>"` — required because a bare quoted path in PowerShell is a
///   string expression, not an invocation. Embedded `"` are backtick-escaped
///   (PowerShell's double-quote escape inside a `"…"` string).
/// * **cmd / Default shell on Windows**: a plain double-quoted path `"<path>"`.
/// * **POSIX shells** (bash/zsh/fish/sh, the Default shell off Windows): a
///   single-quoted path `'<path>'`, with the POSIX `'\''` escape for any
///   embedded single quote.
///
/// Pure (no I/O) so the per-shell quoting is unit-testable. The path is rendered
/// with `Path::display()` (lossy on non-UTF-8 paths — acceptable for a
/// user-picked script path typed into a shell).
fn quote_path_for_shell(path: &std::path::Path, shell_label: &str) -> String {
    let raw = path.display().to_string();
    if shell_label.contains("PowerShell") {
        // PowerShell call operator; `"` → `` ` `` + `"` inside the double-quoted
        // string.
        let escaped = raw.replace('"', "`\"");
        return format!("& \"{escaped}\"");
    }
    if cfg!(windows) {
        // cmd.exe (incl. the Windows "Default shell"): a double-quoted path. cmd
        // has no in-quote escape for `"`, but Windows paths cannot contain `"`,
        // so a plain wrap is correct.
        return format!("\"{raw}\"");
    }
    // POSIX shell: single-quote, escaping any embedded single quote as '\''.
    let escaped = raw.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Map an `egui::Key` (+ modifiers) onto the engine-agnostic [`LogicalKey`] for
/// the special keys the PTY needs as escape sequences. Returns `None` for keys
/// whose text is already delivered via `egui::Event::Text` (ordinary printable
/// characters), so they are not double-sent. Ctrl-letter chords ARE encoded
/// here (egui does not emit `Event::Text` for them) into their C0 control byte.
/// Rebuild the `Event::Key` that `egui-winit` swallowed when it converted a
/// clipboard chord into `Event::Copy` / `Event::Cut`.
///
/// `Event::Copy`/`Event::Cut` carry no key and no modifiers, so the chord is
/// reconstructed from the frame's modifier snapshot plus the key the predicate
/// that fired implies (`C` for copy, `X` for cut). Feeding this back into the
/// event queue lets the ONE existing PTY encoder ([`egui_key_to_logical`] +
/// `PaneTerm::forward_key`) produce the control byte, instead of a second
/// hand-rolled `write_bytes` path that would bypass the kitty keyboard protocol.
fn restored_chord_key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn egui_key_to_logical(
    key: egui::Key,
    mods: c0pl4nd_core::term::KeyModifiers,
) -> Option<c0pl4nd_core::term::LogicalKey> {
    use c0pl4nd_core::term::LogicalKey;
    use egui::Key;
    let lk = match key {
        Key::Enter => LogicalKey::Enter,
        Key::Backspace => LogicalKey::Backspace,
        Key::Tab => LogicalKey::Tab,
        Key::Escape => LogicalKey::Escape,
        Key::Space if mods.ctrl => {
            // Ctrl+Space → NUL (the canonical set-mark byte). Ordinary Space is
            // delivered via Event::Text, so only the Ctrl chord is handled here.
            return Some(LogicalKey::Text(String::from('\u{0}')));
        }
        Key::ArrowUp => LogicalKey::ArrowUp,
        Key::ArrowDown => LogicalKey::ArrowDown,
        Key::ArrowRight => LogicalKey::ArrowRight,
        Key::ArrowLeft => LogicalKey::ArrowLeft,
        Key::Home => LogicalKey::Home,
        Key::End => LogicalKey::End,
        Key::Insert => LogicalKey::Insert,
        Key::Delete => LogicalKey::Delete,
        Key::PageUp => LogicalKey::PageUp,
        Key::PageDown => LogicalKey::PageDown,
        Key::F1 => LogicalKey::Function(1),
        Key::F2 => LogicalKey::Function(2),
        Key::F3 => LogicalKey::Function(3),
        Key::F4 => LogicalKey::Function(4),
        Key::F5 => LogicalKey::Function(5),
        Key::F6 => LogicalKey::Function(6),
        Key::F7 => LogicalKey::Function(7),
        Key::F8 => LogicalKey::Function(8),
        Key::F9 => LogicalKey::Function(9),
        Key::F10 => LogicalKey::Function(10),
        Key::F11 => LogicalKey::Function(11),
        Key::F12 => LogicalKey::Function(12),
        other => {
            // Ctrl + a-z → the C0 control byte (Ctrl+C = 0x03, etc.). egui does
            // not emit Event::Text for these chords, so encode them here.
            if mods.ctrl {
                if let Some(name) = other.name().chars().next() {
                    let up = name.to_ascii_uppercase();
                    if up.is_ascii_uppercase() {
                        let ctrl_byte = (up as u8) & 0x1f;
                        return Some(LogicalKey::Text(
                            String::from_utf8(vec![ctrl_byte]).unwrap_or_default(),
                        ));
                    }
                }
            }
            return None;
        }
    };
    Some(lk)
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

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
#[cfg(test)]
mod config_load_tests {
    //! F5-2: a present-but-broken config file must surface an error (so the host
    //! can toast it), an absent file must NOT, and a valid file parses cleanly.
    use super::load_config_from;

    #[test]
    fn absent_config_yields_defaults_with_no_error() {
        let (cfg, err) = load_config_from(None);
        assert_eq!(cfg, c0pl4nd_core::Config::default());
        assert!(
            err.is_none(),
            "an absent config is normal — no error surfaced"
        );
    }

    #[test]
    fn corrupt_config_yields_defaults_with_a_surfaced_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is = not valid toml [[[").unwrap();
        let (cfg, err) = load_config_from(Some(path));
        assert_eq!(
            cfg,
            c0pl4nd_core::Config::default(),
            "falls back to defaults"
        );
        assert!(
            err.is_some(),
            "a present-but-invalid config MUST surface an error for the toast"
        );
        let msg = err.unwrap();
        // Plain user copy referencing the settings file, with no leaked parser
        // detail (no raw toml jargon like "[[[" or "expected").
        assert!(msg.to_lowercase().contains("settings"), "{msg}");
        assert!(!msg.contains("[[["), "{msg}");
        assert!(!msg.to_lowercase().contains("expected"), "{msg}");
    }

    #[test]
    fn valid_config_parses_with_no_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"ghost-paper\"\n").unwrap();
        let (cfg, err) = load_config_from(Some(path));
        assert_eq!(cfg.theme, "ghost-paper");
        assert!(err.is_none());
    }
}
