//! Windows system-tray icon for the SHIPPING C0PL4ND window (`egui_main`).
//!
//! ## What it does
//!
//! * Shows a tray icon (the sigil) with a tooltip.
//! * **Single LEFT click toggles minimize ⇄ restore** — the reported-missing
//!   behaviour. A restore reuses the Windows-11 foreground-lock `AttachThreadInput`
//!   dance so `SetForegroundWindow` is honoured (a minimized app otherwise
//!   restores *behind* the window the user was last in).
//! * **Right click** opens a context menu: Show C0PL4ND / Hide to tray / Quit.
//!
//! ## Why this is a binary-local module of `egui_main` (mirrors `win_chrome`)
//!
//! The tray needs the real eframe HWND *and* the running winit event loop, which
//! only the shipping binary has — so, exactly like `win_chrome`, it lives
//! physically under `egui_app/` but is declared with `#[path]` in `egui_main.rs`
//! and is **never** compiled into the `#[path]`-included kittest lib harnesses
//! (which have no real HWND / message loop).
//!
//! ## Events fire even while the window is hidden
//!
//! `tray-icon` delivers events to a global handler on the event-loop thread as
//! the winit message loop pumps them — NOT through per-frame egui polling. That
//! matters: a minimized/hidden window stops repainting, so a per-frame poll would
//! never see the click that is supposed to restore it. The handler does the raw
//! Win32 restore directly (independent of egui frames) and then requests a
//! repaint.
//!
//! ## unsafe
//!
//! `egui_main` is `#![deny(unsafe_code)]`; the audited Win32 FFI is quarantined in
//! the `#[cfg(windows)]` `imp` module behind its own `#![allow(unsafe_code)]`,
//! exactly like `win_foreground` / `win_chrome`. The PURE toggle/menu decision
//! logic lives OUTSIDE the FFI and is unit-tested on every host.

// Off Windows the `#[cfg(windows)] mod imp` FFI half is not compiled, so nothing
// in the PRODUCTION build calls this module's pure logic — the unit tests do, and
// they run on every host, but clippy also lints the non-test build and reports
// every item here as dead. That is a property of the platform, not a dormancy
// bug: on Windows the lint is FULLY ACTIVE, so a genuinely-unwired item is still
// caught on the platform where it must be wired. Scoped to `not(windows)` rather
// than a blanket allow for exactly that reason.
#![cfg_attr(not(windows), allow(dead_code))]

// ---------------------------------------------------------------------------
// PURE decision logic (compiled + tested on every host; used by the Windows imp)
// ---------------------------------------------------------------------------

/// Stable menu-item ids. Using fixed id STRINGS (rather than keeping the
/// `MenuItem` handles alive to compare) lets the pure [`classify_menu`] map an
/// incoming event id to an action with no muda types in the tested logic.
#[cfg(any(windows, test))]
const MENU_SHOW_ID: &str = "c0pl4nd.tray.show";
#[cfg(any(windows, test))]
const MENU_HIDE_ID: &str = "c0pl4nd.tray.hide";
#[cfg(any(windows, test))]
const MENU_QUIT_ID: &str = "c0pl4nd.tray.quit";

/// What a single tray LEFT-click should do, given whether the window is
/// currently hidden or minimized.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToggleAction {
    /// The window is out of view (minimized or tray-hidden) → bring it back.
    Restore,
    /// The window is visible → send it to the taskbar.
    Minimize,
}

/// The toggle decision: restore when the window is out of view, else minimize.
#[cfg(any(windows, test))]
#[must_use]
pub fn toggle_action(is_hidden_or_minimized: bool) -> ToggleAction {
    if is_hidden_or_minimized {
        ToggleAction::Restore
    } else {
        ToggleAction::Minimize
    }
}

/// A context-menu action.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuAction {
    /// Show / restore the window to the foreground.
    ShowRestore,
    /// Minimize the window.
    Hide,
    /// Quit the application (gracefully, via the existing close path).
    Quit,
}

/// Map an incoming menu-event id to its [`MenuAction`], or `None` for an id we
/// did not create (defensive — the global menu channel is process-wide).
#[cfg(any(windows, test))]
#[must_use]
pub fn classify_menu(clicked: &str, show: &str, hide: &str, quit: &str) -> Option<MenuAction> {
    if clicked == show {
        Some(MenuAction::ShowRestore)
    } else if clicked == hide {
        Some(MenuAction::Hide)
    } else if clicked == quit {
        Some(MenuAction::Quit)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Public entry points — driven from `egui_main` (the wiring seam)
// ---------------------------------------------------------------------------

/// Prime the tray with the real main-window handle (from
/// `CreationContext::window_handle()`), so the click/menu handlers can
/// minimize/restore the CORRECT window. Idempotent; a zero handle is ignored. A
/// no-op off Windows.
pub fn prime_hwnd(hwnd: isize) {
    #[cfg(windows)]
    imp::prime_hwnd(hwnd);
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// Create the system-tray icon + context menu and register its click/menu
/// handlers. Best-effort: a build failure (e.g. a headless/session-0 shell where
/// no tray is available) logs at WARN and leaves no tray — it NEVER panics and
/// never blocks startup, mirroring `load_app_icon`. A no-op off Windows.
///
/// Call this AFTER the window exists (from the eframe creation closure) and after
/// [`prime_hwnd`], on the event-loop thread.
pub fn init(ctx: &eframe::egui::Context, icon_rgba: Vec<u8>, width: u32, height: u32) {
    #[cfg(windows)]
    imp::init(ctx, icon_rgba, width, height);
    #[cfg(not(windows))]
    {
        let _ = (ctx, icon_rgba, width, height);
    }
}

// ---------------------------------------------------------------------------
// Windows FFI (the audited unsafe boundary)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    // The audited Win32 FFI is quarantined here with `// SAFETY:` justifications,
    // mirroring the other `#[cfg(windows)]` modules (win_foreground, win_chrome,
    // caption_close, job_object, dll_hardening).
    #![allow(unsafe_code)]

    use std::cell::RefCell;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        PostMessageW, SetForegroundWindow, ShowWindow, SW_MINIMIZE, SW_RESTORE, SW_SHOW, WM_CLOSE,
    };

    use super::{
        classify_menu, toggle_action, MenuAction, ToggleAction, MENU_HIDE_ID, MENU_QUIT_ID,
        MENU_SHOW_ID,
    };

    /// Cached main-window HWND (0 = not yet primed). C0PL4ND uses ONE OS window.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);

    thread_local! {
        /// Keeps the `TrayIcon` alive for the process lifetime — dropping it
        /// removes the icon. Bound to the event-loop thread (`TrayIcon` is
        /// `!Send`); `init` runs on that thread, so this is the only accessor.
        static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    }

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

    /// Build the tray icon + menu and wire the click/menu handlers.
    pub fn init(ctx: &eframe::egui::Context, icon_rgba: Vec<u8>, width: u32, height: u32) {
        let icon = match Icon::from_rgba(icon_rgba, width, height) {
            Ok(icon) => icon,
            Err(err) => {
                tracing::warn!(target: "c0pl4nd::tray", detail = ?err, "tray icon decode failed; no tray");
                return;
            }
        };

        // Right-click context menu. Stable ids let the pure `classify_menu` map
        // events back to actions without holding the item handles.
        let menu = Menu::new();
        let show = MenuItem::with_id(MENU_SHOW_ID, "Show C0PL4ND", true, None);
        let hide = MenuItem::with_id(MENU_HIDE_ID, "Hide to tray", true, None);
        let sep = PredefinedMenuItem::separator();
        let quit = MenuItem::with_id(MENU_QUIT_ID, "Quit C0PL4ND", true, None);
        for item in [
            &show as &dyn tray_icon::menu::IsMenuItem,
            &hide,
            &sep,
            &quit,
        ] {
            if let Err(err) = menu.append(item) {
                tracing::warn!(target: "c0pl4nd::tray", detail = ?err, "tray menu append failed");
            }
        }

        let tray = TrayIconBuilder::new()
            .with_tooltip("C0PL4ND")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            // Left click is our minimize/restore TOGGLE; the menu opens on RIGHT
            // click only (default is left — we turn that off).
            .with_menu_on_left_click(false)
            .build();
        let tray = match tray {
            Ok(tray) => tray,
            Err(err) => {
                tracing::warn!(target: "c0pl4nd::tray", detail = ?err, "tray icon build failed; no tray");
                return;
            }
        };
        TRAY.with(|slot| *slot.borrow_mut() = Some(tray));

        // A LEFT-click (release) toggles minimize/restore. Handled on the
        // event-loop thread as the message loop pumps the event — works even
        // while the window is minimized/hidden and egui has stopped repainting.
        let ctx_click = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window();
                ctx_click.request_repaint();
            }
        }));

        // Context-menu selections.
        let ctx_menu = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            match classify_menu(
                event.id().as_ref(),
                MENU_SHOW_ID,
                MENU_HIDE_ID,
                MENU_QUIT_ID,
            ) {
                Some(MenuAction::ShowRestore) => {
                    restore_main();
                    ctx_menu.request_repaint();
                }
                Some(MenuAction::Hide) => {
                    minimize_main();
                    ctx_menu.request_repaint();
                }
                Some(MenuAction::Quit) => request_quit_main(),
                None => {}
            }
        }));
    }

    /// Toggle the window between minimized/hidden and restored-to-foreground.
    fn toggle_window() {
        let Some(hwnd) = cached() else {
            return;
        };
        // SAFETY: `hwnd` is this process's own main window; `IsIconic` only reads
        // its minimized state.
        let iconic = unsafe { IsIconic(hwnd) }.as_bool();
        // SAFETY: same own handle; `IsWindowVisible` reads its visibility (a
        // tray-hidden window is not visible).
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        match toggle_action(iconic || !visible) {
            ToggleAction::Restore => restore_and_foreground(hwnd),
            ToggleAction::Minimize => minimize(hwnd),
        }
    }

    /// Restore the primed window and raise it to the foreground.
    fn restore_main() {
        if let Some(hwnd) = cached() {
            restore_and_foreground(hwnd);
        }
    }

    /// Minimize the primed window to the taskbar.
    fn minimize_main() {
        if let Some(hwnd) = cached() {
            minimize(hwnd);
        }
    }

    /// Minimize `hwnd` to the taskbar.
    fn minimize(hwnd: HWND) {
        // SAFETY: own main-window handle; `SW_MINIMIZE` only minimizes it.
        let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
    }

    /// Un-hide + un-minimize `hwnd` and pull it to the foreground.
    ///
    /// The foreground step mirrors `win_foreground`'s `AttachThreadInput` dance
    /// (Win11 refuses a raw `SetForegroundWindow` from a background process, so a
    /// restore would otherwise land *behind* the current foreground window).
    /// `win_foreground::force_foreground_main` is `pub(crate)` to the LIB crate
    /// and therefore unreachable from this binary-local module, so the proven
    /// pattern is reproduced here rather than called.
    fn restore_and_foreground(hwnd: HWND) {
        // SAFETY: own handle; `SW_SHOW` un-hides a window hidden to the tray.
        let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
        // SAFETY: own handle; `SW_RESTORE` un-minimizes to the previous size.
        let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };

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
            // SAFETY: detach the exact two thread ids attached above, restoring
            // the independent input queues. Non-fatal if it fails.
            unsafe {
                let _ = AttachThreadInput(fg_thread, our_thread, false);
            }
        }
    }

    /// Quit the app gracefully by posting `WM_CLOSE`, so the existing
    /// `frame_tick` fast-close path runs the real shutdown (persist config + reap
    /// every PTY child) rather than a hard `process::exit` that skips it. Falls
    /// back to `exit(0)` only if the window was never primed.
    fn request_quit_main() {
        let Some(hwnd) = cached() else {
            std::process::exit(0);
        };
        // SAFETY: own handle; posts a standard `WM_CLOSE` with no `lParam`.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

// ---------------------------------------------------------------------------
// Pure-logic tests (run on every host)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_restores_when_out_of_view_else_minimizes() {
        // The whole reported bug: a click must RESTORE a minimized/hidden window
        // and MINIMIZE a visible one.
        assert_eq!(toggle_action(true), ToggleAction::Restore);
        assert_eq!(toggle_action(false), ToggleAction::Minimize);
    }

    #[test]
    fn classify_menu_maps_each_id_and_ignores_unknown() {
        assert_eq!(
            classify_menu(MENU_SHOW_ID, MENU_SHOW_ID, MENU_HIDE_ID, MENU_QUIT_ID),
            Some(MenuAction::ShowRestore),
        );
        assert_eq!(
            classify_menu(MENU_HIDE_ID, MENU_SHOW_ID, MENU_HIDE_ID, MENU_QUIT_ID),
            Some(MenuAction::Hide),
        );
        assert_eq!(
            classify_menu(MENU_QUIT_ID, MENU_SHOW_ID, MENU_HIDE_ID, MENU_QUIT_ID),
            Some(MenuAction::Quit),
        );
        // A foreign id from the process-wide menu channel maps to nothing.
        assert_eq!(
            classify_menu(
                "some.other.app.item",
                MENU_SHOW_ID,
                MENU_HIDE_ID,
                MENU_QUIT_ID
            ),
            None,
        );
    }

    #[test]
    fn menu_ids_are_distinct() {
        // Guards a copy-paste that would collide two actions onto one id (which
        // would silently route, e.g., Quit to Show).
        assert_ne!(MENU_SHOW_ID, MENU_HIDE_ID);
        assert_ne!(MENU_HIDE_ID, MENU_QUIT_ID);
        assert_ne!(MENU_SHOW_ID, MENU_QUIT_ID);
    }
}
