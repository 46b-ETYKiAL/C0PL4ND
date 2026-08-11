//! Unexpected-panic crash diagnostics (finding F4-1).
//!
//! The workspace sets `panic = "abort"` (see `Cargo.toml`), so an *unexpected*
//! panic terminates the process immediately — the GUI window vanishes with zero
//! diagnostic. The reader thread and the failed-spawn paths already surface
//! their own errors; this module covers the residual UNEXPECTED-panic path.
//!
//! [`install`] registers a `std::panic::set_hook` early in `main` that, before
//! the abort fires:
//!
//! 1. Writes the panic message + location + a captured backtrace to a rotating
//!    crash log under the per-user `c0pl4nd` data dir (the same dir as
//!    `config.toml`), reusing the crash-safe [`c0pl4nd_core::atomic_write`]
//!    helper. The log is kept owner-only — a panic payload can contain
//!    user-environment fragments.
//! 2. On Windows, additionally shows a `MessageBoxW` so a user who launched the
//!    GUI (no console attached) is told the app crashed and where the log is.
//! 3. Chains to the previously-installed hook, so the default panic output (and
//!    any earlier custom hook) still runs.
//!
//! The hook composes with `panic = "abort"`: a panic hook runs *before* the
//! runtime aborts, so the report is always written first.

use std::backtrace::Backtrace;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};

/// How many `crash-NN.log` files to keep before recycling. Bounded so a crash
/// loop cannot fill the disk; old entries are overwritten round-robin.
const MAX_CRASH_LOGS: u32 = 5;

/// Install the crash-diagnostics panic hook. Call once, early in `main`.
///
/// Chains to any previously-installed hook so default panic output is
/// preserved. Safe to call before the window/event-loop is created.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Resolve the crash-log directory; if we cannot (no config path), skip
        // the file write but still run the message box + the previous hook.
        if let Some(dir) = crash_log_dir() {
            let report = format_crash_report(info, &Backtrace::force_capture());
            if let Some(path) = write_crash_log(&dir, &report) {
                #[cfg(windows)]
                windows_msgbox::show_crash_dialog(&path);
                let _ = &path; // used only on Windows
            }
        }
        // W1TN3SS Tier-1: ALSO spool a sanitized, opt-in crash report locally so
        // the user can review + consent-send it on the NEXT launch (the consent
        // dialog drains the spool). Nothing transmits here — capture is
        // local-first, default-OFF, consent-gated. Only the panic's STATIC
        // `&'static str` message (a source-literal, e.g. an `expect("…")`
        // string) + our own panic SITE enter the report — a runtime `String`
        // payload (which could embed environment fragments / paths) is
        // deliberately NOT spooled. Best-effort; a spool failure in an
        // already-panicking thread is swallowed (never re-panics).
        #[cfg(not(feature = "legacy-winit"))]
        capture_panic_w1tn3ss(info);
        // Always chain to the previous hook (default abort message, etc.).
        previous(info);
    }));
}

/// W1TN3SS Tier-1 capture: spool a sanitized, opt-in crash report from the
/// panic's STATIC message + our panic SITE via [`crate::reporting::capture_panic`].
///
/// Only a `&'static str` panic payload (a source-literal message, e.g. from
/// `panic!("lit")` / `expect("…")` / `unwrap()` — the latter's std message is a
/// `&'static str`) is spooled, honouring the SDK's static-message discipline: a
/// runtime `String` payload (from `panic!("{}", x)`) could embed environment
/// fragments or a path, so it is deliberately NOT spooled (only the static
/// shape + the location reaches the report). Best-effort: a non-static payload
/// or a spool failure is a no-op — the panic hook must never itself re-panic.
#[cfg(not(feature = "legacy-winit"))]
fn capture_panic_w1tn3ss(info: &PanicHookInfo<'_>) {
    // Only the `&'static str` arm is spooled (the static-message discipline).
    let Some(static_msg) = info.payload().downcast_ref::<&'static str>() else {
        return;
    };
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown>".to_string());
    let _ = crate::reporting::capture_panic(static_msg, &location);
}

/// The directory crash logs are written to: a `crashes/` subdir of the per-user
/// `c0pl4nd` data dir (the parent of `config.toml`). Returns `None` when no
/// config path can be resolved (no `%APPDATA%` / `$HOME`).
pub fn crash_log_dir() -> Option<PathBuf> {
    c0pl4nd_core::Config::default_path()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .map(|d| d.join("crashes"))
}

/// Build the human-readable crash report from the panic info + backtrace.
///
/// Pure function (no I/O) so it is unit-testable with a synthetic payload. The
/// format is deterministic given its inputs: a fixed header, the panic payload,
/// the source location (when available), and the backtrace.
pub fn format_crash_report(info: &PanicHookInfo<'_>, backtrace: &Backtrace) -> String {
    let payload = panic_payload_str(info);
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown>".to_string());

    format!(
        "C0PL4ND crash report\n\
         version: {} {}\n\
         os: {} {}\n\
         location: {}\n\
         message: {}\n\
         \n\
         backtrace:\n{}\n",
        c0pl4nd_core::PRODUCT_NAME,
        c0pl4nd_core::version(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        location,
        payload,
        backtrace,
    )
}

/// Extract the panic payload as a string. `PanicHookInfo::payload()` is a
/// `&dyn Any`; the common shapes are `&str` (from `panic!("lit")`) and `String`
/// (from `panic!("{}", x)`). Anything else falls back to a placeholder.
fn panic_payload_str(info: &PanicHookInfo<'_>) -> String {
    let p = info.payload();
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Write `report` to the next rotating `crash-NN.log` slot under `dir`, returning
/// the path written. Best-effort: returns `None` on any I/O failure (a crash
/// reporter must never itself panic or block the abort).
///
/// Rotation is wall-clock-independent (a panic hook cannot rely on a usable
/// clock): the slot index is `(highest existing index + 1) mod MAX_CRASH_LOGS`,
/// so the writer is deterministic and bounded without needing `SystemTime`.
pub fn write_crash_log(dir: &Path, report: &str) -> Option<PathBuf> {
    let slot = next_crash_slot(dir);
    let path = dir.join(format!("crash-{slot:02}.log"));
    // Owner-only: a panic payload + backtrace can leak environment fragments
    // (paths, usernames). Mirrors the workspace-state tightening.
    c0pl4nd_core::atomic_write::atomic_write_owner_only(&path, report.as_bytes()).ok()?;
    Some(path)
}

/// Choose the next rotating slot index in `[0, MAX_CRASH_LOGS)`.
///
/// Scans `dir` for existing `crash-NN.log` files and returns
/// `(max_index + 1) mod MAX_CRASH_LOGS`, or `0` when none exist (or the dir
/// cannot be read). This avoids any dependence on wall-clock time while keeping
/// the newest report distinguishable and the total bounded.
fn next_crash_slot(dir: &Path) -> u32 {
    let mut highest: Option<u32> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(idx) = parse_crash_slot(&entry.file_name().to_string_lossy()) {
                highest = Some(highest.map_or(idx, |h| h.max(idx)));
            }
        }
    }
    match highest {
        Some(h) => (h + 1) % MAX_CRASH_LOGS,
        None => 0,
    }
}

/// Parse the slot index out of a `crash-NN.log` file name, or `None` if it does
/// not match that exact shape (or the index is out of range).
fn parse_crash_slot(name: &str) -> Option<u32> {
    let stem = name.strip_prefix("crash-")?.strip_suffix(".log")?;
    let idx: u32 = stem.parse().ok()?;
    (idx < MAX_CRASH_LOGS).then_some(idx)
}

/// Surface a FATAL STARTUP error that occurs before the egui window exists
/// (e.g. GPU adapter/device init failure, which `eframe::run_native` returns as
/// a clean `Err` — NOT a panic, so the panic hook never fires). A release GUI
/// build has no console, so without this a user just sees the window never
/// appear with zero explanation. Prints to stderr on every platform AND, on
/// Windows, shows a modal `MessageBox`. Best-effort; never panics.
pub fn show_startup_error(title: &str, body: &str) {
    eprintln!("{title}: {body}");
    #[cfg(windows)]
    windows_msgbox::show_dialog(title, body);
}

/// Windows-only `MessageBoxW` crash notification.
///
/// This is the second audited platform-FFI surface in this otherwise
/// unsafe-free binary (the first is `dll_hardening`). The single `MessageBoxW`
/// call is inherently `unsafe`, so the module opts back in with a
/// narrowly-scoped `#![allow(unsafe_code)]` + a `// SAFETY:` justification,
/// mirroring `dll_hardening.rs` exactly. The parent binary uses
/// `deny(unsafe_code)` (not `forbid`), so this scoped `allow` is permitted.
#[cfg(windows)]
mod windows_msgbox {
    #![allow(unsafe_code)]

    use std::path::Path;
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    /// Show a modal "C0PL4ND crashed" dialog naming the crash-log path. Runs
    /// inside the panic hook (before abort), so it must not itself panic; any
    /// failure is swallowed.
    pub fn show_crash_dialog(_log_path: &Path) {
        // The crash report is written to `_log_path` by the caller; the dialog
        // deliberately does NOT show the path (it can contain the username).
        show_dialog(
            "C0PL4ND crashed",
            "C0PL4ND closed unexpectedly. A crash report was saved on your \
             computer. If this keeps happening, please report it from Settings, \
             Privacy, Report a crash or issue.",
        );
    }

    /// Show a modal error dialog with an arbitrary `title` + `body`. Must not
    /// panic (callers run in pre-abort / pre-window contexts); any failure is
    /// swallowed.
    pub fn show_dialog(title: &str, body: &str) {
        let title = to_wide(title);
        let body = to_wide(body);

        // SAFETY: `MessageBoxW` is a user32 call taking an optional owner `HWND`
        // (we pass `None` for a top-level dialog), two NUL-terminated wide-string
        // pointers, and a `MESSAGEBOX_STYLE` flag set. `title` and `body` are
        // `Vec<u16>` buffers that include a trailing NUL and outlive the call;
        // the `PCWSTR`s point at their first element. The call has no
        // memory-safety preconditions beyond valid NUL-terminated pointers,
        // which are satisfied here. The return value is ignored — a failed
        // dialog must not block the caller.
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(body.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    /// Encode a `&str` as a NUL-terminated UTF-16 buffer for the Win32 `W` API.
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that install a process-GLOBAL panic hook
    /// (`std::panic::set_hook`). Without this, two such tests running in parallel
    /// (the default) race: one test's `set_hook` clobbers the other's before its
    /// `catch_unwind` fires, so the clobbered test's sink is never written and it
    /// spuriously fails. The race widens under the coverage gate's slower
    /// instrumented timing. A shared guard makes the hook-mutating tests mutually
    /// exclusive. Poison-tolerant: a panic inside a guarded section (the tests
    /// panic on purpose, but under `catch_unwind`) must not wedge the rest.
    static PANIC_HOOK_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Acquire [`PANIC_HOOK_TEST_GUARD`], recovering from poisoning so one failed
    /// guarded test does not cascade into the others.
    fn lock_panic_hook_tests() -> std::sync::MutexGuard<'static, ()> {
        PANIC_HOOK_TEST_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The boxed hook shape `std::panic::take_hook` hands back / `set_hook` takes.
    type BoxedPanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

    /// Swap `hook` in as the process-global panic hook and hand back the one it
    /// displaced. The caller MUST already hold [`PANIC_HOOK_TEST_GUARD`].
    fn swap_panic_hook(hook: BoxedPanicHook) -> BoxedPanicHook {
        let previous = std::panic::take_hook();
        std::panic::set_hook(hook);
        previous
    }

    /// Hands the displaced panic hook back — and releases
    /// [`PANIC_HOOK_TEST_GUARD`] — when it drops.
    ///
    /// Field order is load-bearing: `previous` is declared before `_guard`, so
    /// the restoring `set_hook` in [`Drop::drop`] runs while the guard is STILL
    /// held. A section that dies early (a failed assert, an unexpected panic)
    /// therefore still restores instead of leaving its own sink installed
    /// process-wide for every later test to write into.
    ///
    /// `_guard` is an `Option` so
    /// `dropping_a_restore_puts_the_displaced_hook_back` can exercise the REAL
    /// [`Drop`] impl while already holding the (non-reentrant) guard itself.
    /// `install_temporary_panic_hook` always fills it.
    struct PanicHookRestore {
        previous: Option<BoxedPanicHook>,
        _guard: Option<std::sync::MutexGuard<'static, ()>>,
    }

    impl Drop for PanicHookRestore {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::panic::set_hook(previous);
            }
        }
    }

    /// Install `hook` as the process-GLOBAL panic hook for the lifetime of the
    /// returned [`PanicHookRestore`]. The ONLY sanctioned way this module's tests
    /// may touch `std::panic::set_hook`.
    ///
    /// `set_hook`/`take_hook` mutate PROCESS state, so two tests doing it
    /// concurrently (cargo's default) interleave: B's `take_hook` captures A's
    /// still-installed sink, B's `set_hook` clobbers it, and A's panic is then
    /// delivered to B's hook — A's sink stays empty and A fails spuriously.
    /// `w1tn3ss_capture_spools_a_static_message_panic` used to install a hook
    /// WITHOUT taking the guard and failed exactly that way, intermittently.
    ///
    /// Funnelling every install through one helper that takes the guard ITSELF
    /// (rather than trusting each call site to remember) makes "forgot to lock"
    /// unrepresentable instead of merely discouraged; the exclusivity it buys is
    /// asserted by `temporary_panic_hooks_are_mutually_exclusive_across_threads`.
    fn install_temporary_panic_hook<H>(hook: H) -> PanicHookRestore
    where
        H: Fn(&PanicHookInfo<'_>) + Sync + Send + 'static,
    {
        let guard = lock_panic_hook_tests();
        PanicHookRestore {
            previous: Some(swap_panic_hook(Box::new(hook))),
            _guard: Some(guard),
        }
    }

    /// Build a synthetic `PanicHookInfo` is not constructible outside std, so the
    /// writer + formatter are tested via their public, info-free seams: the pure
    /// report shape is exercised by writing a known report string and reading it
    /// back; the rotation logic is exercised directly.
    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("c0pl4nd-crash-test-{}-{}", std::process::id(), tag))
    }

    #[test]
    fn write_crash_log_writes_content_and_returns_path() {
        let dir = scratch_dir("write");
        let _ = std::fs::remove_dir_all(&dir);
        let report = "C0PL4ND crash report\nmessage: synthetic boom\n";
        let path = write_crash_log(&dir, report).expect("write should succeed");
        assert!(path.exists(), "crash log file must exist");
        let read = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(read, report, "written content must round-trip exactly");
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "crash-00.log",
            "first crash in an empty dir uses slot 0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_advances_and_wraps() {
        let dir = scratch_dir("rotate");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");

        // Empty dir → slot 0.
        assert_eq!(next_crash_slot(&dir), 0);

        // Seed crash-00..crash-04 (the full ring) → next wraps back to 0.
        for i in 0..MAX_CRASH_LOGS {
            std::fs::write(dir.join(format!("crash-{i:02}.log")), b"x").expect("seed");
        }
        assert_eq!(
            next_crash_slot(&dir),
            0,
            "highest index {} + 1 must wrap to 0",
            MAX_CRASH_LOGS - 1
        );

        // With only crash-02 present, next is 3.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("crash-02.log"), b"x").expect("seed");
        assert_eq!(next_crash_slot(&dir), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_crash_slot_only_matches_exact_shape() {
        assert_eq!(parse_crash_slot("crash-00.log"), Some(0));
        assert_eq!(parse_crash_slot("crash-04.log"), Some(4));
        // Out of ring range.
        assert_eq!(parse_crash_slot("crash-99.log"), None);
        // Wrong shapes.
        assert_eq!(parse_crash_slot("crash-.log"), None);
        assert_eq!(parse_crash_slot("crash-00.txt"), None);
        assert_eq!(parse_crash_slot("notacrash.log"), None);
        assert_eq!(parse_crash_slot("config.toml"), None);
    }

    #[test]
    fn format_crash_report_includes_key_fields() {
        // We cannot construct a real `PanicHookInfo`, so exercise the formatter
        // by capturing inside an actual (caught) panic on a worker thread, where
        // the hook receives a genuine info. We assert the report names version,
        // os/arch, the panic message, and a backtrace section — without aborting
        // the test process.
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = captured.clone();
        // Install a temporary hook that formats into the sink; the returned guard
        // holds PANIC_HOOK_TEST_GUARD and restores the previous hook on drop.
        let restore = install_temporary_panic_hook(move |info| {
            let bt = Backtrace::disabled();
            *sink.lock().unwrap() = format_crash_report(info, &bt);
        });
        let _ = std::panic::catch_unwind(|| panic!("synthetic boom 42"));
        drop(restore);

        let report = captured.lock().unwrap().clone();
        assert!(report.contains("C0PL4ND crash report"), "header: {report}");
        assert!(
            report.contains(c0pl4nd_core::version()),
            "version: {report}"
        );
        assert!(report.contains(std::env::consts::OS), "os: {report}");
        assert!(report.contains(std::env::consts::ARCH), "arch: {report}");
        assert!(report.contains("synthetic boom 42"), "message: {report}");
        assert!(report.contains("location:"), "location field: {report}");
        assert!(report.contains("backtrace:"), "backtrace section: {report}");
    }

    /// Drive a real panic of a given payload through a temporary hook and return
    /// the formatted report. The hook is restored before returning.
    ///
    /// Goes through [`install_temporary_panic_hook`] like every other
    /// hook-mutating site here, so it holds [`PANIC_HOOK_TEST_GUARD`] for the
    /// whole install/panic/restore window: this helper installs the same
    /// process-GLOBAL hook, so without the guard its callers raced
    /// `format_crash_report_includes_key_fields` and each other -- the loser's
    /// `set_hook` was clobbered before its `catch_unwind` fired, leaving the sink
    /// empty and failing the assert. Callers must not lock again (the mutex is
    /// not reentrant).
    fn report_for_panic<F: FnOnce() + std::panic::UnwindSafe>(f: F) -> String {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = captured.clone();
        let restore = install_temporary_panic_hook(move |info| {
            *sink.lock().unwrap() = format_crash_report(info, &Backtrace::disabled());
        });
        let _ = std::panic::catch_unwind(f);
        drop(restore);
        let report = captured.lock().unwrap().clone();
        report
    }

    #[test]
    fn format_crash_report_renders_a_string_payload() {
        // A `panic!("{}", x)` produces a `String` payload (not `&str`); the
        // payload extractor's String arm must surface it verbatim.
        let dynamic = String::from("runtime-built");
        let report = report_for_panic(move || panic!("dynamic {dynamic} message"));
        assert!(
            report.contains("dynamic runtime-built message"),
            "the String-payload arm must render the formatted message: {report}"
        );
        assert!(
            report.contains("message:"),
            "message field present: {report}"
        );
    }

    #[test]
    fn format_crash_report_handles_a_non_string_payload() {
        // A panic with a non-string payload (here an integer via
        // `std::panic::panic_any`) hits the fallback arm and renders the
        // placeholder rather than crashing the formatter.
        let report = report_for_panic(|| std::panic::panic_any(42u32));
        assert!(
            report.contains("<non-string panic payload>"),
            "a non-string payload renders the explicit placeholder: {report}"
        );
    }

    #[cfg(not(feature = "legacy-winit"))]
    #[test]
    fn w1tn3ss_capture_spools_a_static_message_panic() {
        // The W1TN3SS Tier-1 capture seam: a `&'static str` panic payload is
        // extracted and spooled via reporting::capture_panic. We drive a real
        // panic through a temporary hook that calls the capture function, and
        // assert it ran (it returns a structured outcome, never re-panics).
        // capture_panic uses the GLOBAL config dir; here we only assert the
        // static-message EXTRACTION path executes without re-panicking inside
        // the already-panicking thread.
        //
        // Routed through `install_temporary_panic_hook` (which takes
        // PANIC_HOOK_TEST_GUARD) because this test previously installed the
        // process-GLOBAL hook WITHOUT the guard: when a sibling hook-mutating
        // test won the race, its `set_hook` replaced this one before the panic
        // below fired, `flag` was never stored, and this assert failed — the
        // intermittent failure this routing removes.
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ran.clone();
        let restore = install_temporary_panic_hook(move |info| {
            // Exercise the static-message extraction exactly as the hook does.
            capture_panic_w1tn3ss(info);
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let _ = std::panic::catch_unwind(|| panic!("a static literal message"));
        drop(restore);
        assert!(
            ran.load(std::sync::atomic::Ordering::SeqCst),
            "the W1TN3SS capture path ran inside the panic hook without re-panicking"
        );
    }

    /// [`PanicHookRestore`]'s [`Drop`] must actually put the DISPLACED hook back,
    /// not merely drop its own.
    ///
    /// Nothing else in this module notices if it does not: every install captures
    /// whatever hook happens to be set at the time, so a chain of installs that
    /// never restore still hands each test its own sink and each one still passes.
    /// The leak only bites AFTER the last hook-mutating test, when a real panic
    /// elsewhere in the binary is swallowed by a dead test sink instead of
    /// reaching the default hook — which is exactly why the restore needs its own
    /// assertion rather than riding on the other tests.
    ///
    /// The guard is taken ONCE for the whole test and the inner `PanicHookRestore`
    /// is built with `_guard: None`: the mutex is not reentrant, so the inner
    /// install must not try to take it again.
    #[test]
    fn dropping_a_restore_puts_the_displaced_hook_back() {
        let _guard = lock_panic_hook_tests();

        // Stand in for "the hook that was already installed" — in a real run the
        // default hook, or an outer test's.
        let sentinel = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sentinel_sink = sentinel.clone();
        let original = swap_panic_hook(Box::new(move |info| {
            *sentinel_sink.lock().unwrap() = format!("sentinel:{}", panic_payload_str(info));
        }));

        // Stack a temporary hook on top of the sentinel exactly as
        // `install_temporary_panic_hook` does, then let the REAL Drop run.
        let inner = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let inner_sink = inner.clone();
        let restore = PanicHookRestore {
            previous: Some(swap_panic_hook(Box::new(move |info| {
                *inner_sink.lock().unwrap() = format!("inner:{}", panic_payload_str(info));
            }))),
            _guard: None,
        };

        let _ = std::panic::catch_unwind(|| panic!("while stacked"));
        assert_eq!(
            *inner.lock().unwrap(),
            "inner:while stacked",
            "the stacked hook must receive the panic while it is installed"
        );
        assert!(
            sentinel.lock().unwrap().is_empty(),
            "the displaced hook must NOT receive panics while it is displaced"
        );

        drop(restore);

        let _ = std::panic::catch_unwind(|| panic!("after restore"));
        assert_eq!(
            *sentinel.lock().unwrap(),
            "sentinel:after restore",
            "dropping the restore must reinstate the DISPLACED hook, not leave the \
             stacked one (or nothing) installed"
        );
        assert_eq!(
            *inner.lock().unwrap(),
            "inner:while stacked",
            "the dropped hook must stop receiving panics"
        );

        // Leave the process exactly as we found it.
        std::panic::set_hook(original);
    }

    /// The isolation regression guard for [`install_temporary_panic_hook`].
    ///
    /// Every hook-mutating test here installs a PROCESS-GLOBAL hook and then
    /// asserts on what its OWN sink received — sound only while no other thread
    /// can install a hook in between. So drive the helper from several threads at
    /// once and require every single install to observe exclusively its own
    /// panic, identified by a per-install token.
    ///
    /// Remove the shared guard from the helper and the installs interleave: one
    /// thread's panic is delivered to another thread's still-installed hook, so
    /// the loser's sink holds the WRONG token (or none) and `crosstalk` is
    /// non-empty. That is the deterministic form of the failure that used to
    /// surface as a rare flaky run of
    /// `w1tn3ss_capture_spools_a_static_message_panic`, which is why this test
    /// asserts on token IDENTITY rather than merely "the hook ran".
    #[test]
    fn temporary_panic_hooks_are_mutually_exclusive_across_threads() {
        const THREADS: usize = 4;
        const ROUNDS: usize = 30;

        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                std::thread::spawn(move || {
                    let mut foreign: Vec<String> = Vec::new();
                    for round in 0..ROUNDS {
                        let token = format!("hook-thread-{t}-round-{round}");
                        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
                        let sink = seen.clone();
                        let restore = install_temporary_panic_hook(move |info| {
                            *sink.lock().unwrap() = panic_payload_str(info);
                        });
                        let boom = token.clone();
                        let _ = std::panic::catch_unwind(move || panic!("{boom}"));
                        drop(restore);
                        let got = seen.lock().unwrap().clone();
                        if got != token {
                            foreign.push(format!("expected {token:?}, hook saw {got:?}"));
                        }
                    }
                    foreign
                })
            })
            .collect();

        let mut crosstalk: Vec<String> = Vec::new();
        for h in handles {
            crosstalk.extend(h.join().expect("a hook thread must not itself panic"));
        }
        assert!(
            crosstalk.is_empty(),
            "the global panic hook was not exclusive: {} of {} installs saw a \
             foreign panic (first few: {:?})",
            crosstalk.len(),
            THREADS * ROUNDS,
            &crosstalk[..crosstalk.len().min(5)]
        );
    }

    #[test]
    fn crash_log_dir_is_a_crashes_subdir_when_resolvable() {
        // When a config path resolves, the crash dir is its parent's `crashes/`
        // subdir. When it does not resolve, None — never a panic. We assert the
        // shape conditionally so the test is hermetic on any host.
        if let Some(dir) = crash_log_dir() {
            assert_eq!(
                dir.file_name().and_then(|n| n.to_str()),
                Some("crashes"),
                "the crash dir is the `crashes/` subdir of the config parent"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn show_startup_error_does_not_panic_off_windows() {
        // Smoke: the pre-window fatal-error surface prints to stderr and must
        // never panic. NOT exercised on Windows because there it pops a MODAL
        // MessageBox that would BLOCK a headless test run — only the no-window
        // stderr path is safe to call headlessly.
        show_startup_error("test title", "test body");
    }
}
