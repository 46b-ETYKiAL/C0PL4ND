//! The single live-PTY probe for this crate's PTY-backed test lanes.
//!
//! WHY IT EXISTS. `PaneTerm::terminal_for_test` returns `None` for exactly one
//! reason: the PTY spawn failed. Eighteen tests in `pane_term.rs` consumed that
//! with `let Some(term) = pane.terminal_for_test() else { return; };`, which
//! keys the guard on **the very subsystem those tests protect**. A regression in
//! ConPTY/openpty setup, in shell resolution, or in the job-object assignment
//! turns all eighteen green having asserted nothing. That the failed-spawn state
//! is reachable is not hypothetical — `pane_term.rs` proves it in the same file
//! (`has_running_command_is_false_for_a_failed_spawn_pane`).
//!
//! Two of the eighteen are security tests: OSC 52 clipboard READS are denied
//! until explicitly allowed (`pane_denies_clipboard_reads_until_told_otherwise`)
//! and the setting must reach the emulator in both directions
//! (`set_clipboard_read_allowed_moves_the_terminal_gate_both_ways`). A silent
//! skip there leaves a fail-closed security invariant unprotected while CI
//! reports green.
//!
//! WHY `C0PL4ND_REQUIRE_PTY` EXISTS. The skip is honest on a host that genuinely
//! has no usable shell: nobody is relying on that run. It is NOT honest in CI,
//! where the platform default shell exists on all three runners and a missing
//! PTY is a real defect. So the CI `Build & Test` job sets
//! `C0PL4ND_REQUIRE_PTY=1` on every OS leg and an absent PTY becomes a hard
//! failure with an actionable message instead of a quiet pass.
//!
//! WHY THE DECISION IS A SEPARATE, PURE FUNCTION. The obvious way to test the
//! guard is to break the PTY and watch it fire — but a test cannot reliably
//! un-PTY its own host, so the panic branch would never be exercised, and a
//! guard nobody has seen fail is indistinguishable from one that always passes.
//! [`enforce`] therefore takes the probe result as an ARGUMENT: the panic path
//! is directly reachable and is asserted below. This mirrors the shape already
//! proven in the fleet (`scr1b3`'s `gpu_probe::enforce`) and this crate's own
//! `tests/common::require_gpu`, which is documented as "It must NEVER skip".
//!
//! WHY BOTH [`require_live_pty`] AND [`expect_live_pty`]. Some call sites cannot
//! do anything meaningful without a terminal — the fixture builders in
//! `close_path_tests.rs`, `taskbar_wiring_tests.rs` and `mod_tests.rs` construct
//! an app *around* a live pane, so they have always been unconditionally strict
//! (`.expect("PTY spawn must succeed — a skipped wiring test proves nothing")`).
//! That strictness is correct and is preserved verbatim; what was wrong was that
//! the SAME decision was derived twice, correctly in those files and incorrectly
//! in `pane_term.rs`. Both entry points now route through one [`enforce`], so
//! the two cannot drift apart again.

use std::ffi::OsStr;
use std::sync::{Arc, Mutex};

use c0pl4nd_core::Terminal;

use super::pane_term::PaneTerm;

/// The `C0PL4ND_REQUIRE_PTY` parsing contract, as a pure function.
///
/// Only the literal `1` arms it, so a typo'd value cannot silently disarm the
/// CI lane the way `C0PL4ND_HEADLESS` once did.
fn env_arms_requirement(v: Option<&OsStr>) -> bool {
    v.is_some_and(|v| v == "1")
}

/// Whether this run declared a live PTY mandatory.
fn pty_required() -> bool {
    env_arms_requirement(std::env::var_os("C0PL4ND_REQUIRE_PTY").as_deref())
}

/// Decide what a PTY-spawn probe result means for this run.
///
/// Returns whether the PTY-backed body may run. A failed spawn is a clean skip
/// UNLESS the run declared a live PTY mandatory, in which case it is a failure —
/// silence there would report green over a body that never executed.
///
/// # Panics
/// When `required` is true and `spawned` is false.
fn enforce(spawned: bool, required: bool) -> bool {
    assert!(
        spawned || !required,
        "C0PL4ND_REQUIRE_PTY=1 but this pane's PTY spawn failed, so there is no \
         terminal to drive. This run declared a live PTY mandatory, and a silent \
         skip here would report GREEN over tests that asserted nothing — \
         including the OSC 52 clipboard-read security gates. The platform \
         default shell resolves on all three CI runners, so this is a real \
         regression in shell resolution, ConPTY/openpty setup, or the \
         job-object assignment; read `PaneTerm::error()` for the spawn failure. \
         Unset C0PL4ND_REQUIRE_PTY only for a host that genuinely has no shell."
    );
    spawned
}

/// The pane's live terminal, or `None` when the PTY did not spawn and this run
/// did not declare one mandatory.
///
/// Replaces `pane.terminal_for_test()` at every guard site whose test body is
/// meaningless without a terminal.
///
/// # Panics
/// When `C0PL4ND_REQUIRE_PTY=1` and the pane's PTY spawn failed.
pub(super) fn require_live_pty(pane: &PaneTerm) -> Option<Arc<Mutex<Terminal>>> {
    let term = pane.terminal_for_test();
    if enforce(term.is_some(), pty_required()) {
        term
    } else {
        None
    }
}

/// Whether the pane spawned at all — for the two cwd probes that read the
/// pane's GRID rather than driving its terminal, so they check
/// [`PaneTerm::error`] instead of taking the terminal handle.
///
/// # Panics
/// When `C0PL4ND_REQUIRE_PTY=1` and the pane reported a spawn error.
pub(super) fn require_live_spawn(pane: &PaneTerm) -> bool {
    enforce(pane.error().is_none(), pty_required())
}

/// The pane's live terminal, unconditionally — for fixture builders that cannot
/// produce a usable app without one, so a skip would prove nothing at all.
///
/// Deliberately NOT flag-gated: these sites were already strict before this
/// module existed, and routing them through the optional form would weaken them.
///
/// # Panics
/// Whenever the pane's PTY spawn failed.
pub(super) fn expect_live_pty(pane: &PaneTerm) -> Arc<Mutex<Terminal>> {
    let term = pane.terminal_for_test();
    enforce(term.is_some(), true);
    term.expect("enforce(_, true) returns only when the terminal is present")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the decision (the part that must not be trusted untested) ----

    #[test]
    #[should_panic(expected = "this pane's PTY spawn failed")]
    fn a_mandatory_lane_with_no_pty_fails_instead_of_skipping() {
        // THE point of this module. Without this branch, a CI job whose PTY
        // setup regressed turns eighteen pane tests — two of them security
        // tests — into no-ops and still reports green.
        enforce(false, true);
    }

    #[test]
    fn a_failed_spawn_is_a_clean_skip_when_a_pty_is_not_mandatory() {
        assert!(
            !enforce(false, false),
            "a host with no usable shell must skip, not fail — nobody is relying on it"
        );
    }

    #[test]
    fn a_live_pty_runs_the_body_either_way() {
        assert!(enforce(true, true), "pty present + required => run");
        assert!(enforce(true, false), "pty present + optional => run");
    }

    // ---- the env contract ----

    #[test]
    fn only_the_exact_value_1_arms_the_requirement() {
        assert!(env_arms_requirement(Some(OsStr::new("1"))), "1 arms it");
        assert!(!env_arms_requirement(Some(OsStr::new("0"))), "0 must not");
        assert!(
            !env_arms_requirement(Some(OsStr::new("true"))),
            "only the literal 1 arms it"
        );
        assert!(
            !env_arms_requirement(Some(OsStr::new(""))),
            "empty must not"
        );
        assert!(!env_arms_requirement(None), "unset must not");
    }
}
