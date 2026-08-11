//! Single-instance guard + argv forwarding for the SHIPPING C0PL4ND window.
//!
//! ## Why this exists
//!
//! Every `c0pl4nd.exe` invocation used to start a whole new process with its own
//! window, GPU device and shell. That was survivable while the only way to launch
//! was a shortcut — but the Explorer **"Open C0PL4ND here"** verb ships now
//! (`c0pl4nd.exe --cwd "%V"`), so right-clicking five folders means five separate
//! C0PL4ND windows. Every other terminal (Windows Terminal, iTerm2, Kitty) opens
//! a TAB in the running instance instead.
//!
//! So: the first process claims a named mutex and becomes the **primary**. A
//! later process finds the mutex already held, hands its argv to the primary over
//! `WM_COPYDATA`, and exits. The primary opens a pane for the forwarded `--cwd`
//! and raises itself.
//!
//! ## Shape
//!
//! ```text
//!   second c0pl4nd.exe                     running c0pl4nd.exe (primary)
//!   ------------------                     -----------------------------
//!   CreateMutexW -> ERROR_ALREADY_EXISTS
//!   EnumWindows + GetPropW  ------------->  window carrying HWND_PROP
//!   SendMessageW(WM_COPYDATA, argv) ----->  subclass proc
//!   exit(0)                                   decode argv
//!                                             push_forwarded_launch(cwd)
//!                                             restore + foreground
//!                                          frame_tick -> new pane
//! ```
//!
//! **Discovery is by window PROPERTY, not by title or class.** The title is
//! user-visible and the class is winit-generated; a property we set ourselves is
//! the only marker that is both ours and stable.
//!
//! ## Why a subclass on the real window rather than a message-only window
//!
//! Same reason `quake` subclasses: the app already owns exactly one HWND that the
//! winit message loop pumps, and the handler needs that HWND anyway (to restore
//! and raise it). A second, message-only window would add a window class
//! registration and a lifetime to manage for no gain.
//!
//! ## Why this is a binary-local module of `egui_main` (mirrors `tray`/`quake`)
//!
//! It needs the real eframe HWND *and* the running winit event loop, so — exactly
//! like `win_chrome`, `tray` and `quake` — it lives physically under `egui_app/`
//! but is declared with `#[path]` in `egui_main.rs` and is **never** compiled into
//! the `#[path]`-included kittest lib harnesses (which have no real HWND or
//! message loop).
//!
//! ## unsafe
//!
//! `egui_main` is `#![deny(unsafe_code)]`; the audited Win32 FFI is quarantined in
//! the `#[cfg(windows)]` `imp` module behind its own `#![allow(unsafe_code)]`,
//! exactly like `tray` / `quake` / `win_foreground`. The PURE decision + codec
//! logic lives OUTSIDE the FFI and is unit-tested on every host.

// Off Windows the `#[cfg(windows)] mod imp` FFI half is not compiled, so nothing
// in the PRODUCTION build calls this module's pure logic — the unit tests do, and
// they run on every host, but clippy also lints the non-test build and reports
// every item here as dead. That is a property of the platform, not a dormancy
// bug: on Windows the lint is FULLY ACTIVE, so a genuinely-unwired item is still
// caught on the platform where it must be wired. Scoped to `not(windows)` rather
// than a blanket allow for exactly that reason (mirrors `tray`).
#![cfg_attr(not(windows), allow(dead_code))]

// ---------------------------------------------------------------------------
// PURE decision + codec logic (compiled + tested on every host)
// ---------------------------------------------------------------------------

/// The single-instance mutex name.
///
/// `Local\` (the per-SESSION namespace), deliberately NOT `Global\`: with a
/// machine-wide mutex a second USER's launch on the same box would find the
/// mutex held and forward its directory into the FIRST user's session — handing
/// one user's argv to another user's window. Per-session is both the correct
/// scope for a desktop app and the safe one.
pub const MUTEX_NAME: &str = r"Local\com.itashacorp.c0pl4nd.single-instance";

/// The window property that marks the primary's window.
///
/// Discovery keys on this rather than on the window TITLE (user-visible, and the
/// shell retitles it) or the window CLASS (winit-generated, shared with any other
/// winit app in the same process family).
pub const HWND_PROP: &str = "com.itashacorp.c0pl4nd.primary";

/// Which instance this process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// No other instance held the mutex — open the window and serve forwards.
    Primary,
    /// Another instance is already running; we handed off and should exit.
    Secondary,
}

/// The role implied by the mutex acquisition. `CreateMutexW` succeeds either way;
/// `ERROR_ALREADY_EXISTS` is the ONLY signal that distinguishes the two, which is
/// why this is a decision worth naming and testing rather than an inline `if`.
#[must_use]
pub fn role_for(mutex_already_existed: bool) -> Role {
    if mutex_already_existed {
        Role::Secondary
    } else {
        Role::Primary
    }
}

/// What became of a secondary's attempt to hand its argv over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    /// The primary accepted the argv; this process should exit quietly.
    Forwarded,
    /// The mutex was held but no primary window could be found. See
    /// [`role_after_handoff`].
    NoPrimaryFound,
}

/// The role a secondary ends up with after trying to hand off.
///
/// A failed handoff falls back to [`Role::Primary`] — this process opens its own
/// window. That is deliberate and load-bearing: the mutex is claimed the instant
/// the first process starts, but the window property cannot exist until its
/// window does, so a launch during that startup gap finds the mutex held and no
/// window to talk to. Exiting there would mean a double-click that opens NOTHING
/// — a far worse failure than briefly having two windows. The same fallback
/// covers a primary that is wedged or mid-shutdown.
#[must_use]
pub fn role_after_handoff(handoff: Handoff) -> Role {
    match handoff {
        Handoff::Forwarded => Role::Secondary,
        Handoff::NoPrimaryFound => Role::Primary,
    }
}

/// The separator between forwarded arguments.
///
/// NUL cannot occur inside a Windows command-line argument (it terminates the
/// command line itself), so it is the one byte that can never appear in the data
/// being framed — unlike a space or a tab, which appear in real paths
/// (`C:\Program Files\...`) and would split one argument into two.
const ARG_SEP: u8 = 0;

/// Frame an argv for transport over `WM_COPYDATA`.
///
/// `WM_COPYDATA` carries an opaque byte block, so the argument boundaries have to
/// survive the trip explicitly.
#[must_use]
pub fn encode_argv(args: &[String]) -> Vec<u8> {
    args.join(std::str::from_utf8(&[ARG_SEP]).unwrap_or("\0"))
        .into_bytes()
}

/// Recover an argv framed by [`encode_argv`].
///
/// Lossy on invalid UTF-8 rather than failing: the payload arrives from another
/// process, and a mangled byte must degrade to a mangled path (which the `--cwd`
/// validator then rejects with a clear message) instead of discarding a launch
/// the user made. An empty payload is an empty argv, not one empty argument.
#[must_use]
pub fn decode_argv(bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes
        .split(|b| *b == ARG_SEP)
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// The directory a forwarded launch is asking for, or `None` for a plain launch.
///
/// Delegates to the EXISTING `--cwd` / `-d` parser rather than re-reading argv:
/// the flag's spelling, its alias, and its validation (must exist, must be a
/// directory) then have exactly one definition, and a forwarded launch is
/// validated by the same code as a direct one. A malformed value is dropped to
/// `None` — the forwarding path has no console and no dialog to complain
/// through, so the pane opens in the default directory rather than not at all.
#[must_use]
pub fn forwarded_cwd(args: &[String]) -> Option<String> {
    match crate::cli_cwd::parse_startup_cwd(args) {
        Ok(Some(dir)) => Some(dir.to_string_lossy().into_owned()),
        Ok(None) | Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Public entry points — driven from `egui_main` (the wiring seam)
// ---------------------------------------------------------------------------

/// Claim the single-instance mutex, or hand `args` to the instance that already
/// holds it.
///
/// Returns [`Role::Secondary`] ONLY when another instance accepted the argv, in
/// which case `main` should return immediately without opening a window. Every
/// other outcome — including every Win32 failure — returns [`Role::Primary`], so
/// a broken guard degrades to today's behaviour (a second window) rather than to
/// a launch that silently does nothing. Always [`Role::Primary`] off Windows.
pub fn acquire_or_forward(args: &[String]) -> Role {
    #[cfg(windows)]
    {
        imp::acquire_or_forward(args)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Role::Primary
    }
}

/// Mark this process's window as the primary and start accepting forwarded
/// launches on it. Idempotent; a zero handle is ignored. A no-op off Windows.
///
/// Call this AFTER the window exists (from the eframe creation closure), on the
/// event-loop thread.
pub fn install(ctx: &eframe::egui::Context, hwnd: isize) {
    #[cfg(windows)]
    imp::install(ctx, hwnd);
    #[cfg(not(windows))]
    {
        let _ = (ctx, hwnd);
    }
}

// ---------------------------------------------------------------------------
// Windows FFI (the audited unsafe boundary)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    // The audited Win32 FFI is quarantined here with `// SAFETY:` justifications,
    // mirroring the other `#[cfg(windows)]` modules (tray, quake, win_foreground,
    // win_chrome, caption_close, job_object, dll_hardening).
    #![allow(unsafe_code)]

    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::OnceLock;

    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
    };
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetForegroundWindow, GetPropW, GetWindowThreadProcessId,
        IsIconic, SendMessageW, SetForegroundWindow, SetPropW, ShowWindow, SW_RESTORE, SW_SHOW,
        WM_COPYDATA, WM_NCDESTROY,
    };

    use super::{
        decode_argv, encode_argv, forwarded_cwd, role_after_handoff, role_for, Handoff, Role,
        HWND_PROP, MUTEX_NAME,
    };

    /// A stable, arbitrary subclass id for our single subclass entry. Distinct
    /// from `win_chrome`'s and `quake`'s so all three coexist on the one window.
    const SUBCLASS_ID: usize = 0x00C0_51DE;

    /// Our `WM_COPYDATA` payload tag. `dwData` is caller-defined; checking it
    /// means a `WM_COPYDATA` from any OTHER source is passed straight through to
    /// the next window procedure rather than being decoded as an argv.
    const COPYDATA_TAG: usize = 0x00C0_1A00;

    /// How long a secondary keeps looking for the primary's window before giving
    /// up and opening its own. Covers the startup gap between the primary
    /// claiming the mutex and its window existing.
    const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
    /// Poll interval while waiting for the primary's window to appear.
    const DISCOVERY_POLL: std::time::Duration = std::time::Duration::from_millis(50);

    /// Cached main-window HWND (0 = not yet primed). C0PL4ND uses ONE OS window.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);

    /// The egui context, so a forwarded launch can wake a repaint. A `OnceLock`
    /// rather than a captured closure because a subclass proc is a bare
    /// `extern "system"` fn and cannot capture; `egui::Context` is
    /// `Send + Sync + Clone` (the same reasoning as `quake`).
    static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

    /// The single-instance mutex handle, held for the life of the process.
    ///
    /// Parked in a `OnceLock` purely to OWN it: Windows releases a mutex when the
    /// last handle closes, so dropping this would free the name and let a third
    /// process believe it was the primary. Never read back.
    static MUTEX: OnceLock<OwnedHandle> = OnceLock::new();

    /// A `HANDLE` that is closed when the process ends rather than leaked
    /// outright, and that is `Send + Sync` so it can live in a `static`.
    struct OwnedHandle(HANDLE);

    // SAFETY: a Win32 mutex HANDLE is a kernel object usable from any thread;
    // this wrapper only stores it and closes it once, so sharing the value across
    // threads cannot introduce a data race.
    unsafe impl Send for OwnedHandle {}
    // SAFETY: as above — the handle is never mutated after construction.
    unsafe impl Sync for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the handle `CreateMutexW` returned to us and is
            // closed exactly once, here, when the process tears the static down.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Claim the mutex, or forward `args` to whoever already holds it.
    pub fn acquire_or_forward(args: &[String]) -> Role {
        let name = HSTRING::from(MUTEX_NAME);
        // SAFETY: creates (or opens) a named mutex. Null security attributes =
        // default descriptor; `false` = do not take initial ownership (we only
        // need the name's EXISTENCE as the signal, never the lock itself, so we
        // can never deadlock on it). `name` outlives the call.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) };
        let Ok(handle) = handle else {
            // Cannot tell primary from secondary — degrade to opening our own
            // window rather than exiting on a launch the user asked for.
            tracing::warn!(
                target: "c0pl4nd::single_instance",
                "could not create the single-instance mutex; running as an independent window"
            );
            return Role::Primary;
        };
        // SAFETY: reads this thread's last-error code, set by the call above.
        // Must be read BEFORE any other Win32 call can overwrite it.
        let already = unsafe { windows::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS;

        match role_for(already) {
            Role::Primary => {
                // Hold the handle for the process lifetime so the name stays
                // claimed. If the slot is somehow already filled, close ours.
                if MUTEX.set(OwnedHandle(handle)).is_err() {
                    // SAFETY: our own freshly-created handle, closed once because
                    // another already occupies the slot.
                    let _ = unsafe { CloseHandle(handle) };
                }
                Role::Primary
            }
            Role::Secondary => {
                // SAFETY: our own handle; we are about to hand off and exit, and
                // the PRIMARY's separate handle keeps the name claimed.
                let _ = unsafe { CloseHandle(handle) };
                role_after_handoff(forward(args))
            }
        }
    }

    /// Find the primary's window and hand it `args`.
    fn forward(args: &[String]) -> Handoff {
        let deadline = std::time::Instant::now() + DISCOVERY_TIMEOUT;
        loop {
            if let Some(hwnd) = find_primary_window() {
                if send_argv(hwnd, args) {
                    return Handoff::Forwarded;
                }
                // The window answered that it did not take the payload (it is
                // mid-shutdown, or a stale property outlived its window). Opening
                // our own window is better than dropping the launch.
                return Handoff::NoPrimaryFound;
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    target: "c0pl4nd::single_instance",
                    "another instance holds the lock but exposed no window; opening an independent window"
                );
                return Handoff::NoPrimaryFound;
            }
            std::thread::sleep(DISCOVERY_POLL);
        }
    }

    /// The HWND found during the current [`find_primary_window`] scan.
    static FOUND: AtomicIsize = AtomicIsize::new(0);

    /// Locate the window carrying our primary marker property.
    fn find_primary_window() -> Option<HWND> {
        FOUND.store(0, Ordering::SeqCst);
        // SAFETY: enumerates top-level windows, invoking `enum_proc` for each.
        // The callback is a plain `extern "system"` fn that touches only a
        // `static` atomic and read-only Win32 queries; `lparam` is unused.
        let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(0)) };
        let raw = FOUND.load(Ordering::SeqCst);
        (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
    }

    /// `EnumWindows` callback: stop at the first window carrying [`HWND_PROP`].
    unsafe extern "system" fn enum_proc(hwnd: HWND, _lparam: LPARAM) -> windows::core::BOOL {
        let prop = HSTRING::from(HWND_PROP);
        // SAFETY: `hwnd` is supplied by `EnumWindows` and is valid for the
        // duration of the callback; `GetPropW` only reads a property off it and
        // returns a null handle when the property is absent. `prop` outlives the
        // call.
        let found = unsafe { GetPropW(hwnd, PCWSTR(prop.as_ptr())) };
        // Absent property, or a marker whose value is not this window's own
        // handle (a stale or copied property) — keep looking.
        if found.is_invalid() || found.0 != hwnd.0 {
            return true.into();
        }
        FOUND.store(hwnd.0 as isize, Ordering::SeqCst);
        false.into() // stop
    }

    /// Send `args` to `hwnd` over `WM_COPYDATA`. Returns whether it was accepted.
    fn send_argv(hwnd: HWND, args: &[String]) -> bool {
        let payload = encode_argv(args);
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_TAG,
            cbData: u32::try_from(payload.len()).unwrap_or(u32::MAX),
            lpData: payload.as_ptr() as *mut core::ffi::c_void,
        };
        // SAFETY: a SYNCHRONOUS `SendMessage`, which is what `WM_COPYDATA`
        // REQUIRES — the receiver may only read `lpData` for the duration of the
        // call, and `payload` is alive across it because this call blocks until
        // the receiver returns. (`PostMessageW` here would hand the other process
        // a dangling pointer the instant this function returned.)
        let r = unsafe {
            SendMessageW(
                hwnd,
                WM_COPYDATA,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const cds as isize)),
            )
        };
        drop(payload);
        r.0 != 0
    }

    /// Mark our window as the primary and subclass it to accept forwards.
    pub fn install(ctx: &eframe::egui::Context, hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        // Only the FIRST install does anything — `OnceLock::set` failing means a
        // previous call already primed us, so the subclass must not be added
        // twice (two entries would decode each payload twice and open two panes).
        if CTX.set(ctx.clone()).is_err() {
            return;
        }
        CACHED_HWND.store(hwnd, Ordering::Relaxed);
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let prop = HSTRING::from(HWND_PROP);
        // The property's VALUE is the window's own handle. It has to be non-null
        // (`GetPropW` reports absence as a null handle, so a null value would be
        // indistinguishable from "no property"), and using the HWND rather than
        // an arbitrary sentinel lets `enum_proc` verify the marker actually
        // belongs to the window carrying it.
        //
        // SAFETY: our own main window; sets one property on it.
        if let Err(err) = unsafe { SetPropW(h, PCWSTR(prop.as_ptr()), Some(HANDLE(h.0))) } {
            tracing::warn!(
                target: "c0pl4nd::single_instance",
                detail = ?err,
                "could not mark the primary window; later launches will open their own"
            );
            return;
        }
        // SAFETY: adds an ADDITIVE subclass entry on our own window with a
        // process-unique id, chaining to whatever is already installed
        // (`win_chrome`'s and `quake`'s entries are untouched). Removed on
        // `WM_NCDESTROY` below.
        let ok = unsafe { SetWindowSubclass(h, Some(subclass_proc), SUBCLASS_ID, 0) }.as_bool();
        if !ok {
            tracing::warn!(
                target: "c0pl4nd::single_instance",
                "could not subclass the primary window; later launches will open their own"
            );
        }
    }

    /// Window procedure: accept a forwarded argv, queue it, and raise the window.
    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _ref_data: usize,
    ) -> LRESULT {
        match msg {
            WM_COPYDATA => {
                // SAFETY: for `WM_COPYDATA` the OS guarantees `lparam` points to
                // a `COPYDATASTRUCT` that stays valid for this call.
                let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
                if cds.dwData != COPYDATA_TAG {
                    // Not ours — let the rest of the chain see it untouched.
                    // SAFETY: forwards to the next procedure with the original,
                    // unmodified arguments.
                    return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
                }
                let bytes = if cds.lpData.is_null() || cds.cbData == 0 {
                    Vec::new()
                } else {
                    // SAFETY: the sender used a synchronous `SendMessage`, so its
                    // buffer is alive for this call; `cbData` is its length. The
                    // bytes are COPIED here and nothing borrows `lpData` after.
                    unsafe {
                        std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize)
                            .to_vec()
                    }
                };
                let args = decode_argv(&bytes);
                // Queue only — the sending process is BLOCKED in `SendMessage`
                // until this returns, so the pane is opened by the next
                // `frame_tick` rather than here.
                c0pl4nd::egui_app::push_forwarded_launch(forwarded_cwd(&args));
                restore_and_foreground(hwnd);
                if let Some(ctx) = CTX.get() {
                    ctx.request_repaint();
                }
                LRESULT(1) // accepted
            }
            WM_NCDESTROY => {
                // SAFETY: removes OUR subclass entry (matched by id) before the
                // window goes away, leaving other entries in the chain intact.
                let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
                // SAFETY: the rest of the chain must still see WM_NCDESTROY.
                unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
            }
            // SAFETY: every other message is passed through untouched.
            _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
        }
    }

    /// Un-minimize `hwnd` and pull it to the foreground.
    ///
    /// Mirrors `tray`'s restore: Win11's foreground lock refuses a raw
    /// `SetForegroundWindow`, so the `AttachThreadInput` dance is required or the
    /// forwarded launch would open a pane in a window that stays buried.
    fn restore_and_foreground(hwnd: HWND) {
        // SAFETY: own handle; `SW_SHOW` un-hides a window hidden to the tray.
        let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
        // SAFETY: own handle; reads whether it is currently minimized.
        if unsafe { IsIconic(hwnd) }.as_bool() {
            // SAFETY: own handle; restores it to its previous size.
            let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
        }
        // SAFETY: borrows nothing; returns the current foreground window
        // (possibly null) by value.
        let fg = unsafe { GetForegroundWindow() };
        // SAFETY: reads the owning thread id of `fg` (0 for a null handle); the
        // process-id out-param is `None`, so nothing is written back.
        let fg_thread = unsafe { GetWindowThreadProcessId(fg, None) };
        // SAFETY: returns THIS thread's id; borrows no memory.
        let our_thread = unsafe { GetCurrentThreadId() };
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
}

// ---------------------------------------------------------------------------
// Pure-logic tests (run on every host)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_is_secondary_only_when_the_mutex_already_existed() {
        // The whole guard turns on this one bit. Inverting it would either make
        // every launch a secondary (the app could never start) or every launch a
        // primary (the feature silently does nothing).
        assert_eq!(
            role_for(false),
            Role::Primary,
            "a free mutex name means WE are the first instance"
        );
        assert_eq!(
            role_for(true),
            Role::Secondary,
            "an already-held mutex name means someone else is the primary"
        );
    }

    #[test]
    fn a_failed_handoff_falls_back_to_opening_our_own_window() {
        // The startup-gap case: the mutex is claimed before the primary's window
        // exists, so a launch in that window finds no one to talk to. Exiting
        // there would mean a double-click that opens NOTHING.
        assert_eq!(
            role_after_handoff(Handoff::Forwarded),
            Role::Secondary,
            "an accepted handoff means we exit quietly"
        );
        assert_eq!(
            role_after_handoff(Handoff::NoPrimaryFound),
            Role::Primary,
            "a launch that reached nobody must open its own window, never vanish"
        );
    }

    #[test]
    fn argv_round_trips_through_the_wire_format() {
        for case in [
            vec![],
            vec!["c0pl4nd.exe".to_string()],
            vec![
                "c0pl4nd.exe".to_string(),
                "--cwd".to_string(),
                "C:\\p".to_string(),
            ],
        ] {
            assert_eq!(
                decode_argv(&encode_argv(&case)),
                case,
                "round trip {case:?}"
            );
        }
    }

    #[test]
    fn a_path_containing_spaces_survives_as_one_argument() {
        // The reason the separator is NUL and not whitespace: `%V` from Explorer
        // is routinely `C:\Program Files\...`, and splitting on spaces would turn
        // one directory into two arguments and lose the launch.
        let args = vec![
            "c0pl4nd.exe".to_string(),
            "--cwd".to_string(),
            r"C:\Program Files\Some App".to_string(),
        ];
        let back = decode_argv(&encode_argv(&args));
        assert_eq!(
            back, args,
            "a path with spaces must survive the wire format intact"
        );
        assert_eq!(back.len(), 3, "the spaced path stayed ONE argument");
    }

    #[test]
    fn non_ascii_paths_survive_the_wire_format() {
        // Japanese/accented directory names are ordinary on a real desktop.
        let args = vec![
            "c0pl4nd.exe".to_string(),
            "--cwd".to_string(),
            r"C:\ユーザー\プロジェクト".to_string(),
        ];
        assert_eq!(decode_argv(&encode_argv(&args)), args);
    }

    #[test]
    fn an_empty_payload_decodes_to_no_arguments() {
        // Distinct from ONE empty argument: `[""]` would make `forwarded_cwd`
        // parse a bogus argv rather than treating it as a plain launch.
        assert!(
            decode_argv(&[]).is_empty(),
            "an empty payload is NO arguments, not one empty argument"
        );
    }

    #[test]
    fn a_mangled_payload_degrades_instead_of_dropping_the_launch() {
        // The bytes come from another process. A lone 0xFF is not valid UTF-8;
        // it must become a replacement character (which the `--cwd` validator
        // then refuses) rather than panicking in a window procedure.
        let decoded = decode_argv(&[b'a', 0xFF, b'b']);
        assert_eq!(decoded.len(), 1, "no separator, so one argument");
        assert!(decoded[0].contains('\u{FFFD}'), "invalid bytes replaced");
    }

    #[test]
    fn forwarded_cwd_is_none_for_a_plain_launch() {
        assert_eq!(forwarded_cwd(&["c0pl4nd.exe".to_string()]), None);
    }

    #[test]
    fn forwarded_cwd_reads_a_real_directory_from_the_shipped_flag() {
        // Proves this reuses the SAME `--cwd` parser the direct launch path uses,
        // rather than a second copy that could drift from it.
        let dir = std::env::temp_dir();
        let args = vec![
            "c0pl4nd.exe".to_string(),
            "--cwd".to_string(),
            dir.to_string_lossy().into_owned(),
        ];
        assert!(
            forwarded_cwd(&args).is_some(),
            "an existing directory must be forwarded"
        );
    }

    #[test]
    fn forwarded_cwd_drops_a_bad_directory_rather_than_failing_the_launch() {
        // A secondary has no console and no dialog: a refused path must open the
        // pane in the default directory, never abort the forwarded launch.
        let args = vec![
            "c0pl4nd.exe".to_string(),
            "--cwd".to_string(),
            r"Z:\definitely\not\here\at\all".to_string(),
        ];
        assert_eq!(
            forwarded_cwd(&args),
            None,
            "a nonexistent directory must be dropped, not forwarded as-is"
        );
    }

    #[test]
    fn the_mutex_is_session_scoped_not_machine_wide() {
        // A `Global\` mutex would forward one user's argv into another user's
        // session on a shared machine.
        assert!(
            MUTEX_NAME.starts_with(r"Local\"),
            "single-instance must be per-session: {MUTEX_NAME}"
        );
        assert!(!MUTEX_NAME.contains(r"Global\"));
    }
}
