//! Paste-confirmation gate (pastejacking / accidental-flood defence).
//!
//! A terminal paste is one of the few places where *content the user did not
//! type* is handed straight to a shell. Two distinct hazards live there:
//!
//! * **Multi-line** — the classic pastejacking payload. A copied "command" that
//!   secretly carries a trailing newline (or a second line) executes the instant
//!   it lands, before the user can read it. Guarded by
//!   [`Config::paste_warn_multiline`].
//! * **Very large** — a SINGLE-line paste of tens of kilobytes. No newline, so
//!   the multi-line gate never sees it, yet it is the other half of the same
//!   footgun: a terminal-width-spanning command whose tail scrolls out of view,
//!   a giant base64 blob destined for `sh`, or an accidental whole-file paste
//!   that floods the PTY. Guarded by [`Config::paste_warn_bytes`].
//!
//! Both gates feed ONE decision function, [`paste_confirm_reason`], so the app
//! has a single call site and the policy cannot drift between the two halves.
//! Living in `core` (not the UI crate) keeps it unit-testable without a window.

use crate::Config;

/// Why a paste was held back for confirmation instead of going straight to the
/// PTY. Returned by [`paste_confirm_reason`]; the app uses it to word the
/// confirm overlay so the user learns *which* hazard tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteConfirmReason {
    /// The paste contains a line break, so it can execute the moment it lands.
    MultiLine,
    /// The paste is a single line but exceeds [`Config::paste_warn_bytes`].
    Large,
}

/// Decide whether `text` must be confirmed before it reaches the PTY.
///
/// Returns `None` when the paste is safe to deliver immediately. The two gates
/// are INDEPENDENT — disabling one never disables the other — and are checked
/// multi-line first, because a paste that is both multi-line and huge is most
/// usefully explained by its execution hazard rather than its size.
///
/// A [`Config::paste_warn_bytes`] of `0` disables the size gate outright; size
/// is measured in BYTES (`str::len`), matching what is actually written to the
/// PTY rather than a display-dependent character count.
pub fn paste_confirm_reason(config: &Config, text: &str) -> Option<PasteConfirmReason> {
    if config.paste_warn_multiline && (text.contains('\n') || text.contains('\r')) {
        return Some(PasteConfirmReason::MultiLine);
    }
    if config.paste_warn_bytes > 0 && text.len() > config.paste_warn_bytes {
        return Some(PasteConfirmReason::Large);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn plain_single_line_paste_is_delivered_immediately() {
        assert_eq!(paste_confirm_reason(&cfg(), "ls -la"), None);
    }

    #[test]
    fn multiline_paste_is_confirmed_by_default() {
        assert_eq!(
            paste_confirm_reason(&cfg(), "echo one\necho two"),
            Some(PasteConfirmReason::MultiLine)
        );
    }

    #[test]
    fn a_bare_carriage_return_also_counts_as_multiline() {
        // A lone \r still submits the line to a shell — it must not slip past
        // the gate just because it is not \n.
        assert_eq!(
            paste_confirm_reason(&cfg(), "rm -rf /\rwhoami"),
            Some(PasteConfirmReason::MultiLine)
        );
    }

    #[test]
    fn oversized_single_line_paste_is_confirmed() {
        let c = cfg();
        let huge = "A".repeat(c.paste_warn_bytes + 1);
        assert!(
            !huge.contains('\n'),
            "the size gate must fire with no newline"
        );
        assert_eq!(
            paste_confirm_reason(&c, &huge),
            Some(PasteConfirmReason::Large)
        );
    }

    #[test]
    fn a_paste_exactly_at_the_threshold_is_not_confirmed() {
        // Boundary: the gate is strictly-greater-than, so a paste of exactly
        // `paste_warn_bytes` passes. Kills an off-by-one mutant.
        let c = cfg();
        let at = "A".repeat(c.paste_warn_bytes);
        assert_eq!(paste_confirm_reason(&c, &at), None);
    }

    #[test]
    fn zero_threshold_disables_the_size_gate_only() {
        let mut c = cfg();
        c.paste_warn_bytes = 0;
        let huge = "A".repeat(100_000);
        assert_eq!(paste_confirm_reason(&c, &huge), None, "size gate disabled");
        // The multi-line gate is independent and still fires.
        assert_eq!(
            paste_confirm_reason(&c, "a\nb"),
            Some(PasteConfirmReason::MultiLine)
        );
    }

    #[test]
    fn disabling_the_multiline_gate_leaves_the_size_gate_armed() {
        let mut c = cfg();
        c.paste_warn_multiline = false;
        assert_eq!(paste_confirm_reason(&c, "a\nb"), None, "multiline gate off");
        let huge = "A".repeat(c.paste_warn_bytes + 1);
        assert_eq!(
            paste_confirm_reason(&c, &huge),
            Some(PasteConfirmReason::Large),
            "the size gate is independent of the multi-line gate"
        );
    }

    #[test]
    fn both_gates_off_never_confirms() {
        let mut c = cfg();
        c.paste_warn_multiline = false;
        c.paste_warn_bytes = 0;
        assert_eq!(paste_confirm_reason(&c, &"A".repeat(100_000)), None);
        assert_eq!(paste_confirm_reason(&c, "a\nb\nc"), None);
    }
}
