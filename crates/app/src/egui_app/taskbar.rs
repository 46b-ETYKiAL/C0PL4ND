//! Windows taskbar-button integration for the egui shell: the live CONSUMER of
//! the two terminal effects the emulator already parses but the shipping shell
//! previously drained-and-discarded.
//!
//! - **`OSC 9 ; 4` taskbar progress** (C26) — a build tool (`cargo`, `npm`,
//!   `winget`, `apt`) streams `ESC ] 9 ; 4 ; state ; percent ST` and the app
//!   drives the taskbar button's progress segment from it, exactly as Windows
//!   Terminal does (crediting ConEmu). [`map_progress_state`] maps the terminal
//!   [`ProgressState`] onto a platform-independent [`TaskbarProgress`], and the
//!   Windows [`imp`] converts that to `ITaskbarList3::SetProgressState` /
//!   `SetProgressValue` (`TBPF_*`).
//! - **OSC 9 / OSC 777 notification** — the focused-suppression DECISION for the
//!   unfocused-activity taskbar-attention flash lives here as the pure
//!   [`should_request_attention`]; the flash itself is issued by the caller via
//!   `egui`'s `ViewportCommand::RequestUserAttention` (which, on Windows through
//!   winit, is `FlashWindowEx` — the portable, already-cross-platform seam).
//!
//! Lives as an `egui_app` submodule (mirroring `win_foreground` / `caption_close`
//! / `job_object`) so the `#[path=…]`-included test harnesses resolve it without
//! re-declaring it. The raw Win32 is quarantined in the `#[cfg(windows)]` [`imp`]
//! module (this crate is `deny(unsafe_code)`-clean at the binary; the lib
//! modules scope-`allow` audited FFI) — the shipping `egui_main.rs`
//! (`#![deny(unsafe_code)]`) never gains an `unsafe` block from this wiring.

use c0pl4nd_core::term::{Progress, ProgressState};

/// Pump → attention-flash wiring tests. In a `#[path]`-included file (not this
/// one's `mod tests`) because they drive the whole `C0pl4ndApp` rather than this
/// module's pure functions, and because they must live inside `egui_app` to
/// reach its private `pump_pane_effects` / `terminal_for_test`.
///
/// The desktop-TOAST half of the OSC 9 / OSC 777 story lives in the lib-root
/// [`crate::notify`] module, whose own tests cover the pure decisions and the
/// installer-AUMID correspondence.
#[cfg(test)]
#[path = "taskbar_wiring_tests.rs"]
mod taskbar_wiring_tests;

/// A platform-independent taskbar progress state — the mapping TARGET of an
/// `OSC 9 ; 4` [`ProgressState`]. Mirrors the Win32 `TBPFLAG` set 1:1 but is
/// defined HERE (not re-exported from the `windows` crate, which only compiles
/// on `cfg(windows)`) so [`map_progress_state`] is unit-testable on EVERY
/// platform — including the Linux CI headless build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskbarProgress {
    /// No progress segment — clears the indicator (`TBPF_NOPROGRESS`).
    None,
    /// Determinate green progress at `percent` (`TBPF_NORMAL`).
    Normal,
    /// Determinate red progress at `percent` (`TBPF_ERROR`).
    Error,
    /// Marching/indeterminate spinner; `percent` is ignored (`TBPF_INDETERMINATE`).
    Indeterminate,
    /// Determinate yellow/paused progress at `percent` (`TBPF_PAUSED`) — the
    /// correct target for the terminal's WARNING state ("warning/paused,
    /// typically shown yellow"), the fifth `TBPF_*` beyond the four determinate
    /// green/red/spinner/clear states.
    Paused,
}

/// Map a terminal `OSC 9 ; 4` [`ProgressState`] to the taskbar flag. Pure and
/// platform-independent — the SINGLE SOURCE OF TRUTH for the state mapping, so
/// the wire is unit-tested without a live window.
pub fn map_progress_state(state: ProgressState) -> TaskbarProgress {
    match state {
        ProgressState::Remove => TaskbarProgress::None,
        ProgressState::Normal => TaskbarProgress::Normal,
        ProgressState::Error => TaskbarProgress::Error,
        ProgressState::Indeterminate => TaskbarProgress::Indeterminate,
        ProgressState::Warning => TaskbarProgress::Paused,
    }
}

/// The progress report a single frame's worth of drained [`Progress`] should
/// leave on the (single) taskbar button: the LAST one wins, because a stream of
/// `OSC 9 ; 4` updates within one frame only makes the FINAL state visible on a
/// one-button indicator. `None` when nothing was drained (leave the button
/// untouched). Pure so "latest wins" is unit-testable.
pub fn latest_progress(drained: &[Progress]) -> Option<Progress> {
    drained.last().copied()
}

/// Whether an `OSC 9` / `OSC 777` desktop notification should raise a taskbar
/// attention flash: ONLY when a notification fired AND the window is unfocused.
/// `focused` is `None` before the first focus event (startup) — treated as
/// focused, so a notification at startup does not spuriously flash. Pure so the
/// focused-suppression is unit-testable without a live window.
pub fn should_request_attention(notified: bool, focused: Option<bool>) -> bool {
    notified && !focused.unwrap_or(true)
}

/// Prime this module with THIS process's main window handle (the SAME handle
/// `caption_close` / `win_foreground` cache), taken from eframe's
/// `CreationContext` in `mod.rs`. Idempotent; a zero handle is ignored.
/// Windows-only; a no-op elsewhere.
#[cfg(windows)]
pub(crate) fn set_main_hwnd(hwnd: isize) {
    imp::set_main_hwnd(hwnd);
}

/// No-op on non-Windows platforms. The sole caller (in `mod.rs`) is itself
/// `#[cfg(windows)]`-gated because it reads the Win32 raw window handle, so this
/// stub is never called off-Windows — `allow(dead_code)` keeps the symmetric
/// no-op API surface without tripping the `-D warnings` build.
#[cfg(not(windows))]
#[allow(dead_code)]
pub(crate) fn set_main_hwnd(_hwnd: isize) {}

/// Apply a taskbar progress state to the primed main window. On Windows this
/// drives `ITaskbarList3`; before the HWND is primed (headless tests / a
/// non-window build) it is a no-op. A no-op on non-Windows platforms. Under
/// `cfg(test)` it also records into [`test_spy`] so the pump→taskbar WIRE can be
/// asserted without a live window.
pub(crate) fn apply_progress(progress: TaskbarProgress, percent: u8) {
    #[cfg(test)]
    test_spy::record(progress, percent);
    // On Windows this runs in tests too, but no-ops because the HWND is never
    // primed in a headless test (so no COM call is made) — keeping `imp` live
    // (not dead code) while never touching a real taskbar from a unit test.
    #[cfg(windows)]
    imp::set_progress(progress, percent);
    #[cfg(not(windows))]
    {
        let _ = (progress, percent);
    }
}

/// Test-only spy that records the last [`apply_progress`] call so a wiring test
/// can prove the pump reached the taskbar seam (a test that called
/// [`map_progress_state`] directly would pass forever regardless of whether the
/// pump ever calls it).
#[cfg(test)]
pub(crate) mod test_spy {
    use std::sync::Mutex;

    use super::TaskbarProgress;

    static LAST: Mutex<Option<(TaskbarProgress, u8)>> = Mutex::new(None);

    /// Serialises the tests that share [`LAST`]. `cargo test` runs tests in
    /// PARALLEL threads, and the spy is a process-global `static` — without this
    /// guard two spy tests interleave `reset`/`record`/`take` and one of them
    /// reads the other's value (an order-dependent, parallel-only flake). Every
    /// test that touches the spy must hold this guard for its whole body.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Acquire the spy's serial guard. Poison-tolerant: a panicking spy test
    /// must not cascade into "all other spy tests fail with PoisonError", which
    /// would hide the ONE real failure behind a wall of noise.
    pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record the most recent progress application.
    pub(crate) fn record(progress: TaskbarProgress, percent: u8) {
        *LAST.lock().unwrap() = Some((progress, percent));
    }

    /// Take (and clear) the most recent recorded application.
    pub(crate) fn take() -> Option<(TaskbarProgress, u8)> {
        LAST.lock().unwrap().take()
    }

    /// Clear any recorded application (call at the START of a wiring test so a
    /// prior test's record cannot leak in via the shared `static`).
    pub(crate) fn reset() {
        *LAST.lock().unwrap() = None;
    }
}

#[cfg(windows)]
mod imp {
    // The audited Win32 COM FFI is quarantined here with `// SAFETY:`
    // justifications, mirroring the other `#[cfg(windows)]` modules
    // (caption_close, job_object, win_foreground, dll_hardening).
    #![allow(unsafe_code)]

    use std::cell::RefCell;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        ITaskbarList3, TaskbarList, TBPFLAG, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS,
        TBPF_NORMAL, TBPF_PAUSED,
    };

    use super::TaskbarProgress;

    /// Cached main-window HWND (0 = not yet primed). C0PL4ND uses ONE OS window.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);

    thread_local! {
        /// The shell `ITaskbarList3`, created once per UI thread. COM objects are
        /// apartment-bound; the pump ALWAYS runs on the eframe UI thread, so a
        /// `thread_local` (not a `static`, which would need `Send + Sync`) is the
        /// correct home and never crosses an apartment boundary.
        static TASKBAR: RefCell<Option<ITaskbarList3>> = const { RefCell::new(None) };
    }

    pub fn set_main_hwnd(hwnd: isize) {
        if hwnd != 0 {
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
    }

    fn to_flag(progress: TaskbarProgress) -> TBPFLAG {
        match progress {
            TaskbarProgress::None => TBPF_NOPROGRESS,
            TaskbarProgress::Normal => TBPF_NORMAL,
            TaskbarProgress::Error => TBPF_ERROR,
            TaskbarProgress::Indeterminate => TBPF_INDETERMINATE,
            TaskbarProgress::Paused => TBPF_PAUSED,
        }
    }

    /// Run `f` against the lazily-created `ITaskbarList3` for this thread.
    /// Returns `None` (a clean no-op — never a panic) when COM cannot produce the
    /// interface: a headless / RDP / not-yet-ready shell simply shows no taskbar
    /// progress rather than crashing the terminal.
    fn with_taskbar<R>(f: impl FnOnce(&ITaskbarList3) -> R) -> Option<R> {
        TASKBAR.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                // winit already OleInitializes the event-loop thread (an STA); a
                // second init returns S_FALSE / RPC_E_CHANGED_MODE, which is fine —
                // we only need the apartment to exist, so the HRESULT is ignored.
                // SAFETY: null reserved pointer; per-thread and idempotent.
                let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                // SAFETY: instantiates the well-known `TaskbarList` coclass; a null
                // aggregating outer (`None`) and the standard in-proc context. The
                // returned interface is owned (ref-counted) by `tb`.
                let created: windows::core::Result<ITaskbarList3> =
                    unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_ALL) };
                if let Ok(tb) = created {
                    // `HrInit` MUST be called once before any other method.
                    // SAFETY: freshly-created, live interface; `HrInit` initializes
                    // the object's internal state and touches no caller memory.
                    if unsafe { tb.HrInit() }.is_ok() {
                        *slot = Some(tb);
                    }
                }
            }
            slot.as_ref().map(f)
        })
    }

    pub fn set_progress(progress: TaskbarProgress, percent: u8) {
        let hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            return; // not primed (headless test / non-window build) — nothing to drive.
        }
        let target = HWND(hwnd as *mut core::ffi::c_void);
        let flag = to_flag(progress);
        with_taskbar(|tb| {
            // SAFETY: `target` is this process's own main top-level window handle,
            // primed from eframe's `CreationContext`; `tb` is a live
            // `ITaskbarList3`. The return value is intentionally ignored — a failed
            // state update is best-effort and never fatal to the terminal.
            unsafe {
                let _ = tb.SetProgressState(target, flag);
            }
            // Only the determinate states carry a value; NOPROGRESS clears and
            // INDETERMINATE animates a marquee, both ignoring `percent`.
            if matches!(
                progress,
                TaskbarProgress::Normal | TaskbarProgress::Error | TaskbarProgress::Paused
            ) {
                // SAFETY: same live interface + own-window handle; `percent`/100 is
                // a valid completion fraction. Best-effort; return ignored.
                unsafe {
                    let _ = tb.SetProgressValue(target, u64::from(percent), 100u64);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_progress_state_covers_every_variant() {
        // The full ProgressState -> TaskbarProgress truth table. A wrong or
        // missing arm (e.g. Warning silently folded into Normal) fails here.
        assert_eq!(
            map_progress_state(ProgressState::Remove),
            TaskbarProgress::None
        );
        assert_eq!(
            map_progress_state(ProgressState::Normal),
            TaskbarProgress::Normal
        );
        assert_eq!(
            map_progress_state(ProgressState::Error),
            TaskbarProgress::Error
        );
        assert_eq!(
            map_progress_state(ProgressState::Indeterminate),
            TaskbarProgress::Indeterminate
        );
        // Warning maps to Paused (yellow), NOT Normal — the mapping that would
        // silently swallow the warning state if it were wrong.
        assert_eq!(
            map_progress_state(ProgressState::Warning),
            TaskbarProgress::Paused
        );
    }

    #[test]
    fn latest_progress_takes_the_last_and_handles_empty() {
        assert_eq!(latest_progress(&[]), None, "empty drain leaves the button");
        let stream = [
            Progress {
                state: ProgressState::Normal,
                percent: 10,
            },
            Progress {
                state: ProgressState::Normal,
                percent: 90,
            },
            Progress {
                state: ProgressState::Remove,
                percent: 0,
            },
        ];
        assert_eq!(
            latest_progress(&stream),
            Some(Progress {
                state: ProgressState::Remove,
                percent: 0
            }),
            "the LAST report in a frame wins on the one-button indicator"
        );
    }

    #[test]
    fn should_request_attention_only_when_notified_and_unfocused() {
        // Unfocused + notified → flash.
        assert!(should_request_attention(true, Some(false)));
        // Focused → suppressed even when notified.
        assert!(!should_request_attention(true, Some(true)));
        // No notification → never flash regardless of focus.
        assert!(!should_request_attention(false, Some(false)));
        assert!(!should_request_attention(false, Some(true)));
        // Startup (focus unknown = None) is treated as focused → suppressed, so a
        // notification during launch does not spuriously flash.
        assert!(!should_request_attention(true, None));
        assert!(!should_request_attention(false, None));
    }

    #[test]
    fn apply_progress_reaches_the_seam_spy() {
        // Proves apply_progress routes to the recording seam the pump wiring test
        // observes. (The pump→apply_progress wire itself is asserted in mod.rs's
        // taskbar_wiring_tests, driving the REAL pump.)
        let _guard = test_spy::serial();
        test_spy::reset();
        apply_progress(TaskbarProgress::Error, 73);
        assert_eq!(test_spy::take(), Some((TaskbarProgress::Error, 73)));
    }
}
