//! Additive Win32 custom-frame support for the Windows 11 **Snap Layouts** flyout
//! on the SHIPPING egui window (`egui_main`).
//!
//! ## Why this module exists (and where the rest of the fix is)
//!
//! `egui_main.rs` restores `WS_MAXIMIZEBOX` at window creation (see its
//! `ViewportBuilder`), which is what re-enables drag-to-top-maximize / Win+Arrow /
//! Snap Assist. The Windows 11 Snap Layouts **flyout**, however, is triggered by
//! exactly one thing: a `WM_NCHITTEST` that answers `HTMAXBUTTON` over the maximize
//! button. A frameless egui window answers `HTCLIENT` everywhere, so the flyout can
//! never appear no matter how the button is painted.
//!
//! This module installs a window SUBCLASS on the real eframe HWND that:
//!   * calls `DwmDefWindowProc` FIRST for the caption-button message set (per the
//!     Microsoft custom-frame guidance — that call is what renders the flyout),
//!   * answers `HTMAXBUTTON` over the app-published maximize-button rect, and
//!   * handles the resulting non-client mouse messages (egui never sees a click on
//!     an `HTMAXBUTTON` region) by posting `SC_MAXIMIZE` / `SC_RESTORE`.
//!
//! It is **additive** over winit's frame: it deliberately does NOT touch
//! `WM_NCCALCSIZE` (winit owns the frame and already zeroes it), so it never fights
//! winit's frameless composition.
//!
//! ## Coordination with the (un-editable) titlebar layout
//!
//! The maximize-button rect is computed here from the SAME constants
//! `egui_app::chrome.rs` lays the caption cluster out with (a 40px top titlebar
//! panel; the cluster anchored to `content_rect().right() - 8`; 42px-wide /
//! 28px-tall buttons with a 2px gap; laid out right-to-left as
//! `[close, maximize, minimize, gear]`). `chrome.rs` cannot be edited to publish
//! the rect itself, so the geometry is mirrored here and driven every frame from
//! `egui_main`'s begin-pass hook via [`tick`]. See the module tests + the
//! honest-limits note in the crate report: the flyout actually appearing (and
//! whether the restored snap bits re-admit doubled caption buttons on a
//! transparent window) needs a real Windows 11 window to confirm.
//!
//! ## What else this module owns
//!
//! Three further pieces of window chrome the shipping window was missing. Each
//! carries a PORT NOTE at its definition explaining what did and did not
//! transfer from the legacy `win_snap` custom frame:
//!
//!   * **`WM_GETMINMAXINFO` clamp** — a genuine port of `win_snap::clamp_maxinfo`,
//!     minus its `ptMaxTrackSize` write (which would forbid a manual resize
//!     across two monitors) and applied AFTER the chain rather than before it.
//!   * **Titlebar right-click system menu** — NOT a port; `win_snap` never had
//!     one, and the textbook `WM_NCRBUTTONUP`/`HTCAPTION` handler would be
//!     dormant here because this subclass answers `HTCLIENT` everywhere but the
//!     maximize button. It runs off the CLIENT `WM_RBUTTONUP` behind a
//!     two-condition gate instead. Restores the affordance `caption_close`
//!     documents as the cost of stripping `WS_SYSMENU`.
//!   * **Window material** — `DWMWCP_ROUND` corners (as the legacy frame asked
//!     for) plus an explicit `DWMSBT_NONE` backdrop, both through winit's TYPED
//!     `WindowExtWindows` extension rather than raw `DwmSetWindowAttribute`.
//!
//! # THE WINDOW-CHROME COVERAGE BOUNDARY
//!
//! This is the map of where window-chrome coverage STOPS being achievable, and
//! why. It exists so that an untested region is never mistaken for a tested one,
//! and so the next person does not re-derive the same dead end. It spans the
//! whole chrome surface, not just this module, because the boundary does.
//!
//! There are THREE reasons a chrome behaviour is not covered, and they are not
//! interchangeable — the third is the one that hides.
//!
//! ## Tier 1 — driven headlessly today (no boundary here)
//!
//! Every DECISION this module makes is extracted into a pure function and
//! exhaustively unit-tested on every host: `hit_code`, `sc_for_toggle`,
//! `clamped_max_info`, `caption_menu_allowed`, `right_button_action`,
//! `system_menu_item_states`, `desired_window_material`,
//! `titlebar_strip_bottom_px`, `maximize_button_logical_rect`,
//! `logical_rect_to_physical`, `split_lparam`. The message-routing tables are
//! asserted, and the Windows-only constant pins tie `HT_*`/`SC_*` to the
//! `windows` crate.
//!
//! [`tick`]'s publish half runs against a REAL (headless) `egui::Context` —
//! `tick_publishes_both_the_maximize_rect_and_the_caption_gate` and
//! `the_gate_opens_over_inert_titlebar_space_and_closes_over_a_widget` — because
//! it reads only `content_rect()` and `pixels_per_point()`, and those are exactly
//! the two things `egui_kittest` DOES supply (see Tier 3). Outside this module,
//! the caption-cluster clicks, the F11/Esc/palette fullscreen surfaces, the
//! titlebar layout, and the accessible names are all driven through the real
//! `frame_tick` by the `egui_kittest` suites.
//!
//! ## Tier 2 — needs a real OS window (the honest limits)
//!
//! A real HWND and an OS message pump. Cross-references elsewhere in the crate
//! to "the honest-limits note in `win_chrome`'s module docs" mean this section.
//!
//! Everything inside `imp::subclass_proc` needs a real HWND and an OS message
//! pump, so it cannot be driven headlessly. The pure decisions above ARE tested —
//! but the arms that call them, `show_system_menu`'s `TrackPopupMenu` round-trip,
//! and whether `set_corner_preference` / `set_system_backdrop` actually change how
//! DWM draws the window all need a real Windows 11 window to confirm. So does
//! whether `GetSystemMenu` still returns a live handle after `caption_close` has
//! cleared `WS_SYSMENU` — the code treats an invalid handle as a quiet decline
//! either way, which is the only outcome that is safe under both answers. Also
//! here: the `WM_GETMINMAXINFO` clamp being honoured by the OS, the
//! `SC_MAXIMIZE`/`SC_RESTORE` post taking effect, and `win_foreground`'s
//! `AttachThreadInput` raise (which is doubly out of reach headlessly — its call
//! site is gated on `self.live_window`, and `C0pl4ndApp::bootstrap()` leaves that
//! false).
//!
//! A SUBSET of Tier 2 is not merely un-automated but genuinely un-assertable:
//! only a human on a real Windows 11 desktop can confirm that the Snap Layouts
//! flyout actually renders over the maximize button, that the rounded corners and
//! the suppressed backdrop look right, that the restored snap bits do not
//! re-admit a doubled native caption button on a transparent window, and that
//! `StartDrag` moves the window. Those are eyes-on checks by construction; no
//! harness makes them green. `docs/control-test-ledger.md` marks that class 🟡.
//!
//! ## Tier 3 — unreachable because the harness never publishes the state
//!
//! This is the category that hides, because the code compiles, the test runs, and
//! nothing is red — the branch simply never executes.
//!
//! `egui_kittest` (0.34.3, `src/lib.rs:130-135`) seeds exactly two things into
//! `RawInput`: `screen_rect`, and `viewports[ROOT].native_pixels_per_point`.
//! **Every other `ViewportInfo` field stays at `Default` — i.e. `None`.** So any
//! branch guarded by `if let Some(x) = i.viewport().x` never matches, and any
//! `i.viewport().x.unwrap_or(fallback)` is pinned to the fallback forever. The
//! affected chrome branches, with what it would take to reach each:
//!
//! | field | what is dead without it | to reach it |
//! |---|---|---|
//! | `fullscreen` | the OS-reconcile branch (`mod.rs`, `frame_tick`) | DONE — see the worked example below |
//! | `maximized` | the "restore" glyph + its `"restore"` accessible label (`chrome.rs`); titlebar double-click; the caption `◻`; and therefore the ENTIRE restore arm of `toggle_maximize` (`Maximized(false)` + `InnerSize`) | publish `Some(true)` and drive the `◻` |
//! | `monitor_size` | the re-centre-on-restore `OuterPosition` inside `toggle_maximize` | publish it AND `maximized` (it is downstream) |
//! | `outer_rect` | 5 of the 8 frameless-resize directions — anything with a west or north component (`West`, `North`, `NorthWest`, `NorthEast`, `SouthWest`) returns early rather than let the window drift. `East`/`South`/`SouthEast` do run | publish an outer rect and drag an edge |
//! | `inner_rect` | the `restore_size` capture | publish it AND drive `ui()` (see below) |
//! | `focused` | the DEC ?1004 focus-in/out edge that tells the pane's program | publish it and flip it across a frame |
//!
//! Note the resize row in particular: the apply half is NOT an OS-level operation
//! (the shipping code sends `InnerSize`/`OuterPosition` itself — `BeginResize` was
//! removed because it hung the window), so it is not Tier 2. It is pure Tier 3:
//! headless-testable in principle, blocked only by an unpublished field.
//!
//! ### The worked example: `viewport().fullscreen`
//!
//! `frame_tick` mirrors the OS fullscreen state back each frame, skipped on a
//! frame that just commanded a change. Because `egui_kittest` leaves
//! `viewports[ROOT].fullscreen` at `None`, `if let Some(os)` had NEVER matched in
//! any test in this repo — the guard was reviewed as "plausible but unproven"
//! precisely because no test could say anything about it.
//!
//! `crates/app/tests/egui_fullscreen_paths.rs`'s `FakeOs` is what armed it, and
//! the SHAPE of that fix is the reusable part: it publishes the field from what
//! the app COMMANDED (the emitted `ViewportCommand::Fullscreen`), never from the
//! app's own `self.fullscreen` mirror. Reading the mirror back would make the
//! test agree with itself and prove nothing. Anything that arms a Tier-3 branch
//! must derive its published value from an independent source the same way.
//!
//! ### The second, independent axis: `fn ui` is never entered
//!
//! Every `egui_kittest` harness in this crate is built as
//! `Harness::new(|ctx| app.frame_tick(ctx))`, which enters at `frame_tick` and
//! BYPASSES `impl eframe::App::ui` entirely. The only tests that go through `ui`
//! are the `build_eframe` ones, and all of them are `#[ignore]`d (they need a real
//! GPU). So the work `ui` does before it calls `frame_tick` —
//! `caption_close::ensure_close_button_stripped()` and the `restore_size` capture —
//! is unexecuted by a default `cargo test`, independently of Tier 3.
//!
//! ## A fourth trap: covered ACTION, uncovered EFFECT
//!
//! The caption `—` and `◻` clicks are driven by real `egui_kittest` tests, but
//! those tests assert `C0pl4ndApp::last_window_cmd()`, which `frame_tick` sets
//! BEFORE it calls `send_viewport_cmd`. No test in this crate asserts the emitted
//! `ViewportCommand::Minimized`/`Maximized` at all (only `RequestUserAttention`
//! and `Fullscreen` are ever read back out of `viewport_output`). That is
//! measured, not inferred: deleting the `send_viewport_cmd` for minimize outright
//! leaves `clicking_minimize_caption_issues_a_minimize_command` passing. The click
//! is proven; the OS command it is supposed to issue is not. Read the ledger's
//! caption rows with that in mind — the boundary there is assertion strength, not
//! the harness, and it is reachable: assert `h.output().viewport_output`.
//!
//! ## unsafe
//!
//! `egui_main` is `#![deny(unsafe_code)]`; the audited Win32 FFI is quarantined
//! here — this module opts back in with a scoped `#![allow(unsafe_code)]`, exactly
//! like `dll_hardening` / `win_foreground` / `caption_close`. The PURE geometry +
//! hit classification lives OUTSIDE the `#[cfg(windows)]` FFI and is unit-tested on
//! every host.
//!
//! ## Escape hatch
//!
//! Setting `C0PL4ND_DISABLE_SNAP_CHROME` (to any value) makes [`tick`] a no-op:
//! the subclass is never installed and no rect is published, so the window keeps
//! winit's plain frame behaviour (drag-to-top / Win+Arrow still work via the
//! restored `WS_MAXIMIZEBOX`; only the flyout's `HTMAXBUTTON` reply is withheld).
#![allow(unsafe_code)]
// Off Windows the `#[cfg(windows)] mod imp` FFI half is not compiled, so nothing
// in the PRODUCTION build calls this module's pure logic — the unit tests do, and
// they run on every host, but clippy also lints the non-test build and reports
// every item here as dead. That is a property of the platform, not a dormancy
// bug: on Windows the lint is FULLY ACTIVE, so a genuinely-unwired item is still
// caught on the platform where it must be wired. Scoped to `not(windows)` rather
// than a blanket allow for exactly that reason.
#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

// ---------------------------------------------------------------------------
// PURE geometry + hit classification (compiles + tests on every host)
// ---------------------------------------------------------------------------

/// The `WM_NCHITTEST` reply for ordinary client area (`HTCLIENT`). Spelled out to
/// stay platform-independent; a windows-only test asserts it equals the
/// `windows`-crate constant.
const HT_CLIENT: isize = 1;
/// The `WM_NCHITTEST` reply that triggers the Windows 11 Snap Layouts flyout
/// (`HTMAXBUTTON`). A windows-only test asserts it equals `HTMAXBUTTON`.
const HT_MAXBUTTON: isize = 9;

/// `SC_MAXIMIZE` — posted as `WM_SYSCOMMAND` when the (restored) window's maximize
/// button is clicked. A windows-only test asserts the value.
const SC_MAXIMIZE_CMD: u32 = 0xF030;
/// `SC_RESTORE` — posted when an already-maximized window's button is clicked.
const SC_RESTORE_CMD: u32 = 0xF120;
/// `SC_SIZE` — the system menu's "Size" verb. A windows-only test asserts it.
const SC_SIZE_CMD: u32 = 0xF000;
/// `SC_MOVE` — the system menu's "Move" verb.
const SC_MOVE_CMD: u32 = 0xF010;
/// `SC_MINIMIZE` — the system menu's "Minimize" verb.
const SC_MINIMIZE_CMD: u32 = 0xF020;
/// `SC_CLOSE` — the system menu's "Close" verb (also its DEFAULT item).
const SC_CLOSE_CMD: u32 = 0xF060;

// The caption-cluster layout constants, mirrored from `egui_app::chrome.rs`
// (which this module cannot edit). See the module docs for the layout.
/// Right inset of the caption cluster from the window content edge.
const RIGHT_INSET: f32 = 8.0;
/// Caption-button width.
const CAPTION_BTN_W: f32 = 42.0;
/// Caption-button height.
const CAPTION_BTN_H: f32 = 28.0;
/// Gap between caption buttons.
const CAPTION_GAP: f32 = 2.0;
/// The fixed titlebar panel height (`TopBottomPanel::top("titlebar").exact_height`).
const TITLEBAR_HEIGHT: f32 = 40.0;

/// A physical-pixel rectangle in client coordinates (left/top inclusive,
/// right/bottom exclusive — the standard Win32 `RECT` convention).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RectPx {
    /// Left edge (inclusive).
    pub left: i32,
    /// Top edge (inclusive).
    pub top: i32,
    /// Right edge (exclusive).
    pub right: i32,
    /// Bottom edge (exclusive).
    pub bottom: i32,
}

impl RectPx {
    /// Construct a rect from its four edges.
    #[must_use]
    const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Whether `(x, y)` is inside the rect (left/top inclusive, right/bottom
    /// exclusive).
    #[must_use]
    const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// A rect with no area is never hit — guards a stale/zeroed published rect.
    #[must_use]
    const fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// The maximize/restore caption-button rect in LOGICAL (egui-point) coordinates,
/// derived from the window content width the same way `chrome.rs` lays the caption
/// cluster out. Returns `(left, top, right, bottom)`.
///
/// The cluster is anchored to `content_right - RIGHT_INSET` and laid out
/// right-to-left as `[close, maximize, …]`, so the maximize button is exactly one
/// button-slot (`CAPTION_BTN_W + CAPTION_GAP`) left of the content edge inset. The
/// button is vertically centred in the fixed titlebar panel.
#[must_use]
fn maximize_button_logical_rect(content_right: f32) -> (f32, f32, f32, f32) {
    let right_edge = content_right - RIGHT_INSET;
    // One slot left of the close button (index 1 in the right-to-left cluster).
    let btn_right = right_edge - (CAPTION_BTN_W + CAPTION_GAP);
    let btn_left = btn_right - CAPTION_BTN_W;
    let cy = TITLEBAR_HEIGHT / 2.0;
    let top = cy - CAPTION_BTN_H / 2.0;
    let bottom = cy + CAPTION_BTN_H / 2.0;
    (btn_left, top, btn_right, bottom)
}

/// Convert a LOGICAL (egui-point) rect to physical client pixels at `scale`
/// (`pixels_per_point`). Win32 hit-testing is in physical device pixels, so a
/// window on a 150% display that published raw logical coordinates would claim a
/// rect a third of the size in the wrong place. Rounded (not truncated) so a
/// 1.25/1.5 scale lands on the pixel the renderer painted. A non-finite or
/// non-positive scale yields an EMPTY rect (never hit) rather than garbage.
#[must_use]
fn logical_rect_to_physical(left: f32, top: f32, right: f32, bottom: f32, scale: f32) -> RectPx {
    if !scale.is_finite() || scale <= 0.0 {
        return RectPx::default();
    }
    let px = |v: f32| -> i32 {
        if v.is_finite() {
            (v * scale).round() as i32
        } else {
            0
        }
    };
    RectPx::new(px(left), px(top), px(right), px(bottom))
}

/// Split a `WM_NCHITTEST`-style packed `LPARAM` into SIGNED screen coordinates.
/// The halves are signed 16-bit: a window on a monitor left of / above the primary
/// has negative screen coordinates, and a naive `as u16` would wrap `-8` into
/// `65528` and miss every hit on that monitor.
#[cfg(any(windows, test))]
#[must_use]
const fn split_lparam(lparam: isize) -> (i32, i32) {
    let x = (lparam & 0xFFFF) as u16 as i16 as i32;
    let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
    (x, y)
}

/// Classify a client-space point against the published maximize-button rect. This
/// is the "MaximizeButtonOnly" policy: the rect answers `HTMAXBUTTON`, everything
/// else answers `HTCLIENT`, so egui keeps drag + resize everywhere else.
#[cfg(any(windows, test))]
#[must_use]
fn hit_code(button: Option<RectPx>, x: i32, y: i32) -> isize {
    match button {
        Some(rect) if !rect.is_empty() && rect.contains(x, y) => HT_MAXBUTTON,
        _ => HT_CLIENT,
    }
}

/// The `SC_*` command to post for a maximize-button click, given the current
/// maximized state (maximize when restored, restore when maximized).
#[cfg(any(windows, test))]
#[must_use]
const fn sc_for_toggle(is_maximized: bool) -> u32 {
    if is_maximized {
        SC_RESTORE_CMD
    } else {
        SC_MAXIMIZE_CMD
    }
}

// ---------------------------------------------------------------------------
// `WM_GETMINMAXINFO` clamp (ported from the legacy `win_snap` custom frame)
// ---------------------------------------------------------------------------
//
// PORT NOTE — what changed vs `win_snap::clamp_maxinfo`, and why.
//
// The legacy binary CREATED its snap-able frame by OR-ing `WS_THICKFRAME |
// WS_CAPTION` onto an undecorated window, which makes `DefWindowProc` compute a
// maximized window rect that is the work area GROWN by the (now invisible)
// frame — the classic borderless-window "maximize covers the taskbar" bug. The
// shipping egui window reaches the same style set a different way (winit's base
// style is already `WS_CAPTION | WS_BORDER | WS_SIZEBOX | WS_MAXIMIZEBOX`), so
// the same default applies here.
//
// Two things differ from the legacy context and had to be re-derived rather
// than assumed:
//
//  * **winit already half-covers this.** winit's own `WM_NCCALCSIZE` handler
//    clamps the CLIENT rect to `rcWork` when the window is maximized, so the
//    visible client is usually already correct. That clamp FAILS OPEN, though:
//    it is skipped when `MonitorFromRect(.., MONITOR_DEFAULTTONULL)` yields no
//    monitor or `GetMonitorInfoW` fails, and it never corrects the WINDOW rect
//    that DWM/Snap-Layouts geometry is derived from. Clamping
//    `ptMaxSize`/`ptMaxPosition` here makes the window rect itself honest and is
//    the defence-in-depth for winit's fail-open path.
//  * **`ptMaxTrackSize` is deliberately NOT ported.** The legacy code also
//    clamped the max TRACK size to one monitor's work area. Track size bounds a
//    user DRAG-resize, not the maximize, so carrying it over would forbid
//    manually resizing the window across two monitors. Only the two fields that
//    actually place the maximized window are written.
//
// winit's `WM_GETMINMAXINFO` arm returns 0 WITHOUT calling `DefWindowProc` and
// writes only `ptMinTrackSize`, so this clamp runs AFTER `DefSubclassProc` (see
// the subclass proc) and cannot be overwritten by anything below us.

/// The two `MINMAXINFO` fields that place a maximized window, in the
/// monitor-relative coordinates the message expects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MaxInfo {
    /// `ptMaxPosition.x` — the maximized left edge, RELATIVE to the monitor.
    pub pos_x: i32,
    /// `ptMaxPosition.y` — the maximized top edge, RELATIVE to the monitor.
    pub pos_y: i32,
    /// `ptMaxSize.x` — the maximized width (work-area width).
    pub size_x: i32,
    /// `ptMaxSize.y` — the maximized height (work-area height).
    pub size_y: i32,
}

/// The maximized placement for a monitor whose full bounds are `monitor` and
/// whose taskbar-excluded work area is `work`.
///
/// `ptMaxPosition` is documented as relative to the MONITOR origin, not the
/// desktop, which is why the monitor rect is subtracted — a secondary monitor at
/// a negative virtual-desktop origin would otherwise be placed a whole screen
/// away. Returns `None` (write nothing, keep the OS defaults) for an empty or
/// inverted work rect, so a failed `GetMonitorInfoW` can never collapse the
/// window to a zero-size maximize.
#[cfg(any(windows, test))]
#[must_use]
fn clamped_max_info(work: RectPx, monitor: RectPx) -> Option<MaxInfo> {
    if work.is_empty() {
        return None;
    }
    Some(MaxInfo {
        pos_x: work.left - monitor.left,
        pos_y: work.top - monitor.top,
        size_x: work.right - work.left,
        size_y: work.bottom - work.top,
    })
}

// ---------------------------------------------------------------------------
// Titlebar right-click system menu
// ---------------------------------------------------------------------------
//
// PORT NOTE — this is NOT a move; `win_snap` never had a system menu.
//
// `caption_close.rs` strips `WS_SYSMENU` every frame to stop DWM compositing a
// second native "×" over the custom titlebar, and records the cost in its own
// docs: "Clearing WS_SYSMENU removes the in-window system menu (Alt+Space, and
// the title-bar right-click menu)". This restores the right-click half of that
// affordance explicitly.
//
// It CANNOT be done the textbook way. The textbook handler answers
// `WM_NCRBUTTONUP` with `wParam == HTCAPTION`, but this subclass answers
// `HTCLIENT` everywhere except the maximize button (that is the whole point of
// the MaximizeButtonOnly policy — egui must keep receiving titlebar clicks), so
// no `WM_NCRBUTTON*` message is ever generated over the titlebar. A ported
// non-client handler would be dormant code that can never fire.
//
// The reachable message is the CLIENT `WM_RBUTTONUP`, and the danger there is
// stealing a right-click egui wants (pane context menu, tab strip, …). The gate
// is therefore two independent conditions that must BOTH hold:
//
//  1. message-time: the click's client-space Y is inside the published titlebar
//     strip, and
//  2. frame-time: egui reported it did not want the pointer on the last pass
//     (`Context::wants_pointer_input()` is false — no widget is hovered or
//     being interacted with).
//
// Condition 2 is what makes this safe without mirroring `chrome.rs`'s widget
// layout: every interactive titlebar control (the wordmark drag-handle, the tab
// chips, `+`, the caption cluster) is an egui widget, so hovering one sets the
// flag and the menu is declined. The flag defaults to "egui wants it", so a
// right-click that arrives before the first publish is never stolen.

/// The titlebar strip's bottom edge in physical client pixels at `ppp`. A
/// non-finite or non-positive scale yields `0`, which the gate treats as "no
/// strip published" (never a claim), matching [`logical_rect_to_physical`].
#[must_use]
fn titlebar_strip_bottom_px(ppp: f32) -> i32 {
    if !ppp.is_finite() || ppp <= 0.0 {
        return 0;
    }
    (TITLEBAR_HEIGHT * ppp).round() as i32
}

/// Whether a client-space right-click at `y` should open the window system menu.
///
/// Both gates must pass: the point is inside the published titlebar strip
/// (`0 <= y < strip_bottom`, and a zero/negative `strip_bottom` means nothing is
/// published) AND egui did not want the pointer on the last frame.
#[cfg(any(windows, test))]
#[must_use]
const fn caption_menu_allowed(y: i32, strip_bottom: i32, egui_owns_pointer: bool) -> bool {
    strip_bottom > 0 && y >= 0 && y < strip_bottom && !egui_owns_pointer
}

/// What the subclass should do with a CLIENT right-button message.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RightButtonAction {
    /// Not ours — hand it down the chain so egui receives it unchanged.
    Forward,
    /// Swallow the press and remember it; the menu opens on the release.
    Latch,
    /// Swallow the release and open the system menu.
    OpenMenu,
    /// Swallow the release WITHOUT opening: we swallowed the matching press, so
    /// delivering a lone release would leave egui's pointer state inconsistent.
    Swallow,
}

/// Resolve a client right-button message against the caption-menu gate and the
/// press latch.
///
/// Split out of the subclass proc — whose only impure steps are reading the
/// published gate and calling Win32 — so the press/release state machine is
/// exhaustively unit-testable without a live HWND. The two behaviours that are
/// easy to get wrong and cannot be observed headlessly are pinned here: a press
/// the gate declined is FORWARDED (egui keeps its right-click), and a release
/// whose press we swallowed is ALWAYS swallowed, even if the pointer has since
/// moved onto a widget.
#[cfg(any(windows, test))]
#[must_use]
const fn right_button_action(is_down: bool, gate_open: bool, latched: bool) -> RightButtonAction {
    match (is_down, gate_open, latched) {
        (true, true, _) => RightButtonAction::Latch,
        (true, false, _) => RightButtonAction::Forward,
        (false, _, false) => RightButtonAction::Forward,
        (false, true, true) => RightButtonAction::OpenMenu,
        (false, false, true) => RightButtonAction::Swallow,
    }
}

/// The enable/disable state each system-menu verb must carry for the current
/// maximized state, as `(SC_* command, enabled)`.
///
/// `GetSystemMenu` hands back the menu with whatever states it last had;
/// `DefWindowProc` normally fixes them up on `WM_INITMENUPOPUP`, which never
/// runs for a menu we track ourselves. Without this, a maximized window would
/// offer "Maximize" and a restored one would offer "Restore" — both no-ops that
/// read as a broken menu.
#[cfg(any(windows, test))]
#[must_use]
const fn system_menu_item_states(is_maximized: bool) -> [(u32, bool); 6] {
    [
        (SC_RESTORE_CMD, is_maximized),
        (SC_MOVE_CMD, !is_maximized),
        (SC_SIZE_CMD, !is_maximized),
        (SC_MINIMIZE_CMD, true),
        (SC_MAXIMIZE_CMD, !is_maximized),
        (SC_CLOSE_CMD, true),
    ]
}

// ---------------------------------------------------------------------------
// Window material: rounded corners + system backdrop
// ---------------------------------------------------------------------------

/// The Windows 11 corner treatment to request. Mirrors winit's
/// `platform::windows::CornerPreference`; a windows-only test asserts the
/// mapping so this platform-independent enum cannot drift from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CornerStyle {
    /// Round the corners if the OS considers it appropriate (`DWMWCP_ROUND`).
    Round,
    /// Never round (`DWMWCP_DONOTROUND`).
    ///
    /// [`desired_window_material`] never yields this today — it exists as the
    /// NEGATIVE case of the winit mapping test, which is what proves
    /// `to_winit` is a real map and not a constant function answering `Round`
    /// for everything. Same pattern (and same reasoning) as `SUBCLASS_ID`
    /// below: the allow is scoped to `not(test)` rather than blanket, so if
    /// that assertion is ever deleted the dead-code lint fires on this
    /// leftover instead of it rotting silently.
    #[cfg_attr(not(test), allow(dead_code))]
    Square,
}

/// The DWM system backdrop material to request. Mirrors winit's
/// `platform::windows::BackdropType`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackdropStyle {
    /// Explicitly draw NO system material (`DWMSBT_NONE`).
    None,
    /// The Background Acrylic material (`DWMSBT_TRANSIENTWINDOW`).
    Acrylic,
}

/// The corner + backdrop pair the shipping window asks the OS for.
///
/// **Corners: always `Round`.** The window is frameless, so nothing else rounds
/// it; the legacy `win_snap::install` asked for the same `DWMWCP_ROUND` through
/// a raw `DwmSetWindowAttribute` call. Here it goes through winit's typed
/// extension instead, so the binary's `#![deny(unsafe_code)]` holds and the
/// attribute id/size marshalling is winit's problem, not ours.
///
/// **Backdrop: `None` when the window composites its own transparency.** This is
/// the one place the legacy context genuinely does not transfer. C0PL4ND clears
/// to `[0,0,0,0]` and lets the `opacity` slider drive every panel's alpha
/// (`window_effects.rs`), which is the ONLY effect that composites on the
/// hybrid-GPU target — the reason `window_mode`/`acrylic` were removed from the
/// config. Leaving the backdrop at its `Auto` default lets DWM pick a material
/// and paint it UNDER our transparent surface, so the slider stops reaching the
/// desktop. `DWMSBT_NONE` declines the material explicitly and hands the alpha
/// budget back to the app. An opaque window has no such conflict and takes the
/// acrylic material.
#[must_use]
pub fn desired_window_material(transparent: bool) -> (CornerStyle, BackdropStyle) {
    let backdrop = if transparent {
        BackdropStyle::None
    } else {
        BackdropStyle::Acrylic
    };
    (CornerStyle::Round, backdrop)
}

// ---------------------------------------------------------------------------
// Published maximize-button rect (physical client pixels) — all platforms
// ---------------------------------------------------------------------------
//
// Four independent atomics rather than a lock: the rect is read from inside the
// window procedure, where taking a `Mutex` risks a re-entrant / cross-thread
// stall on a hot OS callback. A torn read during a resize can at worst mis-answer
// one `WM_NCHITTEST` by a few pixels and self-corrects on the next frame's
// publish; it can never claim a region outside the union of the old and new rects.

static BTN_LEFT: AtomicI32 = AtomicI32::new(0);
static BTN_TOP: AtomicI32 = AtomicI32::new(0);
static BTN_RIGHT: AtomicI32 = AtomicI32::new(0);
static BTN_BOTTOM: AtomicI32 = AtomicI32::new(0);

/// The titlebar strip's bottom edge (physical client px). `0` = nothing
/// published yet, which the caption-menu gate reads as "no strip".
static STRIP_BOTTOM: AtomicI32 = AtomicI32::new(0);

/// Whether egui wanted the pointer on the last completed pass. Starts `true`
/// (FAIL-SAFE: before the first publish we assume egui owns every click, so a
/// right-click can never be stolen by an unprimed gate).
static EGUI_OWNS_POINTER: AtomicBool = AtomicBool::new(true);

/// Compute the maximize-button rect for `content_right` logical points at `ppp`
/// device scale and publish it (physical client pixels) for the subclass proc.
/// Pure aside from the atomic store, so it is directly unit-testable.
pub fn publish_from_geometry(content_right: f32, ppp: f32) {
    let (l, t, r, b) = maximize_button_logical_rect(content_right);
    let rect = logical_rect_to_physical(l, t, r, b, ppp);
    BTN_LEFT.store(rect.left, Ordering::Relaxed);
    BTN_TOP.store(rect.top, Ordering::Relaxed);
    BTN_RIGHT.store(rect.right, Ordering::Relaxed);
    BTN_BOTTOM.store(rect.bottom, Ordering::Relaxed);
}

/// Publish the titlebar strip depth (physical client px) for the caption-menu
/// gate, and whether egui OWNED the pointer on this pass. Pure aside from the
/// atomic stores, so the publish is directly unit-testable.
pub fn publish_caption_menu_gate(ppp: f32, egui_owns_pointer: bool) {
    STRIP_BOTTOM.store(titlebar_strip_bottom_px(ppp), Ordering::Relaxed);
    EGUI_OWNS_POINTER.store(egui_owns_pointer, Ordering::Relaxed);
}

/// Whether a client-space right-click at `y` should open the system menu, read
/// from the published gate. Read by the subclass proc (Windows) and the tests.
#[cfg(any(windows, test))]
#[must_use]
pub fn caption_menu_gate_open(y: i32) -> bool {
    caption_menu_allowed(
        y,
        STRIP_BOTTOM.load(Ordering::Relaxed),
        EGUI_OWNS_POINTER.load(Ordering::Relaxed),
    )
}

/// The currently-published maximize-button rect, or `None` when nothing usable is
/// published (an empty/inverted rect is treated as absent, never as a claim). Read
/// by the subclass proc (Windows) and the tests.
#[cfg(any(windows, test))]
#[must_use]
pub fn published_rect() -> Option<RectPx> {
    let r = RectPx::new(
        BTN_LEFT.load(Ordering::Relaxed),
        BTN_TOP.load(Ordering::Relaxed),
        BTN_RIGHT.load(Ordering::Relaxed),
        BTN_BOTTOM.load(Ordering::Relaxed),
    );
    (!r.is_empty()).then_some(r)
}

/// Whether the Snap-Layouts caption subclass is disabled by the operator escape
/// hatch. Default OFF (the subclass runs); set `C0PL4ND_DISABLE_SNAP_CHROME` to
/// disable it.
#[must_use]
fn disabled() -> bool {
    std::env::var_os("C0PL4ND_DISABLE_SNAP_CHROME").is_some()
}

// ---------------------------------------------------------------------------
// Public entry points — driven from `egui_main`
// ---------------------------------------------------------------------------

/// Prime the module with the real eframe main-window handle (from
/// `CreationContext::window_handle()`), so [`tick`] subclasses the CORRECT window.
/// Idempotent; a zero handle is ignored. A no-op off Windows.
pub fn prime_hwnd(hwnd: isize) {
    #[cfg(windows)]
    imp::prime_hwnd(hwnd);
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// Per-frame tick, called from `egui_main`'s `on_begin_pass` hook: publish the
/// maximize-button rect (tracking window resize + DPI) and, on Windows, install
/// the caption subclass once. A no-op when the escape hatch is set, and off
/// Windows it only publishes the (unread) rect.
pub fn tick(ctx: &eframe::egui::Context) {
    if disabled() {
        return;
    }
    let content = ctx.content_rect();
    let ppp = ctx.pixels_per_point();
    publish_from_geometry(content.right(), ppp);
    // The caption right-click gate, read HERE — in egui-land, on the frame
    // thread — because the subclass proc runs on an OS callback where the egui
    // context must not be touched.
    //
    // NOT `wants_pointer_input()`. That predicate is
    // `is_using_pointer() || is_pointer_over_egui()`, and `is_pointer_over_egui`
    // is TRUE anywhere outside the central panel's available rect — i.e. over the
    // ENTIRE titlebar panel, widget or not. Gating on it would close the gate
    // permanently and leave the system menu as dormant code that can never fire.
    //
    // `interaction_snapshot().contains_pointer` is the strict per-WIDGET set:
    // every widget containing the pointer this pass, regardless of click/drag
    // state. Empty means the pointer is over inert space — which over the
    // titlebar is exactly where a system menu belongs, and everywhere else is
    // still ruled out by the strip check at message time.
    let egui_owns_pointer =
        ctx.egui_is_using_pointer() || ctx.interaction_snapshot(|s| !s.contains_pointer.is_empty());
    publish_caption_menu_gate(ppp, egui_owns_pointer);
    #[cfg(windows)]
    imp::ensure_subclassed();
}

/// Apply the Windows 11 window material — rounded corners + the backdrop
/// decision from [`desired_window_material`] — to the real eframe window.
///
/// Called ONCE from `egui_main`'s creation closure with `cc.winit_window()`.
/// Goes through winit's TYPED `WindowExtWindows` extension rather than a raw
/// `DwmSetWindowAttribute`, so no `unsafe` (and no attribute-id/size
/// marshalling) enters this binary. A no-op when the escape hatch is set, and
/// off Windows.
/// Takes the ALREADY-RESOLVED pair rather than the `transparent` flag so the
/// shipping call site and the test that pins it share one decision helper — a
/// mutated argument at the call site cannot then diverge from what is asserted.
pub fn apply_window_material(
    window: &winit::window::Window,
    (corner, backdrop): (CornerStyle, BackdropStyle),
) {
    if disabled() {
        return;
    }
    #[cfg(windows)]
    {
        use winit::platform::windows::WindowExtWindows;
        window.set_corner_preference(corner.to_winit());
        window.set_system_backdrop(backdrop.to_winit());
    }
    #[cfg(not(windows))]
    {
        let _ = (window, corner, backdrop);
    }
}

#[cfg(windows)]
impl CornerStyle {
    /// The winit `CornerPreference` this style maps to.
    #[must_use]
    fn to_winit(self) -> winit::platform::windows::CornerPreference {
        use winit::platform::windows::CornerPreference;
        match self {
            Self::Round => CornerPreference::Round,
            Self::Square => CornerPreference::DoNotRound,
        }
    }
}

#[cfg(windows)]
impl BackdropStyle {
    /// The winit `BackdropType` this style maps to.
    #[must_use]
    fn to_winit(self) -> winit::platform::windows::BackdropType {
        use winit::platform::windows::BackdropType;
        match self {
            Self::None => BackdropType::None,
            Self::Acrylic => BackdropType::TransientWindow,
        }
    }
}

// ---------------------------------------------------------------------------
// Windows FFI (the audited unsafe boundary)
// ---------------------------------------------------------------------------

/// This module's window-subclass id, re-exported for collision assertions.
///
/// Two subclass entries share one HWND. A COLLIDING id makes the second
/// `SetWindowSubclass` REPLACE this entry instead of chaining, which silently
/// kills the caption/Snap-Layouts handling with no error anywhere. The sibling
/// `quake` installer therefore asserts its own id differs — and it must assert
/// against THIS constant, not a hard-coded copy of it, or the assertion goes
/// stale the moment this value changes and the collision it exists to catch
/// ships undetected.
///
/// Read only by that assertion, so it is genuinely unused in a non-test build —
/// the allow is scoped to `not(test)` rather than blanket, so the dead-code lint
/// still fires if the assertion is ever deleted and this anchor is left behind.
#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SUBCLASS_ID: usize = imp::SUBCLASS_ID;

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Dwm::DwmDefWindowProc;
    use windows::Win32::Graphics::Gdi::{
        ClientToScreen, GetMonitorInfoW, MonitorFromWindow, ScreenToClient, HMONITOR, MONITORINFO,
        MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    };
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnableMenuItem, GetSystemMenu, GetWindowLongPtrW, PostMessageW, SetForegroundWindow,
        SetMenuDefaultItem, TrackPopupMenu, GWL_STYLE, HTMAXBUTTON, MF_BYCOMMAND, MF_ENABLED,
        MF_GRAYED, MINMAXINFO, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_GETMINMAXINFO, WM_NCHITTEST,
        WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCRBUTTONDOWN,
        WM_NCRBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSCOMMAND, WS_MAXIMIZE,
    };

    use super::{
        caption_menu_gate_open, clamped_max_info, hit_code, published_rect, right_button_action,
        sc_for_toggle, split_lparam, system_menu_item_states, RectPx, RightButtonAction,
        SC_CLOSE_CMD,
    };

    /// Cached main-window HWND (0 = not yet primed). C0PL4ND uses ONE OS window.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Set once the subclass is successfully installed (install is one-shot).
    static SUBCLASSED: AtomicBool = AtomicBool::new(false);
    /// Set while a non-client left-press landed on the maximize button, so the
    /// matching button-UP is ours to act on (a press-elsewhere-release-here is not).
    static BTN_PRESSED: AtomicBool = AtomicBool::new(false);
    /// Set while a CLIENT right-press was swallowed by the caption-menu gate, so
    /// the matching release is swallowed too (egui never saw the press, and an
    /// unpaired release would leave its pointer state inconsistent).
    static RBTN_PRESSED: AtomicBool = AtomicBool::new(false);

    /// A stable, arbitrary subclass id for our single subclass entry.
    ///
    /// `pub(super)` so the sibling `quake` subclass installer can assert against
    /// THIS value rather than a copy of it — see the re-export below.
    pub(super) const SUBCLASS_ID: usize = 0x00C0_041D;

    /// The non-client message set handed to `DwmDefWindowProc` FIRST, so DWM can
    /// run its own caption-button behaviour (hover/press visuals and, on Windows
    /// 11, the Snap Layouts flyout) over the region we answer `HTMAXBUTTON` for.
    const DWM_CAPTION_MESSAGES: [u32; 7] = [
        WM_NCHITTEST,
        WM_NCMOUSEMOVE,
        WM_NCMOUSELEAVE,
        WM_NCLBUTTONDOWN,
        WM_NCLBUTTONUP,
        WM_NCRBUTTONDOWN,
        WM_NCRBUTTONUP,
    ];

    /// Prime the cached HWND (see the public wrapper). Stores only a non-zero
    /// value; idempotent.
    pub fn prime_hwnd(hwnd: isize) {
        if hwnd != 0 {
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
    }

    /// Whether the window is currently maximized (its `WS_MAXIMIZE` style is set).
    fn is_maximized(hwnd: HWND) -> bool {
        // SAFETY: `hwnd` is this process's own main window; `GetWindowLongPtrW`
        // only reads this window's style word.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        style & WS_MAXIMIZE.0 != 0
    }

    /// Classify a packed screen-coordinate `LPARAM` into a `WM_NCHITTEST` reply
    /// against the published maximize-button rect (in physical client pixels).
    fn classify(hwnd: HWND, lparam: LPARAM) -> isize {
        let (sx, sy) = split_lparam(lparam.0);
        let mut pt = POINT { x: sx, y: sy };
        // SAFETY: `hwnd` is this process's own window; `pt` is a live local the OS
        // writes back into. A failure returns FALSE and leaves `pt` unusable, so we
        // treat that as plain client area.
        let ok = unsafe { ScreenToClient(hwnd, &mut pt) };
        if !ok.as_bool() {
            return super::HT_CLIENT;
        }
        hit_code(published_rect(), pt.x, pt.y)
    }

    /// Ask for a `WM_NCMOUSELEAVE` so a pointer that leaves the maximize button
    /// clears the pressed latch even if it exits by a path that produces no
    /// further non-client move.
    fn track_nc_mouse_leave(hwnd: HWND) {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: core::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE | TME_NONCLIENT,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: canonical `TrackMouseEvent`; `tme.cbSize` is set and the struct is
        // a live local the OS only reads.
        let _ = unsafe { TrackMouseEvent(&mut tme) };
    }

    /// The caption subclass: answer `WM_NCHITTEST` with `HTMAXBUTTON` over the
    /// published maximize button so Windows 11 offers Snap Layouts, and act on the
    /// resulting non-client clicks (which egui never sees). It deliberately does
    /// NOT touch `WM_NCCALCSIZE` — winit owns the frame.
    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _ref: usize,
    ) -> LRESULT {
        // --- DWM FIRST -------------------------------------------------------
        // The MS custom-frame guidance is explicit that `DwmDefWindowProc` gets
        // the caption-button messages BEFORE the app looks at them — that call is
        // what renders the Snap Layouts flyout and DWM's caption hover/press
        // visuals over our region.
        if DWM_CAPTION_MESSAGES.contains(&msg) {
            let mut dwm_result = LRESULT(0);
            // SAFETY: `hwnd` is this process's own window; `dwm_result` is a live
            // local out-param. Non-zero return means DWM handled the message.
            let handled = unsafe { DwmDefWindowProc(hwnd, msg, wparam, lparam, &mut dwm_result) };
            if handled.as_bool() {
                return dwm_result;
            }
        }

        match msg {
            // --- Snap Layouts: claim the maximize button ---------------------
            WM_NCHITTEST => {
                let code = classify(hwnd, lparam);
                if code != super::HT_CLIENT {
                    return LRESULT(code);
                }
                // Fall through to the default proc for plain client area.
            }
            // --- hover tracking (only to clear the pressed latch on leave) ----
            WM_NCMOUSEMOVE if wparam.0 as u32 == HTMAXBUTTON => {
                track_nc_mouse_leave(hwnd);
            }
            WM_NCMOUSELEAVE => {
                BTN_PRESSED.store(false, Ordering::Relaxed);
            }
            // --- clicks on the claimed region --------------------------------
            // HTMAXBUTTON means egui never sees these, so the button would be dead
            // without this. Swallow the DOWN (so the OS starts no caption drag) and
            // act on the UP, which makes a press-then-drag-away correctly cancel.
            WM_NCLBUTTONDOWN if wparam.0 as u32 == HTMAXBUTTON => {
                BTN_PRESSED.store(true, Ordering::Relaxed);
                return LRESULT(0);
            }
            WM_NCLBUTTONUP if wparam.0 as u32 == HTMAXBUTTON => {
                if BTN_PRESSED.swap(false, Ordering::Relaxed) {
                    let cmd = sc_for_toggle(is_maximized(hwnd));
                    // SAFETY: `hwnd` is this process's own window; posting a standard
                    // `WM_SYSCOMMAND` with an `SC_*` command and no `lParam`.
                    let _ = unsafe {
                        PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0))
                    };
                }
                return LRESULT(0);
            }
            // --- maximized placement: keep the window off the taskbar ---------
            // Run the REST OF THE CHAIN FIRST, then clamp. winit's own
            // `WM_GETMINMAXINFO` arm writes `ptMinTrackSize` and returns without
            // calling `DefWindowProc`; clamping afterwards means nothing below us
            // can overwrite the two fields we set, whatever winit does next.
            WM_GETMINMAXINFO => {
                // SAFETY: forwarding the unmodified message down the subclass chain.
                let r = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
                // SAFETY: `hwnd` is the live window and `lparam` is the OS-provided
                // `MINMAXINFO*` for this message; `clamp_max_placement` null-checks it.
                unsafe { clamp_max_placement(hwnd, lparam) };
                return r;
            }
            // --- titlebar right-click: the window system menu -----------------
            // `lparam` here packs CLIENT-space coordinates (unlike `WM_NCHITTEST`),
            // so the gate reads the click's client Y directly.
            WM_RBUTTONDOWN | WM_RBUTTONUP => {
                let (x, y) = split_lparam(lparam.0);
                let is_down = msg == WM_RBUTTONDOWN;
                // The release CONSUMES the latch (so a second stray release is
                // forwarded); the press only reads it.
                let latched = if is_down {
                    RBTN_PRESSED.load(Ordering::Relaxed)
                } else {
                    RBTN_PRESSED.swap(false, Ordering::Relaxed)
                };
                match right_button_action(is_down, caption_menu_gate_open(y), latched) {
                    RightButtonAction::Forward => {}
                    RightButtonAction::Latch => {
                        RBTN_PRESSED.store(true, Ordering::Relaxed);
                        return LRESULT(0);
                    }
                    RightButtonAction::OpenMenu => {
                        // SAFETY: `hwnd` is this process's own window; the
                        // coordinates are the live message's client-space point.
                        unsafe { show_system_menu(hwnd, x, y) };
                        return LRESULT(0);
                    }
                    RightButtonAction::Swallow => return LRESULT(0),
                }
            }
            _ => {}
        }

        // SAFETY: hand everything we did not claim to the default subclass chain
        // (→ winit → `DefWindowProc`), so winit's frame handling is undisturbed.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// A Win32 `RECT` as this module's platform-independent [`RectPx`].
    fn rect_px(r: RECT) -> RectPx {
        RectPx {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }

    /// Clamp `WM_GETMINMAXINFO`'s maximized placement to the nearest monitor's
    /// WORK area, so a maximized frameless window never extends over the taskbar.
    /// Writes nothing when the monitor query fails or the work rect is unusable —
    /// the OS defaults then stand (which is the behaviour we have today), never a
    /// zero-size maximize.
    unsafe fn clamp_max_placement(hwnd: HWND, lparam: LPARAM) {
        let mmi_ptr = lparam.0 as *mut MINMAXINFO;
        if mmi_ptr.is_null() {
            return;
        }
        // SAFETY: `hwnd` is this process's own window; the call only reads it and
        // returns the nearest-monitor handle.
        let hmon: HMONITOR = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO {
            cbSize: core::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `hmon` is the handle just returned by `MonitorFromWindow`; `&mut
        // mi` is a live local with its `cbSize` set, which the call fills on success.
        if !unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
            return;
        }
        let Some(info) = clamped_max_info(rect_px(mi.rcWork), rect_px(mi.rcMonitor)) else {
            return;
        };
        // SAFETY: for `WM_GETMINMAXINFO` the OS passes `lparam` as a valid, aligned,
        // writable `MINMAXINFO*` (null-checked above) that lives for the duration of
        // the message; reborrowing it as `&mut` is sound and we hold no other alias.
        let mmi = unsafe { &mut *mmi_ptr };
        mmi.ptMaxPosition.x = info.pos_x;
        mmi.ptMaxPosition.y = info.pos_y;
        mmi.ptMaxSize.x = info.size_x;
        mmi.ptMaxSize.y = info.size_y;
    }

    /// Track the window's own system menu at the given CLIENT-space point.
    ///
    /// `caption_close` clears `WS_SYSMENU` every frame, so `GetSystemMenu` may
    /// legitimately hand back an invalid handle; that is a quiet decline (the
    /// right-click simply does nothing), never a swallowed click that pretends to
    /// have opened a menu.
    unsafe fn show_system_menu(hwnd: HWND, client_x: i32, client_y: i32) {
        // SAFETY: `hwnd` is this process's own window. `brevert = false` asks for
        // the window's live menu handle, which the window owns — we never free it.
        let menu = unsafe { GetSystemMenu(hwnd, false) };
        if menu.is_invalid() {
            return;
        }
        // `DefWindowProc` normally fixes the verb states up on `WM_INITMENUPOPUP`,
        // which never runs for a menu we track ourselves — so do it explicitly or
        // a maximized window offers a dead "Maximize".
        for (cmd, enabled) in system_menu_item_states(is_maximized(hwnd)) {
            let flags = if enabled {
                MF_BYCOMMAND | MF_ENABLED
            } else {
                MF_BYCOMMAND | MF_GRAYED
            };
            // SAFETY: `menu` is this window's live system menu; `cmd` is a standard
            // `SC_*` id addressed `MF_BYCOMMAND`.
            let _ = unsafe { EnableMenuItem(menu, cmd, flags) };
        }
        // SAFETY: same live menu; making Close the default matches every other
        // Windows title-bar menu.
        let _ = unsafe { SetMenuDefaultItem(menu, SC_CLOSE_CMD, 0) };

        let mut pt = POINT {
            x: client_x,
            y: client_y,
        };
        // SAFETY: `hwnd` is the live window; `pt` is a live local the OS writes the
        // converted screen coordinates into. `TrackPopupMenu` takes SCREEN space.
        if !unsafe { ClientToScreen(hwnd, &mut pt) }.as_bool() {
            return;
        }
        // Documented `TrackPopupMenu` prerequisite: the owner window must be the
        // foreground window, or the menu does not dismiss on an outside click.
        // SAFETY: `hwnd` is this process's own window, already focused (the user
        // just clicked it).
        let _ = unsafe { SetForegroundWindow(hwnd) };
        // SAFETY: `menu` is this window's live system menu; `hwnd` owns it;
        // `TPM_RETURNCMD` makes the call return the chosen id instead of posting it.
        let chosen = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                None,
                hwnd,
                None,
            )
        };
        if chosen.0 != 0 {
            // SAFETY: `hwnd` is this process's own window; posting a standard
            // `WM_SYSCOMMAND` with the id the menu returned and no `lParam`.
            let _ = unsafe {
                PostMessageW(
                    Some(hwnd),
                    WM_SYSCOMMAND,
                    WPARAM(chosen.0 as usize),
                    LPARAM(0),
                )
            };
        }
    }

    /// Install the caption subclass on the primed HWND exactly once. Early frames
    /// may run before the window is ready; this retries each frame until the
    /// install succeeds, then stops. A no-op before the HWND is primed.
    pub fn ensure_subclassed() {
        let hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 || SUBCLASSED.load(Ordering::Relaxed) {
            return;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        // SAFETY: `h` is this process's own main top-level window handle (primed
        // from eframe's `CreationContext`); this is the canonical comctl32 subclass
        // install with our own static proc and a stable id.
        let ok = unsafe { SetWindowSubclass(h, Some(subclass_proc), SUBCLASS_ID, 0) };
        if ok.as_bool() {
            SUBCLASSED.store(true, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ht_and_sc_constants_match_the_windows_crate() {
            // `hit_test.rs`-style drift guard: the pure `HT_*` / `SC_*` literals used
            // by the classifier must equal the real Win32 constants, or the subclass
            // would answer the wrong non-client code.
            assert_eq!(
                super::super::HT_MAXBUTTON,
                HTMAXBUTTON as isize,
                "HTMAXBUTTON"
            );
            use windows::Win32::UI::WindowsAndMessaging::{SC_MAXIMIZE, SC_RESTORE};
            assert_eq!(super::super::SC_MAXIMIZE_CMD, SC_MAXIMIZE, "SC_MAXIMIZE");
            assert_eq!(super::super::SC_RESTORE_CMD, SC_RESTORE, "SC_RESTORE");
        }

        #[test]
        fn system_menu_sc_constants_match_the_windows_crate() {
            // The system-menu verbs are posted BACK to the window as `WM_SYSCOMMAND`
            // ids, so a wrong literal would silently invoke the wrong verb (or
            // nothing). Same drift guard as the caption constants above.
            use windows::Win32::UI::WindowsAndMessaging::{
                SC_CLOSE, SC_MINIMIZE, SC_MOVE, SC_SIZE,
            };
            assert_eq!(super::super::SC_SIZE_CMD, SC_SIZE, "SC_SIZE");
            assert_eq!(super::super::SC_MOVE_CMD, SC_MOVE, "SC_MOVE");
            assert_eq!(super::super::SC_MINIMIZE_CMD, SC_MINIMIZE, "SC_MINIMIZE");
            assert_eq!(super::super::SC_CLOSE_CMD, SC_CLOSE, "SC_CLOSE");
            // MF_ENABLED / MF_GRAYED must be DISTINCT, or `system_menu_item_states`
            // would compute a state the menu cannot express (both are 0-valued
            // families in the Win32 headers, which is exactly the trap).
            assert_ne!(MF_ENABLED.0, MF_GRAYED.0, "enable and gray must differ");
            assert_eq!(MF_BYCOMMAND.0, 0, "MF_BYCOMMAND must not set a state bit");
        }

        #[test]
        fn the_new_arms_are_not_routed_through_dwm_first() {
            // `DwmDefWindowProc` is for the CAPTION-BUTTON message set only. Handing
            // it `WM_GETMINMAXINFO` or a CLIENT mouse message is outside its
            // contract and would let DWM answer before our clamp / caption-menu
            // gate ever ran.
            for msg in [WM_GETMINMAXINFO, WM_RBUTTONDOWN, WM_RBUTTONUP] {
                assert!(
                    !DWM_CAPTION_MESSAGES.contains(&msg),
                    "message {msg:#x} must NOT be pre-routed to DwmDefWindowProc"
                );
            }
        }

        #[test]
        fn window_material_maps_onto_the_documented_dwm_values() {
            // The platform-independent `CornerStyle`/`BackdropStyle` exist so the
            // decision is testable off Windows; this asserts the mapping lands on
            // the winit variants whose discriminants ARE the documented
            // `DWMWCP_*` / `DWMSBT_*` values. A silent re-map (e.g. Round →
            // DoNotRound) is otherwise invisible until someone looks at a window.
            use winit::platform::windows::{BackdropType, CornerPreference};
            assert_eq!(
                super::super::CornerStyle::Round.to_winit(),
                CornerPreference::Round
            );
            assert_eq!(
                super::super::CornerStyle::Square.to_winit(),
                CornerPreference::DoNotRound
            );
            assert_eq!(CornerPreference::Round as i32, 2, "DWMWCP_ROUND");
            assert_eq!(CornerPreference::DoNotRound as i32, 1, "DWMWCP_DONOTROUND");
            assert_eq!(
                super::super::BackdropStyle::None.to_winit(),
                BackdropType::None
            );
            assert_eq!(
                super::super::BackdropStyle::Acrylic.to_winit(),
                BackdropType::TransientWindow
            );
            assert_eq!(BackdropType::None as i32, 1, "DWMSBT_NONE");
            assert_eq!(
                BackdropType::TransientWindow as i32,
                3,
                "DWMSBT_TRANSIENTWINDOW"
            );
            // `Auto` is what we are deliberately NOT leaving the window on — assert
            // it is a distinct value so the decline is a real change of state.
            assert_ne!(BackdropType::None as i32, BackdropType::Auto as i32);
        }

        #[test]
        fn dwm_caption_message_set_covers_the_flyout_messages() {
            // The flyout is rendered by DWM, so DwmDefWindowProc must see these
            // FIRST. This asserts the routing TABLE; the ordering inside the proc is
            // a code-structure invariant not reachable without a real HWND.
            for msg in [
                WM_NCHITTEST,
                WM_NCMOUSEMOVE,
                WM_NCMOUSELEAVE,
                WM_NCLBUTTONDOWN,
                WM_NCLBUTTONUP,
            ] {
                assert!(
                    DWM_CAPTION_MESSAGES.contains(&msg),
                    "message {msg:#x} must route to DwmDefWindowProc first"
                );
            }
        }

        #[test]
        fn a_zero_handle_never_primes_and_unprimed_ensure_is_a_noop() {
            // `CACHED_HWND` is a process-global no test primes with a real handle
            // (a fabricated HWND would violate every SAFETY precondition below), so
            // it is 0 for the whole binary and these assertions are order-independent.
            assert_eq!(CACHED_HWND.load(Ordering::Relaxed), 0, "starts unprimed");
            prime_hwnd(0);
            assert_eq!(
                CACHED_HWND.load(Ordering::Relaxed),
                0,
                "a zero handle must never be cached"
            );
            // Unprimed ensure must return before touching Win32 and must not panic.
            ensure_subclassed();
            assert!(
                !SUBCLASSED.load(Ordering::Relaxed),
                "nothing is subclassed while unprimed"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Pure-logic tests (run on every host)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises every test that WRITES the published atomics.
    ///
    /// The maximize rect and the caption-menu gate are process-globals, and
    /// `cargo test` runs this module's tests on parallel threads — so a test that
    /// publishes 1.5x scale and then reads it back can otherwise observe a
    /// sibling's 1.0x publish and fail for a reason that has nothing to do with
    /// the code under test. Every publish-then-read test takes this lock; the
    /// pure-function tests (which touch no global) do not need it.
    static PUBLISH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`PUBLISH_LOCK`], tolerating poisoning: a panic in one publishing
    /// test must fail THAT test, not cascade into every sibling.
    fn publish_guard() -> std::sync::MutexGuard<'static, ()> {
        PUBLISH_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn maximize_rect_mirrors_the_chrome_caption_layout() {
        // content width 1100 logical px (the first-run default), scale 1.0.
        let (l, t, r, b) = maximize_button_logical_rect(1100.0);
        // right_edge = 1100 - 8 = 1092; maximize is one 44px slot left of it:
        // right = 1092 - 44 = 1048; left = 1048 - 42 = 1006.
        assert_eq!((l, r), (1006.0, 1048.0));
        // cy = 40/2 = 20; bh = 28 → top 6, bottom 34.
        assert_eq!((t, b), (6.0, 34.0));
    }

    #[test]
    fn publish_round_trips_through_the_atomics_at_scale_1() {
        let _g = publish_guard();
        publish_from_geometry(1100.0, 1.0);
        assert_eq!(
            published_rect(),
            Some(RectPx::new(1006, 6, 1048, 34)),
            "the published physical rect must match the logical layout at ppp 1.0"
        );
    }

    #[test]
    fn dpi_scaling_maps_the_button_rect_to_physical_pixels() {
        // At 150% the physical rect is 1.5x — a hit test must land on the scaled
        // coordinates, never the logical ones.
        let _g = publish_guard();
        publish_from_geometry(1100.0, 1.5);
        let phys = published_rect().expect("a rect is published");
        assert_eq!(phys, RectPx::new(1509, 9, 1572, 51)); // 1006*1.5=1509, 1048*1.5=1572, 34*1.5=51
        assert_eq!(hit_code(Some(phys), 1540, 30), HT_MAXBUTTON);
        // The UNSCALED logical rect would miss the physical pointer — prove scaling
        // is load-bearing.
        let unscaled = logical_rect_to_physical(1006.0, 6.0, 1048.0, 34.0, 1.0);
        assert!(!unscaled.contains(1540, 30), "logical px must not match");
    }

    #[test]
    fn hit_code_claims_only_the_button_rect() {
        let btn = RectPx::new(1006, 6, 1048, 34);
        // Inside → HTMAXBUTTON.
        assert_eq!(hit_code(Some(btn), 1020, 20), HT_MAXBUTTON);
        // Right/bottom exclusive; left one-past; absent rect → all client.
        for (x, y) in [(1048, 20), (1020, 34), (1005, 20)] {
            assert_eq!(hit_code(Some(btn), x, y), HT_CLIENT, "({x},{y}) is outside");
        }
        assert_eq!(hit_code(None, 1020, 20), HT_CLIENT, "no rect → client");
        // A zeroed/inverted rect is treated as absent, never a giant claim.
        assert_eq!(hit_code(Some(RectPx::new(0, 0, 0, 0)), 0, 0), HT_CLIENT);
        assert_eq!(
            hit_code(Some(RectPx::new(50, 50, 10, 10)), 30, 30),
            HT_CLIENT
        );
    }

    #[test]
    fn lparam_split_handles_negative_screen_coordinates() {
        // A monitor left of / above the primary produces NEGATIVE coordinates; a
        // naive `as u16` read would wrap -8 into 65528 and miss every hit there.
        let pack = |x: i16, y: i16| -> isize {
            ((x as u16 as u32) | ((y as u16 as u32) << 16)) as i32 as isize
        };
        assert_eq!(split_lparam(pack(930, 20)), (930, 20));
        assert_eq!(split_lparam(pack(-8, -120)), (-8, -120));
        assert_eq!(split_lparam(pack(-1920, 540)), (-1920, 540));
        assert_eq!(split_lparam(pack(0, 0)), (0, 0));
    }

    #[test]
    fn sc_command_toggles_on_maximized_state() {
        assert_eq!(sc_for_toggle(false), SC_MAXIMIZE_CMD, "restored → maximize");
        assert_eq!(sc_for_toggle(true), SC_RESTORE_CMD, "maximized → restore");
    }

    // -----------------------------------------------------------------------
    // `WM_GETMINMAXINFO` clamp (ported from `win_snap::clamp_maxinfo`)
    // -----------------------------------------------------------------------

    #[test]
    fn max_placement_is_the_work_area_relative_to_the_monitor() {
        // PRIMARY monitor at the desktop origin with a 48px bottom taskbar: the
        // maximized window starts at the monitor origin and is the work-area size.
        let mon = RectPx::new(0, 0, 1920, 1080);
        let work = RectPx::new(0, 0, 1920, 1032);
        assert_eq!(
            clamped_max_info(work, mon),
            Some(MaxInfo {
                pos_x: 0,
                pos_y: 0,
                size_x: 1920,
                size_y: 1032,
            }),
            "a bottom taskbar shortens the height and moves nothing"
        );
    }

    #[test]
    fn max_position_is_monitor_relative_not_desktop_relative() {
        // A monitor LEFT of and ABOVE the primary (negative virtual-desktop
        // origin) with a LEFT taskbar. `ptMaxPosition` is documented as relative
        // to the MONITOR, so the desktop origin must cancel out: a raw
        // work.left/-work.top would place the window a whole screen away.
        let mon = RectPx::new(-1920, -200, 0, 880);
        let work = RectPx::new(-1848, -200, 0, 880);
        let info = clamped_max_info(work, mon).expect("a usable work area");
        assert_eq!(
            (info.pos_x, info.pos_y),
            (72, 0),
            "the 72px left taskbar is the only offset; the monitor origin cancels"
        );
        assert_eq!((info.size_x, info.size_y), (1848, 1080));
        // Prove the monitor origin is load-bearing: pretending the monitor sits at
        // the desktop origin yields the WRONG (off-screen) position.
        let wrong = clamped_max_info(work, RectPx::new(0, 0, 1920, 1080)).expect("some info");
        assert_ne!((wrong.pos_x, wrong.pos_y), (info.pos_x, info.pos_y));
    }

    #[test]
    fn max_placement_declines_an_unusable_work_area() {
        // A failed `GetMonitorInfoW` leaves a zeroed / inverted rect. Writing that
        // into MINMAXINFO would maximize the window to nothing, so the clamp must
        // decline and leave the OS defaults standing.
        let mon = RectPx::new(0, 0, 1920, 1080);
        for bad in [
            RectPx::new(0, 0, 0, 0),
            RectPx::new(10, 10, 10, 200),
            RectPx::new(10, 10, 200, 10),
            RectPx::new(500, 500, 100, 100),
        ] {
            assert_eq!(
                clamped_max_info(bad, mon),
                None,
                "{bad:?} must never become a maximized placement"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Titlebar right-click system-menu gate
    // -----------------------------------------------------------------------

    #[test]
    fn titlebar_strip_scales_with_dpi_and_rejects_garbage() {
        assert_eq!(titlebar_strip_bottom_px(1.0), 40);
        assert_eq!(titlebar_strip_bottom_px(1.5), 60);
        assert_eq!(titlebar_strip_bottom_px(1.25), 50);
        for bad in [0.0_f32, -2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                titlebar_strip_bottom_px(bad),
                0,
                "scale {bad} must publish no strip"
            );
        }
    }

    #[test]
    fn caption_menu_needs_both_the_strip_and_a_free_pointer() {
        // Inside the strip and egui does not want the pointer → the menu opens.
        assert!(caption_menu_allowed(20, 40, false));
        assert!(
            caption_menu_allowed(0, 40, false),
            "the top row is in-strip"
        );
        // Each gate ALONE is insufficient — this is what stops the subclass
        // swallowing a right-click egui wants (tab chip, wordmark, caption cluster,
        // or the terminal's own context menu).
        assert!(
            !caption_menu_allowed(20, 40, true),
            "an egui widget owns the pointer → decline"
        );
        assert!(
            !caption_menu_allowed(40, 40, false),
            "the strip's bottom edge is exclusive — the pane below is not caption"
        );
        assert!(!caption_menu_allowed(400, 40, false), "deep in a pane");
        assert!(
            !caption_menu_allowed(-3, 40, false),
            "a negative Y is outside the client"
        );
        // Nothing published yet → never a claim.
        assert!(!caption_menu_allowed(20, 0, false));
        assert!(!caption_menu_allowed(20, -40, false));
    }

    #[test]
    fn the_published_gate_round_trips_and_an_unpublished_strip_claims_nothing() {
        let _g = publish_guard();
        // Publish a 150% strip with no widget under the pointer: a click at 50 physical px
        // is inside the 60px strip, one at 60 is not.
        publish_caption_menu_gate(1.5, false);
        assert!(caption_menu_gate_open(50));
        assert!(!caption_menu_gate_open(60));
        // egui becoming interested closes the gate at the SAME coordinate, proving
        // the wants-pointer half is read from the publish and not ignored.
        publish_caption_menu_gate(1.5, true);
        assert!(!caption_menu_gate_open(50));
        // A garbage scale publishes no strip, so nothing is claimed at all.
        publish_caption_menu_gate(f32::NAN, false);
        assert!(!caption_menu_gate_open(0));
        assert!(!caption_menu_gate_open(50));
    }

    #[test]
    fn right_button_state_machine_is_exhaustive_and_never_strands_egui() {
        use RightButtonAction::{Forward, Latch, OpenMenu, Swallow};
        // All 8 (is_down, gate_open, latched) combinations, spelled out — the
        // subclass arm that consumes this cannot be driven without a real HWND, so
        // this table IS the coverage for the press/release state machine.
        let cases = [
            // A press the gate declined is FORWARDED: egui keeps its right-click
            // (pane context menu, tab chip, caption cluster).
            ((true, false, false), Forward),
            ((true, false, true), Forward),
            // A press the gate accepted is swallowed + latched.
            ((true, true, false), Latch),
            ((true, true, true), Latch),
            // A release with NO latch is forwarded — we never swallow a release
            // whose press egui already saw.
            ((false, false, false), Forward),
            ((false, true, false), Forward),
            // A latched release opens the menu while the gate still holds…
            ((false, true, true), OpenMenu),
            // …and is still SWALLOWED when the pointer has drifted onto a widget
            // between press and release. Delivering it would hand egui a release
            // with no matching press.
            ((false, false, true), Swallow),
        ];
        for ((is_down, gate_open, latched), want) in cases {
            assert_eq!(
                right_button_action(is_down, gate_open, latched),
                want,
                "down={is_down} gate={gate_open} latched={latched}"
            );
        }
        // Only a latched release may open the menu — nothing else, ever.
        for is_down in [false, true] {
            for gate_open in [false, true] {
                for latched in [false, true] {
                    let opens = right_button_action(is_down, gate_open, latched) == OpenMenu;
                    assert_eq!(
                        opens,
                        !is_down && gate_open && latched,
                        "only a gated, latched release opens the system menu"
                    );
                }
            }
        }
    }

    #[test]
    fn system_menu_verbs_track_the_maximized_state() {
        // Restored: everything but Restore is available.
        let restored = system_menu_item_states(false);
        let enabled = |set: &[(u32, bool); 6], cmd: u32| {
            set.iter()
                .find(|(c, _)| *c == cmd)
                .map(|(_, e)| *e)
                .expect("every verb is listed")
        };
        assert!(!enabled(&restored, SC_RESTORE_CMD), "nothing to restore");
        assert!(enabled(&restored, SC_MAXIMIZE_CMD));
        assert!(enabled(&restored, SC_MOVE_CMD));
        assert!(enabled(&restored, SC_SIZE_CMD));
        // Maximized: Move/Size/Maximize are meaningless, Restore becomes live.
        let maxed = system_menu_item_states(true);
        assert!(enabled(&maxed, SC_RESTORE_CMD));
        assert!(!enabled(&maxed, SC_MAXIMIZE_CMD), "already maximized");
        assert!(
            !enabled(&maxed, SC_MOVE_CMD),
            "a maximized window cannot move"
        );
        assert!(!enabled(&maxed, SC_SIZE_CMD));
        // Minimize + Close are ALWAYS available, in both states.
        for set in [&restored, &maxed] {
            assert!(enabled(set, SC_MINIMIZE_CMD));
            assert!(enabled(set, SC_CLOSE_CMD));
        }
        // No verb is listed twice (a duplicate would let one entry silently
        // overwrite the other's state).
        let mut cmds: Vec<u32> = restored.iter().map(|(c, _)| *c).collect();
        cmds.sort_unstable();
        let before = cmds.len();
        cmds.dedup();
        assert_eq!(cmds.len(), before, "each SC_* verb appears exactly once");
    }

    // -----------------------------------------------------------------------
    // Window material (rounded corners + backdrop)
    // -----------------------------------------------------------------------

    /// The per-frame hook must publish BOTH surfaces.
    ///
    /// `tick` is the only thing that ever writes them in the shipping binary
    /// (`egui_main` installs it as an `on_begin_pass` hook), so deleting either
    /// publish would leave the subclass reading a permanently-stale rect / a
    /// permanently-closed gate — with every pure-logic test below still green.
    /// Driving a real headless `egui::Context` through `tick` is what closes that
    /// gap.
    #[test]
    fn tick_publishes_both_the_maximize_rect_and_the_caption_gate() {
        let _g = publish_guard();
        assert!(
            !disabled(),
            "precondition: C0PL4ND_DISABLE_SNAP_CHROME must be unset in the test env"
        );
        // Clear BOTH publications first, so a sibling test's leftover value can
        // never be mistaken for this tick's work.
        publish_from_geometry(1100.0, f32::NAN);
        publish_caption_menu_gate(f32::NAN, true);
        assert_eq!(published_rect(), None, "cleared");
        assert!(!caption_menu_gate_open(20), "cleared");

        let ctx = eframe::egui::Context::default();
        tick_with_pointer(&ctx, None);

        assert_eq!(
            published_rect(),
            Some(RectPx::new(1006, 6, 1048, 34)),
            "tick must publish the maximize rect from the live content_rect"
        );
        assert!(
            caption_menu_gate_open(20),
            "tick must publish the caption gate (40px strip, no widget hovered)"
        );
        assert!(
            !caption_menu_gate_open(40),
            "and the strip must end at the titlebar height, not run the window"
        );
    }

    /// Drive a headless `Context` through one pass with a button parked at
    /// `(10,10)-(110,40)` — standing in for the titlebar's wordmark / tab chips /
    /// caption cluster — and then run [`tick`] against it.
    ///
    /// Two passes are run because egui computes the interaction snapshot for a
    /// pass from the PREVIOUS pass's widget rects: a single pass would report an
    /// empty `contains_pointer` for every position and make the widget case
    /// indistinguishable from the inert one.
    fn tick_with_pointer(ctx: &eframe::egui::Context, pointer: Option<(f32, f32)>) {
        use eframe::egui;
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1100.0, 720.0),
                )),
                events: pointer
                    .map(|(x, y)| vec![egui::Event::PointerMoved(egui::pos2(x, y))])
                    .unwrap_or_default(),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                ui.put(
                    egui::Rect::from_min_max(egui::pos2(10.0, 10.0), egui::pos2(110.0, 40.0)),
                    egui::Button::new("chrome widget"),
                );
            });
        }
        tick(ctx);
    }

    /// The gate must DISTINGUISH inert titlebar space from a titlebar widget.
    ///
    /// This is the test that would have caught the original mistake: gating on
    /// `Context::wants_pointer_input()` reads TRUE anywhere outside the central
    /// panel — i.e. over the whole titlebar — so the gate would have been shut
    /// forever and the system menu could never open. Both halves are asserted at
    /// the SAME y, so only the pointer's x (widget vs. empty space) can explain
    /// the difference.
    #[test]
    fn the_gate_opens_over_inert_titlebar_space_and_closes_over_a_widget() {
        let _g = publish_guard();
        let ctx = eframe::egui::Context::default();

        // Pointer parked in empty titlebar space (x = 600, well right of the
        // widget, y = 25 inside the 40px strip) → the menu is available.
        tick_with_pointer(&ctx, Some((600.0, 25.0)));
        assert!(
            caption_menu_gate_open(25),
            "inert titlebar space must offer the system menu"
        );

        // Same y, pointer moved ONTO the widget → the gate closes, so egui keeps
        // the right-click.
        tick_with_pointer(&ctx, Some((60.0, 25.0)));
        assert!(
            !caption_menu_gate_open(25),
            "a hovered widget must keep its own right-click"
        );
    }

    #[test]
    fn window_material_rounds_always_and_declines_the_backdrop_when_transparent() {
        // Corners are rounded either way — a frameless window has nothing else to
        // round it.
        assert_eq!(desired_window_material(true).0, CornerStyle::Round);
        assert_eq!(desired_window_material(false).0, CornerStyle::Round);
        // The transparent path (what C0PL4ND always launches as) must DECLINE the
        // system material: DWM would otherwise paint it under the surface the
        // opacity slider composites through.
        assert_eq!(desired_window_material(true).1, BackdropStyle::None);
        // The opaque path has no such conflict and takes the acrylic material.
        assert_eq!(desired_window_material(false).1, BackdropStyle::Acrylic);
    }

    #[test]
    fn logical_to_physical_rejects_garbage_scales() {
        for bad in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                logical_rect_to_physical(0.0, 0.0, 42.0, 28.0, bad).is_empty(),
                "scale {bad} must produce an unhittable rect"
            );
        }
        assert_eq!(
            logical_rect_to_physical(0.0, 0.0, 42.0, 28.0, 1.25),
            RectPx::new(0, 0, 53, 35), // 42*1.25=52.5→53, 28*1.25=35
        );
    }
}
