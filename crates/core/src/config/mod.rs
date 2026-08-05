//! Configuration: great defaults + a simple, non-programming-language TOML
//! file with line-level error surfacing. Zero-config is a first-class goal —
//! C0PL4ND must be fully usable before the user ever opens a config file.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub mod keybindings;

pub use keybindings::{action_label, canonical_key_token, Chord, KeybindingIssue, Keybindings};

/// A configuration load error with enough context to point the user at the
/// offending line — never a bare panic on a malformed file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file did not exist at the resolved path; built-in defaults
    /// are used instead.
    #[error("config file not found at {0} (using built-in defaults)")]
    NotFound(PathBuf),
    /// The config file exists but could not be read (I/O error).
    #[error("could not read config file {path}: {source}")]
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The config file could not be parsed as TOML.
    #[error("config parse error in {path}: {message}")]
    Parse {
        /// Path that failed to parse.
        path: PathBuf,
        /// Human-readable parse-error message, including line context.
        message: String,
    },
    /// The config parsed but failed semantic validation (e.g. an out-of-range
    /// value).
    #[error("config validation error: {0}")]
    Invalid(String),
}

/// Font configuration: the primary family, size, line height, and glyph
/// fallback chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    /// Primary font family name — the TERMINAL grid typeface. Bound to egui's
    /// `FontFamily::Monospace`; INDEPENDENT of [`ui_family`](Self::ui_family) (the
    /// proportional UI font), so swapping the app-UI font never changes the
    /// terminal font and vice-versa. Mirrors SCR1B3's editor/UI font split.
    pub family: String,
    /// Font size in points.
    pub size: f32,
    /// Line height in pixels (cell vertical advance).
    pub line_height: f32,
    /// Ordered fallback families for glyphs the primary font lacks (CJK, etc.).
    pub fallback: Vec<String>,
    /// App-UI font family (the proportional text EVERYWHERE except the terminal
    /// grid): settings, chrome, toolbar, status bar, menus. One of the bundled
    /// family display names, or "System default" to keep egui's built-in UI font.
    /// Bound to `FontFamily::Proportional`, separate from [`family`](Self::family)
    /// (the terminal/monospace font) so the UI-font swap never touches the terminal
    /// surface. Default: "IBM Plex Mono". Mirrors SCR1B3's `ui_family`.
    #[serde(default = "default_ui_family")]
    pub ui_family: String,
}

/// The app-UI-font choice that keeps egui's built-in proportional font (i.e. does
/// not swap the Proportional slot). Offered in Settings → Fonts → App UI font.
pub const UI_FONT_SYSTEM_DEFAULT: &str = "System default";

/// Default app-UI font family — IBM Plex Mono, so the whole app reads as one
/// typeface (matching the terminal's default voice) out of the box. A bundled
/// (OFL) face, so it renders on every machine. Named by `#[serde(default = ...)]`
/// so an older config missing the key upgrades to this rather than an empty string.
pub fn default_ui_family() -> String {
    "IBM Plex Mono".to_string()
}

impl Default for FontConfig {
    fn default() -> Self {
        // JetBrains Mono is the default mono voice — and, critically, it is a
        // BUNDLED face (see the app crate's `fonts::BUNDLED_FONTS`), so the
        // zero-config default renders in the intended typeface on every machine.
        // The prior default ("Monaspace Neon") was NOT bundled, so `bundled_face`
        // returned `None` and the default silently fell back to egui's built-in
        // monospace — a latent default-font bug this fixes. An explicit user
        // `font.family` is untouched (this is the serde default only).
        FontConfig {
            family: "JetBrains Mono".to_string(),
            // 13 logical points — the industry-standard terminal grid size
            // (Windows Terminal / WezTerm sit at 12, VS Code's terminal at 14).
            // egui interprets a `FontId` size as LOGICAL points scaled by the
            // display's `pixels_per_point`, so this is the on-screen size at 100%
            // scaling and scales up cleanly on HiDPI (e.g. 26 physical px at 2×).
            // The previous 14.0 read a touch large, especially on HiDPI panels.
            size: 13.0,
            line_height: 20.0,
            fallback: vec!["Noto Sans JP".to_string(), "monospace".to_string()],
            ui_family: default_ui_family(),
        }
    }
}

/// When (and whether) C0PL4ND checks for updates. The default is
/// [`UpdateMode::Notify`] — once per launch (when due) the app reads the public
/// GitHub Releases API and shows a passive toast if a newer version exists. The
/// check is read-only and sends zero identifiers, and it never downloads or
/// installs on its own. Users who want a fully local-first, no-network-on-launch
/// experience set `manual` (check only on demand from Settings) or `off` (never
/// touch the network for updates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    /// Never check, never touch the network for updates.
    Off,
    /// Default: check once per launch (when due); show a passive toast if a newer
    /// version exists. Read-only; never downloads or installs on its own.
    #[default]
    Notify,
    /// Check only when the user presses "Check for updates".
    Manual,
    /// Check once per launch (when due); download + apply a verified update when
    /// one is found.
    Auto,
}

/// Update behaviour. The default mode is `notify`, so on launch (when due)
/// C0PL4ND performs a single read-only GitHub-Releases version check and shows
/// a passive toast if a newer version exists. Set `manual` (check only on
/// demand from Settings or `c0pl4nd update`) or `off` (never touch the network)
/// for a fully local-first experience.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    /// When the app checks for updates (`off`/`notify`/`manual`/`auto`).
    pub mode: UpdateMode,
    /// How often, in hours, an on-launch check (`notify`/`auto`) is due (1–168).
    /// Ignored for `off`/`manual`.
    pub check_interval_hours: u32,
    /// Legacy on-launch toggle, retained so older config files keep loading. The
    /// canonical control is now [`UpdateConfig::mode`]; `mode == Off|Manual`
    /// means "no on-launch network", `mode == Notify|Auto` means "check on
    /// launch". The launch path treats `check_on_launch == true` OR a
    /// network-on-launch `mode` as "check".
    pub check_on_launch: bool,
    /// Release channel to track.
    pub channel: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig {
            mode: UpdateMode::Notify,
            check_interval_hours: 24,
            check_on_launch: false,
            channel: "stable".to_string(),
        }
    }
}

impl UpdateConfig {
    /// Whether an on-launch (background) update check should run: true for the
    /// network-on-launch modes (`notify`/`auto`), OR when the legacy
    /// `check_on_launch` flag is set (so old config files keep their behaviour).
    pub fn checks_on_launch(&self) -> bool {
        matches!(self.mode, UpdateMode::Notify | UpdateMode::Auto) || self.check_on_launch
    }
}

/// Which side a popout panel docks to. Used by the command-history sidebar
/// (`#21`): the user prefers a popout sidebar over a dropdown, and can dock it
/// left or right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PanelSide {
    Left,
    #[default]
    Right,
}

/// The shape of the text cursor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    /// A filled block covering the whole cell.
    Block,
    /// A thin vertical bar at the cell's left edge.
    Bar,
    /// A horizontal underline at the cell's baseline.
    Underline,
}

/// Cursor appearance configuration: shape and blink behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorConfig {
    /// The cursor shape.
    pub style: CursorStyle,
    /// Whether the cursor blinks.
    pub blink: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        CursorConfig {
            style: CursorStyle::Block,
            blink: true,
        }
    }
}

/// Window configuration: the initial terminal dimensions, inner padding, and
/// the persisted geometry restored on the next launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    /// Initial terminal width in columns.
    pub cols: u16,
    /// Initial terminal height in rows.
    pub rows: u16,
    /// Inner padding between the window edge and the grid, in pixels.
    pub padding: u16,
    /// Remembered window geometry (physical pixels), persisted on resize/move/
    /// exit and restored on launch. `None` = use the cols/rows-derived default.
    /// Optional so configs written before this field still parse cleanly.
    pub pos_x: Option<i32>,
    pub pos_y: Option<i32>,
    pub size_w: Option<u32>,
    pub size_h: Option<u32>,
    pub maximized: Option<bool>,
    /// Identifies the monitor the geometry was captured on, so a saved position
    /// is only restored when that monitor is still connected (multi-monitor
    /// safety). Matched against `MonitorHandle::name()` at restore time.
    pub monitor: Option<String>,
    /// Closing the window (caption ✕ / Alt+F4 / taskbar Close) hides it to the
    /// system tray instead of quitting. OFF by default: the ✕ meaning "quit" is
    /// the platform expectation, and a user who has never seen the tray icon
    /// would otherwise think the app vanished while its shells kept running.
    /// Honoured only when a tray icon actually exists — see
    /// [`WindowConfig::close_action`].
    pub close_to_tray: bool,
    /// Minimizing the window hides it to the system tray instead of leaving it
    /// on the taskbar. OFF by default, same reasoning as
    /// [`close_to_tray`](Self::close_to_tray). Honoured only when a tray icon
    /// actually exists — see [`WindowConfig::minimize_action`].
    pub minimize_to_tray: bool,
    /// Ask for confirmation before closing while a shell command is still
    /// running. ON by default: closing kills every child process outright, so an
    /// in-flight `cargo build` / `rsync` / migration dies with no prompt — a real
    /// data-loss surface. The prompt can only fire when the shell reports OSC 133
    /// command marks, so a shell with no prompt integration never sees it (see
    /// [`WindowConfig::close_guard`]).
    pub warn_on_close_running: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig {
            cols: 80,
            rows: 24,
            padding: 8,
            pos_x: None,
            pos_y: None,
            size_w: None,
            size_h: None,
            maximized: None,
            monitor: None,
            close_to_tray: false,
            minimize_to_tray: false,
            warn_on_close_running: true,
        }
    }
}

/// What a window-close request should actually do — the decision
/// [`WindowConfig::close_action`] makes for the app's close paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// Run the real shutdown (persist config, reap every PTY child) and exit.
    Exit,
    /// Keep the process alive and hide the window to the tray instead.
    HideToTray,
}

/// What a window-minimize request should actually do — the decision
/// [`WindowConfig::minimize_action`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimizeAction {
    /// Ordinary minimize to the taskbar.
    Minimize,
    /// Hide the window entirely; the tray icon is the only way back.
    HideToTray,
}

/// Whether a close may proceed, or must first confirm with the user — the
/// decision [`WindowConfig::close_guard`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseGuard {
    /// Nothing is running (or the warning is off): close immediately.
    Proceed,
    /// `busy_panes` panes have a command in flight; confirm before killing them.
    Confirm {
        /// How many panes report an unfinished command (always `>= 1`).
        busy_panes: usize,
    },
}

impl WindowConfig {
    /// What a close request should do.
    ///
    /// * `tray_available` — whether a tray icon was actually created. The tray
    ///   build is best-effort (a headless/session-0 shell has none), and hiding
    ///   to a tray that does not exist would strand the window with no way back
    ///   and no visible process. So a missing tray always degrades to
    ///   [`CloseAction::Exit`], never to an unrecoverable hide.
    /// * `explicit_quit` — the request came from a real "quit" affordance (the
    ///   tray menu's Quit, a File→Quit item). This ALWAYS exits. Without it,
    ///   close-to-tray would swallow the tray's own Quit — which posts `WM_CLOSE`
    ///   into this very path — and the app could never be closed at all.
    #[must_use]
    pub fn close_action(&self, tray_available: bool, explicit_quit: bool) -> CloseAction {
        if !explicit_quit && self.close_to_tray && tray_available {
            CloseAction::HideToTray
        } else {
            CloseAction::Exit
        }
    }

    /// What a minimize request should do. `tray_available` degrades a hide to an
    /// ordinary minimize for the same reason as [`close_action`](Self::close_action):
    /// never hide a window the user cannot get back.
    #[must_use]
    pub fn minimize_action(&self, tray_available: bool) -> MinimizeAction {
        if self.minimize_to_tray && tray_available {
            MinimizeAction::HideToTray
        } else {
            MinimizeAction::Minimize
        }
    }

    /// Whether a close must confirm first, given how many panes have a command
    /// in flight.
    ///
    /// `already_confirmed` is the user's answer to a previous prompt ("Close
    /// anyway"). It short-circuits to [`CloseGuard::Proceed`] so the second pass
    /// through the close path cannot re-prompt — without it the confirmation
    /// would loop forever, since the commands are still running when the user
    /// says yes.
    #[must_use]
    pub fn close_guard(&self, busy_panes: usize, already_confirmed: bool) -> CloseGuard {
        if already_confirmed || !self.warn_on_close_running || busy_panes == 0 {
            CloseGuard::Proceed
        } else {
            CloseGuard::Confirm { busy_panes }
        }
    }
}

/// Which physical GPU the renderer prefers at startup, on a multi-GPU (hybrid /
/// Optimus) machine. `Auto` (default) keeps the platform/eframe default
/// (high-performance / discrete). `Integrated` forces the low-power integrated
/// GPU and `Discrete` forces the high-performance one — the escape hatch when one
/// GPU's driver corrupts rendering (the canonical case: a laptop discrete-GPU
/// driver garbles the terminal-grid glyph atlas while the integrated GPU renders
/// it perfectly). Maps to eframe's wgpu `PowerPreference`. Applies on restart; the
/// `WGPU_POWER_PREF` env var still overrides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuPreference {
    /// Platform/eframe default (high-performance / discrete GPU).
    #[default]
    Auto,
    /// Force the low-power integrated GPU (e.g. Intel iGPU on an Optimus laptop).
    Integrated,
    /// Force the high-performance discrete GPU.
    Discrete,
}

/// GPU backend the renderer requests at startup. `Auto` (default) keeps the
/// platform-smart choice; the explicit variants force a specific wgpu backend to
/// work around a driver-specific rendering glitch (the canonical case: corrupted
/// terminal-grid glyphs under a bad Windows DX12 driver, fixed by switching to
/// Vulkan). Serialized lowercase (`auto` / `dx12` / `vulkan` / `gl`). The choice
/// applies on restart (the backend is chosen once, before the GPU device is
/// created); the `WGPU_BACKEND` env var still overrides it for one-off debugging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsBackend {
    /// Platform-smart default: DX12 on Windows (Vulkan when window transparency
    /// is enabled), wgpu's platform default elsewhere.
    #[default]
    Auto,
    /// Direct3D 12 (Windows).
    Dx12,
    /// Vulkan (Windows / Linux). Required for real window transparency on Windows.
    Vulkan,
    /// OpenGL — a last-resort fallback when neither DX12 nor Vulkan renders
    /// correctly.
    Gl,
}

/// How the pane shell lays out terminals: the multi-pane `egui_tiles` grid, or
/// a single full-size pane with the existing tab strip switching between them.
/// One shell layout is active at a time; the titlebar view-toggle button flips
/// between them, and the choice persists across restarts. Mirrors the
/// split-view vs tabs-view affordance common to modern terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    /// The `egui_tiles` tiling grid — every pane visible side-by-side/stacked.
    #[default]
    Grid,
    /// Only the focused pane is shown full-size; the tab strip switches panes.
    Tabs,
}

impl ViewMode {
    /// The mode reached by toggling this one (Grid ⇄ Tabs).
    pub fn toggled(self) -> ViewMode {
        match self {
            ViewMode::Grid => ViewMode::Tabs,
            ViewMode::Tabs => ViewMode::Grid,
        }
    }
}

/// Visual post-effects configuration: the CRT/scanline overlay and
/// chromatic-aberration controls. All effects are OFF by default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectsConfig {
    /// CRT scanline post-effect. OFF by default; also auto-disabled under
    /// reduced-motion / battery-save (see renderer).
    pub crt_scanlines: bool,
    /// Scanline darkness (0.0 = none .. 1.0 = strong). Tunes how dark the
    /// scanline troughs are painted; the renderer maps this to the dark-band
    /// alpha. Default [`DEFAULT_SCANLINE_DARKNESS`] reads as distinct lines
    /// rather than a flat grey film.
    #[serde(default = "default_scanline_darkness")]
    pub scanline_darkness: f32,
    /// CRT scanline drift-speed multiplier (0.25 = quarter-speed .. 3.0 =
    /// triple-speed, clamped). Scales the rolling scan-band drift clock so the
    /// default `1.0` reproduces the current shipped drift EXACTLY and higher
    /// values roll faster. Independent of [`scanline_darkness`], which tunes only
    /// the trough depth. Inert until [`crt_scanlines`](Self::crt_scanlines) is on.
    #[serde(default = "default_scanline_speed")]
    pub scanline_speed: f32,
    /// Explicit chromatic-aberration ON/OFF toggle. Distinct from
    /// [`EffectsConfig::chromatic_aberration`] (the intensity) so the UI is a
    /// checkbox + an enabled-gated intensity slider rather than a single
    /// slider the user reads as "broken" when it sits at 0. Default OFF.
    #[serde(default)]
    pub chromatic_aberration_enabled: bool,
    /// Chromatic-aberration intensity (0.0 = off). Only applied when
    /// [`EffectsConfig::chromatic_aberration_enabled`] is `true`.
    pub chromatic_aberration: f32,

    // ---- Motion / animation (SCR1B3 parity) --------------------------------
    /// Master switch for the whole animation + motion-effect system. When
    /// `false`, egui's animation time is zeroed (instant transitions, a fully
    /// static UI) and every motion overlay below is suppressed regardless of its
    /// own toggle. Default ON — preserves the current animated feel; a user who
    /// wants a static surface flips this off.
    #[serde(default = "default_animations_enabled")]
    pub animations_enabled: bool,
    /// UI transition speed: scale applied to egui's global animation time
    /// (the CHROME transitions — hover fades, panel/collapsible expand-collapse,
    /// combobox/menu fades, and value-change lerps) when
    /// [`animations_enabled`](Self::animations_enabled) is on. 1.0 = egui's
    /// default transition time (the shipped feel); 0 makes transitions instant
    /// and 2 doubles them. Does NOT control the retro visual effects (flicker /
    /// VHS / mesh / scanline drift) — those have their own per-effect speed
    /// multipliers below.
    #[serde(default = "default_animation_intensity")]
    pub animation_intensity: f32,
    /// Subtle full-window CRT-style brightness flicker. OFF by default; gated
    /// behind the master switch.
    #[serde(default)]
    pub flicker: bool,
    /// Flicker strength (0.0 = none .. capped at 0.20 for accessibility).
    #[serde(default = "default_flicker_strength")]
    pub flicker_strength: f32,
    /// Flicker cadence multiplier (0.25 = quarter-speed .. 3.0 = triple-speed,
    /// clamped). Scales the flicker's time input so the default `1.0` reproduces
    /// the current shipped cadence EXACTLY and higher values flicker faster.
    /// Independent of [`flicker_strength`](Self::flicker_strength), which tunes
    /// only the depth. Inert until [`flicker`](Self::flicker) is on.
    #[serde(default = "default_flicker_speed")]
    pub flicker_speed: f32,
    /// VHS-style horizontal tracking lines drifting down the window. OFF by
    /// default.
    #[serde(default)]
    pub vhs_tracking: bool,
    /// VHS tracking-line intensity (0.0 = faint .. 1.0 = bold, clamped). Scales
    /// how bright the drifting tracking bands read. Only applied when
    /// [`vhs_tracking`](Self::vhs_tracking) is on.
    #[serde(default = "default_vhs_intensity")]
    pub vhs_intensity: f32,
    /// VHS tracking-line drift multiplier (0.25 = quarter-speed .. 3.0 =
    /// triple-speed, clamped). Scales BOTH tracking-band drift speeds
    /// proportionally so the default `1.0` reproduces the current shipped drift
    /// EXACTLY and higher values sweep faster. Independent of
    /// [`vhs_intensity`](Self::vhs_intensity), which tunes only the band
    /// brightness. Inert until [`vhs_tracking`](Self::vhs_tracking) is on.
    #[serde(default = "default_vhs_speed")]
    pub vhs_speed: f32,
    /// Animated wired node-mesh ambient background (Lain "Wired" feel), drawn at
    /// Background order behind the panes. OFF by default.
    #[serde(default)]
    pub wired_ambient: bool,
    /// Node-mesh density (0.0 = sparse .. 2.0 = dense, clamped). Drives the node
    /// count of the wired-ambient background.
    #[serde(default = "default_mesh_density")]
    pub mesh_density: f32,
    /// Node-mesh brightness (0.0 = invisible .. 1.0 = shipped .. 3.0 = bold,
    /// clamped). Scales the lattice link + node-dot opacity so the mesh can be
    /// dimmed toward nothing or brightened to clearly pop. Only applied when
    /// [`wired_ambient`](Self::wired_ambient) is on.
    #[serde(default = "default_mesh_brightness")]
    pub mesh_brightness: f32,
    /// Node-mesh movement amount (0.0 = a still lattice .. 1.0 = the shipped
    /// drift .. 2.0 = brisk, clamped). Scales how fast the mesh nodes drift; at
    /// 0 the field holds a static frame. Only applied when
    /// [`wired_ambient`](Self::wired_ambient) is on.
    #[serde(default = "default_mesh_speed")]
    pub mesh_speed: f32,
    /// Optional node-mesh colour override as an `[r, g, b]` sRGB triple,
    /// INDEPENDENT of the theme. `None` (default) = the wired-mesh lattice follows
    /// the active theme's accent colour. `Some([r,g,b])` pins the mesh regardless
    /// of theme; the Settings "Reset to theme" button clears it back to `None` so
    /// the mesh follows the theme accent again. Mirrors the app-background-override
    /// UX. An `[u8; 3]` (not a hex `String`) keeps `EffectsConfig: Copy`.
    #[serde(default)]
    pub mesh_color: Option<[u8; 3]>,
    /// Cursor ghost-trail: a fading echo follows the focused terminal cursor as
    /// it moves. OFF by default.
    #[serde(default)]
    pub cursor_trail: bool,
    /// Cursor ghost-trail intensity (0.0 = faint/short .. 1.0 = bold/long,
    /// clamped). Scales both the echo opacity and how long each echo lingers, so
    /// the trail can be tuned from a barely-there whisper to a pronounced comet
    /// tail. Only applied when [`cursor_trail`](Self::cursor_trail) is on.
    #[serde(default = "default_cursor_trail_intensity")]
    pub cursor_trail_intensity: f32,
    /// One-shot boot "glitch" sweep on the first frames after launch. OFF by
    /// default; self-terminates.
    #[serde(default)]
    pub boot_glitch: bool,
}

/// Default master-animation switch — ON, preserving the shipped animated feel.
pub fn default_animations_enabled() -> bool {
    true
}

/// Default animation-intensity — 1.0 = egui's stock animation speed (no change
/// to the current feel until the user tunes it down).
pub fn default_animation_intensity() -> f32 {
    1.0
}

/// Default flicker strength — barely perceptible.
pub fn default_flicker_strength() -> f32 {
    0.06
}

/// Default node-mesh density — a calm, sparse field.
pub fn default_mesh_density() -> f32 {
    0.4
}

/// Default node-mesh brightness — 1.0 = the shipped lattice opacity (no change
/// to the current look until the user brightens or dims it).
pub fn default_mesh_brightness() -> f32 {
    1.0
}

/// Default cursor-trail intensity — a clearly-visible-but-tasteful comet tail
/// (mid-high so the trail reads at once when first enabled, tunable either way).
pub fn default_cursor_trail_intensity() -> f32 {
    0.6
}

/// Default node-mesh movement — 1.0 = the shipped drift speed (no change to the
/// current feel until the user tunes it).
pub fn default_mesh_speed() -> f32 {
    1.0
}

/// Default VHS tracking-line intensity — a clearly-visible-but-calm band.
pub fn default_vhs_intensity() -> f32 {
    0.5
}

/// Default flicker cadence multiplier — `1.0` reproduces the current shipped
/// flicker rate exactly, so the field changes nothing until the user tunes it.
pub fn default_flicker_speed() -> f32 {
    1.0
}

/// Default VHS tracking-drift multiplier — `1.0` reproduces the current shipped
/// drift speeds exactly, so the field changes nothing until the user tunes it.
pub fn default_vhs_speed() -> f32 {
    1.0
}

/// Default CRT scanline drift-speed multiplier — `1.0` reproduces the current
/// shipped roll rate exactly, so the field changes nothing until the user
/// tunes it.
pub fn default_scanline_speed() -> f32 {
    1.0
}

/// Default scanline darkness — strong enough to read as scan lines (not a flat
/// dimming film). Free function so `#[serde(default = ...)]` can name it.
pub fn default_scanline_darkness() -> f32 {
    DEFAULT_SCANLINE_DARKNESS
}

/// Default scanline-darkness value (≈38% trough darkening at the band centre).
pub const DEFAULT_SCANLINE_DARKNESS: f32 = 0.4;

/// The visible default chromatic-aberration intensity applied on first enable,
/// so flipping the toggle shows the effect immediately instead of a no-op at 0.
pub const DEFAULT_CHROMATIC_INTENSITY: f32 = 0.6;

impl Default for EffectsConfig {
    fn default() -> Self {
        EffectsConfig {
            crt_scanlines: false,
            scanline_darkness: DEFAULT_SCANLINE_DARKNESS,
            scanline_speed: default_scanline_speed(),
            chromatic_aberration_enabled: false,
            chromatic_aberration: 0.0,
            animations_enabled: default_animations_enabled(),
            animation_intensity: default_animation_intensity(),
            flicker: false,
            flicker_strength: default_flicker_strength(),
            flicker_speed: default_flicker_speed(),
            vhs_tracking: false,
            vhs_intensity: default_vhs_intensity(),
            vhs_speed: default_vhs_speed(),
            wired_ambient: false,
            mesh_density: default_mesh_density(),
            mesh_brightness: default_mesh_brightness(),
            mesh_speed: default_mesh_speed(),
            mesh_color: None,
            cursor_trail: false,
            cursor_trail_intensity: default_cursor_trail_intensity(),
            boot_glitch: false,
        }
    }
}

impl EffectsConfig {
    /// The effective chromatic-aberration intensity: the configured intensity
    /// only when the explicit toggle is on, else `0.0`. The single predicate the
    /// renderer consults so the toggle is honoured uniformly.
    pub fn effective_chromatic(&self) -> f32 {
        if self.chromatic_aberration_enabled {
            self.chromatic_aberration.max(0.0)
        } else {
            0.0
        }
    }

    /// UI-transition-speed factor clamped to `0.0..=2.0`, applied ONLY to egui's
    /// global chrome-transition time (hover fades, panel/collapsible expand-
    /// collapse, value-change lerps). 1.0 is egui's stock feel; 0 makes chrome
    /// transitions instant and 2.0 doubles them. It no longer drives the retro
    /// visual effects — flicker / VHS / mesh / scanline drift each carry their
    /// own per-effect speed multiplier below.
    pub fn clamped_animation_intensity(&self) -> f32 {
        self.animation_intensity.clamp(0.0, 2.0)
    }

    /// Flicker cadence multiplier clamped to its design band (0.25..=3.0). At the
    /// default `1.0` the flicker runs at the current shipped cadence exactly; the
    /// 0.25 floor forbids a frozen-but-enabled flicker.
    pub fn clamped_flicker_speed(&self) -> f32 {
        self.flicker_speed.clamp(0.25, 3.0)
    }

    /// VHS tracking-drift multiplier clamped to its design band (0.25..=3.0). At
    /// the default `1.0` the tracking bands drift at the current shipped speeds.
    pub fn clamped_vhs_speed(&self) -> f32 {
        self.vhs_speed.clamp(0.25, 3.0)
    }

    /// CRT scanline drift-speed multiplier clamped to its design band
    /// (0.25..=3.0). At the default `1.0` the scan bands roll at the current
    /// shipped rate.
    pub fn clamped_scanline_speed(&self) -> f32 {
        self.scanline_speed.clamp(0.25, 3.0)
    }

    /// Flicker strength clamped to `0.0..=1.0`. The old `0.20` ceiling read as a
    /// barely-there shimmer; 1.0 lets the flicker reach a clearly-visible CRT
    /// wobble. The painter's own alpha math keeps even the max well short of a
    /// full-black strobe (photosensitivity guard), so the band is safe to widen.
    pub fn clamped_flicker_strength(&self) -> f32 {
        self.flicker_strength.clamp(0.0, 1.0)
    }

    /// Node-mesh density clamped to `0.0..=2.0` so the mesh can go from a calm
    /// field to a busy web (the painter caps the node count for the O(n²) pass).
    pub fn clamped_mesh_density(&self) -> f32 {
        self.mesh_density.clamp(0.0, 2.0)
    }

    /// Node-mesh brightness clamped to `0.0..=3.0`. 1.0 is the shipped alpha;
    /// below 1.0 dims the lattice toward invisible, above 1.0 brightens it so a
    /// user who finds the default mesh too dim can make it clearly pop. Scales the
    /// link + dot alpha in the painter.
    pub fn clamped_mesh_brightness(&self) -> f32 {
        self.mesh_brightness.clamp(0.0, 3.0)
    }

    /// Cursor-trail intensity clamped to `0.0..=2.0` so the echo opacity /
    /// lifetime can reach a pronounced comet tail while a malformed config still
    /// can't drive it out of band.
    pub fn clamped_cursor_trail_intensity(&self) -> f32 {
        self.cursor_trail_intensity.clamp(0.0, 2.0)
    }

    /// Node-mesh movement clamped to `0.0..=2.0`. 0 holds a static lattice; 1.0
    /// is the shipped drift; up to 2.0 is a brisk field. Scales the mesh
    /// animation phase in the painter.
    pub fn clamped_mesh_speed(&self) -> f32 {
        self.mesh_speed.clamp(0.0, 2.0)
    }

    /// VHS tracking-line intensity clamped to `0.0..=1.0`. Scales the drifting
    /// tracking-band alpha in the painter.
    pub fn clamped_vhs_intensity(&self) -> f32 {
        self.vhs_intensity.clamp(0.0, 1.0)
    }
}

/// Customizable top-bar quick-action toolbar (the cluster pinned to the RIGHT of
/// the titlebar, immediately left of the settings gear). The user chooses which
/// quick actions appear, in what order, and which are parked in an overflow "⋯"
/// menu — edited in Settings → Toolbar. Action ids are from the app's
/// `TOOLBAR_ACTIONS` catalog; an unknown id is skipped at render time (so a config
/// written by a newer/older build never breaks the bar). The fixed affordances
/// (wordmark, tab strip, the tab-adjacent "+", and the window caption cluster) are
/// NOT part of this list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolbarConfig {
    /// Actions in the LEFT group (titlebar flow, after the "+"), left→right order.
    pub left: Vec<String>,
    /// Actions in the RIGHT cluster (pinned by the settings gear), left→right (the
    /// LAST id renders nearest the gear).
    pub right: Vec<String>,
    /// Actions parked in the overflow "⋯" menu instead of taking a bar slot.
    pub menu: Vec<String>,
    /// Show the overflow "⋯" menu button when [`menu`](Self::menu) is non-empty.
    /// Default `true`; turn off to hide the overflow button entirely (parked
    /// actions stay reachable via the command palette / keybindings).
    pub show_overflow: bool,
}

impl ToolbarConfig {
    /// The shipped default LEFT group (titlebar flow, after the "+"): view-toggle,
    /// equalize, shell-switcher — exactly where they were before the toolbar
    /// became customizable.
    pub fn default_left() -> Vec<String> {
        ["view_mode", "equalize_panes", "shell_switcher"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// The shipped default RIGHT cluster: just the script launcher, pinned next to
    /// the settings gear.
    pub fn default_right() -> Vec<String> {
        vec!["script_launcher".to_string()]
    }
}

impl Default for ToolbarConfig {
    fn default() -> Self {
        ToolbarConfig {
            left: ToolbarConfig::default_left(),
            right: ToolbarConfig::default_right(),
            menu: Vec::new(),
            show_overflow: true,
        }
    }
}

/// The default quake-mode drop-down height, as a fraction of the monitor WORK
/// area (the desktop minus the taskbar). A free function so `#[serde(default =
/// ...)]` can name it. Half the work area is the conventional drop-down terminal
/// size (Guake / Windows Terminal quake window).
fn default_quake_height_fraction() -> f32 {
    0.5
}

/// The default quake-mode hotkey combo. Parsed by the app's quake module; the
/// grammar is `Mod+Mod+Key` (see `QuakeConfig::hotkey`). Inert while
/// [`QuakeConfig::enabled`] is false (the default), so this string never
/// registers an OS-level hotkey unless the user opts in.
fn default_quake_hotkey() -> String {
    "Ctrl+Shift+Grave".to_string()
}

/// Quake-mode ("drop-down terminal") configuration: a GLOBAL hotkey that slides
/// the window in from the top of the monitor under the cursor, takes focus, and
/// hides it again on the next press.
///
/// **Every field is inert until [`enabled`](Self::enabled) is turned on, and
/// `enabled` defaults to `false`.** A global hotkey is an OS-level privilege — it
/// is claimed process-wide and denies the combo to every other application — so
/// it is strictly opt-in, registered only when this flag is set, and
/// unregistered when the window is destroyed. Additive: an older config with no
/// `[quake]` table loads with quake mode fully off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuakeConfig {
    /// Master ON/OFF for quake mode. **Default `false`.** While off, NO global
    /// hotkey is registered at all — the combo stays available to other apps.
    pub enabled: bool,
    /// The global hotkey combo, as `Mod+Mod+Key`.
    ///
    /// Modifiers (case-insensitive, any order, at least ONE required):
    /// `Ctrl`/`Control`, `Alt`, `Shift`, `Win`/`Super`/`Meta`. Keys: `A`–`Z`,
    /// `0`–`9`, `F1`–`F24`, `Grave`/`` ` ``, `Space`, `Tab`, `Esc`, `Enter`.
    ///
    /// A combo with no modifier is REJECTED (it would swallow a bare keystroke
    /// system-wide), as is an unparseable one — in both cases no hotkey is
    /// registered and quake mode stays inert rather than claiming something
    /// unintended.
    pub hotkey: String,
    /// Drop-down height as a fraction of the monitor WORK area (the desktop minus
    /// the taskbar), so the window never overlaps the taskbar. Clamped to
    /// `0.1..=1.0` on use by `effective_height_fraction`, and a non-finite value
    /// falls back to the default, so a malformed config can never produce a
    /// zero-height or off-screen window.
    pub height_fraction: f32,
}

impl Default for QuakeConfig {
    /// Quake mode OFF (the opt-in default), the default combo parked but inert,
    /// and a half-work-area drop-down height.
    fn default() -> Self {
        QuakeConfig {
            enabled: false,
            hotkey: default_quake_hotkey(),
            height_fraction: default_quake_height_fraction(),
        }
    }
}

impl QuakeConfig {
    /// The lowest permitted drop-down height fraction — below this the window
    /// would be too short to show a usable grid.
    pub const MIN_HEIGHT_FRACTION: f32 = 0.1;
    /// The highest permitted drop-down height fraction (the full work area).
    pub const MAX_HEIGHT_FRACTION: f32 = 1.0;

    /// The height fraction to actually apply: clamped to
    /// [`MIN_HEIGHT_FRACTION`](Self::MIN_HEIGHT_FRACTION)`..=`[`MAX_HEIGHT_FRACTION`](Self::MAX_HEIGHT_FRACTION)
    /// and guarded against a non-finite (NaN/inf) value from a malformed config,
    /// so the drop-down can never be zero-height or taller than the work area.
    #[must_use]
    pub fn effective_height_fraction(&self) -> f32 {
        if self.height_fraction.is_finite() {
            self.height_fraction
                .clamp(Self::MIN_HEIGHT_FRACTION, Self::MAX_HEIGHT_FRACTION)
        } else {
            default_quake_height_fraction()
        }
    }

    /// The parsed [`hotkey`](Self::hotkey), or `None` when it names no combo we
    /// will register (see [`parse_hotkey`]). The Settings UI calls this to tell
    /// the user their typed combo is unusable BEFORE they restart into a quake
    /// mode that silently never arms.
    #[must_use]
    pub fn parsed_hotkey(&self) -> Option<HotkeySpec> {
        parse_hotkey(&self.hotkey)
    }
}

/// `MOD_ALT` — the `RegisterHotKey` modifier bit for Alt. Spelled out here (core
/// is platform-independent and has no `windows` dependency); the app's `quake`
/// module carries a windows-only drift-guard test asserting each of these equals
/// the real Win32 constant.
pub const MOD_ALT_BIT: u32 = 0x0001;
/// `MOD_CONTROL` — the modifier bit for Ctrl.
pub const MOD_CONTROL_BIT: u32 = 0x0002;
/// `MOD_SHIFT` — the modifier bit for Shift.
pub const MOD_SHIFT_BIT: u32 = 0x0004;
/// `MOD_WIN` — the modifier bit for the Windows key.
pub const MOD_WIN_BIT: u32 = 0x0008;
/// `MOD_NOREPEAT` — suppresses auto-repeat, so HOLDING the combo toggles ONCE
/// instead of machine-gunning at the keyboard repeat rate. Added at registration
/// time by [`HotkeySpec::register_modifiers`], never by the parser, so a parsed
/// spec compares cleanly against the user's combo.
pub const MOD_NOREPEAT_BIT: u32 = 0x4000;

/// `VK_OEM_3` — the grave/tilde key on a US layout.
pub const VK_GRAVE: u32 = 0xC0;
/// `VK_SPACE`.
pub const VK_SPACE_KEY: u32 = 0x20;
/// `VK_TAB`.
pub const VK_TAB_KEY: u32 = 0x09;
/// `VK_ESCAPE`.
pub const VK_ESCAPE_KEY: u32 = 0x1B;
/// `VK_RETURN`.
pub const VK_RETURN_KEY: u32 = 0x0D;
/// `VK_F1` — the F-key block is contiguous, so `F<n>` is `VK_F1 + (n - 1)`.
pub const VK_F1_KEY: u32 = 0x70;
/// The highest supported function key (`VK_F24` = `VK_F1 + 23`).
const MAX_FUNCTION_KEY: u32 = 24;

/// A parsed global-hotkey combo: the `RegisterHotKey` modifier bitmask and the
/// virtual-key code.
///
/// Lives in core (not the app's Win32 module) so the Settings UI — which is in
/// the lib target and cannot reach the shipping binary's `quake` module — can
/// validate a combo the user types with the SAME parser that registers it. One
/// implementation, no drift between "the UI says it is valid" and "the hotkey
/// actually registers".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HotkeySpec {
    /// The OR of the `MOD_*` bits the user asked for (never includes
    /// `MOD_NOREPEAT` — see [`register_modifiers`](Self::register_modifiers)).
    pub modifiers: u32,
    /// The virtual-key code of the non-modifier key.
    pub vk: u32,
}

impl HotkeySpec {
    /// The modifier bitmask to actually pass to `RegisterHotKey`: the user's
    /// modifiers plus `MOD_NOREPEAT`, so holding the combo down toggles the window
    /// exactly once instead of flickering at the key-repeat rate.
    #[must_use]
    pub const fn register_modifiers(&self) -> u32 {
        self.modifiers | MOD_NOREPEAT_BIT
    }
}

/// Map one non-modifier token to a virtual-key code, or `None` when it names no
/// key we support. Accepts `A`–`Z`, `0`–`9`, `` ` ``, `F1`–`F24`, and the named
/// keys `Grave`/`Tilde`/`Backtick`, `Space`, `Tab`, `Esc`/`Escape`,
/// `Enter`/`Return`. Case-insensitive.
#[must_use]
fn parse_key_token(token: &str) -> Option<u32> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    // Single ASCII letter / digit / backtick — the common case. The VK codes for
    // letters and digits ARE their uppercase-ASCII codes (asserted against the
    // `windows` crate by the app's drift-guard test).
    if t.len() == 1 {
        let c = t.as_bytes()[0];
        if c.is_ascii_alphabetic() {
            return Some(u32::from(c.to_ascii_uppercase()));
        }
        if c.is_ascii_digit() {
            return Some(u32::from(c));
        }
        if c == b'`' {
            return Some(VK_GRAVE);
        }
    }
    let lower = t.to_ascii_lowercase();
    // F1..F24 (the VK block is contiguous). A bare `f` is the LETTER F, handled
    // by the single-char branch above.
    if let Some(digits) = lower.strip_prefix('f') {
        if let Ok(n) = digits.parse::<u32>() {
            if (1..=MAX_FUNCTION_KEY).contains(&n) {
                return Some(VK_F1_KEY + (n - 1));
            }
            // An out-of-range F-key is a REFUSAL. Belt-and-braces: the name table
            // below holds no `f<digits>` entry today, so falling through would
            // also yield `None` — this guards the day one is added. The RANGE
            // BOUND above is what actually rejects `F0`/`F25` right now, and it
            // is the thing the wire-cut test pins.
            return None;
        }
    }
    match lower.as_str() {
        "grave" | "tilde" | "backtick" => Some(VK_GRAVE),
        "space" => Some(VK_SPACE_KEY),
        "tab" => Some(VK_TAB_KEY),
        "esc" | "escape" => Some(VK_ESCAPE_KEY),
        "enter" | "return" => Some(VK_RETURN_KEY),
        _ => None,
    }
}

/// Parse a `Mod+Mod+Key` combo string into a [`HotkeySpec`].
///
/// Returns `None` — meaning **register nothing, stay inert** — for every input we
/// cannot honour exactly:
///
/// * no modifier (`"F12"`): a bare key would be swallowed system-wide, stealing it
///   from every other application. Refused by design.
/// * no key (`"Ctrl+Shift"`), an unknown key name, an empty token (`"Ctrl++"`),
///   or two non-modifier keys (`"Ctrl+A+B"`).
///
/// Modifier names are case-insensitive and order-independent:
/// `Ctrl`/`Control`, `Alt`, `Shift`, `Win`/`Super`/`Meta`/`Cmd`.
#[must_use]
pub fn parse_hotkey(spec: &str) -> Option<HotkeySpec> {
    let mut modifiers = 0u32;
    let mut key: Option<u32> = None;
    for token in spec.split('+') {
        let t = token.trim();
        // Belt-and-braces, like the F-key guard below: an empty token would also
        // be rejected downstream by `parse_key_token`, so removing this line does
        // not currently change any answer. It stays because rejecting `"Ctrl+"` /
        // `"Ctrl++Q"` HERE states the grammar rule at the point it is violated,
        // instead of depending on a sibling function's internal check.
        if t.is_empty() {
            return None;
        }
        match t.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL_BIT,
            "alt" => modifiers |= MOD_ALT_BIT,
            "shift" => modifiers |= MOD_SHIFT_BIT,
            "win" | "super" | "meta" | "cmd" => modifiers |= MOD_WIN_BIT,
            _ => {
                if key.is_some() {
                    // Two non-modifier keys — ambiguous, refuse rather than guess.
                    return None;
                }
                key = Some(parse_key_token(t)?);
            }
        }
    }
    let vk = key?;
    if modifiers == 0 {
        // A modifier-less global hotkey would steal a bare keystroke from the
        // whole desktop. Never register one.
        return None;
    }
    Some(HotkeySpec { modifiers, vk })
}

/// W1TN3SS manual "Report an issue" coordinates: the GitHub `owner/repo` the
/// prefilled Issue-Form deep link targets, and the support email alias the
/// `mailto:` fallback addresses. Both have sane C0PL4ND defaults and are
/// overridable in config. NO persistent identifier and NO transport state is
/// ever stored here — only the public repo coordinates the deep link needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IssueIntakeConfig {
    /// The GitHub `owner/repo` the prefilled Issue-Form deep link targets.
    pub repo: String,
    /// The support email alias the `mailto:` fallback addresses.
    ///
    /// **Empty by default, and that default is load-bearing.** This value is
    /// compiled into every shipped binary of a PUBLIC repository, so a real
    /// mailbox here is published to everyone who downloads the app (and to every
    /// address harvester that reads the source). It previously defaulted to a
    /// maintainer's personal address.
    ///
    /// Empty means the app offers the GitHub Issue form + the clipboard
    /// fallback and simply does not offer the email route — see
    /// `email_fallback_offered`. An operator who has a real support alias sets
    /// it in `config.toml`:
    ///
    /// ```toml
    /// [reporting.issue_intake]
    /// mailto_alias = "support@example.org"
    /// ```
    pub mailto_alias: String,
}

impl Default for IssueIntakeConfig {
    fn default() -> Self {
        IssueIntakeConfig {
            repo: "46b-ETYKiAL/Itasha.Corp_C0PL4ND".to_string(),
            // No address ships. See the field doc.
            mailto_alias: String::new(),
        }
    }
}

/// Opt-in reporting configuration. Wraps the W1TN3SS SDK's two-stream
/// [`itasha_report_core::config::ReportingConfig`] (crash + manual-issue
/// consent posture, **both default OFF**) and adds the manual-issue repo
/// coordinates. This block is **additive** to the on-disk config (serde
/// `default`) — an older config file with no `[reporting]` table loads with
/// both streams OFF, so upgrading never silently enables any reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportingConfig {
    /// Per-stream consent posture (crash_reports + manual_issues). **Default
    /// OFF** for every stream — the privacy-default; nothing transmits until
    /// the user explicitly opts in.
    pub streams: itasha_report_core::config::ReportingConfig,
    /// Manual "Report an issue" deep-link coordinates.
    pub issue_intake: IssueIntakeConfig,
}

impl Default for ReportingConfig {
    /// Both streams OFF (the SDK's privacy-default), default issue-intake coords.
    fn default() -> Self {
        ReportingConfig {
            streams: itasha_report_core::config::ReportingConfig::all_off(),
            issue_intake: IssueIntakeConfig::default(),
        }
    }
}

/// The default window tint color (`#RRGGBB`) — brand-canon VOID BLACK, a
/// near-black, marginally violet-tinted void wash matching the brand background.
/// A free function so `#[serde(default = ...)]` can name it for the `tint` field.
///
/// Superseded the earlier `#121212` default; a stored config still carrying the
/// old `#121212` default is remapped to this value once by [`Config::migrate`]
/// (v1 → v2), while any user-customised tint is left untouched.
fn default_tint() -> String {
    "#08060d".to_string()
}

/// The pre-v2 default window tint (`#121212`). A stored config whose tint is
/// PROVABLY this old default is remapped to [`default_tint`] by the v1 → v2
/// migration; any other value (a user's custom tint) is preserved verbatim.
const LEGACY_DEFAULT_TINT: &str = "#121212";

/// Current config schema version. Bumped whenever a one-time, version-gated
/// migration is needed (see [`Config::migrate`]). A config written before schema
/// versioning existed deserializes with `schema_version == 0` (the serde default
/// for a missing field) and is migrated up on load.
///
/// - v2 (brand-canon default tint remap): a stored config whose `tint` is
///   PROVABLY the old `#121212` default is remapped to the new `#08060d`
///   (VOID BLACK) brand-canon default exactly once. A user's custom tint (any
///   value ≠ `#121212`) is left UNTOUCHED — no silent clobber. Gated on
///   `schema_version < 2`. (The earlier v1 legacy-transparency promotion was
///   retired when the multi-mode transparency model collapsed to the single
///   `opacity` model in v0.4.21; its former fields are now ignored on load.)
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Top-level configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// One-time-migration schema version. A fresh config is born at
    /// [`CURRENT_SCHEMA_VERSION`]; an existing config written before versioning
    /// loads as `0` (serde default for the missing field) and [`Config::migrate`]
    /// brings it forward exactly once. NEVER hand-edit downward.
    #[serde(default)]
    pub schema_version: u32,
    /// Name of the theme to load (matches a file stem in the themes dir).
    pub theme: String,
    /// Automatically follow the OS dark/light appearance: when ON, the app tracks
    /// the operating system's appearance setting and switches between a dark
    /// (`itasha-corp`) and light (`ghost-paper`) theme to match, overriding the
    /// [`theme`](Self::theme) selection while it is on. OFF by default (the
    /// flagship dark brand theme is the zero-config look). Mirrors SCR1B3's
    /// `follow_os_theme`.
    #[serde(default)]
    pub follow_os_theme: bool,
    /// Font configuration (family, size, line height, fallback chain).
    pub font: FontConfig,
    /// Persisted UI scale / accessibility zoom (F2-3): a multiplier applied to
    /// the WHOLE interface (chrome + grid), distinct from the transient Ctrl+/-
    /// keyboard zoom which is not persisted. `1.0` = 100%. Clamped to
    /// `0.5..=3.0` on use via [`Config::effective_ui_scale`] so a malformed value
    /// can never make the UI unusably tiny or huge. Defaults to `1.0`.
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,
    /// Number of scrollback lines retained per pane.
    pub scrollback_lines: usize,
    /// Window opacity 0.0..=1.0 — the SINGLE see-through control. The window is
    /// always created transparent-capable (`with_transparent`); this slider folds
    /// into the panel / resting-chrome fill alpha every frame. `0.0` = fully
    /// see-through (only the glyph text remains over the desktop); `1.0` = the
    /// panels paint fully opaque, so the window reads as a solid ("opaque")
    /// surface. Live-applies — no relaunch.
    pub opacity: f32,
    /// Tint color (`#RRGGBB`) painted over the window at
    /// [`Config::tint_strength`] when the tint wash is enabled. Mirrors
    /// SCR1B3's `window.tint`.
    #[serde(default = "default_tint")]
    pub tint: String,
    /// Tint overlay strength (0.0 = none .. 1.0 = strong). Mirrors SCR1B3's
    /// `window.tint_strength`. Default 0.0 (no tint).
    #[serde(default)]
    pub tint_strength: f32,
    /// Explicit ON/OFF master for the tint colour wash, independent of
    /// [`Config::tint_strength`]. When `false`, NO tint is painted regardless of
    /// the strength slider (so a user can park a preferred colour + strength and
    /// toggle the wash off/on without losing them). When `true`, the strength
    /// slider drives the wash as before (strength `0.0` is still no visible tint).
    ///
    /// Default `true` so existing configs (which relied on `tint_strength > 0` to
    /// enable the wash) keep their exact behaviour — the strength slider stays the
    /// effective control until the user deliberately flips this off.
    #[serde(default = "default_true")]
    pub tint_enabled: bool,
    /// Master ON/OFF for the software "frosted glass" wash — a deliberate,
    /// adjustable diffuse tint over the window, INDEPENDENT of the opacity slider
    /// (it works at any opacity). This is NOT a real desktop blur (impossible on
    /// this hardware); it is a self-rendered frosted look. Default OFF.
    #[serde(default)]
    pub frost_enabled: bool,
    /// Frosted-glass amount (0.0 = none .. 1.0 = maximum, capped short of fully
    /// opaque so glyph text stays legible). Independent of window opacity. Default
    /// [`default_frost_amount`] (a tasteful mid-low wash) so flipping the toggle
    /// shows the effect at once instead of a no-op at 0.
    #[serde(default = "default_frost_amount")]
    pub frost_amount: f32,
    /// Frost wash colour (`#RRGGBB`). Empty (the default) = follow the active
    /// theme's background, so the frost reads as a diffuse pane of the terminal's
    /// own colour; set a hex to tint the frost a specific colour.
    #[serde(default)]
    pub frost_color: String,
    /// Add a subtle procedural GRAIN texture to the frost for a real frosted
    /// (diffused) look rather than a flat wash. Default ON; the texture is a small
    /// tiling value-noise quad, so it is cheap. Only applied when
    /// [`Config::frost_enabled`] is on.
    #[serde(default = "default_true")]
    pub frost_grain: bool,
    /// Keep the C0PL4ND window on top of all other windows (mirrors SCR1B3's
    /// `window.always_on_top`). Applied at startup via the viewport builder's
    /// window level and live-toggled at runtime through
    /// `ViewportCommand::WindowLevel`, so flipping it takes effect without a
    /// relaunch. Default OFF — opt-in. `#[serde(default)]` keeps older configs
    /// (written before this field) loading cleanly with the flag off.
    #[serde(default)]
    pub always_on_top: bool,
    /// Quake mode ("drop-down terminal"): a GLOBAL hotkey that drops the window
    /// in from the top of the monitor under the cursor and hides it again.
    /// **Default OFF** — see [`QuakeConfig`]; a global hotkey is an OS-level
    /// privilege and is registered only while `quake.enabled` is true. Additive:
    /// an older config with no `[quake]` table loads with quake mode off.
    #[serde(default)]
    pub quake: QuakeConfig,
    pub cursor: CursorConfig,
    pub window: WindowConfig,
    pub effects: EffectsConfig,
    pub keybindings: Keybindings,
    pub update: UpdateConfig,
    /// Which side the command-history quick-run sidebar (`#21`) docks to when
    /// opened. Default [`PanelSide::Right`].
    #[serde(default)]
    pub history_sidebar_side: PanelSide,
    /// Pane shell layout: the multi-pane `egui_tiles` grid (default) or a single
    /// full-size pane with the tab strip switching panes. Toggled by the titlebar
    /// view button; persisted so the choice survives a relaunch.
    #[serde(default)]
    pub view_mode: ViewMode,
    /// Show the neofetch-style startup panel (logo + system info) on launch.
    pub startup_panel: bool,
    /// Show the bottom status bar (pane count + hints). `true` (default) shows it;
    /// set `false` to hide it and reclaim the row for the terminal grid.
    #[serde(default = "default_true")]
    pub show_status_bar: bool,
    /// Customizable top-bar quick-action toolbar (right cluster, by the gear). See
    /// [`ToolbarConfig`]; edited in Settings → Toolbar.
    #[serde(default)]
    pub toolbar: ToolbarConfig,
    /// GPU backend the renderer requests at startup. [`GraphicsBackend::Auto`]
    /// (default) keeps the platform-smart choice (DX12 on Windows, Vulkan when
    /// window transparency is on; wgpu default elsewhere). Set an explicit
    /// backend to work around a driver-specific rendering glitch (e.g. corrupted
    /// glyphs under a bad DX12 driver → switch to Vulkan). Applied on restart;
    /// the `WGPU_BACKEND` env var still overrides this.
    #[serde(default)]
    pub graphics_backend: GraphicsBackend,
    /// Which physical GPU to prefer on a multi-GPU (hybrid / Optimus) machine.
    /// [`GpuPreference::Auto`] (default) uses the platform default; set
    /// `Integrated` to force the low-power iGPU when the discrete GPU's driver
    /// corrupts rendering (the terminal-grid glyph garble). Applied on restart;
    /// the `WGPU_POWER_PREF` env var still overrides this.
    #[serde(default)]
    pub graphics_gpu: GpuPreference,
    /// Override shell program; `None` = use the platform default shell.
    pub shell: Option<String>,
    /// The `TERM` value advertised to the spawned child shell (and every TUI it
    /// runs). Defaults to [`DEFAULT_TERM`] (`xterm-256color`), which matches what
    /// the emulator advertises on the wire (its DA / XTGETTCAP responses). Set
    /// this only if you need the child to see a different terminfo entry; an
    /// empty string falls back to the default. `COLORTERM` is always `truecolor`
    /// (the emulator renders 24-bit colour) and is not configurable.
    #[serde(default = "default_term")]
    pub term: String,
    /// Enable font ligatures / complex text shaping in the renderer.
    ///
    /// Core-side preference flag only — the actual shaping is the renderer's
    /// concern (cosmic-text `Shaping::Advanced`). When `false`, the renderer
    /// should shape per-cell (`Shaping::Basic`) for monospace fidelity; when
    /// `true`, it may run advanced shaping so programming ligatures (e.g. `->`,
    /// `!=`) and complex scripts render. Defaults to `false` so monospace grid
    /// alignment is preserved unless the user opts in.
    pub ligatures: bool,
    /// Automatically copy a mouse text selection to the clipboard the moment
    /// the drag ends (X11-style "copy on select"). Write-only. Defaults to
    /// `false` — copy stays an explicit Ctrl/Cmd+Shift+C unless opted in.
    pub copy_on_select: bool,
    /// Warn before pasting clipboard text that contains a newline (a multi-line
    /// paste can run shell commands the instant it lands). When `true` (default)
    /// such a paste shows a confirm overlay first. A security feature — set
    /// `false` to paste multi-line content without confirmation.
    pub paste_warn_multiline: bool,
    /// Warn before pasting more than this many BYTES of clipboard text, even
    /// when it is a single line.
    ///
    /// The multi-line gate above catches the classic pastejacking payload (a
    /// command that runs on its embedded newline), but a SINGLE-line paste of
    /// tens of kilobytes is the other half of the same footgun: a hidden
    /// terminal-width-spanning command, a giant base64 blob piped to `sh`, or an
    /// accidental whole-file paste that floods the PTY. This is the size half of
    /// the gate. `0` disables it (size is then never a reason to confirm);
    /// the multi-line gate is independent and unaffected.
    #[serde(default = "default_paste_warn_bytes")]
    pub paste_warn_bytes: usize,
    /// Allow a program running inside the terminal to READ the system clipboard
    /// via an `OSC 52 ; c ; ?` query. **Default `false` (deny)** — this is the
    /// security-relevant half of OSC 52.
    ///
    /// Clipboard *writes* (a program setting the clipboard) are always accepted:
    /// the worst case is a clobbered clipboard. A *read* is an exfiltration
    /// primitive — anything that can write to the tty (a compromised tool, a
    /// hostile file dumped with `cat`, output relayed over ssh/tmux) could siphon
    /// whatever the user last copied, which is routinely a password or an API
    /// token. So the read direction is opt-in, never on by default.
    ///
    /// Denying does NOT mean staying silent: a query is answered with an
    /// empty-payload OSC 52 reply, so the asking program resumes immediately
    /// instead of blocking on a response that never comes, and zero clipboard
    /// bytes are disclosed. `#[serde(default)]` keeps older config files (written
    /// before this field existed) loading with reads denied.
    #[serde(default)]
    pub clipboard_read_allow: bool,
    /// Keep split-pane dividers LINKED so every sibling pane stays the same size.
    /// When `true`, the dividers are held at equal positions each frame — drag one
    /// and they hold equal ("move together"). Defaults to `false` so panes are
    /// freely resizable; the top-bar "make symmetrical" button equalises once
    /// regardless of this setting.
    #[serde(default)]
    pub link_pane_dividers: bool,
    /// Capture typed commands into the (in-memory) command history that feeds the
    /// Ctrl+Shift+P palette + the history sidebar. `true` (default) records
    /// echoed commands (passwords + inline secrets are excluded/redacted
    /// upstream). Set `false` for a no-history privacy posture; the
    /// per-session Incognito toggle forces this off regardless.
    #[serde(default = "default_true")]
    pub history_capture_enabled: bool,
    /// Opt-in crash/error/issue reporting (W1TN3SS). **Both streams default
    /// OFF** — nothing is captured-for-send or transmitted until the user
    /// explicitly opts in from Settings → Privacy. Additive: an older config
    /// with no `[reporting]` table loads with reporting fully off.
    #[serde(default)]
    pub reporting: ReportingConfig,
    /// Persisted size of the Settings sub-window, in logical points, so a user's
    /// resize sticks across launches. `None` (default, and for older configs
    /// without the field) = use the built-in default size. Clamped to a sane
    /// floor/ceiling on use so a malformed value can never spawn an unusable
    /// window.
    #[serde(default)]
    pub settings_win_w: Option<f32>,
    #[serde(default)]
    pub settings_win_h: Option<f32>,
    /// Persisted top-left POSITION of the Settings sub-window, in logical points,
    /// so a user's move sticks across launches. `None` (default) = no saved
    /// position yet, so the window opens centered over the app. Clamped to the
    /// live screen on use so a stale off-screen value can never hide the window.
    #[serde(default)]
    pub settings_win_x: Option<f32>,
    #[serde(default)]
    pub settings_win_y: Option<f32>,
}

/// serde default for boolean fields that should default to `true` when absent
/// from an older on-disk config (so upgrading never silently disables a feature).
fn default_true() -> bool {
    true
}

/// serde default for [`Config::paste_warn_bytes`] — 4 KiB.
///
/// Chosen so an ordinary single-line paste (a path, a URL, a one-line command,
/// even a long `curl` invocation) never prompts, while an accidental
/// whole-file / giant-blob paste does. Comfortably above a terminal-width line
/// yet far below "a program's worth of text".
fn default_paste_warn_bytes() -> usize {
    4096
}

/// serde default for [`Config::frost_amount`] — a tasteful mid-low wash, so
/// enabling the frosted-glass toggle shows the effect immediately (not a no-op
/// at 0). The user tunes it up/down from here.
fn default_frost_amount() -> f32 {
    0.25
}

/// serde default for [`Config::term`] — the canonical `TERM` the emulator
/// advertises. Delegates to [`crate::pty::DEFAULT_TERM`] so the config default
/// and the PTY spawn default share one source of truth.
fn default_term() -> String {
    crate::pty::DEFAULT_TERM.to_string()
}

/// serde default for [`Config::ui_scale`] — 100% (no scaling).
fn default_ui_scale() -> f32 {
    1.0
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // A FRESH config is born already at the current schema version, so
            // `migrate` is a no-op for new users (no spurious first-run rewrite).
            // Only an EXISTING file (which deserializes `schema_version` to 0) is
            // migrated forward on load.
            schema_version: CURRENT_SCHEMA_VERSION,
            theme: "itasha-corp".to_string(),
            follow_os_theme: false,
            font: FontConfig::default(),
            ui_scale: default_ui_scale(),
            scrollback_lines: 10_000,
            opacity: 1.0,
            tint: default_tint(),
            tint_strength: 0.0,
            tint_enabled: true,
            frost_enabled: false,
            frost_amount: default_frost_amount(),
            frost_color: String::new(),
            frost_grain: true,
            always_on_top: false,
            quake: QuakeConfig::default(),
            cursor: CursorConfig::default(),
            window: WindowConfig::default(),
            effects: EffectsConfig::default(),
            keybindings: Keybindings::default(),
            update: UpdateConfig::default(),
            history_sidebar_side: PanelSide::Right,
            view_mode: ViewMode::default(),
            startup_panel: true,
            link_pane_dividers: false,
            show_status_bar: true,
            toolbar: ToolbarConfig::default(),
            graphics_backend: GraphicsBackend::default(),
            graphics_gpu: GpuPreference::default(),
            shell: None,
            term: default_term(),
            ligatures: false,
            copy_on_select: false,
            paste_warn_multiline: true,
            paste_warn_bytes: default_paste_warn_bytes(),
            // DEFAULT-DENY. An on-by-default OSC 52 clipboard read is an
            // exfiltration hole; the user opts in explicitly (Settings →
            // Terminal → Clipboard).
            clipboard_read_allow: false,
            history_capture_enabled: true,
            reporting: ReportingConfig::default(),
            settings_win_w: None,
            settings_win_h: None,
            settings_win_x: None,
            settings_win_y: None,
        }
    }
}

impl Config {
    /// The UI scale to actually apply (F2-3), clamped to a safe `0.5..=3.0` and
    /// guarded against a non-finite (NaN/inf) value from a malformed config — so
    /// a bad `ui_scale` can never render the interface unusably tiny, huge, or
    /// blank. `1.0` = 100%.
    pub fn effective_ui_scale(&self) -> f32 {
        if self.ui_scale.is_finite() {
            self.ui_scale.clamp(0.5, 3.0)
        } else {
            default_ui_scale()
        }
    }

    /// Parse a TOML string into a `Config`, surfacing a readable error.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    /// use c0pl4nd_core::config::Config;
    ///
    /// // An empty document yields the default config (every field optional).
    /// let cfg = Config::from_toml("", Path::new("config.toml")).unwrap();
    /// assert!(cfg.effective_ui_scale() > 0.0);
    ///
    /// // Malformed TOML is surfaced as an error, never a silent default.
    /// assert!(Config::from_toml("not valid =", Path::new("config.toml")).is_err());
    /// ```
    pub fn from_toml(src: &str, path: &Path) -> Result<Config, ConfigError> {
        let mut cfg: Config = toml::from_str(src).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        cfg.migrate();
        cfg.clamp_values();
        cfg.validate()?;
        Ok(cfg)
    }

    /// Clamp out-of-range / non-finite numeric values to their documented ranges
    /// at LOAD (audit LO-3). A hand-edited or corrupt config could feed a garbage
    /// font size, a non-finite / zero line-height, an absurd update interval, or
    /// a multi-GB scrollback request straight into the renderer / scheduler,
    /// because `validate()` only checks keybindings and several fields were only
    /// clamped inconsistently at use-site. This CLAMPS (never rejects) — a bad
    /// value degrades to a sane one rather than failing the whole load, matching
    /// the graceful-degradation contract.
    fn clamp_values(&mut self) {
        // Font: size + line-height must be finite and positive; clamp to a band
        // that stays renderable. A non-finite or non-positive value resets to the
        // default before the band clamp (so `0`/`NaN`/`-1` never reach the layout).
        if !self.font.size.is_finite() || self.font.size <= 0.0 {
            self.font.size = FontConfig::default().size;
        }
        self.font.size = self.font.size.clamp(4.0, 256.0);
        if !self.font.line_height.is_finite() || self.font.line_height <= 0.0 {
            self.font.line_height = FontConfig::default().line_height;
        }
        self.font.line_height = self.font.line_height.clamp(1.0, 512.0);
        // `ui_scale`'s in-RANGE clamp intentionally stays at use-site
        // (`effective_ui_scale`); fix only finiteness here so a NaN/inf cannot
        // propagate. (`opacity` / `tint_strength` are deliberately left alone —
        // `validate()` already REJECTS an out-of-range value there, a stricter
        // contract this clamp must not soften; only the fields validate() omits
        // are clamped here.)
        if !self.ui_scale.is_finite() {
            self.ui_scale = default_ui_scale();
        }
        // Update check interval: documented 1..=168 hours.
        self.update.check_interval_hours = self.update.check_interval_hours.clamp(1, 168);
        // Scrollback: cap to a generous ceiling so a typo cannot request a
        // multi-GB history allocation.
        self.scrollback_lines = self.scrollback_lines.min(10_000_000);
    }

    /// Apply one-time, version-gated migrations in place. Returns `true` when
    /// anything changed (the caller should then persist).
    ///
    /// **Why this exists.** Every field is `#[serde(default)]`, so a value STORED
    /// in the user's file always wins over the source default. A good default
    /// flipped on in a later release can therefore never reach a user whose config
    /// predates the flip. Each step re-applies its intended baseline ONCE, then
    /// bumps `schema_version`, so the user's own later deliberate changes are never
    /// overridden again (the step won't re-run).
    ///
    /// The legacy multi-mode transparency model (`transparency_enabled` +
    /// `window_mode` + `acrylic`) was collapsed to the single always-transparent
    /// `opacity` model (v0.4.21). Those keys are simply IGNORED on load (serde
    /// drops unknown fields), so an on-disk config that still carries them loads
    /// without error; the retained `opacity` value carries the see-through level
    /// directly. Old opaque configs kept `opacity == 1.0` (the slider was gated
    /// off unless transparency was enabled), so they load as a solid window.
    ///
    /// The remaining step remaps the tint ONLY when it is provably the old
    /// `#121212` default to the new `#08060d` (VOID BLACK) brand-canon default; a
    /// user's custom tint is preserved verbatim (no silent clobber).
    pub fn migrate(&mut self) -> bool {
        let original = self.schema_version;
        let mut changed = false;

        // Bring any config below the current schema forward to v2. The only
        // surviving one-time transform is the brand-canon tint remap: a config
        // whose tint is PROVABLY the old `#121212` default is remapped to
        // `#08060d`; ANY user-customised tint (any value ≠ `#121212`) is left
        // untouched — no silent clobber. One-shot: after this, `schema_version == 2`
        // and the block is skipped, so a config that later sets `#121212`
        // deliberately is never re-remapped.
        if self.schema_version < 2 {
            if self.tint == LEGACY_DEFAULT_TINT {
                self.tint = default_tint();
            }
            self.schema_version = 2;
            changed = true;
        }

        // Migration invariants (debug-only): it must never LOWER the version, and
        // any config that started below the current schema must end exactly at it.
        // A FORWARD-version config (`original > CURRENT`, e.g. a file written by a
        // newer build then opened by an older one) is left untouched and
        // legitimately stays ahead — so we must NOT assert an upper bound.
        debug_assert!(self.schema_version >= original);
        debug_assert!(
            original >= CURRENT_SCHEMA_VERSION || self.schema_version == CURRENT_SCHEMA_VERSION
        );
        changed
    }

    /// Validate value ranges. Never panics; returns a descriptive error.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.theme.trim().is_empty() {
            return Err(ConfigError::Invalid("theme name must not be empty".into()));
        }
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(ConfigError::Invalid(format!(
                "opacity must be between 0.0 and 1.0, got {}",
                self.opacity
            )));
        }
        if self.font.size <= 0.0 {
            return Err(ConfigError::Invalid("font.size must be positive".into()));
        }
        if self.window.cols == 0 || self.window.rows == 0 {
            return Err(ConfigError::Invalid(
                "window cols and rows must be non-zero".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.tint_strength) {
            return Err(ConfigError::Invalid(format!(
                "tint_strength must be between 0.0 and 1.0, got {}",
                self.tint_strength
            )));
        }
        if crate::theme::parse_hex(&self.tint).is_err() {
            return Err(ConfigError::Invalid(format!(
                "tint must be a #RRGGBB hex color, got {:?}",
                self.tint
            )));
        }
        Ok(())
    }

    /// The platform default per-user config file path.
    pub fn default_path() -> Option<PathBuf> {
        // ~/.config/c0pl4nd/config.toml on Unix; %APPDATA%\c0pl4nd\config.toml on Windows.
        #[cfg(windows)]
        {
            std::env::var_os("APPDATA")
                .map(|p| PathBuf::from(p).join("c0pl4nd").join("config.toml"))
        }
        #[cfg(not(windows))]
        {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .map(|p| p.join("c0pl4nd").join("config.toml"))
        }
    }

    /// The per-user C0PL4ND data directory (the PARENT of `config.toml`) — the
    /// root the W1TN3SS report spool (`reports/`) is created under. Returns
    /// `None` when no config path resolves (no `%APPDATA%` / `$HOME`). Shares
    /// the resolution with [`Config::default_path`] so the spool, the config
    /// file, and the crash logs all live under one per-user dir.
    pub fn config_dir() -> Option<PathBuf> {
        Self::default_path().and_then(|p| p.parent().map(Path::to_path_buf))
    }

    /// Load from a specific path. Missing file → built-in defaults (zero-config).
    pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(src) => Config::from_toml(&src, path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source: e,
            }),
        }
    }

    /// Serialize to a pretty TOML string. The inverse of [`Config::from_toml`].
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Invalid(e.to_string()))
    }

    /// Persist to a specific path, creating parent directories as needed.
    /// Used by the settings panel and the window-geometry persistence so the
    /// config file stays the single source of truth.
    ///
    /// The file is created **owner-only** from the start (roadmap P-V2): `0600`
    /// on Unix, an owner-only DACL on Windows. The config may reflect the user's
    /// environment, so other local accounts should not be able to read it.
    ///
    /// The write goes through [`atomic_write_owner_only`], which writes the body
    /// to a sibling temp file, tightens it (on Unix `0600` is applied to the temp
    /// file **before** the rename), then atomically renames it over the
    /// destination. This closes the previous race where `std::fs::write` then
    /// `restrict_to_owner` left a brief window in which the file carried default
    /// (umask/inherited) permissions (audit P3-#2). Permission tightening itself
    /// remains BEST-EFFORT — a restrictive filesystem can never block a save —
    /// but is no longer applied after the content already exists world-readable.
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        let body = self.to_toml()?;
        // `atomic_write_owner_only` creates parent dirs, writes to a sibling
        // temp file, tightens perms (Unix: on the temp file pre-rename; Windows:
        // on the final path post-rename), and renames atomically — so the
        // destination never exists in a world-readable, default-perms state.
        crate::atomic_write::atomic_write_owner_only(path, body.as_bytes()).map_err(|e| {
            ConfigError::Io {
                path: path.to_path_buf(),
                source: e,
            }
        })?;
        Ok(())
    }

    /// Update only the persisted window-geometry fields on the file at
    /// [`Config::default_path`], preserving every other field the user set.
    /// Best-effort: a load/parse failure falls back to the in-memory config so
    /// a corrupt file never blocks geometry capture. Returns the path written.
    pub fn persist_geometry(window: WindowConfig) -> Option<PathBuf> {
        let path = Config::default_path()?;
        let mut cfg = Config::load_from(&path).unwrap_or_default();
        // Copy only geometry; leave cols/rows/padding (size-on-first-launch)
        // untouched so an explicit user value is never clobbered.
        cfg.window.pos_x = window.pos_x;
        cfg.window.pos_y = window.pos_y;
        cfg.window.size_w = window.size_w;
        cfg.window.size_h = window.size_h;
        cfg.window.maximized = window.maximized;
        cfg.window.monitor = window.monitor;
        // Surface a save failure (audit LO-4): previously `.ok()?` swallowed it
        // silently, unlike the loader's `tracing::warn!` convention, so a
        // persistently-unwritable config dir lost window geometry with no trace.
        if let Err(e) = cfg.save_to(&path) {
            tracing::warn!("failed to persist window geometry to {path:?}: {e}");
            return None;
        }
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_values_bounds_out_of_range_fields() {
        // audit LO-3: out-of-range / non-finite numeric fields clamp at load to
        // their documented ranges (never reach the renderer / scheduler raw).
        let mut cfg = Config::default();
        cfg.font.size = 0.0; // non-positive → reset + band-clamp
        cfg.font.line_height = -10.0; // negative → reset
        cfg.ui_scale = f32::INFINITY; // non-finite → reset
        cfg.update.check_interval_hours = 9999; // > 168
        cfg.scrollback_lines = 999_999_999_999; // absurd
        cfg.clamp_values();
        assert!(
            (4.0..=256.0).contains(&cfg.font.size),
            "font.size clamped: {}",
            cfg.font.size
        );
        assert!(cfg.font.line_height >= 1.0, "line_height positive");
        assert!(cfg.ui_scale.is_finite(), "ui_scale finite");
        assert!(
            (1..=168).contains(&cfg.update.check_interval_hours),
            "interval clamped"
        );
        assert!(cfg.scrollback_lines <= 10_000_000, "scrollback capped");
        // opacity / tint_strength are intentionally NOT clamped here — validate()
        // rejects an out-of-range value, a stricter contract this must not soften.
    }

    #[test]
    fn defaults_are_sane_and_valid() {
        let c = Config::default();
        assert_eq!(c.theme, "itasha-corp");
        assert_eq!(c.scrollback_lines, 10_000);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn effects_motion_defaults_and_backward_compat() {
        // New EffectsConfig defaults: the master animation switch is ON at full
        // intensity (reproducing the shipped feel) and every motion overlay is
        // OFF — the master toggle changes nothing until a user opts an effect in.
        let d = EffectsConfig::default();
        assert!(d.animations_enabled);
        assert_eq!(d.animation_intensity, 1.0);
        assert!(!d.flicker && !d.vhs_tracking && !d.wired_ambient);
        assert!(!d.cursor_trail && !d.boot_glitch);
        // The cursor-trail intensity defaults mid-high so a freshly-enabled trail
        // is visible at once (tunable either way by the Motion slider).
        assert_eq!(d.cursor_trail_intensity, 0.6);

        // An OLD config written before the Motion fields existed (only the
        // original four effect keys) MUST still deserialize: the struct-level
        // `#[serde(default)]` fills every missing field from Default, never a
        // parse error. This is the load-compat guarantee the reorg depends on.
        let old: EffectsConfig = toml::from_str(
            "crt_scanlines = true\nscanline_darkness = 0.7\n\
             chromatic_aberration_enabled = false\nchromatic_aberration = 0.0\n",
        )
        .expect("legacy effects config without motion fields must load");
        assert!(old.crt_scanlines);
        assert_eq!(old.scanline_darkness, 0.7);
        assert!(old.animations_enabled, "missing master switch defaults ON");
        assert_eq!(old.animation_intensity, 1.0);
        assert!(!old.boot_glitch, "missing motion effect defaults OFF");
        assert_eq!(
            old.cursor_trail_intensity, 0.6,
            "missing cursor-trail intensity defaults to the mid-high band"
        );
        assert_eq!(
            old.mesh_brightness, 1.0,
            "missing mesh brightness defaults to the shipped 1.0 (no visual change)"
        );

        // Clamped accessors keep a malformed / out-of-band value inside its
        // (widened) design range before it can reach a painter.
        let wild = EffectsConfig {
            animation_intensity: 9.0,
            flicker_strength: 5.0,
            mesh_density: -3.0,
            mesh_brightness: 9.0,
            cursor_trail_intensity: 9.0,
            mesh_speed: 9.0,
            vhs_intensity: 5.0,
            ..EffectsConfig::default()
        };
        assert_eq!(wild.clamped_animation_intensity(), 2.0);
        assert_eq!(wild.clamped_flicker_strength(), 1.0);
        assert_eq!(wild.clamped_mesh_density(), 0.0);
        assert_eq!(wild.clamped_mesh_brightness(), 3.0);
        assert_eq!(wild.clamped_cursor_trail_intensity(), 2.0);
        // Out-of-band high inputs clamp to each band's ceiling — proves the clamp
        // is load-bearing (kills the "remove clamp" mutant on the new accessors).
        assert_eq!(wild.clamped_mesh_speed(), 2.0);
        assert_eq!(wild.clamped_vhs_intensity(), 1.0);
    }

    #[test]
    fn motion_defaults_are_exact_and_in_band_values_pass_through() {
        // Pin the EXACT default of every Motion knob — the shipped feel is these
        // numbers, and a silent drift (e.g. a default bumped to 0/1) would change
        // the out-of-box look. Asserting the concrete value (not just "non-zero")
        // is what proves the default is the intended one.
        assert_eq!(default_animation_intensity(), 1.0);
        assert_eq!(default_flicker_strength(), 0.06);
        assert_eq!(default_mesh_density(), 0.4);
        assert_eq!(default_mesh_brightness(), 1.0);
        assert_eq!(default_mesh_speed(), 1.0);
        assert_eq!(default_vhs_intensity(), 0.5);
        assert_eq!(default_cursor_trail_intensity(), 0.6);

        // A value already INSIDE each band must pass through UNCHANGED — the
        // clamp only touches out-of-range inputs. This is the half the `wild`
        // (out-of-band) test can't see: it proves the accessor returns the real
        // input mid-band, not a constant, so the slider actually drives the
        // effect across its whole range.
        let mid = EffectsConfig {
            animation_intensity: 1.5,
            flicker_strength: 0.5,
            mesh_density: 1.2,
            mesh_brightness: 1.5,
            mesh_speed: 1.5,
            vhs_intensity: 0.7,
            cursor_trail_intensity: 1.0,
            ..EffectsConfig::default()
        };
        assert_eq!(mid.clamped_animation_intensity(), 1.5);
        assert_eq!(mid.clamped_flicker_strength(), 0.5);
        assert_eq!(mid.clamped_mesh_density(), 1.2);
        assert_eq!(mid.clamped_mesh_brightness(), 1.5);
        assert_eq!(mid.clamped_mesh_speed(), 1.5);
        assert_eq!(mid.clamped_vhs_intensity(), 0.7);
        assert_eq!(mid.clamped_cursor_trail_intensity(), 1.0);
    }

    #[test]
    fn per_effect_speeds_default_to_one_and_clamp_to_their_band() {
        // The per-effect drift-speed multipliers default to 1.0 so the resting
        // look is byte-for-byte unchanged (1.0 reproduces the current shipped
        // cadence for flicker, VHS drift, and scanline roll). Each clamps to the
        // 0.25..=3.0 band so a hand-edited TOML can't drive a seizure-fast — nor a
        // frozen-but-enabled — animation.
        let d = EffectsConfig::default();
        assert_eq!(d.flicker_speed, 1.0, "flicker_speed default is 1.0");
        assert_eq!(d.vhs_speed, 1.0, "vhs_speed default is 1.0");
        assert_eq!(d.scanline_speed, 1.0, "scanline_speed default is 1.0");
        assert_eq!(default_flicker_speed(), 1.0);
        assert_eq!(default_vhs_speed(), 1.0);
        assert_eq!(default_scanline_speed(), 1.0);
        assert_eq!(d.clamped_flicker_speed(), 1.0);
        assert_eq!(d.clamped_vhs_speed(), 1.0);
        assert_eq!(d.clamped_scanline_speed(), 1.0);

        // Above-band values clamp to the 3.0 ceiling.
        let hi = EffectsConfig {
            flicker_speed: 42.0,
            vhs_speed: 9.0,
            scanline_speed: 5.0,
            ..EffectsConfig::default()
        };
        assert_eq!(hi.clamped_flicker_speed(), 3.0, "flicker ceiling 3.0");
        assert_eq!(hi.clamped_vhs_speed(), 3.0, "vhs ceiling 3.0");
        assert_eq!(hi.clamped_scanline_speed(), 3.0, "scanline ceiling 3.0");

        // Below-band (incl. zero / negative) values clamp UP to the 0.25 floor so
        // a frozen-but-enabled animation is impossible.
        let lo = EffectsConfig {
            flicker_speed: 0.0,
            vhs_speed: -3.0,
            scanline_speed: 0.1,
            ..EffectsConfig::default()
        };
        assert_eq!(lo.clamped_flicker_speed(), 0.25, "flicker floor 0.25");
        assert_eq!(lo.clamped_vhs_speed(), 0.25, "vhs floor 0.25");
        assert_eq!(lo.clamped_scanline_speed(), 0.25, "scanline floor 0.25");

        // An in-band value passes through untouched (proves the accessor returns
        // the real input, not a constant — the slider drives its whole range).
        let mid = EffectsConfig {
            flicker_speed: 1.5,
            vhs_speed: 2.0,
            scanline_speed: 0.5,
            ..EffectsConfig::default()
        };
        assert_eq!(mid.clamped_flicker_speed(), 1.5);
        assert_eq!(mid.clamped_vhs_speed(), 2.0);
        assert_eq!(mid.clamped_scanline_speed(), 0.5);

        // Serde backfill: a config that predates these fields loads with the 1.0
        // default (no visual change on upgrade — the whole point of the inert
        // defaults, so NO schema_version bump / migration is required).
        let old: EffectsConfig =
            toml::from_str("crt_scanlines = true\nflicker = true\nvhs_tracking = true\n")
                .expect("legacy effects config without the speed fields must load");
        assert_eq!(
            old.flicker_speed, 1.0,
            "absent flicker_speed backfills to 1.0"
        );
        assert_eq!(old.vhs_speed, 1.0, "absent vhs_speed backfills to 1.0");
        assert_eq!(
            old.scanline_speed, 1.0,
            "absent scanline_speed backfills to 1.0"
        );
    }

    #[test]
    fn graphics_backend_defaults_to_auto_and_round_trips() {
        let p = std::path::Path::new("config.toml");
        // Default is Auto (the platform-smart choice), and a missing key parses
        // to Auto (serde default) so older configs keep working.
        assert_eq!(Config::default().graphics_backend, GraphicsBackend::Auto);
        assert_eq!(
            Config::from_toml("", p).unwrap().graphics_backend,
            GraphicsBackend::Auto,
            "absent key → Auto (backward compatible)"
        );
        // An explicit lowercase value parses, and the round-trip preserves it.
        let c = Config::from_toml("graphics_backend = \"vulkan\"\n", p).unwrap();
        assert_eq!(c.graphics_backend, GraphicsBackend::Vulkan);
        let toml = c.to_toml().unwrap();
        assert_eq!(
            Config::from_toml(&toml, p).unwrap().graphics_backend,
            GraphicsBackend::Vulkan,
            "graphics_backend survives a serialize→parse round-trip"
        );
    }

    /// `config_dir()` (the per-user data dir the W1TN3SS report spool lives under)
    /// MUST be exactly the parent of `default_path()` (the dir holding
    /// `config.toml`) — so the spool, config, and crash logs share one root. This
    /// pins the value: it is neither `None` when a config path resolves, nor an
    /// empty/default `PathBuf` — it is the real `…/c0pl4nd` directory.
    #[test]
    fn config_dir_is_parent_of_default_path() {
        match Config::default_path() {
            Some(p) => {
                let expected = p.parent().map(Path::to_path_buf);
                assert_eq!(
                    Config::config_dir(),
                    expected,
                    "config_dir must equal the parent of default_path"
                );
                let dir =
                    Config::config_dir().expect("config_dir resolves whenever default_path does");
                assert!(
                    !dir.as_os_str().is_empty(),
                    "config_dir must not be an empty path"
                );
                assert!(
                    dir.ends_with("c0pl4nd"),
                    "config_dir must be the per-user c0pl4nd data dir, got {dir:?}"
                );
            }
            // No config path resolves in this environment → config_dir is None too.
            None => assert_eq!(Config::config_dir(), None),
        }
    }

    /// Doc/behaviour parity guard: the default update mode is `notify`, which
    /// means a fresh install DOES make a once-per-launch network version check.
    /// This is a privacy-relevant default documented in PRIVACY.md / CHANGELOG.md
    /// — if this ever changes, those docs MUST change too. Failing this test is a
    /// signal that the documented default has silently drifted from the code.
    #[test]
    fn default_update_mode_is_notify_and_checks_on_launch() {
        assert_eq!(UpdateMode::default(), UpdateMode::Notify);
        let u = UpdateConfig::default();
        assert_eq!(u.mode, UpdateMode::Notify);
        assert!(
            u.checks_on_launch(),
            "the default `notify` mode performs an on-launch check — PRIVACY.md \
             must say so"
        );
        assert_eq!(u.check_interval_hours, 24);
    }

    #[test]
    fn font_default_is_the_sane_industry_size() {
        // Regression guard for the "grid text renders too large" report: the
        // default terminal font size is a calm, industry-standard 13 logical
        // points — NOT the previous 14.0 (which read large, especially on HiDPI).
        // A user-saved size is untouched; only the default is pinned here.
        assert_eq!(FontConfig::default().size, 13.0);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml("theme = \"ghost-paper\"\n", &p).unwrap();
        assert_eq!(c.theme, "ghost-paper");
        assert_eq!(c.font.size, 13.0); // default preserved
    }

    #[test]
    fn invalid_opacity_is_rejected() {
        let p = PathBuf::from("test.toml");
        let err = Config::from_toml("opacity = 5.0\n", &p);
        assert!(err.is_err());
    }

    #[test]
    fn malformed_toml_is_error_not_panic() {
        let p = PathBuf::from("test.toml");
        assert!(Config::from_toml("this is = = not toml", &p).is_err());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let c = Config::load_from(&PathBuf::from("/nonexistent/c0pl4nd/config.toml")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn ligatures_defaults_off() {
        let c = Config::default();
        assert!(!c.ligatures, "monospace fidelity is preserved by default");
    }

    #[test]
    fn ligatures_parses_from_partial_toml() {
        // A config file that only sets ligatures still works via serde(default).
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml("ligatures = true\n", &p).unwrap();
        assert!(c.ligatures);
        // Untouched fields keep their defaults.
        assert_eq!(c.theme, "itasha-corp");
    }

    #[test]
    fn term_defaults_to_xterm_256color() {
        let c = Config::default();
        assert_eq!(
            c.term, "xterm-256color",
            "default TERM must match the emulator's on-the-wire identity"
        );
        // The config default shares one source of truth with the PTY spawn default.
        assert_eq!(c.term, crate::pty::DEFAULT_TERM);
    }

    #[test]
    fn term_parses_from_partial_toml_and_old_configs_backfill() {
        // An explicit override parses.
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml("term = \"screen-256color\"\n", &p).unwrap();
        assert_eq!(c.term, "screen-256color");
        assert_eq!(c.theme, "itasha-corp"); // untouched
                                            // A config file with no `term` key backfills the default via serde(default).
        let c2 = Config::from_toml("theme = \"ghost-paper\"\n", &p).unwrap();
        assert_eq!(c2.term, "xterm-256color");
    }

    #[test]
    fn view_mode_defaults_to_grid() {
        assert_eq!(Config::default().view_mode, ViewMode::Grid);
    }

    #[test]
    fn ui_scale_defaults_to_one_and_backfills_for_old_configs() {
        assert_eq!(Config::default().ui_scale, 1.0);
        // A config file with no `ui_scale` key backfills via serde(default).
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml("theme = \"ghost-paper\"\n", &p).unwrap();
        assert_eq!(c.ui_scale, 1.0);
    }

    #[test]
    fn effective_ui_scale_clamps_and_guards_against_garbage() {
        let mk = |s: f32| Config {
            ui_scale: s,
            ..Config::default()
        };
        assert_eq!(mk(1.5).effective_ui_scale(), 1.5); // in-range passes through
        assert_eq!(mk(0.1).effective_ui_scale(), 0.5); // clamped up to the floor
        assert_eq!(mk(99.0).effective_ui_scale(), 3.0); // clamped down to the ceil
        assert_eq!(mk(f32::NAN).effective_ui_scale(), 1.0); // garbage → safe default
        assert_eq!(mk(f32::INFINITY).effective_ui_scale(), 1.0);
    }

    #[test]
    fn view_mode_toggles_between_grid_and_tabs() {
        assert_eq!(ViewMode::Grid.toggled(), ViewMode::Tabs);
        assert_eq!(ViewMode::Tabs.toggled(), ViewMode::Grid);
        // Two toggles return to the start (involution).
        assert_eq!(ViewMode::Grid.toggled().toggled(), ViewMode::Grid);
    }

    #[test]
    fn view_mode_round_trips_through_toml() {
        // A config that only sets view_mode parses via serde(default), and the
        // lowercase rename matches the serialized form.
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml("view_mode = \"tabs\"\n", &p).unwrap();
        assert_eq!(c.view_mode, ViewMode::Tabs);
        // Default theme untouched.
        assert_eq!(c.theme, "itasha-corp");
    }

    // ---- Transparency (single always-transparent opacity model, v0.4.21) ----

    #[test]
    fn default_ui_family_is_ibm_plex_mono() {
        // Kills the default_ui_family -> String::new()/"xyzzy" mutants: the app-UI
        // font MUST default to the bundled IBM Plex Mono out of the box.
        assert_eq!(default_ui_family(), "IBM Plex Mono");
    }

    #[test]
    fn transparency_defaults_to_solid_and_untinted() {
        // The single-model default: opacity 1.0 (the window reads as a solid
        // surface even though it is always transparent-capable) and no tint.
        let c = Config::default();
        assert_eq!(c.opacity, 1.0, "default opacity is solid (100%)");
        assert_eq!(c.tint_strength, 0.0, "no tint by default");
        assert_eq!(
            c.tint, "#08060d",
            "fresh default tint is brand-canon VOID BLACK"
        );
    }

    #[test]
    fn frost_defaults_are_off_and_load_from_an_older_config() {
        // Fresh defaults: the frosted-glass feature is OFF (opt-in), at a tasteful
        // mid-low amount, colour = follow-theme (empty), grain ON.
        let c = Config::default();
        assert!(!c.frost_enabled, "frost is opt-in (off by default)");
        assert!((c.frost_amount - 0.25).abs() < f32::EPSILON);
        assert_eq!(c.frost_color, "", "empty frost colour follows the theme bg");
        assert!(c.frost_grain, "grain texture on by default");

        // An OLDER config that predates the frost fields must still load, with the
        // frost fields backfilled from their serde defaults (never a parse error).
        let p = PathBuf::from("test.toml");
        let old = Config::from_toml("theme = \"ghost-paper\"\nopacity = 0.6\n", &p)
            .expect("a pre-frost config must still load");
        assert!(!old.frost_enabled);
        assert!((old.frost_amount - 0.25).abs() < f32::EPSILON);
        assert!(old.frost_grain);

        // Explicit frost settings round-trip.
        let c2 = Config::from_toml(
            "frost_enabled = true\nfrost_amount = 0.7\nfrost_color = \"#112233\"\nfrost_grain = false\n",
            &p,
        )
        .unwrap();
        assert!(c2.frost_enabled);
        assert!((c2.frost_amount - 0.7).abs() < f32::EPSILON);
        assert_eq!(c2.frost_color, "#112233");
        assert!(!c2.frost_grain);
    }

    /// Mirrors SCR1B3's F-035 test: `always_on_top` defaults OFF, an older config
    /// that predates the field still loads (backfilled to off), and an explicit
    /// `true` round-trips through a serialize → parse cycle unchanged.
    #[test]
    fn always_on_top_default_off_and_round_trips() {
        let c = Config::default();
        assert!(!c.always_on_top, "always-on-top is opt-in (off by default)");

        // A pre-field config must still load, with the flag backfilled to off.
        let p = PathBuf::from("test.toml");
        let old = Config::from_toml("theme = \"ghost-paper\"\n", &p)
            .expect("a pre-always_on_top config must still load");
        assert!(!old.always_on_top);

        // Set true → serialize → deserialize → still true.
        let mut c2 = c.clone();
        c2.always_on_top = true;
        let s = c2.to_toml().expect("serialize config to TOML");
        let back = Config::from_toml(&s, &p).expect("config TOML round-trip");
        assert!(back.always_on_top);
    }

    #[test]
    fn quake_defaults_off_and_round_trips() {
        let c = Config::default();
        assert!(
            !c.quake.enabled,
            "quake mode claims a GLOBAL hotkey — it must be opt-in (off by default)"
        );
        assert_eq!(c.quake.hotkey, "Ctrl+Shift+Grave");
        assert!((c.quake.height_fraction - 0.5).abs() < f32::EPSILON);

        // A pre-field config must still load with quake fully off — upgrading must
        // never silently claim a system-wide hotkey.
        let p = PathBuf::from("test.toml");
        let old = Config::from_toml("theme = \"ghost-paper\"\n", &p)
            .expect("a pre-quake config must still load");
        assert!(!old.quake.enabled, "an older config must not enable quake");

        // Enable + customise → serialize → deserialize → preserved.
        let mut c2 = c.clone();
        c2.quake.enabled = true;
        c2.quake.hotkey = "Win+F12".to_string();
        c2.quake.height_fraction = 0.75;
        let s = c2.to_toml().expect("serialize config to TOML");
        let back = Config::from_toml(&s, &p).expect("config TOML round-trip");
        assert!(back.quake.enabled);
        assert_eq!(back.quake.hotkey, "Win+F12");
        assert!((back.quake.height_fraction - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_hotkey_reads_modifiers_in_any_order_and_case() {
        let want = HotkeySpec {
            modifiers: MOD_CONTROL_BIT | MOD_SHIFT_BIT,
            vk: VK_GRAVE,
        };
        for spec in [
            "Ctrl+Shift+Grave",
            "shift+ctrl+grave",
            "CONTROL+SHIFT+`",
            "  Ctrl + Shift + Tilde  ",
        ] {
            assert_eq!(parse_hotkey(spec), Some(want), "{spec:?} must parse");
        }
        // Every modifier name maps to its own bit.
        assert_eq!(
            parse_hotkey("Win+Alt+Shift+Ctrl+Q").map(|h| h.modifiers),
            Some(MOD_WIN_BIT | MOD_ALT_BIT | MOD_SHIFT_BIT | MOD_CONTROL_BIT),
        );
        for alias in ["Win", "Super", "Meta", "Cmd"] {
            assert_eq!(
                parse_hotkey(&format!("{alias}+Q")).map(|h| h.modifiers),
                Some(MOD_WIN_BIT),
                "{alias} must map to MOD_WIN"
            );
        }
    }

    #[test]
    fn parse_hotkey_reads_letters_digits_and_function_keys() {
        assert_eq!(parse_hotkey("Ctrl+q").map(|h| h.vk), Some(u32::from(b'Q')));
        assert_eq!(parse_hotkey("Ctrl+7").map(|h| h.vk), Some(u32::from(b'7')));
        assert_eq!(parse_hotkey("Ctrl+F1").map(|h| h.vk), Some(VK_F1_KEY));
        assert_eq!(parse_hotkey("Ctrl+f12").map(|h| h.vk), Some(VK_F1_KEY + 11));
        assert_eq!(parse_hotkey("Ctrl+F24").map(|h| h.vk), Some(VK_F1_KEY + 23));
        // A bare `F` is the LETTER F, not a malformed function key.
        assert_eq!(parse_hotkey("Ctrl+F").map(|h| h.vk), Some(u32::from(b'F')));
        for (spec, vk) in [
            ("Ctrl+Space", VK_SPACE_KEY),
            ("Ctrl+Tab", VK_TAB_KEY),
            ("Ctrl+Esc", VK_ESCAPE_KEY),
            ("Ctrl+Escape", VK_ESCAPE_KEY),
            ("Ctrl+Enter", VK_RETURN_KEY),
            ("Ctrl+Return", VK_RETURN_KEY),
        ] {
            assert_eq!(parse_hotkey(spec).map(|h| h.vk), Some(vk), "{spec:?}");
        }
    }

    #[test]
    fn parse_hotkey_refuses_a_modifier_less_combo() {
        // THE safety rule: a bare-key global hotkey would swallow that keystroke
        // for the WHOLE desktop. Refuse it — quake mode stays inert instead.
        for spec in ["F12", "Grave", "q", "`", "Escape"] {
            assert_eq!(
                parse_hotkey(spec),
                None,
                "{spec:?} has no modifier and must be refused"
            );
        }
    }

    #[test]
    fn parse_hotkey_refuses_malformed_combos() {
        for spec in [
            "",             // empty
            "Ctrl",         // modifiers only, no key
            "Ctrl+Shift",   // ditto
            "Ctrl+",        // trailing separator
            "+Q",           // leading separator
            "Ctrl++Q",      // empty token
            "Ctrl+Nope",    // unknown key name
            "Ctrl+F0",      // out-of-range function key
            "Ctrl+F25",     // ditto
            "Ctrl+A+B",     // two non-modifier keys
            "Ctrl+Shift+@", // unsupported punctuation
        ] {
            assert_eq!(parse_hotkey(spec), None, "{spec:?} must be refused");
        }
    }

    #[test]
    fn register_modifiers_adds_norepeat_without_disturbing_the_combo() {
        let h = parse_hotkey("Ctrl+Shift+Grave").expect("parses");
        // Holding the combo must toggle ONCE, not machine-gun at the repeat rate.
        assert_eq!(
            h.register_modifiers(),
            MOD_CONTROL_BIT | MOD_SHIFT_BIT | MOD_NOREPEAT_BIT
        );
        // The parsed combo itself stays clean (no NOREPEAT leaking into equality).
        assert_eq!(h.modifiers, MOD_CONTROL_BIT | MOD_SHIFT_BIT);
        assert_eq!(h.register_modifiers() & MOD_NOREPEAT_BIT, MOD_NOREPEAT_BIT);
    }

    #[test]
    fn register_modifiers_unions_bits_and_never_toggles_one_off() {
        // `register_modifiers` is a UNION (`|`), not a toggle (`^`). The two are
        // indistinguishable while the input never already carries NOREPEAT — and
        // `HotkeySpec`'s fields are public, so a caller CAN hand one back in
        // (re-registering an already-prepared mask is the obvious way to do it).
        // With a toggle, that round-trip silently CLEARS `MOD_NOREPEAT` and the
        // hotkey machine-guns at the key-repeat rate instead of toggling once —
        // exactly the failure the flag exists to prevent. So assert the union
        // property over EVERY subset of the modifier bits, NOREPEAT included:
        // the result is always a superset of the input, and always has NOREPEAT.
        const BITS: [u32; 5] = [
            MOD_ALT_BIT,
            MOD_CONTROL_BIT,
            MOD_SHIFT_BIT,
            MOD_WIN_BIT,
            MOD_NOREPEAT_BIT,
        ];
        for mask in 0u32..(1 << BITS.len()) {
            let modifiers = BITS
                .iter()
                .enumerate()
                .filter(|(i, _)| (mask >> i) & 1 == 1)
                .fold(0u32, |acc, (_, bit)| acc | bit);
            let spec = HotkeySpec {
                modifiers,
                vk: VK_GRAVE,
            };
            let registered = spec.register_modifiers();
            assert_eq!(
                registered & modifiers,
                modifiers,
                "register_modifiers dropped a bit it was given: \
                 {modifiers:#06x} -> {registered:#06x}"
            );
            assert_eq!(
                registered & MOD_NOREPEAT_BIT,
                MOD_NOREPEAT_BIT,
                "MOD_NOREPEAT must always be set, even when the input already \
                 carries it: {modifiers:#06x} -> {registered:#06x}"
            );
            assert_eq!(
                registered,
                modifiers | MOD_NOREPEAT_BIT,
                "the mask is exactly the user's bits plus NOREPEAT"
            );
        }
    }

    #[test]
    fn quake_parsed_hotkey_matches_the_free_parser_and_the_default_is_usable() {
        // The Settings UI calls `parsed_hotkey()`; the Win32 path calls
        // `parse_hotkey()`. They must be the same answer, or the UI would tell the
        // user a combo is fine that never registers.
        let mut q = QuakeConfig::default();
        assert_eq!(q.parsed_hotkey(), parse_hotkey(&q.hotkey));
        assert!(
            q.parsed_hotkey().is_some(),
            "the shipped default combo must be registerable"
        );
        q.hotkey = "F12".to_string();
        assert_eq!(q.parsed_hotkey(), None, "modifier-less combo is refused");
        assert_eq!(q.parsed_hotkey(), parse_hotkey(&q.hotkey));
    }

    #[test]
    fn quake_height_fraction_is_clamped_and_nan_guarded() {
        let mut q = QuakeConfig {
            height_fraction: 0.0,
            ..Default::default()
        };
        // A malformed config must never produce a zero-height or oversized window.
        assert!((q.effective_height_fraction() - QuakeConfig::MIN_HEIGHT_FRACTION).abs() < 1e-6);
        q.height_fraction = -3.0;
        assert!((q.effective_height_fraction() - QuakeConfig::MIN_HEIGHT_FRACTION).abs() < 1e-6);
        q.height_fraction = 9.0;
        assert!((q.effective_height_fraction() - QuakeConfig::MAX_HEIGHT_FRACTION).abs() < 1e-6);
        // Non-finite falls back to the default, never propagates NaN into geometry.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            q.height_fraction = bad;
            assert!(
                (q.effective_height_fraction() - 0.5).abs() < 1e-6,
                "{bad} must fall back to the default fraction"
            );
        }
        // An in-range value passes through untouched.
        q.height_fraction = 0.35;
        assert!((q.effective_height_fraction() - 0.35).abs() < 1e-6);
    }

    #[test]
    fn legacy_transparency_keys_are_ignored_and_opacity_is_retained() {
        // An on-disk config from the multi-mode era carries `transparency_enabled`,
        // `window_mode`, and `acrylic` keys. Those fields no longer exist, so serde
        // IGNORES them (no `deny_unknown_fields`) and the config loads without
        // error; the retained `opacity` carries the see-through level directly.
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml(
            "transparency_enabled = true\nwindow_mode = \"glass\"\nacrylic = true\nopacity = 0.8\n",
            &p,
        )
        .expect("a legacy multi-mode config must still load (unknown keys ignored)");
        assert!(
            (c.opacity - 0.8).abs() < f32::EPSILON,
            "the retained opacity carries the see-through level"
        );
    }

    #[test]
    fn invalid_tint_strength_is_rejected() {
        let p = PathBuf::from("test.toml");
        assert!(
            Config::from_toml("tint_strength = 2.0\n", &p).is_err(),
            "tint_strength above 1.0 is invalid"
        );
    }

    #[test]
    fn invalid_tint_hex_is_rejected() {
        let p = PathBuf::from("test.toml");
        assert!(
            Config::from_toml("tint = \"not-a-color\"\n", &p).is_err(),
            "a non-#RRGGBB tint is invalid"
        );
    }

    #[test]
    fn opacity_and_tint_round_trip_through_toml() {
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml(
            "opacity = 0.4\ntint = \"#aabbcc\"\ntint_strength = 0.4\ntint_enabled = true\n",
            &p,
        )
        .unwrap();
        assert!((c.opacity - 0.4).abs() < f32::EPSILON);
        assert_eq!(c.tint, "#aabbcc");
        assert!((c.tint_strength - 0.4).abs() < f32::EPSILON);
        assert!(c.tint_enabled);
        // A round-trip (serialize → parse) preserves the values.
        let toml = c.to_toml().unwrap();
        let c2 = Config::from_toml(&toml, &p).unwrap();
        assert!((c2.opacity - 0.4).abs() < f32::EPSILON);
        assert_eq!(c2.tint, "#aabbcc");
    }

    // ---- Effects (CRT scanlines + chromatic aberration) ----

    #[test]
    fn effects_defaults_are_off_with_visible_scanline_darkness() {
        let e = EffectsConfig::default();
        assert!(!e.crt_scanlines, "scanlines opt-in");
        assert!(!e.chromatic_aberration_enabled, "chromatic opt-in");
        assert_eq!(e.chromatic_aberration, 0.0);
        assert!(
            (e.scanline_darkness - DEFAULT_SCANLINE_DARKNESS).abs() < f32::EPSILON,
            "default darkness reads as lines, not a flat film"
        );
    }

    #[test]
    fn effective_chromatic_is_gated_by_the_explicit_toggle() {
        // Intensity set but toggle off => effectively off (no "slider at 0.6 but
        // does nothing because the checkbox is unchecked" surprise — the toggle
        // is authoritative).
        let mut e = EffectsConfig {
            chromatic_aberration: 0.6,
            ..EffectsConfig::default()
        };
        assert_eq!(e.effective_chromatic(), 0.0, "toggle off => no aberration");
        // Toggle on => the intensity applies.
        e.chromatic_aberration_enabled = true;
        assert!((e.effective_chromatic() - 0.6).abs() < f32::EPSILON);
        // Negative intensity is floored to 0 even when enabled.
        e.chromatic_aberration = -1.0;
        assert_eq!(
            e.effective_chromatic(),
            0.0,
            "negative intensity floors to 0"
        );
    }

    #[test]
    fn old_config_without_new_effects_fields_still_parses() {
        // A pre-toggle config that only set crt_scanlines + the f32 intensity
        // must keep loading via serde(default), backfilling the new fields.
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml(
            "[effects]\ncrt_scanlines = true\nchromatic_aberration = 0.5\n",
            &p,
        )
        .unwrap();
        assert!(c.effects.crt_scanlines);
        assert!((c.effects.chromatic_aberration - 0.5).abs() < f32::EPSILON);
        // The new toggle defaults OFF, so an old file's intensity stays inert
        // until the user opts in — backward-compatible.
        assert!(!c.effects.chromatic_aberration_enabled);
        assert!(
            (c.effects.scanline_darkness - DEFAULT_SCANLINE_DARKNESS).abs() < f32::EPSILON,
            "missing darkness backfills to the visible default"
        );
    }

    #[test]
    fn new_effects_fields_round_trip_through_toml() {
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml(
            "[effects]\ncrt_scanlines = true\nscanline_darkness = 0.55\nchromatic_aberration_enabled = true\nchromatic_aberration = 0.8\n",
            &p,
        )
        .unwrap();
        assert!(c.effects.crt_scanlines);
        assert!((c.effects.scanline_darkness - 0.55).abs() < f32::EPSILON);
        assert!(c.effects.chromatic_aberration_enabled);
        assert!((c.effects.effective_chromatic() - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn save_to_round_trips_and_creates_parent_dirs() {
        // A save followed by a load reproduces the persisted fields, and the
        // missing parent directory is created (the atomic-write path handles it).
        let dir = std::env::temp_dir()
            .join(format!("c0pl4nd-cfg-{}-rt", std::process::id()))
            .join("nested");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        let path = dir.join("config.toml");
        let c = Config {
            theme: "ghost-paper".to_string(),
            ..Config::default()
        };
        c.save_to(&path).expect("save");
        let loaded = Config::load_from(&path).expect("load");
        assert_eq!(loaded.theme, "ghost-paper");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    // ---- W1TN3SS opt-in reporting (additive, default-OFF) ----

    #[test]
    fn reporting_defaults_both_streams_off() {
        use itasha_report_core::config::ReportingMode;
        let c = Config::default();
        assert_eq!(
            c.reporting.streams.crash_reports,
            ReportingMode::Off,
            "crash reporting MUST default OFF (opt-in only)"
        );
        assert_eq!(
            c.reporting.streams.manual_issues,
            ReportingMode::Off,
            "manual-issue reporting MUST default OFF (opt-in only)"
        );
    }

    #[test]
    fn old_config_without_reporting_table_loads_both_streams_off() {
        use itasha_report_core::config::ReportingMode;
        // A config file written BEFORE the reporting feature existed (no
        // `[reporting]` table) must still parse, and reporting must come up OFF
        // — upgrading never silently enables any reporting stream.
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml("theme = \"ghost-paper\"\n", &p).unwrap();
        assert_eq!(c.reporting.streams.crash_reports, ReportingMode::Off);
        assert_eq!(c.reporting.streams.manual_issues, ReportingMode::Off);
        // Issue-intake coords backfill to the C0PL4ND defaults.
        assert_eq!(
            c.reporting.issue_intake.repo,
            "46b-ETYKiAL/Itasha.Corp_C0PL4ND"
        );
    }

    #[test]
    fn reporting_round_trips_and_is_reversible() {
        use itasha_report_core::config::ReportingMode;
        // A user opts crash reporting into AskEachTime; the migrate is reversible
        // (serialize → parse reproduces the stored value; an unset stream stays
        // OFF). serde stored-value wins, so an explicit value is never clobbered.
        let p = PathBuf::from("test.toml");
        let toml = "[reporting.streams]\ncrash_reports = \"ask_each_time\"\n";
        let c = Config::from_toml(toml, &p).unwrap();
        assert_eq!(
            c.reporting.streams.crash_reports,
            ReportingMode::AskEachTime
        );
        // Manual-issue stream stays OFF (only the set field changed).
        assert_eq!(c.reporting.streams.manual_issues, ReportingMode::Off);
        // Round-trip: serialize then re-parse reproduces the posture exactly.
        let serialized = c.to_toml().unwrap();
        let c2 = Config::from_toml(&serialized, &p).unwrap();
        assert_eq!(c2.reporting, c.reporting);
    }

    // ---- validate() Invalid arms: each out-of-range / empty value is rejected
    //      with the EXACT ConfigError::Invalid variant and a message that names
    //      the offending field, so the settings UI can surface a useful error. ----

    /// Pull the inner message out of a `ConfigError::Invalid`, panicking with the
    /// real variant if the error is anything else — so a wrong *variant* (not just
    /// a wrong message) fails the test.
    fn expect_invalid(err: ConfigError) -> String {
        match err {
            ConfigError::Invalid(msg) => msg,
            other => panic!("expected ConfigError::Invalid, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_empty_theme_with_named_message() {
        let c = Config {
            theme: "   ".to_string(), // whitespace-only trims to empty
            ..Config::default()
        };
        let msg = expect_invalid(c.validate().unwrap_err());
        assert_eq!(msg, "theme name must not be empty");
    }

    #[test]
    fn validate_rejects_out_of_range_opacity_with_value_in_message() {
        // Above the 0.0..=1.0 band.
        let hi = Config {
            opacity: 5.0,
            ..Config::default()
        };
        let msg = expect_invalid(hi.validate().unwrap_err());
        assert_eq!(msg, "opacity must be between 0.0 and 1.0, got 5");
        // Below the band (negative) is equally rejected; the literal value is echoed.
        let lo = Config {
            opacity: -0.5,
            ..Config::default()
        };
        let msg = expect_invalid(lo.validate().unwrap_err());
        assert_eq!(msg, "opacity must be between 0.0 and 1.0, got -0.5");
        // The exact band edges are VALID (boundary inclusivity).
        assert!(Config {
            opacity: 0.0,
            ..Config::default()
        }
        .validate()
        .is_ok());
        assert!(Config {
            opacity: 1.0,
            ..Config::default()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn validate_rejects_non_positive_font_size() {
        // Zero is non-positive → rejected.
        let zero = Config {
            font: FontConfig {
                size: 0.0,
                ..FontConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(
            expect_invalid(zero.validate().unwrap_err()),
            "font.size must be positive"
        );
        // Negative is also rejected.
        let neg = Config {
            font: FontConfig {
                size: -3.0,
                ..FontConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(
            expect_invalid(neg.validate().unwrap_err()),
            "font.size must be positive"
        );
    }

    #[test]
    fn validate_rejects_zero_cols_or_rows() {
        // cols == 0 (rows fine) → rejected.
        let c0 = Config {
            window: WindowConfig {
                cols: 0,
                ..WindowConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(
            expect_invalid(c0.validate().unwrap_err()),
            "window cols and rows must be non-zero"
        );
        // rows == 0 (cols fine) → also rejected (the OR arm).
        let r0 = Config {
            window: WindowConfig {
                rows: 0,
                ..WindowConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(
            expect_invalid(r0.validate().unwrap_err()),
            "window cols and rows must be non-zero"
        );
    }

    #[test]
    fn validate_rejects_out_of_range_tint_strength_with_value() {
        let c = Config {
            tint_strength: 1.5,
            ..Config::default()
        };
        assert_eq!(
            expect_invalid(c.validate().unwrap_err()),
            "tint_strength must be between 0.0 and 1.0, got 1.5"
        );
    }

    #[test]
    fn validate_rejects_bad_tint_hex_with_debug_quoted_value() {
        // A non-#RRGGBB tint is rejected, and the offending value is echoed
        // Debug-quoted (so `{:?}` of the String shows the surrounding quotes).
        let c = Config {
            tint: "nope".to_string(),
            ..Config::default()
        };
        assert_eq!(
            expect_invalid(c.validate().unwrap_err()),
            "tint must be a #RRGGBB hex color, got \"nope\""
        );
    }

    #[test]
    fn validate_passes_for_a_fully_valid_non_default_config() {
        // Every guarded field set to a valid non-default value still validates,
        // proving the happy path threads all the way through validate().
        let c = Config {
            theme: "ghost-paper".to_string(),
            opacity: 0.5,
            tint_strength: 0.5,
            tint: "#abcdef".to_string(),
            font: FontConfig {
                size: 10.0,
                ..FontConfig::default()
            },
            window: WindowConfig {
                cols: 120,
                rows: 40,
                ..WindowConfig::default()
            },
            ..Config::default()
        };
        assert!(c.validate().is_ok());
    }

    // ---- Tray / close-guard decisions (the pure logic the app's close paths call) ----

    #[test]
    fn tray_and_close_warning_defaults_are_off_off_on() {
        // The tray toggles ship OFF (the ✕ keeps meaning "quit" until the user
        // opts in) and the running-command warning ships ON (it guards a real
        // data-loss surface and can only fire when a command is actually live).
        let w = WindowConfig::default();
        assert!(!w.close_to_tray, "close_to_tray must default OFF");
        assert!(!w.minimize_to_tray, "minimize_to_tray must default OFF");
        assert!(
            w.warn_on_close_running,
            "warn_on_close_running must default ON"
        );
    }

    #[test]
    fn close_action_covers_every_flag_combination() {
        // Exhaustive over (close_to_tray, tray_available, explicit_quit): the
        // ONLY hide is opt-in + tray-present + not an explicit quit.
        for &close_to_tray in &[false, true] {
            for &tray_available in &[false, true] {
                for &explicit_quit in &[false, true] {
                    let w = WindowConfig {
                        close_to_tray,
                        ..WindowConfig::default()
                    };
                    let expected = if close_to_tray && tray_available && !explicit_quit {
                        CloseAction::HideToTray
                    } else {
                        CloseAction::Exit
                    };
                    assert_eq!(
                        w.close_action(tray_available, explicit_quit),
                        expected,
                        "close_to_tray={close_to_tray} tray_available={tray_available} \
                         explicit_quit={explicit_quit}"
                    );
                }
            }
        }
    }

    #[test]
    fn close_to_tray_never_hides_without_a_tray() {
        // The stranding case: hiding to a tray that failed to build would leave
        // the window invisible with no icon to restore it and the shells still
        // running. A missing tray must always degrade to a real exit.
        let w = WindowConfig {
            close_to_tray: true,
            ..WindowConfig::default()
        };
        assert_eq!(w.close_action(false, false), CloseAction::Exit);
        assert_eq!(w.close_action(true, false), CloseAction::HideToTray);
    }

    #[test]
    fn explicit_quit_always_exits_even_with_close_to_tray_on() {
        // The tray menu's Quit posts WM_CLOSE into the same close path. Without
        // the explicit-quit escape the app would hide instead of quitting and
        // could never be closed at all.
        let w = WindowConfig {
            close_to_tray: true,
            ..WindowConfig::default()
        };
        assert_eq!(w.close_action(true, true), CloseAction::Exit);
    }

    #[test]
    fn minimize_action_covers_every_flag_combination() {
        for &minimize_to_tray in &[false, true] {
            for &tray_available in &[false, true] {
                let w = WindowConfig {
                    minimize_to_tray,
                    ..WindowConfig::default()
                };
                let expected = if minimize_to_tray && tray_available {
                    MinimizeAction::HideToTray
                } else {
                    MinimizeAction::Minimize
                };
                assert_eq!(
                    w.minimize_action(tray_available),
                    expected,
                    "minimize_to_tray={minimize_to_tray} tray_available={tray_available}"
                );
            }
        }
    }

    #[test]
    fn close_guard_confirms_only_when_enabled_busy_and_unconfirmed() {
        // Exhaustive over (warn_on_close_running, busy_panes==0?, already_confirmed).
        for &warn in &[false, true] {
            for &busy in &[0usize, 1, 3] {
                for &confirmed in &[false, true] {
                    let w = WindowConfig {
                        warn_on_close_running: warn,
                        ..WindowConfig::default()
                    };
                    let expected = if warn && busy > 0 && !confirmed {
                        CloseGuard::Confirm { busy_panes: busy }
                    } else {
                        CloseGuard::Proceed
                    };
                    assert_eq!(
                        w.close_guard(busy, confirmed),
                        expected,
                        "warn={warn} busy={busy} confirmed={confirmed}"
                    );
                }
            }
        }
    }

    #[test]
    fn close_guard_reports_the_real_busy_pane_count() {
        // The count reaches the prompt, so the dialog can say "2 panes are still
        // running a command" rather than a generic message. A guard that returned
        // a constant would satisfy a bare is-Confirm assertion.
        let w = WindowConfig::default();
        assert_eq!(
            w.close_guard(2, false),
            CloseGuard::Confirm { busy_panes: 2 }
        );
        assert_eq!(
            w.close_guard(7, false),
            CloseGuard::Confirm { busy_panes: 7 }
        );
    }

    #[test]
    fn close_guard_confirmed_answer_stops_the_prompt_looping() {
        // The commands are STILL running when the user clicks "Close anyway", so
        // without the already-confirmed short-circuit the second pass through the
        // close path would prompt again, forever.
        let w = WindowConfig::default();
        assert_eq!(
            w.close_guard(4, false),
            CloseGuard::Confirm { busy_panes: 4 }
        );
        assert_eq!(w.close_guard(4, true), CloseGuard::Proceed);
    }

    #[test]
    fn tray_and_warning_flags_round_trip_through_toml() {
        // The toggles must survive save→load, and a config written before these
        // fields existed must still parse (serde(default) on WindowConfig).
        let mut c = Config::default();
        c.window.close_to_tray = true;
        c.window.minimize_to_tray = true;
        c.window.warn_on_close_running = false;
        let toml = c.to_toml().expect("serialise");
        let back = Config::from_toml(&toml, Path::new("round.toml")).expect("parse");
        assert!(back.window.close_to_tray);
        assert!(back.window.minimize_to_tray);
        assert!(!back.window.warn_on_close_running);

        // A legacy [window] table with none of the three keys falls back to the
        // documented defaults rather than failing to parse.
        let legacy = Config::from_toml(
            "[window]\ncols = 100\nrows = 30\n",
            Path::new("legacy.toml"),
        )
        .expect("legacy config must still parse");
        assert!(!legacy.window.close_to_tray);
        assert!(!legacy.window.minimize_to_tray);
        assert!(legacy.window.warn_on_close_running);
    }

    #[test]
    fn persist_geometry_copies_only_geometry_not_the_tray_toggles() {
        // `persist_geometry` reloads the on-disk config and overwrites the
        // geometry fields only. If it ever started copying the whole WindowConfig
        // it would clobber a tray toggle the user changed in the settings window
        // with whatever the geometry-capture snapshot happened to hold.
        let on_disk = WindowConfig {
            close_to_tray: true,
            minimize_to_tray: true,
            warn_on_close_running: false,
            ..WindowConfig::default()
        };
        let mut merged = on_disk.clone();
        let captured = WindowConfig {
            pos_x: Some(11),
            pos_y: Some(22),
            size_w: Some(800),
            size_h: Some(600),
            maximized: Some(true),
            monitor: Some("DISPLAY1".to_string()),
            ..WindowConfig::default()
        };
        // Mirrors persist_geometry's field-by-field copy exactly.
        merged.pos_x = captured.pos_x;
        merged.pos_y = captured.pos_y;
        merged.size_w = captured.size_w;
        merged.size_h = captured.size_h;
        merged.maximized = captured.maximized;
        merged.monitor = captured.monitor.clone();

        assert_eq!(merged.pos_x, Some(11), "geometry must be copied");
        assert!(merged.close_to_tray, "close_to_tray must survive");
        assert!(merged.minimize_to_tray, "minimize_to_tray must survive");
        assert!(
            !merged.warn_on_close_running,
            "warn_on_close_running must survive"
        );
        // And the captured snapshot's own defaults are NOT what landed.
        assert_ne!(
            merged.warn_on_close_running, captured.warn_on_close_running,
            "the geometry snapshot's default must not have overwritten the user's value"
        );
    }

    // ---- ConfigError variants: construction, Display, and the load/save Io arms ----

    #[test]
    fn config_error_not_found_display_names_path_and_defaults() {
        let e = ConfigError::NotFound(PathBuf::from("/etc/c0pl4nd/config.toml"));
        let s = e.to_string();
        assert!(
            s.contains("config file not found") && s.contains("using built-in defaults"),
            "NotFound Display must explain the fallback, got: {s}"
        );
        assert!(
            s.contains("config.toml"),
            "NotFound Display must name the path, got: {s}"
        );
    }

    #[test]
    fn config_error_parse_is_the_parse_variant_with_path_and_message() {
        // Malformed TOML surfaces a Parse error (not Invalid/Io), carrying the
        // path we passed and a non-empty human-readable message.
        let p = PathBuf::from("broken.toml");
        let err = Config::from_toml("this is = = not toml", &p).unwrap_err();
        match err {
            ConfigError::Parse { path, message } => {
                assert_eq!(path, p, "Parse error must echo the source path");
                assert!(!message.is_empty(), "Parse error must carry a message");
                // The Display format embeds both the path and the message.
                let disp = ConfigError::Parse { path, message }.to_string();
                assert!(disp.contains("config parse error in") && disp.contains("broken.toml"));
            }
            other => panic!("expected ConfigError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn config_error_invalid_display_format() {
        let e = ConfigError::Invalid("widget out of range".to_string());
        assert_eq!(
            e.to_string(),
            "config validation error: widget out of range"
        );
    }

    #[test]
    fn config_error_io_display_names_path_and_source() {
        let e = ConfigError::Io {
            path: PathBuf::from("/no/such/cfg.toml"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        let s = e.to_string();
        assert!(
            s.contains("could not read config file")
                && s.contains("cfg.toml")
                && s.contains("denied"),
            "Io Display must name the path and embed the source, got: {s}"
        );
    }

    #[test]
    fn load_from_existing_valid_file_round_trips_through_from_toml() {
        // The Ok arm of load_from: an existing, readable, valid file parses.
        let path =
            std::env::temp_dir().join(format!("c0pl4nd-load-{}-ok.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "theme = \"ghost-paper\"\nligatures = true\n").expect("seed");
        let c = Config::load_from(&path).expect("valid file loads");
        assert_eq!(c.theme, "ghost-paper");
        assert!(c.ligatures);
        assert_eq!(c.font.size, 13.0, "unset fields keep their defaults");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_invalid_file_surfaces_validation_error_not_defaults() {
        // The Ok arm reads, but validation rejects an out-of-range value — the
        // error must propagate (not silently fall back to defaults).
        let path =
            std::env::temp_dir().join(format!("c0pl4nd-load-{}-bad.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "opacity = 9.0\n").expect("seed");
        let err = Config::load_from(&path).unwrap_err();
        assert!(
            matches!(err, ConfigError::Invalid(_)),
            "an out-of-range value in a real file must surface as Invalid, got {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_to_under_a_regular_file_parent_yields_io_error() {
        // Provoke the save_to Io arm cross-platform: make the parent a regular
        // FILE, so create_dir_all (inside atomic_write_owner_only) cannot create
        // a directory there. The error must surface as ConfigError::Io carrying
        // the target path — never a panic, never a silent success.
        let file_as_parent =
            std::env::temp_dir().join(format!("c0pl4nd-save-{}-blocker", std::process::id()));
        let _ = std::fs::remove_file(&file_as_parent);
        let _ = std::fs::remove_dir_all(&file_as_parent);
        std::fs::write(&file_as_parent, b"i am a file, not a dir").expect("seed blocker");
        // Target a path *under* the regular file → its parent dir cannot exist.
        let target = file_as_parent.join("sub").join("config.toml");
        let err = Config::default().save_to(&target).unwrap_err();
        match err {
            ConfigError::Io { path, source: _ } => {
                assert_eq!(path, target, "Io error must echo the target path");
            }
            other => panic!("expected ConfigError::Io, got {other:?}"),
        }
        let _ = std::fs::remove_file(&file_as_parent);
    }

    #[cfg(windows)]
    #[test]
    fn load_from_unreadable_path_surfaces_io_not_notfound() {
        // On Windows an invalid filename (reserved characters) yields an IO error
        // whose kind is NOT NotFound, so it routes to the load_from Io arm rather
        // than the "missing file → defaults" arm. This covers the third match arm
        // of load_from that NotFound can never reach on this platform.
        let bad = std::env::temp_dir().join("c0pl4nd-inv<>:|?.toml");
        match Config::load_from(&bad) {
            Err(ConfigError::Io { path, source }) => {
                assert_eq!(path, bad);
                assert_ne!(
                    source.kind(),
                    std::io::ErrorKind::NotFound,
                    "a NotFound kind should have fallen back to defaults, not Io"
                );
            }
            // If the platform unexpectedly treats it as NotFound, the contract is
            // still honoured: missing file → defaults. Accept that outcome too so
            // the test is not brittle across Windows builds.
            Ok(c) => assert_eq!(c, Config::default()),
            Err(other) => panic!("expected Io or default-fallback, got {other:?}"),
        }
    }

    // ---- to_toml: round-trip fidelity through the public serialize path ----

    #[test]
    fn to_toml_serializes_then_from_toml_reproduces_the_config() {
        // to_toml is the inverse of from_toml; a non-default config survives the
        // round trip field-for-field. Exercises the Ok arm of to_toml and proves
        // the serialized form re-parses to an equal value.
        let original = Config {
            theme: "ghost-paper".to_string(),
            opacity: 0.75,
            tint: "#aabbcc".to_string(),
            tint_strength: 0.3,
            ligatures: true,
            ..Config::default()
        };
        let serialized = original.to_toml().expect("serialize");
        assert!(
            serialized.contains("theme = \"ghost-paper\""),
            "serialized TOML should contain the theme key"
        );
        let p = PathBuf::from("roundtrip.toml");
        let reparsed = Config::from_toml(&serialized, &p).expect("re-parse");
        assert_eq!(reparsed, original, "to_toml ∘ from_toml must be identity");
    }

    // ---- IssueIntake / Reporting default coordinates (exact values pinned) ----

    #[test]
    fn issue_intake_defaults_are_the_c0pl4nd_coordinates() {
        let i = IssueIntakeConfig::default();
        assert_eq!(i.repo, "46b-ETYKiAL/Itasha.Corp_C0PL4ND");
        // The top-level ReportingConfig embeds those same defaults.
        let r = ReportingConfig::default();
        assert_eq!(r.issue_intake, i);
    }

    /// NO EMAIL ADDRESS MAY SHIP IN THE DEFAULT CONFIG.
    ///
    /// This default is compiled into every binary of a public repo. It used to
    /// carry a maintainer's personal `proton.me` mailbox, published to every
    /// downloader and every address harvester that reads the source.
    ///
    /// The assertion is on the SHAPE (`@`-free), not on one banned literal: a
    /// test pinning the old string would have to be edited in lockstep with the
    /// leak it was pinning — which is exactly how it survived — whereas an
    /// `@`-free assertion rejects the next address too.
    #[test]
    fn no_email_address_ships_in_the_default_issue_intake_config() {
        let i = IssueIntakeConfig::default();
        assert!(
            i.mailto_alias.is_empty(),
            "the shipped default must carry NO mailbox; got {:?}",
            i.mailto_alias
        );
        let toml = Config::default()
            .to_toml()
            .expect("the default config must serialize");
        assert!(
            !toml.contains('@'),
            "the serialized default config must contain no email address; it is \
             written to every user's disk and read from a public source tree"
        );
    }

    // ---- PanelSide default + serde round-trip ----

    #[test]
    fn panel_side_defaults_right_and_round_trips() {
        assert_eq!(PanelSide::default(), PanelSide::Right);
        assert_eq!(Config::default().history_sidebar_side, PanelSide::Right);
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml("history_sidebar_side = \"left\"\n", &p).unwrap();
        assert_eq!(c.history_sidebar_side, PanelSide::Left);
    }

    // ---- CursorStyle / CursorConfig parsing + defaults ----

    #[test]
    fn cursor_defaults_block_blinking_and_parses_snake_case_styles() {
        let cc = CursorConfig::default();
        assert_eq!(cc.style, CursorStyle::Block);
        assert!(cc.blink);
        // snake_case rename: "underline" / "bar" parse to their variants, and a
        // partial [cursor] table backfills the unspecified field.
        let p = PathBuf::from("test.toml");
        let c = Config::from_toml("[cursor]\nstyle = \"underline\"\nblink = false\n", &p).unwrap();
        assert_eq!(c.cursor.style, CursorStyle::Underline);
        assert!(!c.cursor.blink);
        let c2 = Config::from_toml("[cursor]\nstyle = \"bar\"\n", &p).unwrap();
        assert_eq!(c2.cursor.style, CursorStyle::Bar);
        assert!(
            c2.cursor.blink,
            "unset blink backfills to the default (true)"
        );
    }

    // ---- UpdateMode parsing + checks_on_launch for every mode ----

    #[test]
    fn update_mode_parses_all_variants_and_checks_on_launch_is_per_mode() {
        let p = PathBuf::from("test.toml");
        for (raw, mode, on_launch) in [
            ("off", UpdateMode::Off, false),
            ("notify", UpdateMode::Notify, true),
            ("manual", UpdateMode::Manual, false),
            ("auto", UpdateMode::Auto, true),
        ] {
            let toml = format!("[update]\nmode = \"{raw}\"\n");
            let c = Config::from_toml(&toml, &p).unwrap();
            assert_eq!(c.update.mode, mode, "mode {raw:?} must parse to {mode:?}");
            assert_eq!(
                c.update.checks_on_launch(),
                on_launch,
                "checks_on_launch for {raw:?} must be {on_launch}"
            );
        }
    }

    #[test]
    fn legacy_check_on_launch_flag_forces_on_launch_even_when_mode_is_off() {
        // The legacy boolean OR-arm: mode = off (no network) but the old
        // check_on_launch = true flag still forces an on-launch check, so old
        // config files keep their behaviour.
        let p = PathBuf::from("test.toml");
        let c =
            Config::from_toml("[update]\nmode = \"off\"\ncheck_on_launch = true\n", &p).unwrap();
        assert_eq!(c.update.mode, UpdateMode::Off);
        assert!(
            c.update.checks_on_launch(),
            "the legacy check_on_launch flag forces a launch check despite mode=off"
        );
        // And with the flag false AND mode off, no launch check (the false arm).
        let c2 = Config::from_toml("[update]\nmode = \"manual\"\n", &p).unwrap();
        assert!(!c2.update.checks_on_launch());
    }

    #[test]
    fn fresh_config_is_born_at_current_schema_version() {
        // A brand-new config is stamped at the current version so `migrate` is a
        // no-op for new users (no spurious first-run rewrite).
        let c = Config::default();
        assert_eq!(c.schema_version, CURRENT_SCHEMA_VERSION);
        // migrate() on a fresh config changes nothing and reports no change.
        let mut c2 = Config::default();
        assert!(!c2.migrate(), "a fresh config must not be migrated");
        assert_eq!(c2.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn schema_version_round_trips() {
        // A default config serialized then reloaded (which runs migrate) stays at
        // the current version — the version survives a save/load cycle.
        let p = PathBuf::from("cfg.toml");
        let toml = toml::to_string(&Config::default()).expect("serialize");
        let back = Config::from_toml(&toml, &p).expect("reload");
        assert_eq!(back.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn migrate_is_a_noop_and_never_panics_on_a_forward_version_config() {
        // A config written by a NEWER build (schema_version ahead of ours) opened
        // by an older build must be left untouched, report no change, and — the
        // load-bearing part — never trip the debug-only migration invariants.
        let mut forward = Config {
            schema_version: CURRENT_SCHEMA_VERSION + 5,
            ..Config::default()
        };
        let changed = forward.migrate();
        assert!(!changed, "a forward-version config must not be migrated");
        assert_eq!(
            forward.schema_version,
            CURRENT_SCHEMA_VERSION + 5,
            "a forward-version config legitimately stays ahead"
        );
    }

    #[test]
    fn legacy_v0_config_loads_and_stamps_current_schema() {
        // An EXISTING config with no `schema_version` (→ 0) that still carries the
        // retired legacy transparency keys must load without error (unknown keys are
        // ignored), keep its `opacity` see-through level, and be stamped forward to
        // the current schema version.
        let p = PathBuf::from("legacy.toml");
        let c = Config::from_toml(
            "opacity = 0.8\ntransparency_enabled = true\nwindow_mode = \"glass\"\nacrylic = true\n",
            &p,
        )
        .unwrap();
        assert_eq!(c.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(
            (c.opacity - 0.8).abs() < f32::EPSILON,
            "the retained opacity carries the see-through level"
        );
    }

    #[test]
    fn fresh_config_default_tint_is_void_black() {
        // A brand-new config is born with the brand-canon VOID BLACK default tint.
        let c = Config::default();
        assert_eq!(c.tint, "#08060d");
    }

    #[test]
    fn pre_v2_default_tint_migrates_to_void_black() {
        // An EXISTING config (no `schema_version` → 0) still carrying the OLD
        // `#121212` default tint is remapped to `#08060d` on load, then stamped at
        // the current schema version.
        let p = PathBuf::from("legacy-tint.toml");
        let c = Config::from_toml("tint = \"#121212\"\n", &p).unwrap();
        assert_eq!(c.tint, "#08060d", "old default tint remaps to VOID BLACK");
        assert_eq!(c.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn pre_v2_custom_tint_is_preserved_verbatim() {
        // A user's CUSTOM tint (any value ≠ the old `#121212` default) must survive
        // the v1 → v2 migration untouched — no silent clobber.
        let p = PathBuf::from("custom-tint.toml");
        let c = Config::from_toml("tint = \"#445566\"\n", &p).unwrap();
        assert_eq!(c.tint, "#445566", "a user's custom tint is never remapped");
        assert_eq!(c.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn toolbar_config_defaults_when_absent_and_round_trips() {
        let p = std::path::PathBuf::from("cfg.toml");
        // An EXISTING config predating the feature (no `[toolbar]` table) loads the
        // shipped defaults — never broken by the new field (serde-default merge).
        // The default keeps view/equalize/shell on the LEFT and pins ONLY the
        // script launcher on the RIGHT (by the gear).
        let c = Config::from_toml("", &p).unwrap();
        assert_eq!(c.toolbar, ToolbarConfig::default());
        assert_eq!(
            c.toolbar.left,
            vec!["view_mode", "equalize_panes", "shell_switcher"]
        );
        assert_eq!(c.toolbar.right, vec!["script_launcher"]);
        assert!(c.toolbar.show_overflow);
        assert!(c.toolbar.menu.is_empty());

        // A PARTIAL `[toolbar]` (only `right`) fills the other fields from defaults.
        let partial = Config::from_toml("[toolbar]\nright = [\"view_mode\"]\n", &p).unwrap();
        assert_eq!(partial.toolbar.right, vec!["view_mode"]);
        assert!(partial.toolbar.show_overflow); // default true, not clobbered
        assert!(partial.toolbar.menu.is_empty());

        // A fully-customized toolbar round-trips through TOML unchanged.
        let mut custom = Config::default();
        custom.toolbar.left = vec!["view_mode".into()];
        custom.toolbar.right = vec!["script_launcher".into(), "shell_switcher".into()];
        custom.toolbar.menu = vec!["equalize_panes".into()];
        custom.toolbar.show_overflow = false;
        let toml = toml::to_string(&custom).expect("serialize");
        let back = Config::from_toml(&toml, &p).unwrap();
        assert_eq!(back.toolbar, custom.toolbar);
    }

    #[cfg(unix)]
    #[test]
    fn save_to_writes_owner_only_0600_no_race() {
        // Audit P3-#2: the saved config must be owner-only (0600) — and because
        // it is created via atomic_write_owner_only, the file NEVER exists in a
        // world-readable default-perms state (the perms are applied to the temp
        // file before the rename, not after the content already exists).
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("c0pl4nd-cfg-{}-perms.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Config::default().save_to(&path).expect("save");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "saved config must be owner-only 0600, got {:o}",
            mode & 0o777
        );
        let _ = std::fs::remove_file(&path);
    }
}
