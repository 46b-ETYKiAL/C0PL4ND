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

use std::sync::atomic::{AtomicI32, Ordering};

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
    publish_from_geometry(content.right(), ctx.pixels_per_point());
    #[cfg(windows)]
    imp::ensure_subclassed();
}

// ---------------------------------------------------------------------------
// Windows FFI (the audited unsafe boundary)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Dwm::DwmDefWindowProc;
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    };
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, PostMessageW, GWL_STYLE, HTMAXBUTTON, WM_NCHITTEST, WM_NCLBUTTONDOWN,
        WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCRBUTTONDOWN, WM_NCRBUTTONUP,
        WM_SYSCOMMAND, WS_MAXIMIZE,
    };

    use super::{hit_code, published_rect, sc_for_toggle, split_lparam};

    /// Cached main-window HWND (0 = not yet primed). C0PL4ND uses ONE OS window.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Set once the subclass is successfully installed (install is one-shot).
    static SUBCLASSED: AtomicBool = AtomicBool::new(false);
    /// Set while a non-client left-press landed on the maximize button, so the
    /// matching button-UP is ours to act on (a press-elsewhere-release-here is not).
    static BTN_PRESSED: AtomicBool = AtomicBool::new(false);

    /// A stable, arbitrary subclass id for our single subclass entry.
    const SUBCLASS_ID: usize = 0x00C0_041D;

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
            _ => {}
        }

        // SAFETY: hand everything we did not claim to the default subclass chain
        // (→ winit → `DefWindowProc`), so winit's frame handling is undisturbed.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
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
