//! Windows custom-frame Aero Snap for the frameless window (D4).
//!
//! C0PL4ND runs `with_decorations(false)`, which on Windows produces a window
//! with no `WS_THICKFRAME`/`WS_CAPTION` — so Win+Arrow snap, the maximize /
//! minimize animations, and the snap-layouts flyout do not work. The standard
//! Win32 fix (Microsoft Learn "Custom Window Frame Using DWM" + the widely-used
//! borderless-window pattern, grassator/win32-window-custom-titlebar) is to:
//!
//! 1. Add `WS_THICKFRAME | WS_CAPTION` back to the window style — this is what
//!    tells the DWM the window participates in snap / min-max animations — then
//! 2. handle `WM_NCCALCSIZE` (wparam == TRUE) by returning the proposed client
//!    rect UNCHANGED, so the frame the style would normally draw covers the
//!    whole window and stays invisible (we keep drawing our own chrome), and
//! 3. handle `WM_NCHITTEST` ourselves so the invisible frame still reports the
//!    resize edges + the title-bar drag strip, and
//! 4. clamp `WM_GETMINMAXINFO` to the monitor work area so a maximized window
//!    does not cover the taskbar.
//!
//! ## Relationship to the SHIPPING window's `egui_app::win_chrome`
//!
//! This module belongs to the PRESERVED `c0pl4nd-legacy` binary (`main.rs` ->
//! `window.rs`), which is gated behind the default-OFF `legacy-winit` feature.
//! The canonical `c0pl4nd` binary never links it: its frame comes from winit
//! (which already zeroes `WM_NCCALCSIZE` on an undecorated window) plus the
//! additive `egui_app::win_chrome` caption subclass.
//!
//! Step 4 above — the `WM_GETMINMAXINFO` work-area clamp — has been PORTED to
//! `win_chrome` so the shipping window gets it too. It is a port, not a move:
//! this copy stays because it is the legacy binary's own frame, and the ported
//! copy deliberately differs (it does not clamp `ptMaxTrackSize`, and it runs
//! AFTER the subclass chain rather than before). Steps 1-3 are NOT ported —
//! winit supplies the equivalent on the shipping window, and re-adding them
//! there would fight its frameless composition. See `win_chrome`'s PORT NOTEs.
//!
//! **Do not delete this module.** It has live static importers (`window.rs`
//! calls `install`, `set_interactive_zones`, and `flash_taskbar`), it is a
//! declared `[[bin]]` target in `Cargo.toml`, and `ci.yml` lints it on every
//! push via `cargo clippy --bin c0pl4nd-legacy --features legacy-winit`.
//!
//! All of this is installed via `SetWindowSubclass` so winit's own wndproc keeps
//! running for everything we don't intercept. Entirely `#[cfg(windows)]`; the
//! caller no-ops elsewhere. This module owns ONLY the native frame — the GPU
//! chrome (wordmark, buttons, hover backplates) is unchanged and still drawn by
//! the renderer. Geometry is passed in from `window.rs` so the hit-test strip
//! matches the rendered title bar exactly.

#![cfg(windows)]

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_STYLE, MINMAXINFO,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_GETMINMAXINFO,
    WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_THICKFRAME,
};

// Hit-test result codes (Win32 `HT*`). The windows crate exposes these as `u32`
// constants; the wndproc returns an `isize` LRESULT, so we use the plain numeric
// values for clarity and a single cast site.
const HTCLIENT: isize = 1;
const HTCAPTION: isize = 2;
const HTLEFT: isize = 10;
const HTRIGHT: isize = 11;
const HTTOP: isize = 12;
const HTTOPLEFT: isize = 13;
const HTTOPRIGHT: isize = 14;
const HTBOTTOM: isize = 15;
const HTBOTTOMLEFT: isize = 16;
const HTBOTTOMRIGHT: isize = 17;

/// Subclass id for our snap wndproc (any stable per-process value).
const SUBCLASS_ID: usize = 0xC0_71_4D; // "C0PL4ND"-ish marker.

/// Physical-pixel x-ranges within the title-bar strip that are INTERACTIVE
/// (the tab chips, the new-tab `+`, the settings gear). These MUST hit-test as
/// `HTCLIENT` so a click reaches winit's `MouseInput` (and our tab router)
/// instead of being swallowed by Windows as a title-bar drag (`HTCAPTION`).
/// Published every frame by the renderer (`window.rs`) after it shapes the tab
/// chrome. A single global is correct: C0PL4ND uses ONE OS window — tabs are
/// in-window — so per-HWND keying is unnecessary.
static INTERACTIVE_ZONES: std::sync::OnceLock<std::sync::Mutex<Vec<(i32, i32)>>> =
    std::sync::OnceLock::new();

/// Publish the interactive title-bar x-ranges (physical px) for the hit-test.
/// Called from the renderer each frame. Lock-guarded, no `unsafe`.
pub fn set_interactive_zones(zones: Vec<(i32, i32)>) {
    let cell = INTERACTIVE_ZONES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut g) = cell.lock() {
        *g = zones;
    }
}

/// True when client-space x `px` falls within a published interactive zone.
fn in_interactive_zone(px: i32) -> bool {
    INTERACTIVE_ZONES
        .get()
        .and_then(|m| m.lock().ok())
        .map(|g| zone_contains(px, &g))
        .unwrap_or(false)
}

/// Pure half-open membership test: is `px` within any `[x0, x1)` range? Split
/// out so the hit-test predicate is unit-testable without a window/global.
fn zone_contains(px: i32, zones: &[(i32, i32)]) -> bool {
    zones.iter().any(|&(x0, x1)| px >= x0 && px < x1)
}

/// Geometry the hit-test needs, in PHYSICAL pixels, matching the renderer.
/// Heap-allocated via `Box`; the raw pointer is the subclass `dwrefdata`, so
/// the wndproc can read it without any global state. The `Box` is RECLAIMED
/// (freed) when the subclass is removed — either explicitly via [`uninstall`]
/// or automatically on `WM_NCDESTROY` (see [`reclaim_geom`]) — so the
/// allocation never leaks across a window lifetime.
#[derive(Clone, Copy)]
struct SnapGeometry {
    /// Title-bar drag-strip height (physical px).
    titlebar_h: i32,
    /// Resize-border band thickness (physical px).
    resize_border: i32,
    /// Width (physical px) of the caption-button cluster at the right edge; the
    /// drag strip excludes this region so a click on min/max/close is HTCLIENT
    /// and reaches our own button handler.
    buttons_w: i32,
}

/// Install the custom-frame subclass on `hwnd` and re-add the snap-enabling
/// window styles. The geometry is given in physical pixels and must match the
/// renderer's `TITLEBAR_H`, `RESIZE_BORDER`, and caption-button cluster width.
///
/// # Safety
/// `hwnd` must be a valid top-level window handle for the current process,
/// alive for the duration of the subclass. Called once per window from the UI
/// thread.
pub unsafe fn install(hwnd: isize, titlebar_h: i32, resize_border: i32, buttons_w: i32) {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let geom = Box::new(SnapGeometry {
        titlebar_h,
        resize_border,
        buttons_w,
    });
    let geom_ptr = Box::into_raw(geom) as usize;

    // Add the styles that make the DWM treat the window as snap-able + animatable
    // while keeping it frameless (WM_NCCALCSIZE below hides the frame). Keep
    // whatever winit set and OR in the snap styles.
    // SAFETY: `hwnd` is a valid top-level window handle per this fn's contract;
    // `GetWindowLongPtrW(GWL_STYLE)` only reads the window's style word.
    let cur = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    let add = (WS_THICKFRAME | WS_CAPTION | WS_MAXIMIZEBOX | WS_MINIMIZEBOX).0 as isize;
    // SAFETY: same valid `hwnd`; writing the OR of the existing style with the
    // documented snap-enabling style bits is a well-formed `SetWindowLongPtrW`.
    unsafe { SetWindowLongPtrW(hwnd, GWL_STYLE, cur | add) };

    // SetWindowSubclass inserts our proc into the chain; def_subclass keeps
    // winit's wndproc running for everything we don't intercept.
    // SAFETY: `hwnd` is valid; `snap_wndproc` is a real `extern "system"` proc;
    // `geom_ptr` is the `Box::into_raw` pointer the proc reconstructs/reads and is
    // reclaimed on `WM_NCDESTROY`/`uninstall`, so it stays live while subclassed.
    let _ = unsafe { SetWindowSubclass(hwnd, Some(snap_wndproc), SUBCLASS_ID, geom_ptr) };

    // Force a frame recalculation so WM_NCCALCSIZE runs immediately with the new
    // styles (otherwise the OS frame flashes until the first resize).
    // SAFETY: `hwnd` is valid; the SWP_* no-move/no-size/no-zorder/no-activate
    // flags mean every positional argument is ignored, so this only triggers a
    // frame recalc (`None` z-order insert-after is valid under SWP_NOZORDER).
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
    };

    // Windows 11: round the window corners to match the modern OS chrome.
    // Ignored as a harmless no-op on Windows 10 (the attribute is unknown there).
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    let corner_pref = DWMWCP_ROUND;
    // SAFETY: `hwnd` is valid; the attribute pointer + length describe the live
    // stack `corner_pref` (`size_of_val` bytes), matching the documented
    // `DWMWA_WINDOW_CORNER_PREFERENCE` input contract.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_pref as *const _ as *const core::ffi::c_void,
            core::mem::size_of_val(&corner_pref) as u32,
        )
    };
}

/// Reclaim (free) the heap-allocated [`SnapGeometry`] backing the subclass on
/// `hwnd`, if one is installed, then remove the subclass. Idempotent: a second
/// call finds no subclass and is a no-op, so it can never double-free.
///
/// `RemoveWindowSubclass` does NOT hand back the `dwRefData`, so we first read
/// it with `GetWindowSubclass`, remove the subclass (after which no further
/// wndproc call can observe the pointer), and only THEN reconstruct the `Box`
/// to drop it. Ordering matters: removing before the `Box::from_raw` closes the
/// window where a concurrent message could read freed memory.
///
/// # Safety
/// `hwnd` must be a valid window handle, and any installed subclass on it must
/// have been installed by [`install`] (so the `dwRefData` is a `Box<SnapGeometry>`
/// pointer this module created).
unsafe fn reclaim_geom(hwnd: HWND) {
    let mut refdata: usize = 0;
    // `GetWindowSubclass` returns TRUE and fills `refdata` only when OUR subclass
    // (matching proc + id) is currently installed on the window.
    // SAFETY: `hwnd` is valid per this fn's contract; `&mut refdata` is a live
    // local the call writes only on success. Reads existing subclass state only.
    let installed = unsafe {
        GetWindowSubclass(hwnd, Some(snap_wndproc), SUBCLASS_ID, Some(&mut refdata)).as_bool()
    };
    // Always attempt removal; harmless when not installed.
    // SAFETY: `hwnd` is valid; removing a (possibly absent) subclass by matching
    // proc + id is a documented no-op when not installed.
    let _ = unsafe { RemoveWindowSubclass(hwnd, Some(snap_wndproc), SUBCLASS_ID) };
    if installed && refdata != 0 {
        // SAFETY: `refdata` is the exact pointer produced by `Box::into_raw` in
        // `install`; the subclass is now removed so no wndproc can read it; we
        // own it and drop it exactly once.
        drop(unsafe { Box::from_raw(refdata as *mut SnapGeometry) });
    }
}

/// Remove the subclass and free its geometry allocation. Safe to call if never
/// installed (no-op) and safe to call more than once (idempotent — the second
/// call finds no subclass).
///
/// # Safety
/// `hwnd` must be the same handle passed to [`install`].
#[allow(dead_code)] // Teardown API: winit drops the HWND at process exit (where
                    // WM_NCDESTROY also reclaims), so the happy path may never
                    // call this; kept for explicit teardown + tests.
pub unsafe fn uninstall(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: `hwnd` is the handle passed to `install` per this fn's contract, so
    // any subclass on it was installed by us and its `dwRefData` is our `Box`.
    unsafe { reclaim_geom(hwnd) };
}

/// Flash the taskbar button until the window is next brought to the foreground
/// (`FLASHW_TRAY | FLASHW_TIMERNOFG`). Used to surface an OSC 9/777 desktop
/// notification that arrived while the window was unfocused.
///
/// # Safety
/// `hwnd` must be a live top-level window handle for the current process.
pub unsafe fn flash_taskbar(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{
        FlashWindowEx, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY,
    };
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let info = FLASHWINFO {
        cbSize: core::mem::size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    // SAFETY: `&info` points to a fully-initialised `FLASHWINFO` with its `cbSize`
    // set to its own size and a valid `hwnd` per this fn's contract; the call only
    // reads the struct for the duration of the call.
    let _ = unsafe { FlashWindowEx(&info) };
}

/// The subclass wndproc. Intercepts the three frame messages and forwards
/// everything else down the chain (ultimately winit's wndproc) via
/// `DefSubclassProc`.
unsafe extern "system" fn snap_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    refdata: usize,
) -> LRESULT {
    let geom = refdata as *const SnapGeometry;
    match msg {
        // Returning the proposed client rect unchanged (for wparam == TRUE)
        // makes the client area fill the whole window, hiding the native frame
        // WS_THICKFRAME/WS_CAPTION would otherwise draw — while keeping snap.
        WM_NCCALCSIZE if wparam.0 != 0 => LRESULT(0),

        // Map the cursor to a resize edge, the drag strip, or the client.
        WM_NCHITTEST => {
            if geom.is_null() {
                // SAFETY: forwarding the same `hwnd`/`msg`/`wparam`/`lparam` we
                // received down the subclass chain via `DefSubclassProc`.
                return unsafe { def_subclass(hwnd, msg, wparam, lparam) };
            }
            // SAFETY: `geom` is non-null (checked above) and is our
            // `Box<SnapGeometry>` pointer, still live while the subclass is
            // installed, so reborrowing it as `&SnapGeometry` is valid.
            let geom_ref = unsafe { &*geom };
            // SAFETY: `hwnd` is the live window receiving the message; `geom_ref`
            // borrows the live geometry; `hit_test` only does read-only Win32 reads.
            unsafe { hit_test(hwnd, lparam, geom_ref) }
        }

        // Clamp a maximized window to the monitor work area so it never covers
        // the taskbar (a frameless window otherwise maximizes over it).
        WM_GETMINMAXINFO => {
            // SAFETY: `hwnd` is the live window and `lparam` is the OS-provided
            // `MINMAXINFO*` for this message; `clamp_maxinfo` null-checks it.
            unsafe { clamp_maxinfo(hwnd, lparam) };
            // Let the default proc run too so it fills the other fields.
            // SAFETY: forwarding the unmodified message down the subclass chain.
            unsafe { def_subclass(hwnd, msg, wparam, lparam) }
        }

        // Window is being destroyed — the LAST message a window receives. Free
        // the heap-allocated geometry and detach the subclass here so the `Box`
        // is reclaimed even when the host (winit) never calls `uninstall`. We
        // forward to the default proc FIRST so the rest of the chain sees the
        // destroy, then reclaim (reclaim is idempotent, so a later `uninstall`
        // is a harmless no-op).
        WM_NCDESTROY => {
            // SAFETY: forwarding the destroy message down the subclass chain first.
            let r = unsafe { def_subclass(hwnd, msg, wparam, lparam) };
            // SAFETY: `hwnd` is this window; any subclass on it is ours, so its
            // `dwRefData` is our `Box<SnapGeometry>`; reclaim is idempotent.
            unsafe { reclaim_geom(hwnd) };
            r
        }

        // SAFETY: forwarding every non-intercepted message down the subclass chain.
        _ => unsafe { def_subclass(hwnd, msg, wparam, lparam) },
    }
}

/// Forward to the next handler in the subclass chain (ultimately winit's
/// wndproc). `DefSubclassProc` is the documented continue-the-chain call.
unsafe fn def_subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: `hwnd` is the live window the message was dispatched to; passing the
    // message parameters through unchanged is the documented continue-the-chain call.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Resolve `WM_NCHITTEST`: resize edges within the border band, the title-bar
/// drag strip (HTCAPTION) outside the caption-button region, else HTCLIENT.
unsafe fn hit_test(hwnd: HWND, lparam: LPARAM, geom: &SnapGeometry) -> LRESULT {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::ScreenToClient;

    // lparam packs the screen-space cursor: low word x, high word y (signed).
    let sx = (lparam.0 & 0xFFFF) as i16 as i32;
    let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    let mut pt = POINT { x: sx, y: sy };
    // SAFETY: `hwnd` is the live window per this fn's contract; `&mut pt` is a
    // live local the call writes the converted client-space coordinates into.
    let _ = unsafe { ScreenToClient(hwnd, &mut pt) };

    let mut rc = RECT::default();
    // SAFETY: `hwnd` is valid; `&mut rc` is a live local the call fills with the
    // client rectangle. On error we fall back to HTCLIENT without reading `rc`.
    if unsafe { GetClientRect(hwnd, &mut rc) }.is_err() {
        return LRESULT(HTCLIENT);
    }
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;

    LRESULT(classify_nc_hit(pt.x, pt.y, w, h, geom, || {
        in_interactive_zone(pt.x)
    }))
}

/// Pure `WM_NCHITTEST` classification: map a CLIENT-space point `(px, py)` on a
/// `w`×`h` client rect to its `HT*` code.
///
/// Split out of [`hit_test`] — whose only impure steps are `ScreenToClient`,
/// `GetClientRect`, and reading the published interactive-zone global — so the
/// custom frame's resize-edge and drag-strip routing is unit testable without a
/// live HWND.
///
/// `interactive` supplies the [`in_interactive_zone`] answer for `px` LAZILY: the
/// original only consulted the zone global (taking its mutex) inside the body
/// arm, and only after the `py`/`px` bounds checks short-circuited. Taking a
/// closure rather than a pre-computed `bool` keeps that locking behaviour
/// identical on the resize-edge paths.
fn classify_nc_hit<F: FnOnce() -> bool>(
    px: i32,
    py: i32,
    w: i32,
    h: i32,
    geom: &SnapGeometry,
    interactive: F,
) -> isize {
    let b = geom.resize_border;

    let on_left = px < b;
    let on_right = px >= w - b;
    let on_top = py < b;
    let on_bottom = py >= h - b;

    match (on_top, on_bottom, on_left, on_right) {
        (true, _, true, _) => HTTOPLEFT,
        (true, _, _, true) => HTTOPRIGHT,
        (_, true, true, _) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, ..) => HTTOP,
        (_, true, ..) => HTBOTTOM,
        (_, _, true, _) => HTLEFT,
        (_, _, _, true) => HTRIGHT,
        _ => {
            // Inside the body. The title-bar strip (below the top resize band,
            // above titlebar_h) is the drag region — EXCEPT (a) the
            // caption-button cluster at the right edge and (b) the interactive
            // controls in the tab strip (tab chips, the new-tab `+`, the
            // settings gear), both of which stay HTCLIENT so the click reaches
            // winit's MouseInput handler instead of starting a window drag.
            if py < geom.titlebar_h && px < w - geom.buttons_w && !interactive() {
                HTCAPTION
            } else {
                HTCLIENT
            }
        }
    }
}

/// Clamp `WM_GETMINMAXINFO`'s max size/position to the monitor's work area so a
/// frameless maximize does not paint over the taskbar.
unsafe fn clamp_maxinfo(hwnd: HWND, lparam: LPARAM) {
    let mmi_ptr = lparam.0 as *mut MINMAXINFO;
    if mmi_ptr.is_null() {
        return;
    }
    // SAFETY: for `WM_GETMINMAXINFO` the OS passes `lparam` as a valid, aligned,
    // writable `MINMAXINFO*` (null-checked above) that lives for the duration of
    // the message; reborrowing it as `&mut` is sound and we hold no other alias.
    let mmi = unsafe { &mut *mmi_ptr };
    // SAFETY: `hwnd` is the live window per this fn's contract; the call only
    // reads it and returns the nearest-monitor handle (never null for a real HWND).
    let hmon: HMONITOR = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: core::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `hmon` is the handle just returned by `MonitorFromWindow`; `&mut mi`
    // is a live local with its `cbSize` set, which the call fills on success.
    if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
        let work = mi.rcWork;
        let mon = mi.rcMonitor;
        // Position is relative to the monitor origin. Safe field writes through the
        // `&mut MINMAXINFO` reborrowed above (no further raw-pointer dereferences).
        mmi.ptMaxPosition.x = work.left - mon.left;
        mmi.ptMaxPosition.y = work.top - mon.top;
        mmi.ptMaxSize.x = work.right - work.left;
        mmi.ptMaxSize.y = work.bottom - work.top;
        mmi.ptMaxTrackSize.x = work.right - work.left;
        mmi.ptMaxTrackSize.y = work.bottom - work.top;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_contains_is_half_open_and_multi_range() {
        // Two interactive ranges (e.g. the tab chip and the '+' affordance).
        let zones = [(96, 121), (140, 181)];
        // Inside the first range: HTCLIENT (interactive).
        assert!(zone_contains(96, &zones)); // left edge inclusive
        assert!(zone_contains(120, &zones));
        // The right edge is EXCLUSIVE so adjacent ranges never double-count.
        assert!(!zone_contains(121, &zones));
        // The gap between ranges stays draggable (HTCAPTION).
        assert!(!zone_contains(130, &zones));
        // Inside the second range.
        assert!(zone_contains(140, &zones));
        assert!(zone_contains(180, &zones));
        assert!(!zone_contains(181, &zones));
        // No zones published yet -> nothing is interactive (whole strip drags).
        assert!(!zone_contains(100, &[]));
    }

    #[test]
    fn set_interactive_zones_publishes_for_the_hit_test() {
        set_interactive_zones(vec![(10, 20), (30, 40)]);
        assert!(in_interactive_zone(15));
        assert!(in_interactive_zone(35));
        assert!(!in_interactive_zone(25));
        // Re-publishing replaces (does not accumulate) the previous frame.
        set_interactive_zones(vec![(100, 110)]);
        assert!(in_interactive_zone(105));
        assert!(!in_interactive_zone(15));
    }

    /// The chrome geometry the renderer publishes: a 30px title bar, an 8px
    /// resize band, and a 143px caption-button cluster on an 800x600 client.
    fn geom() -> SnapGeometry {
        SnapGeometry {
            titlebar_h: 30,
            resize_border: 8,
            buttons_w: 143,
        }
    }

    #[test]
    fn nc_hit_reports_every_resize_edge_and_corner() {
        let g = geom();
        // Corners take priority over the straight edges that also match.
        assert_eq!(classify_nc_hit(0, 0, 800, 600, &g, || false), HTTOPLEFT);
        assert_eq!(classify_nc_hit(799, 0, 800, 600, &g, || false), HTTOPRIGHT);
        assert_eq!(
            classify_nc_hit(0, 599, 800, 600, &g, || false),
            HTBOTTOMLEFT
        );
        assert_eq!(
            classify_nc_hit(799, 599, 800, 600, &g, || false),
            HTBOTTOMRIGHT
        );
        // Straight edges, away from the corners.
        assert_eq!(classify_nc_hit(400, 0, 800, 600, &g, || false), HTTOP);
        assert_eq!(classify_nc_hit(400, 599, 800, 600, &g, || false), HTBOTTOM);
        assert_eq!(classify_nc_hit(0, 300, 800, 600, &g, || false), HTLEFT);
        assert_eq!(classify_nc_hit(799, 300, 800, 600, &g, || false), HTRIGHT);
    }

    #[test]
    fn nc_hit_resize_band_is_exactly_resize_border_thick() {
        let g = geom();
        // 0..=7 is the band; 8 is already the body. Without this the window
        // would either be un-resizable or would steal terminal clicks.
        assert_eq!(classify_nc_hit(7, 300, 800, 600, &g, || false), HTLEFT);
        assert_ne!(classify_nc_hit(8, 300, 800, 600, &g, || false), HTLEFT);
        assert_eq!(classify_nc_hit(792, 300, 800, 600, &g, || false), HTRIGHT);
        assert_ne!(classify_nc_hit(791, 300, 800, 600, &g, || false), HTRIGHT);
    }

    #[test]
    fn titlebar_strip_drags_the_window_but_the_buttons_do_not() {
        let g = geom();
        // Below the resize band, above titlebar_h, left of the button cluster:
        // the drag strip. HTCAPTION is what gives a frameless window Aero Snap.
        assert_eq!(classify_nc_hit(400, 20, 800, 600, &g, || false), HTCAPTION);
        // The caption-button cluster (right 143px) must stay HTCLIENT so the
        // click reaches winit's MouseInput and our own min/max/close handler —
        // as HTCAPTION, Windows would swallow it as a window drag.
        assert_eq!(classify_nc_hit(700, 20, 800, 600, &g, || false), HTCLIENT);
        assert_eq!(
            classify_nc_hit(800 - 143, 20, 800, 600, &g, || false),
            HTCLIENT,
            "the cluster's left edge is already a button"
        );
        assert_eq!(
            classify_nc_hit(800 - 144, 20, 800, 600, &g, || false),
            HTCAPTION,
            "one pixel left of the cluster still drags"
        );
        // Below the title bar is the terminal.
        assert_eq!(classify_nc_hit(400, 30, 800, 600, &g, || false), HTCLIENT);
    }

    #[test]
    fn interactive_tab_strip_controls_are_client_not_caption() {
        let g = geom();
        // A tab chip / '+' / gear published by the renderer sits INSIDE the drag
        // strip but must not drag the window — otherwise the tab is unclickable.
        assert_eq!(
            classify_nc_hit(400, 20, 800, 600, &g, || true),
            HTCLIENT,
            "an interactive zone overrides the drag strip"
        );
        // The same point with no zone published drags, proving the override is
        // what changed the answer (and not the coordinates).
        assert_eq!(classify_nc_hit(400, 20, 800, 600, &g, || false), HTCAPTION);
    }

    #[test]
    fn interactive_zones_are_not_consulted_on_the_resize_edges() {
        // The zone lookup takes a mutex; the original only consulted it in the
        // body arm. Passing a closure that panics proves the edge paths never
        // call it, i.e. the lazy short-circuit is preserved.
        let g = geom();
        let boom =
            || -> bool { panic!("in_interactive_zone must not be consulted on a resize edge") };
        assert_eq!(classify_nc_hit(0, 0, 800, 600, &g, boom), HTTOPLEFT);
        assert_eq!(classify_nc_hit(400, 0, 800, 600, &g, boom), HTTOP);
        assert_eq!(classify_nc_hit(0, 300, 800, 600, &g, boom), HTLEFT);
    }

    #[test]
    fn a_click_past_the_titlebar_never_consults_the_zones_either() {
        // `py < titlebar_h` short-circuits before the zone lookup.
        let g = geom();
        let boom = || -> bool { panic!("zones must not be consulted below the title bar") };
        assert_eq!(classify_nc_hit(400, 300, 800, 600, &g, boom), HTCLIENT);
    }
}
