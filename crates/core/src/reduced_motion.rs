//! "Reduce motion" preference detection (F2-2, WCAG 2.3.3).
//!
//! C0PL4ND's CRT post-effect animates a rolling scan band. A user who has asked
//! their OS to reduce motion (vestibular comfort, focus, battery) should not be
//! subjected to that animation. Previously the only signal was the
//! `C0PL4ND_REDUCED_MOTION` env override; this module ADDS an OS-accessibility
//! query so the app honours the system setting out of the box.
//!
//! The OS query uses a **safe, dependency-free platform command** (no `unsafe`
//! FFI, no new crate) and is best-effort: any failure is treated as "motion
//! allowed". It is cached for the process lifetime (the OS setting does not
//! change mid-session in practice). The env override is re-read every call so it
//! always wins and stays test-controllable.

use std::sync::OnceLock;

/// Whether the user prefers reduced motion. True when the
/// `C0PL4ND_REDUCED_MOTION` env var is set to a truthy value, OR the OS
/// accessibility "reduce motion" setting is on. Animations (the CRT scan-band
/// roll) should freeze when this returns true; static visuals are unaffected.
pub fn reduced_motion() -> bool {
    if env_reduced_motion() {
        return true;
    }
    *OS_REDUCED_MOTION.get_or_init(os_reduced_motion)
}

/// The `C0PL4ND_REDUCED_MOTION` env override. Truthy = any non-empty value that
/// is not `0` / `false` (case-insensitive). Re-read every call so a relaunch or
/// a test can flip it, and so it ALWAYS overrides the OS query.
fn env_reduced_motion() -> bool {
    std::env::var("C0PL4ND_REDUCED_MOTION")
        .map(|v| is_truthy(&v))
        .unwrap_or(false)
}

/// Parse a truthy env string: non-empty AND not `0`/`false`/`no`/`off`.
fn is_truthy(v: &str) -> bool {
    let v = v.trim();
    !v.is_empty()
        && !v.eq_ignore_ascii_case("0")
        && !v.eq_ignore_ascii_case("false")
        && !v.eq_ignore_ascii_case("no")
        && !v.eq_ignore_ascii_case("off")
}

static OS_REDUCED_MOTION: OnceLock<bool> = OnceLock::new();

/// Query the OS accessibility "reduce motion" preference via a safe platform
/// command (no FFI). Best-effort: any spawn/parse failure → `false` (motion on).
///
/// - **Windows**: `HKCU\Control Panel\Desktop\WindowMetrics\MinAnimate` is the
///   REG_SZ behind `SPI_GETCLIENTAREAANIMATION`; `"0"` = window animations OFF =
///   reduce motion.
/// - **macOS**: `defaults read com.apple.universalaccess reduceMotion` → `1`.
/// - **Linux (GNOME-family)**: `gsettings get org.gnome.desktop.interface
///   enable-animations` → `false` = reduce motion.
fn os_reduced_motion() -> bool {
    #[cfg(target_os = "windows")]
    {
        // The last whitespace-delimited token of the `reg query` output line is
        // the value (e.g. `MinAnimate    REG_SZ    0`).
        query_cmd(
            "reg",
            &[
                "query",
                r"HKCU\Control Panel\Desktop\WindowMetrics",
                "/v",
                "MinAnimate",
            ],
        )
        .map(|o| o.split_whitespace().last() == Some("0"))
        .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        query_cmd(
            "defaults",
            &["read", "com.apple.universalaccess", "reduceMotion"],
        )
        .map(|o| o.trim() == "1")
        .unwrap_or(false)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        query_cmd(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "enable-animations"],
        )
        .map(|o| o.trim() == "false")
        .unwrap_or(false)
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        all(unix, not(target_os = "macos"))
    )))]
    {
        false
    }
}

/// Run a query command and return its stdout on success; `None` on any failure
/// (binary absent, non-zero exit, non-UTF8). Never panics; never writes.
///
/// Shared with [`crate::forced_colors`], which asks the same class of question
/// (an OS accessibility preference) of the same platform tools, so the two
/// probes cannot drift apart in how they spawn, gate on exit status, or
/// suppress the Windows console flash.
#[cfg_attr(
    not(any(
        target_os = "windows",
        target_os = "macos",
        all(unix, not(target_os = "macos"))
    )),
    allow(dead_code)
)]
pub(crate) fn query_cmd(prog: &str, args: &[&str]) -> Option<String> {
    use crate::win_process::NoConsoleWindow;
    // `no_console_window()` suppresses the console flash on Windows (the OS
    // reduce-motion probe spawns `reg query`, a console program); no-op on the
    // `defaults`/`gsettings` probes off Windows.
    let out = std::process::Command::new(prog)
        .args(args)
        .no_console_window()
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_parsing_covers_the_common_forms() {
        // The env-override truthiness contract (the load-bearing pure logic).
        // We deliberately do NOT mutate the global `C0PL4ND_REDUCED_MOTION` env
        // var in a unit test — that is process-global state and would pollute
        // other parallel tests (and `set_var` is `unsafe` under edition 2024).
        for on in ["1", "true", "TRUE", "yes", "on", "  1 "] {
            assert!(is_truthy(on), "{on:?} should be truthy");
        }
        for off in ["", "  ", "0", "false", "False", "no", "off"] {
            assert!(!is_truthy(off), "{off:?} should be falsy");
        }
    }

    /// `is_truthy` must treat mixed-case / padded negative forms as falsy and a
    /// non-trivial positive token as truthy — the exact contract `reduced_motion`
    /// relies on for the env override. Pins the eq_ignore_ascii_case branches.
    #[test]
    fn truthy_is_case_insensitive_and_trimmed_on_negatives() {
        assert!(!is_truthy("OFF"));
        assert!(!is_truthy("  NO  "));
        assert!(!is_truthy("FALSE"));
        assert!(!is_truthy(" 0 "));
        // Any other word is truthy (the override means "reduce motion").
        assert!(is_truthy("enabled"));
        assert!(is_truthy("reduce"));
        assert!(is_truthy("2"));
    }

    /// `query_cmd` returns `None` when the program does not exist — the
    /// best-effort "motion allowed" fallback. This exercises the `.ok()?` early
    /// return (spawn failure) on every host: a bogus binary name never exists.
    #[test]
    fn query_cmd_missing_binary_is_none() {
        let out = query_cmd("c0pl4nd-definitely-not-a-real-binary-xyz", &["--version"]);
        assert_eq!(out, None, "a nonexistent binary must yield None, not panic");
    }

    /// `query_cmd` returns `Some(stdout)` for a known-present command that exits
    /// 0, proving the success path (status.success() == true → Some). We use a
    /// cross-platform no-arg echo: Windows `cmd /C echo`, POSIX `echo`.
    #[test]
    fn query_cmd_success_returns_stdout() {
        #[cfg(windows)]
        let out = query_cmd("cmd", &["/C", "echo", "c0pl4nd_qc_ok"]);
        #[cfg(not(windows))]
        let out = query_cmd("echo", &["c0pl4nd_qc_ok"]);
        // `cmd` (Windows) and `echo` (POSIX) are always present on every CI host
        // we build on, so success MUST yield Some(stdout). Asserting Some — not a
        // vacuous `if let` — is what kills the `query_cmd -> None` mutant.
        let s = out.expect("a present echo command must return Some(stdout), not None");
        assert!(
            s.contains("c0pl4nd_qc_ok"),
            "captured stdout should contain the echoed token, got {s:?}"
        );
    }

    /// `query_cmd` returns `None` when the command exits non-zero — the
    /// `!status.success()` branch. A POSIX `false` / Windows `cmd /C exit 1`
    /// exits 1; we assert the None mapping where the command is present.
    #[test]
    fn query_cmd_nonzero_exit_is_none() {
        #[cfg(windows)]
        let out = query_cmd("cmd", &["/C", "exit", "1"]);
        #[cfg(not(windows))]
        let out = query_cmd("false", &[]);
        assert_eq!(
            out, None,
            "a command that exits non-zero must map to None (best-effort)"
        );
    }

    /// `os_reduced_motion()` must never panic and returns a plain bool on this
    /// host — it actually issues the platform query (Windows `reg`, macOS
    /// `defaults`, Linux `gsettings`, or the `false` fallback on other targets).
    /// We can only assert it returns *a* bool (the host's real setting is not
    /// controllable in CI), proving the per-platform arm executes without error.
    #[test]
    fn os_reduced_motion_returns_a_bool_without_panicking() {
        // The host's real setting is not controllable here, so we only assert the
        // call completes without panicking and yields a `bool` (the `: bool`
        // binding enforces the return type at compile time).
        let _v: bool = os_reduced_motion();
    }

    /// `reduced_motion()` short-circuits to `true` when the env override is set,
    /// WITHOUT consulting the OS. We cannot mutate the process-global env var in
    /// a parallel unit test (it is `unsafe` and pollutes siblings), but the
    /// composition is: `env_reduced_motion() || OS`. We prove the pure pieces
    /// (`is_truthy` above) and that the cached OS path returns a stable bool.
    #[test]
    fn reduced_motion_is_stable_across_calls() {
        // The OS result is cached in a OnceLock, so two calls must agree.
        let a = reduced_motion();
        let b = reduced_motion();
        assert_eq!(
            a, b,
            "reduced_motion must be stable (OnceLock-cached OS read)"
        );
    }
}
