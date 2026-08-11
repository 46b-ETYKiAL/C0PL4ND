//! The C0PL4ND egui chrome shell.
//!
//! This module is the modern `eframe`/`egui` application shell and the
//! CANONICAL `c0pl4nd` binary; the original winit-driven terminal is preserved
//! beside it as `c0pl4nd-legacy` (see `crates/app/Cargo.toml`). The chrome (frameless
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
// `pub(crate)` (not private) so `crate::notify::plan` can reach the ONE
// focused-suppression predicate rather than reimplementing it. The toast and the
// taskbar flash are two escalations of the same decision; two copies of it would
// drift.
pub(crate) mod taskbar;
pub(crate) use grid_interaction::*;
mod config_load;
pub(crate) use config_load::*;
mod config_watch;
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
/// The terminal-grid PAINTER — `paint_grid_native` and the free helpers it
/// delegates to (underlines, physical-pixel snapping, the text origin, the
/// galley-cache key). Glob-imported so every call site keeps its original
/// spelling, matching how `crt`/`glyph_cache`/`grid_interaction` are wired.
mod paint_grid;
/// The pane-body RENDER PATH — `render_pane_body`, the single largest item this
/// module used to hold. An inherent `impl` block there, so the call site in
/// `grid_ui` keeps its original `Self::render_pane_body` spelling.
mod pane_body;
/// Frameless window edge/corner resize (#24) — the pure hit-test, the cursor
/// mapping, and the per-frame manual resize driver, with their geometry
/// constants and the `resize_tests` guard that pins the hit-test.
mod resize;
use paint_grid::*;
use resize::handle_frameless_resize;

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

/// What a close request actually did — the resolved product of BOTH window-close
/// decisions, in the order the close path applies them:
/// `WindowConfig::close_guard` (is a shell command still running?) then
/// `WindowConfig::close_action` (exit, or hide to the tray?).
///
/// Recorded in [`C0pl4ndApp::last_close_outcome`] because the real effects are
/// invisible to a headless harness: `Exit` calls `process::exit`, `HideToTray`
/// issues an OS viewport command. The outcome is what a test can assert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// Run the shutdown side effects and exit the process.
    Exit,
    /// Hold the close and show the running-command confirmation instead.
    Confirm {
        /// How many panes report a command still in flight (always `>= 1`).
        busy_panes: usize,
    },
    /// Keep the process alive and hide the window to the tray.
    HideToTray,
}

/// A pinned terminal-cursor blink phase, for deterministic visual-QA capture.
///
/// The caret's phase is normally a function of the frame clock, so a snapshot
/// scene captures it wherever the clock happens to land — the same scene showed
/// the caret painted on one run and gone the next, which makes "cursor
/// placement" un-eyeball-able from the PNGs. [`C0pl4ndApp::set_cursor_blink_phase`]
/// pins it for the frames a test is capturing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorBlinkPhase {
    /// The caret is painted this frame, whatever the clock says.
    On,
    /// The caret is not painted this frame, whatever the clock says.
    Off,
}

/// Set when a real "quit" affordance asked the app to close — today the tray
/// menu's own Quit item.
///
/// A process-wide flag rather than a field on [`C0pl4ndApp`] for exactly the
/// reason [`FORWARDED_LAUNCHES`] is: the producer is the tray's global
/// `MenuEvent` handler in the binary-local `tray` module, which runs on the
/// event-loop thread with no `&mut App` to write into, and which signals the
/// close by posting `WM_CLOSE` — i.e. it arrives at the SAME `close_requested`
/// path an ordinary caption-✕ takes and is otherwise indistinguishable from it.
///
/// That indistinguishability is the whole point: without this flag,
/// close-to-tray would swallow the tray's own Quit and the app could never be
/// closed at all (see `WindowConfig::close_action`'s `explicit_quit`).
static EXPLICIT_QUIT_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Record that the NEXT close request is a real quit, and must exit rather than
/// hide to the tray. Called from the tray menu's Quit handler immediately before
/// it posts `WM_CLOSE`.
pub fn request_explicit_quit() {
    EXPLICIT_QUIT_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// CONSUME the explicit-quit flag: `true` once per [`request_explicit_quit`].
///
/// Consuming (rather than peeking) is what stops one tray Quit from making every
/// later close in the session bypass close-to-tray.
pub fn take_explicit_quit() -> bool {
    EXPLICIT_QUIT_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed)
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
    /// exactly how a manually-opened terminal (`spawn_term_in`) already behaves —
    /// after which the debounced resize is a no-op and the cursor stays put.
    pub(crate) pending_spawn: HashSet<PaneId>,
    /// Working directories captured from a previous run's persisted layout
    /// snapshot, keyed by the restored pane id. Consumed (removed) by the
    /// deferred first-spawn in [`render_pane_body`]: a pane with an entry spawns
    /// its shell in that dir, a pane without one spawns in the default dir. Empty
    /// on a fresh launch and after every entry is consumed — so a pane the user
    /// later splits never inherits a stale restored cwd.
    pub(crate) restored_cwds: HashMap<PaneId, String>,
    /// The working directories of recently CLOSED panes, most-recent LAST — the
    /// undo stack behind [`C0pl4ndApp::reopen_closed_tab`].
    ///
    /// Each entry is the pane's OSC-7-reported cwd at the moment it was closed,
    /// or `None` for a pane whose shell never reported one (a failed spawn, or a
    /// shell with no OSC 7 integration). `None` is DELIBERATELY recorded rather
    /// than dropped: the user closed a pane and asked for it back, so the pane
    /// must return either way — we simply cannot say where it was, and it opens
    /// in the default dir. Recording only the panes we happen to know the cwd of
    /// would make the chord silently do nothing on `cmd.exe`.
    ///
    /// Captured in [`C0pl4ndApp::close_pane`] — the ONE function every close path
    /// (tab ×, egui_tiles close button, context menu, the `close_tab` chord)
    /// routes through — so no close path can bypass it. Bounded to
    /// [`MAX_CLOSED_TAB_HISTORY`]; the oldest entry is dropped past the cap so a
    /// long session cannot grow this without limit.
    pub(crate) closed_tab_cwds: Vec<Option<String>>,
    /// The directory the most recent pane spawn was asked to start the shell in,
    /// or `None` when it was asked for the shell's default. Written by
    /// [`C0pl4ndApp::spawn_term_in`] at the branch that actually passes the
    /// directory to the PTY. An observation field in the same family as
    /// [`last_window_cmd`](Self::last_window_cmd).
    pub(crate) last_spawn_cwd: Option<String>,
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
    /// Watches the on-disk `config.toml` so an external edit takes effect LIVE
    /// (see [`Self::config_hot_reload_tick`]). Points at
    /// [`c0pl4nd_core::Config::default_path`] by default; a test repoints it
    /// with [`Self::watch_config_at`].
    pub(crate) config_watch: config_watch::ConfigWatcher,
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
    /// Whether a system-tray icon actually EXISTS for this process.
    ///
    /// The tray is a binary-local module of the shipping `c0pl4nd` binary (it
    /// needs the real HWND + the winit message loop), so the lib cannot ask it
    /// directly; the binary reports in via [`Self::set_tray_available`]. It
    /// starts `false` and that default is load-bearing rather than lazy: a
    /// close-to-tray hide with NO icon would strand the window invisible with no
    /// way back, so the whole feature degrades to a real exit until something
    /// proves a tray exists (see `WindowConfig::close_action`).
    pub(crate) tray_available: bool,
    /// `Some(busy_panes)` while the running-command close confirmation is on
    /// screen — the count `WindowConfig::close_guard` reported. `None` when no
    /// confirmation is pending.
    pub(crate) close_confirm: Option<usize>,
    /// The user's answer to that confirmation ("Close anyway"). Fed straight
    /// into `close_guard`'s `already_confirmed`, which short-circuits to
    /// `Proceed` so the second pass through the close path cannot re-prompt (the
    /// commands are still running when the user says yes). Reset whenever a
    /// close does NOT end in an exit, so a later close prompts again.
    pub(crate) close_confirmed: bool,
    /// The most recent close DECISION ([`Self::close_decision`]). Observable for
    /// the same reason as [`last_window_cmd`](Self::last_window_cmd): the real
    /// effects (`process::exit`, hiding the OS window) are invisible to a
    /// headless harness, so this is what a test asserts the config decision
    /// actually reached.
    pub(crate) last_close_outcome: Option<CloseOutcome>,
    /// How many close requests reached the real exit branch. In a live window
    /// the process is gone before this is read; in the headless harness it is
    /// the observable proof that a close was NOT swallowed by the guard or by
    /// close-to-tray.
    pub(crate) exit_requests: u32,
    /// Deterministic override for the terminal cursor's blink phase, for visual
    /// QA capture. `None` (the default, and the only state the shipping app ever
    /// runs in) leaves the phase free-running off the frame clock. See
    /// [`Self::set_cursor_blink_phase`].
    pub(crate) cursor_blink_phase: Option<CursorBlinkPhase>,
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
/// Launches that a SECOND `c0pl4nd.exe` handed to this already-running instance
/// instead of opening a rival window (see the binary-local `single_instance`
/// module). Each entry is that launch's `--cwd`, or `None` for a plain launch.
///
/// A process-wide queue rather than a field on [`C0pl4ndApp`] because the
/// producer is a bare Win32 window procedure: it is an `extern "system"` fn that
/// cannot capture, runs on the event-loop thread the instant the message is
/// dispatched, and has no `&mut App` to write into. `frame_tick` drains it.
///
/// Bounded at [`MAX_PENDING_FORWARDED_LAUNCHES`]: a script hammering the exe
/// while the app is busy must not grow this without limit, and opening more
/// panes than the grid can hold is pointless anyway.
static FORWARDED_LAUNCHES: std::sync::Mutex<Vec<Option<String>>> =
    std::sync::Mutex::new(Vec::new());

/// The cap on [`FORWARDED_LAUNCHES`]. Comfortably above the 6-pane grid cap, so
/// a realistic burst is never dropped, while still bounded.
const MAX_PENDING_FORWARDED_LAUNCHES: usize = 32;

/// Hand a forwarded launch to the running instance. Called from the
/// single-instance window procedure; the next frame opens a pane for it.
///
/// Never panics and never blocks meaningfully: a poisoned lock is ignored (the
/// forwarded launch is dropped rather than taking down the OS callback that a
/// second process is synchronously waiting on).
pub fn push_forwarded_launch(cwd: Option<String>) {
    if let Ok(mut q) = FORWARDED_LAUNCHES.lock() {
        if q.len() < MAX_PENDING_FORWARDED_LAUNCHES {
            q.push(cwd);
        }
    }
}

/// Drain the forwarded launches queued since the last call.
pub fn take_forwarded_launches() -> Vec<Option<String>> {
    FORWARDED_LAUNCHES
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// How many closed panes [`C0pl4ndApp::closed_tab_cwds`] remembers. Deep enough
/// that "I closed the wrong one" is always recoverable several steps back,
/// shallow enough that a day-long session cannot grow the stack without bound.
const MAX_CLOSED_TAB_HISTORY: usize = 16;

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

/// Most PROMPT (and most FAILED-COMMAND) ticks the scrollbar paints per pane.
///
/// Core retains up to 4096 prompt and 8192 command marks, and a program that
/// emits OSC 133 in a tight loop can fill both. Painting every one of them would
/// cost thousands of rects per frame AND collapse into an unreadable smear on a
/// track only a few hundred points tall, so the bar shows the most RECENT
/// [`MAX_SEMANTIC_SCROLL_MARKS`] of each kind — the ones a user is scrolling
/// back toward. Search hits are NOT capped: they are already bounded by the
/// visible grid.
const MAX_SEMANTIC_SCROLL_MARKS: usize = 256;

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

/// The active shell profile, as a deferred first-spawn needs it: the program to
/// launch (`None` = the platform default shell) and its arguments.
///
/// A borrowed bundle rather than two more loose parameters because it is
/// threaded into [`C0pl4ndApp::render_pane_body`], which is a FREE function (so
/// the egui_tiles closure can borrow `terms`/`theme` disjointly from
/// `grid_tree`) and already carries a long argument list.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SpawnProfile<'a> {
    /// The program to launch; `None` means the platform default shell.
    pub(crate) program: Option<&'a str>,
    /// Arguments passed to `program` (empty for a bare interactive shell).
    pub(crate) args: &'a [String],
}

/// THE ONE PANE-SPAWN FUNNEL: turn a shell profile (`program` + `args`, where
/// `None` is the platform default shell) plus an optional working directory into
/// a live [`PaneTerm`].
///
/// Both spawn paths route through here — [`C0pl4ndApp::spawn_term_in`] (split /
/// new tab / reopen-closed-pane) and the DEFERRED first-spawn in
/// [`C0pl4ndApp::render_pane_body`] (the initial pane and every restored one).
/// That is the point of the funnel: the two used to make the profile-vs-cwd
/// choice independently, and they disagreed. `spawn_term_in`'s named-profile arm
/// dropped the cwd (it called the directory-less `PaneTerm::spawn_program`), and
/// the deferred arm ignored the profile entirely and always spawned the default
/// shell — so a restored layout under PowerShell/WSL came back as the default
/// shell, in the default directory. One funnel, one answer.
///
/// `cwd = None` keeps the pre-existing cwd-less spawn EXACTLY as it was (the
/// shell's own default directory); a `cwd` that no longer exists falls back to
/// home inside the core spawn, and a failed spawn degrades to an error pane —
/// never a panic.
fn spawn_pane_term(
    theme: c0pl4nd_core::Theme,
    program: Option<&str>,
    args: &[String],
    cols: u16,
    rows: u16,
    term: Option<&str>,
    cwd: Option<&str>,
) -> PaneTerm {
    match program {
        // A NAMED profile: its program wins, and the cwd now travels with it.
        Some(program) => {
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            PaneTerm::spawn_program_in(theme, program, &arg_refs, cols, rows, cwd)
        }
        // The platform default shell.
        None => match cwd {
            Some(dir) => PaneTerm::spawn_in_with_term(theme, cols, rows, term, Some(dir)),
            None => PaneTerm::spawn_with_term(theme, cols, rows, term),
        },
    }
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
            closed_tab_cwds: Vec::new(),
            last_spawn_cwd: None,
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
            // Watch the file this config came from, stamped as ALREADY-loaded so
            // the first frame never reloads what we just read. Both constructors
            // (`bootstrap` for tests, `new` for the shipping binary) route
            // through here, so the hot-reload wire exists in exactly one place.
            config_watch: match c0pl4nd_core::Config::default_path() {
                Some(p) => config_watch::ConfigWatcher::watching(p),
                None => config_watch::ConfigWatcher::default(),
            },
            warmed_atlas: None,
            warmup_frames_left: 0,
            font_wait_frames: 0,
            pending_font_save_at: None,
            update_rx: None,
            last_update_notice: None,
            last_window_cmd: None,
            // Fail-safe default: no tray until the shipping binary proves one
            // exists, so a hide can never strand the window (see the field doc).
            tray_available: false,
            close_confirm: None,
            close_confirmed: false,
            last_close_outcome: None,
            exit_requests: 0,
            cursor_blink_phase: None,
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

    /// Spawn a fresh live terminal for `pid`, starting the shell in `cwd` when
    /// one is given (`None` = the shell's own default directory, which is what
    /// every path except reopen-closed-pane wants).
    ///
    /// Deliberately ONE function with an `Option` rather than a plain
    /// `spawn_term` plus an `_in` variant: the wrapper had exactly zero callers
    /// once `split_in` landed, and a dead pass-through is how a second spawn path
    /// starts drifting from the first.
    ///
    /// **The cwd now applies to a NAMED profile too.** It used to apply to the
    /// DEFAULT shell only: this branch called `PaneTerm::spawn_program`, which
    /// had no directory parameter at all, so reopening a closed pane (or
    /// restoring a layout) while a named profile was active silently landed in
    /// the default directory. `PaneTerm::spawn_program_in` closed that gap; the
    /// shared [`spawn_pane_term`] funnel below is where both arms consume it, so
    /// the immediate and deferred spawn paths cannot drift apart again.
    fn spawn_term_in(&mut self, pid: PaneId, cwd: Option<&str>) {
        let theme = self.theme.clone();
        let term_name = self.config.term.clone();
        let profile = self.shell_profiles.get(self.active_shell);
        let program = profile.and_then(|p| p.program.clone());
        let args: Vec<String> = profile.map(|p| p.args.clone()).unwrap_or_default();
        // Record the directory the shell was ACTUALLY asked to start in, at the
        // exact branch that asks for it. Mirrors `last_window_cmd`: it makes an
        // otherwise-invisible spawn argument observable, so the reopen wiring
        // test asserts the cwd reached the spawn rather than merely that a pane
        // appeared (which a pane opened in the wrong directory would also
        // satisfy). Set for BOTH profile arms now that both honour it.
        self.last_spawn_cwd = cwd.map(str::to_string);
        let term = spawn_pane_term(
            theme,
            program.as_deref(),
            &args,
            SPAWN_COLS,
            SPAWN_ROWS,
            Some(term_name.as_str()),
            cwd,
        );
        self.terms.insert(pid, term);
    }

    /// Open one pane per launch that a second process forwarded to us, each in
    /// the directory that launch asked for (`None` = the shell default).
    ///
    /// Reuses the ordinary new-pane path, so a forwarded launch is subject to
    /// exactly the same pane cap and toast as pressing "+" — a script running
    /// the exe in a loop can never grow the grid past its limit.
    pub(crate) fn drain_forwarded_launches(&mut self, ctx: &egui::Context) {
        for cwd in take_forwarded_launches() {
            self.new_terminal_in(cwd.as_deref());
            ctx.request_repaint();
        }
    }

    /// Remember a closed pane's cwd on the reopen stack, dropping the oldest
    /// entry past [`MAX_CLOSED_TAB_HISTORY`].
    fn push_closed_tab_cwd(&mut self, cwd: Option<String>) {
        self.closed_tab_cwds.push(cwd);
        if self.closed_tab_cwds.len() > MAX_CLOSED_TAB_HISTORY {
            self.closed_tab_cwds.remove(0);
        }
    }

    /// Re-open the most recently closed pane, in the directory it was closed in.
    /// A no-op with an empty stack.
    ///
    /// The stack entry is PEEKED and only popped once the pane actually exists:
    /// at the 6-pane cap `new_terminal_in` refuses (with a toast), and popping
    /// regardless would silently consume the user's undo step for a pane they
    /// never got back.
    pub(crate) fn reopen_closed_tab(&mut self) {
        let Some(cwd) = self.closed_tab_cwds.last().cloned() else {
            return;
        };
        if self.new_terminal_in(cwd.as_deref()) {
            self.closed_tab_cwds.pop();
        }
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

    /// A pane's BODY rect (screen points) as of the last rendered frame, or
    /// `None` before the first frame has laid the grid out. The scrollbar is an
    /// overlay on the right edge of this rect, so the scrollbar-mark test uses it
    /// to locate the bar's painted shapes in the frame output.
    #[allow(dead_code)]
    pub fn pane_body_rect(&self, pane_id: PaneId) -> Option<egui::Rect> {
        self.pane_rects.get(&pane_id).copied()
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
        self.split_in(dir, None);
    }

    /// [`split`](Self::split), but starting the new pane's shell in `cwd`.
    /// Returns whether a pane was actually created — `false` at the pane cap or
    /// when the tree refuses the split. The reopen path needs that answer so it
    /// does not consume an undo step for a pane it never got.
    fn split_in(&mut self, dir: egui_tiles::LinearDir, cwd: Option<&str>) -> bool {
        if count_panes(&self.grid_tree) >= grid::MAX_PANES {
            self.toast = Some(format!(
                "You've reached the maximum of {} panes. Close one to open another.",
                grid::MAX_PANES
            ));
            return false;
        }
        let new_pane = self.pane_alloc.alloc();
        if grid::split_focused(&mut self.grid_tree, self.focused_pane, new_pane, dir) {
            self.spawn_term_in(new_pane, cwd);
            self.focused_pane = new_pane;
            self.toast = None;
            return true;
        }
        false
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
        self.new_terminal_in(None);
    }

    /// [`new_terminal`](Self::new_terminal), but starting the new pane's shell in
    /// `cwd`. Returns whether a pane was created (see [`split_in`](Self::split_in)).
    fn new_terminal_in(&mut self, cwd: Option<&str>) -> bool {
        let (w, h) = self.last_focused_size.unwrap_or((16.0, 9.0));
        let dir = if w >= h {
            egui_tiles::LinearDir::Horizontal // wide → side-by-side
        } else {
            egui_tiles::LinearDir::Vertical // tall → stacked
        };
        self.split_in(dir, cwd)
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
            // The ACTIVE shell profile, borrowed disjointly (separate fields from
            // `terms` / `grid_tree`) so a DEFERRED first-spawn runs the SAME shell
            // the immediate `spawn_term_in` path would. It used to always spawn the
            // platform default, so a restored layout captured under a named profile
            // came back running the wrong shell.
            let active_profile = self.shell_profiles.get(self.active_shell);
            let spawn_profile = SpawnProfile {
                program: active_profile.and_then(|p| p.program.as_deref()),
                args: active_profile.map_or(&[][..], |p| p.args.as_slice()),
            };
            // Deterministic cursor-blink phase for visual-QA capture. `None` in the
            // shipping app (and by default in every test), which leaves the phase
            // free-running off the frame clock exactly as before.
            let cursor_blink_phase = self.cursor_blink_phase;
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
                    spawn_profile,
                    font_size,
                    line_height_px,
                    cursor_cfg,
                    cursor_blink_phase,
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
        // Record where this pane was BEFORE its terminal is dropped — after this
        // line the `PaneTerm` (and with it the OSC 7 cwd) is gone for good. This
        // sits in `close_pane` rather than in the `close_tab` action precisely
        // because the action is only ONE of four close paths: the tab-bar ×, the
        // egui_tiles close button, and the right-click Close Pane item all reach
        // the pane's end here and nowhere else, so capturing at the action would
        // silently forget every pane closed by mouse.
        self.push_closed_tab_cwd(self.terms.get(&pid).and_then(PaneTerm::cwd));
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
            // Live-apply the (possibly changed) config: reload + propagate the
            // terminal theme, re-derive the chrome Visuals, re-assert the
            // always-on-top level. Shared verbatim with the config HOT RELOAD
            // path (`config_hot_reload_tick`) so an external `config.toml` edit
            // and a Settings edit can never apply DIFFERENT subsets of a change.
            self.apply_config_live(ctx, outcome.theme_changed);
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
                    // Re-stamp the watcher so OUR write is not read back as an
                    // external edit on the next poll (which would re-apply the
                    // theme + visuals on every slider nudge).
                    self.config_watch.mark_self_written();
                }
            }
        }
    }

    /// Apply the LIVE `self.config` to everything that can change without a
    /// relaunch. The single apply path shared by the Settings window and the
    /// config hot reload, so the two can never diverge:
    ///
    /// 1. **Terminal theme** (only when `theme_changed`) — reloaded from disk /
    ///    the built-in set and propagated to every live pane, because each
    ///    `PaneTerm` holds its own `Theme` clone and the grid's glyph colours
    ///    resolve from it, not from egui Visuals.
    /// 2. **Chrome Visuals** — re-derived from the (possibly new) terminal theme
    ///    plus the window opacity, so titlebar/tabs/status bar follow the theme.
    /// 3. **Always-on-top** — re-asserted as a viewport command. Idempotent, so
    ///    re-sending it on any change is harmless.
    fn apply_config_live(&mut self, ctx: &egui::Context, theme_changed: bool) {
        if theme_changed {
            let (theme, theme_notice) = load_terminal_theme(&self.config);
            self.theme = theme;
            if let Some(notice) = theme_notice {
                // A user-authored theme file existed but failed to parse —
                // surface it instead of silently showing fallback colours.
                self.toast = Some(notice);
            }
            for term in self.terms.values_mut() {
                term.set_theme(self.theme.clone());
            }
        }
        let mut visuals = theme::visuals_from_theme(&self.theme);
        window_effects::apply_window_opacity(&mut visuals, self.config.opacity);
        ctx.set_visuals(visuals);
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            if self.config.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            },
        ));
    }

    /// Point the config watcher at `path` and treat that file's CURRENT contents
    /// as already-loaded, so only edits made from now on hot-reload.
    ///
    /// The shipping binary watches [`c0pl4nd_core::Config::default_path`] (set
    /// in `bootstrap_with`); this repoints the watcher, which is how the
    /// hot-reload wiring test drives a temp config file instead of the user's
    /// real one.
    pub fn watch_config_at(&mut self, path: std::path::PathBuf) {
        self.config_watch = config_watch::ConfigWatcher::watching(path);
    }

    /// One per-frame tick of config HOT RELOAD: when `config.toml` changed on
    /// disk since we last read it, re-parse it and apply it live — no relaunch.
    ///
    /// Called from [`Self::frame_tick`]. The watcher throttles the actual
    /// filesystem `stat` to one per [`config_watch::POLL_INTERVAL`], so the
    /// per-frame cost of this call in the common (unchanged) case is a clock
    /// comparison.
    ///
    /// A file that fails to PARSE never clobbers the running config: the app
    /// keeps the settings it has and surfaces the error as a toast, exactly like
    /// a bad config at launch. Reverting a user's whole live setup because they
    /// saved a half-typed TOML line would be strictly worse than ignoring it.
    /// The bad file's stamp is already recorded, so the app waits quietly for
    /// the next save rather than re-toasting every 400 ms.
    fn config_hot_reload_tick(&mut self, ctx: &egui::Context) {
        let Some(path) = self.config_watch.poll(std::time::Instant::now()) else {
            return;
        };
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            // Vanished/locked between the stat and the read — nothing to apply.
            Err(_) => return,
        };
        match c0pl4nd_core::Config::from_toml(&src, &path) {
            Ok(new_config) => {
                if new_config == self.config {
                    // A touched-but-equivalent file (a comment edit, a
                    // reformat, our own save on a path `mark_self_written`
                    // missed). Nothing to apply, and re-theming would be a
                    // visible flicker for no change.
                    return;
                }
                let theme_changed = new_config.theme != self.config.theme;
                self.config = new_config;
                self.apply_config_live(ctx, theme_changed);
                self.toast = Some("Reloaded config.toml".to_string());
                ctx.request_repaint();
            }
            Err(e) => {
                tracing::warn!(
                    target: "c0pl4nd::config",
                    path = ?path,
                    "config hot reload failed to parse; keeping the running config"
                );
                self.toast = Some(crate::user_error::config_reload_failed(e.to_string()));
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
            // Our own write — re-stamp so the hot-reload watcher does not read
            // it back as an external edit on its next poll.
            self.config_watch.mark_self_written();
        }
    }

    // ---- the close path: guard → action → exit / hide / confirm ----
    //
    // Every close surface funnels through `handle_close_request`, which applies
    // the two `WindowConfig` decisions in order and is the ONLY place that
    // decides whether the app actually goes away.

    /// Report whether a system-tray icon actually exists.
    ///
    /// Called by the shipping binary right after its best-effort `tray::init`
    /// (the tray is binary-local — it needs the real HWND and the winit message
    /// loop — so the lib cannot ask it directly). Until something calls this the
    /// answer is `false`, and close-to-tray degrades to a real exit rather than
    /// hiding the window behind an icon that does not exist.
    pub fn set_tray_available(&mut self, available: bool) {
        self.tray_available = available;
    }

    /// Whether a system-tray icon exists (see [`Self::set_tray_available`]).
    pub fn tray_available(&self) -> bool {
        self.tray_available
    }

    /// Pin the terminal caret's blink phase for deterministic visual-QA capture,
    /// or `None` to restore the free-running phase.
    ///
    /// Snapshot scenes render a handful of frames and then read the pixels; the
    /// caret's phase is a function of the frame clock, so the same scene showed a
    /// painted caret on one run and none on the next. Pin it with
    /// `Some(CursorBlinkPhase::On)` before capturing and the caret is in the
    /// frame every time.
    pub fn set_cursor_blink_phase(&mut self, phase: Option<CursorBlinkPhase>) {
        self.cursor_blink_phase = phase;
    }

    /// The pinned cursor-blink phase, if any (see [`Self::set_cursor_blink_phase`]).
    pub fn cursor_blink_phase(&self) -> Option<CursorBlinkPhase> {
        self.cursor_blink_phase
    }

    /// How many panes have a shell command STILL RUNNING — the count
    /// `WindowConfig::close_guard` decides on.
    ///
    /// Reads each pane's OSC 133 marks via [`PaneTerm::has_running_command`]. A
    /// shell with no prompt integration emits no marks and reports `false`, so
    /// the guard MISSES rather than nags — the documented and deliberate
    /// direction (a spurious "something is running" prompt on every close would
    /// be worse than an occasional silent kill).
    pub fn busy_pane_count(&self) -> usize {
        self.terms
            .values()
            .filter(|t| t.has_running_command())
            .count()
    }

    /// Apply BOTH window-close decisions, in order, and record the result.
    ///
    /// 1. `close_guard(busy_panes, already_confirmed)` — is a shell command still
    ///    running? If so the close is HELD for confirmation. `close_confirmed`
    ///    (the user's "Close anyway") short-circuits it, so the second pass
    ///    cannot re-prompt: the commands are still running when they say yes, and
    ///    without the short-circuit the prompt would loop forever.
    /// 2. `close_action(tray_available, explicit_quit)` — exit, or hide to the
    ///    tray? Two of its inputs are load-bearing rather than cosmetic: with NO
    ///    tray it always exits (a hide would strand the window with no icon to
    ///    restore it), and an explicit quit always exits (the tray menu's own
    ///    Quit posts `WM_CLOSE` into this very path, so without it close-to-tray
    ///    would swallow the one affordance that closes the app).
    pub(crate) fn close_decision(&mut self, explicit_quit: bool) -> CloseOutcome {
        let busy = self.busy_pane_count();
        let outcome = match self.config.window.close_guard(busy, self.close_confirmed) {
            c0pl4nd_core::config::CloseGuard::Confirm { busy_panes } => {
                CloseOutcome::Confirm { busy_panes }
            }
            c0pl4nd_core::config::CloseGuard::Proceed => {
                match self
                    .config
                    .window
                    .close_action(self.tray_available, explicit_quit)
                {
                    c0pl4nd_core::config::CloseAction::HideToTray => CloseOutcome::HideToTray,
                    c0pl4nd_core::config::CloseAction::Exit => CloseOutcome::Exit,
                }
            }
        };
        self.last_close_outcome = Some(outcome);
        outcome
    }

    /// Run [`Self::close_decision`] and carry out whatever it decided.
    ///
    /// `os_close` marks a request the OS already accepted (the `close_requested`
    /// viewport flag): those must be CANCELLED when the close does not proceed,
    /// or winit tears the window down anyway and the guard/tray decision is
    /// cosmetic. An in-app request (`WindowCmd::Close`, the Alt+F4 the caption
    /// subclass swallowed) has nothing to cancel.
    fn handle_close_request(&mut self, ctx: &egui::Context, explicit_quit: bool, os_close: bool) {
        match self.close_decision(explicit_quit) {
            CloseOutcome::Confirm { busy_panes } => {
                self.close_confirm = Some(busy_panes);
                if os_close {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            }
            CloseOutcome::HideToTray => {
                self.close_confirm = None;
                // The close did not happen, so a later one must be able to warn
                // again about whatever is still running.
                self.close_confirmed = false;
                if os_close {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            CloseOutcome::Exit => {
                self.close_confirm = None;
                // Fast clean shutdown: persist config + reap every PTY child so
                // none orphan, then exit immediately. This skips eframe/wgpu's
                // slow graceful GPU-device + swapchain + winit-window-destroy
                // teardown — the real source of the slow-to-close latency.
                self.prepare_shutdown();
                self.exit_requests += 1;
                // Gated on `live_window` so the headless egui_kittest harness —
                // which has no real viewport — records the exit instead of
                // killing the test process mid-run.
                if self.live_window {
                    std::process::exit(0);
                }
            }
        }
    }

    /// The most recent close decision, or `None` if no close has been requested.
    pub fn last_close_outcome(&self) -> Option<CloseOutcome> {
        self.last_close_outcome
    }

    /// How many close requests reached the real exit branch.
    pub fn exit_requests(&self) -> u32 {
        self.exit_requests
    }

    /// The busy-pane count the pending close confirmation is showing, or `None`
    /// when no confirmation is up.
    pub fn close_confirm_busy_panes(&self) -> Option<usize> {
        self.close_confirm
    }

    /// The running-command close confirmation: a small centred modal naming how
    /// many panes still have a command in flight, with "Close anyway" / "Keep
    /// working". Closing kills every child outright, so an in-flight
    /// `cargo build` / `rsync` / migration would otherwise die with no prompt.
    /// Enter = close anyway, Esc = keep working (so the modal is keyboard-drivable,
    /// mirroring the paste confirm).
    fn close_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(busy_panes) = self.close_confirm else {
            return;
        };
        let (confirm, cancel) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let mut do_close = confirm;
        let mut do_cancel = cancel;
        let win = egui::Window::new("Close while a command is running?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let panes = if busy_panes == 1 { "pane" } else { "panes" };
                ui.label(format!(
                    "{busy_panes} {panes} still have a command running. Closing \
                     ends them immediately."
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Close anyway (Enter)").clicked() {
                        do_close = true;
                    }
                    if ui.button("Keep working (Esc)").clicked() {
                        do_cancel = true;
                    }
                });
            });
        // Exclude the confirm modal from the whole-window motion overlays.
        self.note_overlay_rect(win.map(|w| w.response.rect));
        if do_close {
            // Record the answer, then re-enter the SAME close path: the guard now
            // short-circuits to Proceed and `close_action` gets its say.
            self.close_confirmed = true;
            self.close_confirm = None;
            self.handle_close_request(ctx, false, false);
        } else if do_cancel {
            self.close_confirm = None;
            self.close_confirmed = false;
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
        // Fast close for an OS-initiated window-close (taskbar → Close, the system
        // menu, and the tray menu's Quit — which posts WM_CLOSE). The in-app
        // caption-× reaches the same decision via `WindowCmd::Close`; without this,
        // an OS close falls through to eframe/wgpu's slow graceful GPU-device +
        // swapchain + winit-window teardown — the real source of the slow-to-close
        // latency (the PTY teardown is ~2ms).
        //
        // Routed through `handle_close_request` so the running-command guard and
        // the close-to-tray preference actually apply here. `os_close = true`
        // because winit has ALREADY accepted this close and will tear the window
        // down unless the decision cancels it. `take_explicit_quit` consumes the
        // flag the tray's Quit sets — the ONLY thing distinguishing that Quit from
        // an ordinary ✕ by the time it arrives here, and without it close-to-tray
        // would swallow the one affordance that closes the app.
        //
        // No longer gated on `live_window`: the exit itself is (inside
        // `handle_close_request`), so the headless egui_kittest harness can drive
        // the real decision without `process::exit` killing the test process.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.handle_close_request(ctx, take_explicit_quit(), true);
        }
        // Alt+F4 close, restored in-app. Removing WS_SYSMENU (to kill the doubled
        // native close button — see `caption_close`) means DefWindowProc no longer
        // translates Alt+F4 into a WM_CLOSE, so egui/winit still delivers the key
        // event but the OS never turns it into a close_requested — hence
        // `os_close = false`: there is no accepted OS close to cancel here.
        if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::F4)) {
            self.handle_close_request(ctx, take_explicit_quit(), false);
        }
        // Config HOT RELOAD: pick up an external `config.toml` edit live, with
        // no relaunch. Runs BEFORE the theme/motion ticks below so a reloaded
        // theme or motion setting takes effect on THIS frame rather than the
        // next. Throttled inside the watcher to one filesystem stat per
        // `config_watch::POLL_INTERVAL`.
        // A SECOND `c0pl4nd.exe` launch (another "Open C0PL4ND here", or just
        // running the exe again) is forwarded to THIS instance rather than
        // opening a rival window. Its pane is opened here, on the UI thread —
        // the window procedure that received it only queues, because it runs
        // inside a synchronous `SendMessage` the other process is blocked on.
        self.drain_forwarded_launches(ctx);
        self.config_hot_reload_tick(ctx);
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
        // Same per-frame, idempotent apply for the OSC 52 clipboard-READ gate
        // (`clipboard_read_allow`, DEFAULT-DENY) so the setting takes effect —
        // and so turning it back off takes effect just as promptly.
        let clipboard_read_allow = self.config.clipboard_read_allow;
        for term in self.terms.values() {
            term.set_max_scrollback(scrollback);
            term.set_clipboard_read_allowed(clipboard_read_allow);
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
                    // The caption ✕. Same decision funnel as the OS close and
                    // Alt+F4: the running-command guard may hold it for
                    // confirmation, and close-to-tray may hide instead of exit.
                    // `os_close = false` — nothing accepted a close to cancel;
                    // `explicit_quit = false` — the ✕ means "close this window",
                    // which is exactly what close-to-tray reinterprets.
                    self.handle_close_request(ctx, false, false);
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

        // 5c-bis) the running-command close confirmation, if a close was HELD by
        //     `close_guard`. Enter closes anyway (re-entering the same close path
        //     with the answer recorded), Esc keeps working. Only ever up when a
        //     pane reported an unfinished OSC 133 command AND the preference is on.
        if self.close_confirm.is_some() {
            self.close_confirm_window(ctx);
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
        let mut notifications: Vec<c0pl4nd_core::term::Notification> = Vec::new();
        let mut progress: Vec<c0pl4nd_core::term::osc::Progress> = Vec::new();
        for pane in self.terms.values_mut() {
            let fx = pane.pump_host_effects();
            clipboard.extend(fx.clipboard_writes);
            colors.extend(fx.color_sets);
            notifications.extend(fx.notifications);
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
        // OSC 9/777 desktop notification while the window is unfocused → a real
        // OS toast AND the taskbar flash.
        //
        // The flash is KEPT alongside the toast deliberately: it is the only
        // signal on a host where no toast can be shown (non-Windows, notifications
        // disabled by policy, no installed Start-Menu shortcut carrying the
        // AUMID), and it is what leaves the taskbar button highlighted after the
        // toast auto-dismisses. Dropping it would weaken shipped behaviour.
        //
        // `notify::plan` owns BOTH decisions so they cannot drift apart, and it
        // reaches the same focused-suppression predicate this block used to call
        // directly (`taskbar::should_request_attention`) rather than reimplementing
        // it — `focused == None` at startup is treated as focused, so an rc-file's
        // notification during launch neither toasts nor flashes.
        //
        // The notification TEXT is read here and shown. That is not a privacy
        // regression: the old "never surface the text" rule was about LOGGING
        // (an OSC payload can carry a 2FA code or a secret URL), and nothing on
        // this path traces, logs, or persists it — `notify::show` builds it into
        // a toast XML string, hands it to the shell, and drops it.
        let focused = ctx.input(|i| i.viewport().focused);
        let plan = crate::notify::plan(&notifications, focused);
        if let Some(text) = &plan.toast {
            crate::notify::show(text);
        }
        if plan.flash {
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

/// Whether the terminal caret is PAINTED this frame.
///
/// * `forced` — the visual-QA phase pin ([`C0pl4ndApp::set_cursor_blink_phase`]).
///   When set it wins outright, which is the whole point: the free-running form
///   below made a snapshot scene capture the caret solid on one run and gone on
///   the next, so the PNGs could not serve the "cursor placement" eyeball their
///   module doc claims. `None` — the shipping app's only state — leaves the
///   behaviour exactly as it was.
/// * Otherwise the caret blinks only on the FOCUSED pane and only when
///   configured to; an unfocused or blink-disabled caret is steady-on (it is
///   drawn as a hollow outline instead, so it must not also vanish).
///
/// Pure, so the phase wiring is unit-testable without a frame.
fn cursor_blink_on(
    forced: Option<CursorBlinkPhase>,
    blink: bool,
    focused: bool,
    time_secs: f64,
) -> bool {
    if let Some(phase) = forced {
        return phase == CursorBlinkPhase::On;
    }
    if !(blink && focused) {
        return true;
    }
    // Full period = two half-periods, tied to the same constant the idle repaint
    // tick schedules from, so the caret cannot toggle at a rate nothing repaints.
    let period = 2.0 * (CURSOR_BLINK_HALF_PERIOD_MS as f64) / 1000.0;
    (time_secs / period).fract() < 0.5
}

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

/// Outcome of painting one terminal pane's body for a frame.
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
#[path = "close_path_tests.rs"]
mod close_path_tests;
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
