//! Quake mode — a global-hotkey drop-down terminal for the SHIPPING C0PL4ND
//! window (`egui_main`).
//!
//! ## What it does
//!
//! When `quake.enabled` is turned on, a single global hotkey (default
//! `Ctrl+Shift+Grave`) toggles the window:
//!
//! * **Out of view** (hidden or minimized) → **drop down**: position the window
//!   across the FULL width of the WORK AREA of the monitor **under the cursor**,
//!   anchored to the top of that work area, `quake.height_fraction` of its
//!   height tall — then show it and pull it to the foreground.
//! * **Visible but not focused** → also drop down (re-position + take focus).
//!   That is what a quake terminal is for: one keystroke always gets you a
//!   focused terminal, never "hides the thing you were looking at".
//! * **Visible AND focused** → **retract**: hide it.
//!
//! The work area (`MONITORINFO::rcWork`) is used rather than the full monitor
//! rect specifically so the drop-down never sits under the taskbar.
//!
//! ## Default OFF — a global hotkey is an OS-level privilege
//!
//! `RegisterHotKey` claims a combo PROCESS-WIDE and denies it to every other
//! application. So the whole feature is opt-in: with `quake.enabled == false`
//! (the default, and the value every pre-existing config loads with) [`init`]
//! registers NOTHING and installs no subclass, and the combo stays free. The
//! registration is released on `WM_NCDESTROY` (app exit) so it is never left
//! dangling; a hotkey is additionally reclaimed by the OS when the process dies.
//!
//! A combo that does not parse, or that carries NO modifier (which would swallow
//! a bare keystroke system-wide), is REFUSED — it logs at WARN and quake mode
//! stays inert rather than claiming something unintended.
//!
//! ## Why this is a binary-local module of `egui_main` (mirrors `tray`)
//!
//! Like `tray` and `win_chrome`, this needs the real eframe HWND *and* the
//! running winit event loop, which only the shipping binary has — so it lives
//! physically under `egui_app/` but is declared with `#[path]` in `egui_main.rs`
//! and is never compiled into the `#[path]`-included kittest lib harnesses.
//!
//! ## Message plumbing — one more subclass, not a competing message loop
//!
//! `WM_HOTKEY` is posted to the message queue of the window passed to
//! `RegisterHotKey`, so it arrives on the eframe HWND that the winit event loop
//! already pumps. We read it with a `SetWindowSubclass` entry carrying our OWN
//! subclass id — exactly the mechanism `win_chrome` uses for the caption. comctl32
//! supports several subclasses per window keyed by id, and every message we do not
//! claim goes straight down the chain via `DefSubclassProc` (→ winit →
//! `DefWindowProc`). No second message loop, no second window, no polling.
//!
//! ## Coordination with `tray` — the two cannot fight over window state
//!
//! `tray`'s left-click toggle decides from the LIVE OS state (`IsIconic` /
//! `IsWindowVisible`), and so does [`quake_action`] here: neither keeps a private
//! "is it dropped?" latch that the other could invalidate. A quake retract hides
//! the window, which is exactly the state `tray::toggle_action` reads as
//! out-of-view → its next click restores; a tray minimize is likewise read here as
//! out-of-view → the next hotkey drops it down. `egui_main`'s
//! `quake_and_tray_agree_on_window_state` test pins that agreement across all
//! eight state combinations.
//!
//! ## unsafe
//!
//! `egui_main` is `#![deny(unsafe_code)]`; the audited Win32 FFI is quarantined
//! in the `#[cfg(windows)]` `imp` module behind its own `#![allow(unsafe_code)]`,
//! exactly like `tray` / `win_foreground` / `win_chrome`. The PURE hotkey parsing,
//! drop-down geometry, and toggle decision live OUTSIDE the FFI and are
//! unit-tested on every host.

// ---------------------------------------------------------------------------
// PURE logic (compiled + tested on every host; used by the Windows imp)
// ---------------------------------------------------------------------------

/// The hotkey grammar + `MOD_*`/`VK_*` constants live in
/// `c0pl4nd_core::config` — NOT here — so the Settings UI (which is in the lib
/// target and cannot reach this binary-local module) validates a typed combo with
/// the EXACT parser that registers it. Re-exported for the tests + the FFI below;
/// there is one implementation, never a second copy that could drift.
pub use c0pl4nd_core::config::parse_hotkey;

/// A screen-space rectangle in physical pixels (left/top inclusive, right/bottom
/// exclusive — the Win32 `RECT` convention).
///
/// Deliberately distinct from `win_chrome::RectPx`: that one is a CLIENT-space
/// hit-test rect for the caption subclass, this one is a SCREEN-space monitor /
/// window rect. Same shape, different coordinate space — conflating them is how a
/// window ends up positioned in caption coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScreenRect {
    /// Left edge (inclusive).
    pub left: i32,
    /// Top edge (inclusive).
    pub top: i32,
    /// Right edge (exclusive).
    pub right: i32,
    /// Bottom edge (exclusive).
    pub bottom: i32,
}

impl ScreenRect {
    /// Construct a rect from its four edges.
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Width in pixels (saturating, so an inverted rect yields 0 rather than a
    /// negative or wrapped size).
    #[must_use]
    pub const fn width(&self) -> i32 {
        let w = self.right.saturating_sub(self.left);
        if w > 0 {
            w
        } else {
            0
        }
    }

    /// Height in pixels (saturating; see [`width`](Self::width)).
    #[must_use]
    pub const fn height(&self) -> i32 {
        let h = self.bottom.saturating_sub(self.top);
        if h > 0 {
            h
        } else {
            0
        }
    }

    /// A rect with no area — never a usable monitor work area.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }
}

/// The drop-down rectangle for a monitor's WORK AREA: full work-area width,
/// anchored to the TOP of the work area, `fraction` of the work-area height tall.
///
/// Using `rcWork` (not `rcMonitor`) is what keeps the drop-down clear of the
/// taskbar; anchoring to `work.top` (not `0`) is what makes it correct on a
/// secondary monitor and under a TOP-docked taskbar.
///
/// Returns `None` — meaning **do not move the window at all** — for a degenerate
/// work area or a non-finite fraction, so a bad monitor query or a malformed
/// config can never fling the window off-screen or size it to nothing. The
/// fraction is additionally clamped to `0.1..=1.0` as a second line of defence
/// behind `QuakeConfig::effective_height_fraction`, and the resulting height is
/// clamped to at least 1px and at most the full work-area height.
#[must_use]
pub fn quake_rect(work: ScreenRect, fraction: f32) -> Option<ScreenRect> {
    if work.is_empty() || !fraction.is_finite() {
        return None;
    }
    let fraction = fraction.clamp(0.1, 1.0);
    let work_h = work.height();
    // `work_h` is > 0 and `fraction` is in 0.1..=1.0, so the product is finite and
    // well inside i32 for any real display.
    let height = ((work_h as f32) * fraction).round() as i32;
    let height = height.clamp(1, work_h);
    Some(ScreenRect::new(
        work.left,
        work.top,
        work.right,
        work.top + height,
    ))
}

/// What a quake hotkey press should do, given the live OS window state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuakeAction {
    /// Position the window over the cursor's monitor, show it, and focus it.
    DropDown,
    /// Hide the window.
    Retract,
}

/// The quake toggle decision, derived ENTIRELY from the live OS window state —
/// there is deliberately no private "is it dropped?" latch, because a latch would
/// desynchronise the moment the tray icon (or the user, or a Win+D) changed the
/// window state behind quake's back, and the next hotkey would then do the
/// opposite of what the user sees.
///
/// * out of view (`!visible` or `minimized`) → [`DropDown`](QuakeAction::DropDown)
/// * visible but NOT focused → [`DropDown`](QuakeAction::DropDown) (raise + focus;
///   a quake keystroke must always LAND you in the terminal, never hide the window
///   you were about to use)
/// * visible AND focused → [`Retract`](QuakeAction::Retract)
#[must_use]
pub fn quake_action(visible: bool, minimized: bool, foreground: bool) -> QuakeAction {
    // Delegates to `is_out_of_view` rather than re-spelling the predicate, so the
    // toggle the hotkey actually runs and the boolean the tray is fed are provably
    // the same expression — not two copies that can drift apart.
    if is_out_of_view(visible, minimized) || !foreground {
        QuakeAction::DropDown
    } else {
        QuakeAction::Retract
    }
}

/// Whether the window state quake reads counts as "out of view" — the SAME
/// predicate `tray::toggle_action` is fed.
///
/// This is not a test helper: [`quake_action`] (and therefore the live hotkey
/// path) calls it, so the cross-module agreement test in `egui_main` is asserting
/// against the expression the shipping toggle really uses.
#[must_use]
pub const fn is_out_of_view(visible: bool, minimized: bool) -> bool {
    !visible || minimized
}

// ---------------------------------------------------------------------------
// Public entry points — driven from `egui_main` (the wiring seam)
// ---------------------------------------------------------------------------

/// Prime quake mode with the real main-window handle (from
/// `CreationContext::window_handle()`), so the hotkey targets the CORRECT window.
/// Idempotent; a zero handle is ignored. A no-op off Windows.
pub fn prime_hwnd(hwnd: isize) {
    #[cfg(windows)]
    imp::prime_hwnd(hwnd);
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// Register the global quake hotkey and install the message subclass that reads
/// `WM_HOTKEY`.
///
/// **Does nothing at all** when `cfg.enabled` is false (the default) or when
/// `cfg.hotkey` does not parse / carries no modifier — in both cases no global
/// hotkey is claimed and the combo stays available to other applications.
/// Best-effort otherwise: a `RegisterHotKey` failure (another app already owns the
/// combo) logs at WARN and leaves the app fully functional — it NEVER panics and
/// never blocks startup, mirroring `tray::init`.
///
/// Call this AFTER the window exists and after [`prime_hwnd`], on the event-loop
/// thread. A no-op off Windows.
pub fn init(ctx: &eframe::egui::Context, cfg: &c0pl4nd_core::config::QuakeConfig) {
    #[cfg(windows)]
    imp::init(ctx, cfg);
    #[cfg(not(windows))]
    let _ = (ctx, cfg);
}

// ---------------------------------------------------------------------------
// Windows FFI (the audited unsafe boundary)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    // The audited Win32 FFI is quarantined here with `// SAFETY:` justifications,
    // mirroring the other `#[cfg(windows)]` modules (tray, win_foreground,
    // win_chrome, caption_close, job_object, dll_hardening).
    #![allow(unsafe_code)]

    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
    use std::sync::OnceLock;

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    };
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOP, SWP_NOACTIVATE,
        SW_HIDE, SW_RESTORE, SW_SHOW, WM_HOTKEY, WM_NCDESTROY,
    };

    use super::{quake_action, quake_rect, QuakeAction, ScreenRect};

    /// Cached main-window HWND (0 = not yet primed). C0PL4ND uses ONE OS window.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Set once the hotkey is successfully registered, so the `WM_NCDESTROY`
    /// handler knows there is something to release (and `init` stays one-shot).
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    /// The configured drop-down height fraction, stored as `f32` bits so the
    /// window procedure can read it without taking a lock on a hot OS callback.
    static HEIGHT_FRACTION_BITS: AtomicU32 = AtomicU32::new(0);
    /// The egui context, so a toggle can request a repaint. A `OnceLock` rather
    /// than a captured closure because a subclass proc is a bare `extern "system"`
    /// fn and cannot capture; `egui::Context` is `Send + Sync + Clone`.
    static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

    /// A stable, arbitrary subclass id for our single subclass entry. Distinct
    /// from `win_chrome`'s so both subclasses coexist on the one window.
    const SUBCLASS_ID: usize = 0x00C0_0A4E;
    /// The `RegisterHotKey` id for our single hotkey, unique within this process.
    const HOTKEY_ID: i32 = 0x0A4E;

    /// Prime the cached HWND (see the public wrapper). Stores only a non-zero
    /// value; idempotent.
    pub fn prime_hwnd(hwnd: isize) {
        if hwnd != 0 {
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
    }

    /// The cached main-window handle, or `None` before it is primed.
    fn cached() -> Option<HWND> {
        let raw = CACHED_HWND.load(Ordering::Relaxed);
        (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
    }

    /// Register the hotkey + install the subclass. See the public wrapper for the
    /// default-OFF / refuse-on-unparseable contract.
    pub fn init(ctx: &eframe::egui::Context, cfg: &c0pl4nd_core::config::QuakeConfig) {
        if !cfg.enabled {
            // Default path: claim NOTHING. The combo stays free for other apps.
            return;
        }
        let Some(spec) = super::parse_hotkey(&cfg.hotkey) else {
            tracing::warn!(
                target: "c0pl4nd::quake",
                hotkey = %cfg.hotkey,
                "quake hotkey is unparseable or has no modifier; quake mode stays off"
            );
            return;
        };
        let Some(hwnd) = cached() else {
            tracing::warn!(target: "c0pl4nd::quake", "quake init before the window was primed; quake mode stays off");
            return;
        };
        if REGISTERED.load(Ordering::Relaxed) {
            return; // one-shot
        }

        HEIGHT_FRACTION_BITS.store(cfg.effective_height_fraction().to_bits(), Ordering::Relaxed);
        let _ = CTX.set(ctx.clone());

        // SAFETY: `hwnd` is this process's own main top-level window handle
        // (primed from eframe's `CreationContext`); this is the canonical comctl32
        // subclass install with our own static proc and a stable, distinct id.
        let subclassed = unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) };
        if !subclassed.as_bool() {
            tracing::warn!(target: "c0pl4nd::quake", "quake subclass install failed; quake mode stays off");
            return;
        }

        // SAFETY: `hwnd` is this process's own window, so `WM_HOTKEY` is delivered
        // to the queue winit already pumps (and our subclass above reads). The id
        // is unique within this process; the modifier/vk pair comes from the
        // validated parse.
        let ok = unsafe {
            RegisterHotKey(
                Some(hwnd),
                HOTKEY_ID,
                HOT_KEY_MODIFIERS(spec.register_modifiers()),
                spec.vk,
            )
        };
        if ok.is_ok() {
            REGISTERED.store(true, Ordering::Relaxed);
            tracing::info!(target: "c0pl4nd::quake", hotkey = %cfg.hotkey, "quake mode armed");
        } else {
            // Almost always "another application already owns this combo". Back
            // the subclass out again (it would only sit in the message chain doing
            // nothing) and carry on — a busy combo must never stop the terminal
            // from starting, and `REGISTERED` stays false so nothing is left to
            // release later.
            tracing::warn!(
                target: "c0pl4nd::quake",
                hotkey = %cfg.hotkey,
                "RegisterHotKey failed (combo likely owned by another app); quake mode stays off"
            );
            // SAFETY: removing the subclass entry we just installed on our own
            // window, by the same proc + id.
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
        }
    }

    /// The configured drop-down height fraction (already clamped when stored).
    fn height_fraction() -> f32 {
        f32::from_bits(HEIGHT_FRACTION_BITS.load(Ordering::Relaxed))
    }

    /// The WORK AREA (desktop minus taskbar) of the monitor **under the cursor** —
    /// which is the monitor a quake terminal is expected to drop onto. Falls back
    /// to the nearest monitor for an out-of-bounds cursor. `None` when either query
    /// fails, in which case the caller must NOT move the window.
    fn cursor_monitor_work_area() -> Option<ScreenRect> {
        let mut pt = POINT::default();
        // SAFETY: `pt` is a live local the OS writes the cursor position into.
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return None;
        }
        // SAFETY: takes the point by value; `MONITOR_DEFAULTTONEAREST` guarantees a
        // valid monitor handle even for a point outside every monitor.
        let monitor = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: core::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `monitor` is a valid handle from the call above; `info.cbSize` is
        // set and `info` is a live local the OS only writes into.
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return None;
        }
        let w = info.rcWork;
        Some(ScreenRect::new(w.left, w.top, w.right, w.bottom))
    }

    /// Perform one quake toggle against the live window state.
    fn toggle() {
        let Some(hwnd) = cached() else {
            return;
        };
        // SAFETY: `hwnd` is this process's own main window; these only READ its
        // visibility / minimized state.
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        // SAFETY: same own handle; reads the minimized state.
        let minimized = unsafe { IsIconic(hwnd) }.as_bool();
        // SAFETY: borrows nothing; returns the current foreground window by value.
        let foreground = unsafe { GetForegroundWindow() } == hwnd;

        match quake_action(visible, minimized, foreground) {
            QuakeAction::DropDown => drop_down(hwnd),
            QuakeAction::Retract => retract(hwnd),
        }
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    }

    /// Hide the window. Deliberately `SW_HIDE` (not `SW_MINIMIZE`): a hidden
    /// window is the state `tray::toggle_action` reads as out-of-view, so the tray
    /// icon stays a correct escape hatch after a retract.
    fn retract(hwnd: HWND) {
        // SAFETY: own main-window handle; `SW_HIDE` only hides it.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }

    /// Show the window across the top of the cursor's monitor work area and pull
    /// it to the foreground.
    ///
    /// The foreground step mirrors `tray`'s (and `win_foreground`'s)
    /// `AttachThreadInput` dance — Win11 refuses a raw `SetForegroundWindow` from a
    /// background process, so the drop-down would otherwise appear BEHIND whatever
    /// the user was in, which defeats the entire feature.
    fn drop_down(hwnd: HWND) {
        // Un-hide and un-minimize FIRST: `SetWindowPos` on a minimized window does
        // not reliably move the restored placement, so position afterwards.
        // SAFETY: own handle; `SW_SHOW` un-hides a hidden window.
        let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
        // SAFETY: own handle; reads the minimized state.
        if unsafe { IsIconic(hwnd) }.as_bool() {
            // SAFETY: own handle; `SW_RESTORE` un-minimizes to the normal state.
            let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
        }

        // Position over the CURSOR's monitor work area. A failed monitor query or a
        // degenerate work area yields `None` and we simply do not move the window —
        // never a fling to (0,0) or a zero-height window.
        if let Some(rect) =
            cursor_monitor_work_area().and_then(|w| quake_rect(w, height_fraction()))
        {
            // SAFETY: own handle; a plain move/resize to a monitor-derived rect.
            // `SWP_NOACTIVATE` leaves activation to the explicit foreground dance
            // below, which is the part Windows 11 actually honours.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOP),
                    rect.left,
                    rect.top,
                    rect.width(),
                    rect.height(),
                    SWP_NOACTIVATE,
                )
            };
        }

        // SAFETY: borrows nothing; returns the current foreground window (possibly
        // null) by value.
        let fg = unsafe { GetForegroundWindow() };
        // SAFETY: reads the owning thread id of `fg` (0 for a null handle); the
        // process-id out-param is `None`, so nothing is written back.
        let fg_thread = unsafe { GetWindowThreadProcessId(fg, None) };
        // SAFETY: returns THIS thread's id; borrows no memory.
        let our_thread = unsafe { GetCurrentThreadId() };
        // Attach only to a DIFFERENT, known foreground thread (attaching a thread
        // to itself is invalid; a 0 thread cannot be attached).
        let attached = fg_thread != 0 && fg_thread != our_thread && {
            // SAFETY: attaching two real, distinct thread-input queues by id;
            // borrows no memory. Paired with the detach below.
            unsafe { AttachThreadInput(fg_thread, our_thread, true).as_bool() }
        };
        // SAFETY: own handle; raises it to the top of the Z-order (best-effort).
        let _ = unsafe { BringWindowToTop(hwnd) };
        // SAFETY: own handle; requests foreground activation (honoured because we
        // attached to the foreground thread's input above).
        let _ = unsafe { SetForegroundWindow(hwnd) };
        if attached {
            // SAFETY: detach the exact two thread ids attached above, restoring the
            // independent input queues. Non-fatal if it fails.
            unsafe {
                let _ = AttachThreadInput(fg_thread, our_thread, false);
            }
        }
    }

    /// Release the global hotkey. Called from `WM_NCDESTROY` so the combo is never
    /// left dangling for the rest of the desktop session; the OS additionally
    /// reclaims a process's hotkeys when the process dies.
    fn unregister(hwnd: HWND) {
        if REGISTERED.swap(false, Ordering::Relaxed) {
            // SAFETY: own handle; releases the exact id we registered above.
            let _ = unsafe { UnregisterHotKey(Some(hwnd), HOTKEY_ID) };
        }
    }

    /// The quake subclass: read `WM_HOTKEY` for OUR id and toggle; release the
    /// hotkey on `WM_NCDESTROY`. Everything else goes straight down the subclass
    /// chain untouched, so winit's frame handling (and `win_chrome`'s caption
    /// subclass) are undisturbed.
    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _ref: usize,
    ) -> LRESULT {
        match msg {
            WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
                toggle();
                return LRESULT(0);
            }
            WM_NCDESTROY => {
                // Release BEFORE handing the message on, so the window still exists.
                unregister(hwnd);
                // SAFETY: removing the subclass entry we installed on our own
                // window, by the same proc + id. Required on `WM_NCDESTROY`.
                let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
            }
            _ => {}
        }
        // SAFETY: hand everything we did not claim to the default subclass chain
        // (→ winit → `DefWindowProc`).
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn modifier_and_vk_constants_match_the_windows_crate() {
            // Drift guard (mirrors `win_chrome`): the platform-independent `MOD_*` /
            // `VK_*` literals core's parser emits must equal the real Win32
            // constants, or we would register a DIFFERENT combo than the user asked
            // for. Core cannot assert this (it has no `windows` dependency), so the
            // assertion lives here — at the boundary that actually passes them to
            // `RegisterHotKey`.
            use c0pl4nd_core::config as cc;
            use windows::Win32::UI::Input::KeyboardAndMouse::{
                MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_ESCAPE, VK_F1, VK_OEM_3,
                VK_RETURN, VK_SPACE, VK_TAB,
            };
            assert_eq!(cc::MOD_ALT_BIT, MOD_ALT.0, "MOD_ALT");
            assert_eq!(cc::MOD_CONTROL_BIT, MOD_CONTROL.0, "MOD_CONTROL");
            assert_eq!(cc::MOD_SHIFT_BIT, MOD_SHIFT.0, "MOD_SHIFT");
            assert_eq!(cc::MOD_WIN_BIT, MOD_WIN.0, "MOD_WIN");
            assert_eq!(cc::MOD_NOREPEAT_BIT, MOD_NOREPEAT.0, "MOD_NOREPEAT");
            assert_eq!(cc::VK_GRAVE, u32::from(VK_OEM_3.0), "VK_OEM_3");
            assert_eq!(cc::VK_SPACE_KEY, u32::from(VK_SPACE.0), "VK_SPACE");
            assert_eq!(cc::VK_TAB_KEY, u32::from(VK_TAB.0), "VK_TAB");
            assert_eq!(cc::VK_ESCAPE_KEY, u32::from(VK_ESCAPE.0), "VK_ESCAPE");
            assert_eq!(cc::VK_RETURN_KEY, u32::from(VK_RETURN.0), "VK_RETURN");
            assert_eq!(cc::VK_F1_KEY, u32::from(VK_F1.0), "VK_F1");
        }

        #[test]
        fn letter_and_digit_vk_codes_match_the_windows_crate() {
            // The parser relies on "the VK for a letter IS its uppercase ASCII code"
            // and the same for digits. Prove it against the real constants rather
            // than trusting the folklore — a wrong assumption here registers the
            // wrong key with no error anywhere.
            use windows::Win32::UI::Input::KeyboardAndMouse::{VK_0, VK_9, VK_A, VK_Q, VK_Z};
            let vk = |s: &str| super::super::parse_hotkey(&format!("Ctrl+{s}")).map(|h| h.vk);
            assert_eq!(vk("a"), Some(u32::from(VK_A.0)));
            assert_eq!(vk("Q"), Some(u32::from(VK_Q.0)));
            assert_eq!(vk("z"), Some(u32::from(VK_Z.0)));
            assert_eq!(vk("0"), Some(u32::from(VK_0.0)));
            assert_eq!(vk("9"), Some(u32::from(VK_9.0)));
        }

        #[test]
        fn disabled_config_registers_nothing_and_a_zero_handle_never_primes() {
            // `CACHED_HWND` is a process-global that no test primes with a real
            // handle (a fabricated HWND would violate every SAFETY precondition), so
            // it is 0 for the whole binary and these assertions are order-independent.
            assert_eq!(CACHED_HWND.load(Ordering::Relaxed), 0, "starts unprimed");
            prime_hwnd(0);
            assert_eq!(
                CACHED_HWND.load(Ordering::Relaxed),
                0,
                "a zero handle must never be cached"
            );
            // The DEFAULT (disabled) config must claim no global hotkey — this is
            // the opt-in invariant, exercised through the real `init` entry point.
            let ctx = eframe::egui::Context::default();
            init(&ctx, &c0pl4nd_core::config::QuakeConfig::default());
            assert!(
                !REGISTERED.load(Ordering::Relaxed),
                "a default (disabled) config must never register a global hotkey"
            );
            // Enabled but unprimed must also register nothing (and not panic).
            let cfg = c0pl4nd_core::config::QuakeConfig {
                enabled: true,
                ..Default::default()
            };
            init(&ctx, &cfg);
            assert!(
                !REGISTERED.load(Ordering::Relaxed),
                "an unprimed window must never register a global hotkey"
            );
        }

        #[test]
        fn toggle_and_unregister_are_safe_while_unprimed() {
            // Both are reachable from the OS callback before/after the window
            // exists; neither may panic or touch Win32 with a null handle.
            toggle();
            assert!(!REGISTERED.load(Ordering::Relaxed));
        }

        #[test]
        fn height_fraction_round_trips_through_the_atomic() {
            HEIGHT_FRACTION_BITS.store(0.42_f32.to_bits(), Ordering::Relaxed);
            assert!((height_fraction() - 0.42).abs() < 1e-6);
        }

        #[test]
        fn subclass_and_hotkey_ids_are_distinct_from_win_chrome() {
            // Two subclass entries share one window; a colliding id would REPLACE
            // the caption subclass instead of chaining, silently killing Snap
            // Layouts.
            //
            // Compare against win_chrome's ACTUAL constant, never a copy of its
            // literal: a copy keeps passing after win_chrome's id changes, so the
            // one collision this test exists to catch would ship undetected.
            assert_ne!(
                SUBCLASS_ID,
                crate::win_chrome::SUBCLASS_ID,
                "quake and win_chrome must not share a subclass id"
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

    #[test]
    fn quake_rect_uses_the_work_area_top_and_full_width() {
        // 1920x1080 monitor with a 40px bottom taskbar → work area 0,0..1920,1040.
        let work = ScreenRect::new(0, 0, 1920, 1040);
        let r = quake_rect(work, 0.5).expect("a usable work area yields a rect");
        assert_eq!(r, ScreenRect::new(0, 0, 1920, 520));
        assert_eq!(r.width(), 1920, "full work-area width");
        assert_eq!(r.height(), 520, "half the work-area height");
        // It must never reach into the taskbar strip below the work area.
        assert!(r.bottom <= work.bottom, "never overlaps the taskbar");
    }

    #[test]
    fn quake_rect_anchors_to_the_work_top_not_the_screen_top() {
        // A TOP-docked taskbar (work.top = 48) and a SECOND monitor left of the
        // primary (negative x). Anchoring to 0 instead of `work.top` would slide
        // the window UNDER the taskbar; ignoring `work.left` would put it on the
        // wrong monitor entirely.
        let work = ScreenRect::new(-1920, 48, 0, 1080);
        let r = quake_rect(work, 0.5).expect("a usable work area yields a rect");
        assert_eq!(r.top, 48, "must start BELOW a top-docked taskbar");
        assert_eq!(r.left, -1920, "must stay on the left-hand monitor");
        assert_eq!(r.right, 0);
        assert_eq!(r.height(), 516); // (1080-48) * 0.5 = 516
        assert_eq!(r.bottom, 48 + 516);
    }

    #[test]
    fn quake_rect_clamps_the_fraction_and_refuses_garbage() {
        let work = ScreenRect::new(0, 0, 1920, 1000);
        // Full height at 1.0, and clamped there for anything larger.
        assert_eq!(quake_rect(work, 1.0).map(|r| r.height()), Some(1000));
        assert_eq!(quake_rect(work, 5.0).map(|r| r.height()), Some(1000));
        // Clamped UP to the 0.1 floor, never to a zero-height window.
        assert_eq!(quake_rect(work, 0.0).map(|r| r.height()), Some(100));
        assert_eq!(quake_rect(work, -2.0).map(|r| r.height()), Some(100));
        // A non-finite fraction or a degenerate work area means DO NOT MOVE.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                quake_rect(work, bad),
                None,
                "{bad} must not move the window"
            );
        }
        for empty in [
            ScreenRect::new(0, 0, 0, 0),
            ScreenRect::new(0, 0, 1920, 0),
            ScreenRect::new(0, 0, 0, 1080),
            ScreenRect::new(100, 100, 50, 50), // inverted
        ] {
            assert_eq!(
                quake_rect(empty, 0.5),
                None,
                "{empty:?} is not a usable work area"
            );
        }
    }

    #[test]
    fn quake_toggles_only_when_visible_and_focused() {
        // Visible + focused is the ONLY state that hides the window.
        assert_eq!(quake_action(true, false, true), QuakeAction::Retract);
        // Every other state drops it down (shows + focuses).
        assert_eq!(
            quake_action(true, false, false),
            QuakeAction::DropDown,
            "visible but unfocused must FOCUS it, never hide it"
        );
        assert_eq!(
            quake_action(false, false, false),
            QuakeAction::DropDown,
            "hidden → drop down"
        );
        assert_eq!(
            quake_action(true, true, true),
            QuakeAction::DropDown,
            "minimized (even if nominally 'foreground') → drop down"
        );
        assert_eq!(quake_action(false, true, false), QuakeAction::DropDown);
    }

    #[test]
    fn out_of_view_matches_the_tray_predicate_shape() {
        // The same boolean `tray::toggle_action` is fed. Pinning it here (and the
        // cross-module agreement test in `egui_main`) is what stops quake from
        // growing a private latch that desynchronises from the tray.
        assert!(is_out_of_view(false, false), "hidden is out of view");
        assert!(is_out_of_view(true, true), "minimized is out of view");
        assert!(is_out_of_view(false, true));
        assert!(!is_out_of_view(true, false), "visible+restored is in view");
        // Every out-of-view state must drop down, never retract — a retract there
        // would leave the user pressing the hotkey with nothing appearing.
        for (v, m) in [(false, false), (true, true), (false, true)] {
            assert!(is_out_of_view(v, m));
            assert_eq!(quake_action(v, m, true), QuakeAction::DropDown);
            assert_eq!(quake_action(v, m, false), QuakeAction::DropDown);
        }
    }
}
