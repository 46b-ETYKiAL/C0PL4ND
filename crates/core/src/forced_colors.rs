//! OS "forced colors" / high-contrast preference detection (WCAG 1.4.3, 1.4.11).
//!
//! C0PL4ND already SHIPS high-contrast themes (`a11y-high-contrast`,
//! `itasha-void-high-contrast`), but nothing ever asked the OS whether the user
//! had turned high contrast ON — so someone running Windows in High Contrast
//! mode still got the ordinary brand theme and had to find the setting by hand.
//! This module supplies the missing signal and the *precedence rule* that
//! decides whether acting on it is allowed.
//!
//! # Why a registry read and not `SystemParametersInfo(SPI_GETHIGHCONTRAST)`
//!
//! The Win32 API is the canonical source, but this crate deliberately does not
//! reach for it:
//!
//! * `c0pl4nd-core` has **no `windows` / `windows-sys` dependency at all** (only
//!   `c0pl4nd-app` does). Calling `SPI_GETHIGHCONTRAST` here would add a
//!   platform crate to the core engine and an `unsafe` FFI block to a crate that
//!   denies `undocumented_unsafe_blocks`, purely for one boolean.
//! * The sibling module [`crate::reduced_motion`] answers the *exact analogous*
//!   question (its canonical API is `SPI_GETCLIENTAREAANIMATION`) via the
//!   registry value that backs it, for exactly these reasons. Introducing a
//!   second, differently-shaped mechanism for the same class of query would be
//!   the divergence, not the consistency.
//!
//! `HKCU\Control Panel\Accessibility\HighContrast` → `Flags` is the value
//! `SPI_GETHIGHCONTRAST` reports; bit `0x1` is `HCF_HIGHCONTRASTON`. Reading it
//! yields the same answer with no new dependency and no `unsafe`.
//!
//! Everything here is **best-effort and fail-safe**: any spawn, exit, or parse
//! failure is treated as "high contrast OFF", so a query error can never force a
//! theme change. The OS answer is cached for the process lifetime (the setting
//! does not change mid-session in practice); the env override is re-read on
//! every call so it always wins and stays test-controllable.

use std::sync::OnceLock;

/// The theme auto-selected when the OS reports high contrast and the user has
/// expressed no theme preference of their own.
///
/// `a11y-high-contrast` (not `itasha-void-high-contrast`) is the target: it is
/// the neutral, accessibility-first member of the pair, so honouring an OS
/// accessibility request lands on the theme designed for that request rather
/// than on a brand-styled variant.
pub const HIGH_CONTRAST_THEME: &str = "a11y-high-contrast";

/// Whether the OS (or the `C0PL4ND_FORCED_COLORS` override) reports that the
/// user wants forced / high-contrast colors.
///
/// The env override wins over the OS in both directions — set it to a falsy
/// value (`0`, `false`, `no`, `off`) to pin the answer OFF on a machine that
/// genuinely has high contrast enabled.
pub fn forced_colors() -> bool {
    if let Some(forced) = env_forced_colors() {
        return forced;
    }
    *OS_FORCED_COLORS.get_or_init(os_forced_colors)
}

/// The `C0PL4ND_FORCED_COLORS` override. `None` when unset or empty (defer to
/// the OS); `Some(bool)` otherwise. Re-read every call so a relaunch or a test
/// can flip it.
fn env_forced_colors() -> Option<bool> {
    let raw = std::env::var("C0PL4ND_FORCED_COLORS").ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    Some(is_truthy(&raw))
}

/// Parse a truthy env string: not `0`/`false`/`no`/`off` (case-insensitive,
/// trimmed). Mirrors [`crate::reduced_motion`]'s override grammar so the two
/// accessibility switches behave identically.
fn is_truthy(v: &str) -> bool {
    let v = v.trim();
    !v.is_empty()
        && !v.eq_ignore_ascii_case("0")
        && !v.eq_ignore_ascii_case("false")
        && !v.eq_ignore_ascii_case("no")
        && !v.eq_ignore_ascii_case("off")
}

static OS_FORCED_COLORS: OnceLock<bool> = OnceLock::new();

/// `HCF_HIGHCONTRASTON` — bit 0 of the `HighContrast.Flags` value, and of the
/// `HIGHCONTRAST.dwFlags` field `SPI_GETHIGHCONTRAST` fills in.
const HCF_HIGHCONTRASTON: u32 = 0x0000_0001;

/// Query the OS high-contrast preference via a safe platform command (no FFI).
/// Best-effort: any spawn/parse failure → `false` (high contrast off).
///
/// - **Windows**: `HKCU\Control Panel\Accessibility\HighContrast` → `Flags`,
///   the value behind `SPI_GETHIGHCONTRAST`; bit `0x1` = `HCF_HIGHCONTRASTON`.
/// - **macOS**: `defaults read com.apple.universalaccess increaseContrast` →
///   `1`. (macOS has no forced-colors mode; "Increase contrast" is the closest
///   equivalent user intent.)
/// - **Linux (GNOME-family)**: `gsettings get
///   org.gnome.desktop.a11y.interface high-contrast` → `true`.
/// - **Anything else**: `false` — the honest answer for a platform whose
///   preference we cannot read, and the one that changes nothing.
fn os_forced_colors() -> bool {
    #[cfg(target_os = "windows")]
    {
        crate::reduced_motion::query_cmd(
            "reg",
            &[
                "query",
                r"HKCU\Control Panel\Accessibility\HighContrast",
                "/v",
                "Flags",
            ],
        )
        .and_then(|o| parse_high_contrast_flags(&o))
        .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        crate::reduced_motion::query_cmd(
            "defaults",
            &["read", "com.apple.universalaccess", "increaseContrast"],
        )
        .map(|o| o.trim() == "1")
        .unwrap_or(false)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        crate::reduced_motion::query_cmd(
            "gsettings",
            &["get", "org.gnome.desktop.a11y.interface", "high-contrast"],
        )
        .map(|o| o.trim() == "true")
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

/// Extract the high-contrast bit from `reg query … /v Flags` output.
///
/// The output shape is a blank line, the key path, then an indented
/// `Flags    REG_SZ    126` row. We take the **value row** (the one naming a
/// `REG_` type) rather than the last token of the whole blob, so a trailing
/// line or a second value can never be mistaken for the answer. The value is a
/// decimal string in practice (`REG_SZ`) but a `0x…` `REG_DWORD` form is
/// accepted too. `None` when no value row parses — the caller maps that to
/// "high contrast off".
///
/// Pure, so the Windows parsing contract is unit-testable on every host.
fn parse_high_contrast_flags(output: &str) -> Option<bool> {
    output.lines().find_map(|line| {
        // A value row looks like `Flags    REG_SZ    126`. Require the REG_ type
        // token so the key-path line (which also contains "HighContrast") and any
        // banner text are skipped.
        if !line.contains("REG_") {
            return None;
        }
        let raw = line.split_whitespace().last()?;
        let flags = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => raw.parse::<u32>().ok()?,
        };
        Some(flags & HCF_HIGHCONTRASTON != 0)
    })
}

/// The **precedence rule**: which theme, if any, high contrast should select.
///
/// Returns `Some(theme)` only when auto-selection is genuinely allowed, and
/// `None` whenever the user's own choice must stand. Getting this backwards
/// would silently discard a deliberate preference, so it is a pure function
/// with its own tests rather than a condition buried in the UI layer.
///
/// Auto-selection applies **only** when all of the following hold:
///
/// 1. `high_contrast` — the OS actually asked for it. No request, no change.
/// 2. `configured == shipped_default` — the user has expressed **no** theme
///    preference. Any other value is a deliberate pick and **wins**: a user who
///    chose `phosphor-amber` keeps `phosphor-amber` even in High Contrast mode.
/// 3. The default is not already the high-contrast theme (nothing to do).
///
/// # Known and accepted limitation
///
/// A config whose `theme` is *deliberately* set to the shipped default is
/// byte-identical to one that merely never changed it, so it is indistinguishable
/// and will be auto-switched once. This is the same trade-off — and the same
/// resolution — as the schema v2/v3 config migrations in
/// [`crate::config`], which re-point a value only when it is *provably* the old
/// shipped default. Picking any theme afterwards sticks, because the picked
/// value then differs from the default.
pub fn auto_theme_override(
    configured: &str,
    shipped_default: &str,
    high_contrast: bool,
) -> Option<&'static str> {
    if !high_contrast {
        return None;
    }
    // An explicit user choice always wins over the OS auto-selection.
    if configured != shipped_default {
        return None;
    }
    if configured == HIGH_CONTRAST_THEME {
        return None;
    }
    Some(HIGH_CONTRAST_THEME)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE precedence contract: an explicit theme choice must survive high
    /// contrast. Getting this backwards silently overrides a deliberate user
    /// preference, which is the whole failure this feature must not introduce.
    #[test]
    fn an_explicit_user_theme_choice_beats_high_contrast_auto_select() {
        // Every non-default theme is a deliberate pick and must be left alone,
        // even while the OS is asking for high contrast.
        for chosen in [
            "phosphor-amber",
            "ghost-paper",
            "lain-mauve",
            "itasha-neon",
            "my-own-file-theme",
            // Including the OTHER shipped high-contrast theme: the user picked
            // that one specifically, so we must not swap it for ours.
            "itasha-void-high-contrast",
        ] {
            assert_eq!(
                auto_theme_override(chosen, "itasha-corp", true),
                None,
                "{chosen:?} is an explicit choice and must survive high contrast"
            );
        }
    }

    /// The other half of the contract: with NO user choice (the theme is still
    /// the shipped default), high contrast DOES select the accessible theme.
    #[test]
    fn an_unchosen_theme_auto_selects_high_contrast() {
        assert_eq!(
            auto_theme_override("itasha-corp", "itasha-corp", true),
            Some(HIGH_CONTRAST_THEME),
            "an untouched default must follow the OS high-contrast request"
        );
    }

    /// No OS request → never any change, whatever the configured theme is.
    #[test]
    fn without_a_high_contrast_request_nothing_is_overridden() {
        assert_eq!(auto_theme_override("itasha-corp", "itasha-corp", false), None);
        assert_eq!(auto_theme_override("phosphor-amber", "itasha-corp", false), None);
    }

    /// If the shipped default ever BECOMES the high-contrast theme, the override
    /// must be a no-op rather than a redundant re-assignment.
    #[test]
    fn already_on_the_high_contrast_theme_is_a_no_op() {
        assert_eq!(
            auto_theme_override(HIGH_CONTRAST_THEME, HIGH_CONTRAST_THEME, true),
            None
        );
    }

    /// The auto-selected theme must be one that actually ships, or the override
    /// would point at a name the loader cannot resolve.
    #[test]
    fn the_auto_selected_theme_is_a_real_embedded_theme() {
        assert!(
            crate::theme::Theme::EMBEDDED_THEMES
                .iter()
                .any(|(name, _)| *name == HIGH_CONTRAST_THEME),
            "{HIGH_CONTRAST_THEME} must be an embedded theme so it always resolves"
        );
    }

    /// The Windows `Flags` parse: bit 0 set = high contrast ON. `126` (the
    /// common OFF value) and `127` (the common ON value) are the real-world
    /// cases; the REG_SZ decimal form is what `reg query` actually prints.
    #[test]
    fn flags_bit_zero_decides_high_contrast() {
        let off = "\r\nHKEY_CURRENT_USER\\Control Panel\\Accessibility\\HighContrast\r\n    Flags    REG_SZ    126\r\n\r\n";
        let on = "\r\nHKEY_CURRENT_USER\\Control Panel\\Accessibility\\HighContrast\r\n    Flags    REG_SZ    127\r\n\r\n";
        assert_eq!(parse_high_contrast_flags(off), Some(false));
        assert_eq!(parse_high_contrast_flags(on), Some(true));
    }

    /// The key-path line also contains the word "HighContrast"; a naive
    /// last-token-of-the-whole-blob parse would read it (or a trailing line) as
    /// the value. Only the `REG_`-typed value row may be consulted.
    #[test]
    fn the_key_path_line_is_not_mistaken_for_the_value() {
        // No value row at all → None (→ high contrast off), NOT a parse of the
        // path line.
        let path_only =
            "\r\nHKEY_CURRENT_USER\\Control Panel\\Accessibility\\HighContrast\r\n\r\n";
        assert_eq!(parse_high_contrast_flags(path_only), None);
    }

    /// A `REG_DWORD` `0x…` rendering must parse too, so the reader does not
    /// depend on the value's storage type.
    #[test]
    fn hex_dword_form_parses() {
        assert_eq!(
            parse_high_contrast_flags("    Flags    REG_DWORD    0x7f"),
            Some(true)
        );
        assert_eq!(
            parse_high_contrast_flags("    Flags    REG_DWORD    0x7e"),
            Some(false)
        );
    }

    /// Garbage in the value position is a parse failure (`None`), never a panic
    /// and never a spurious `true` that would force a theme change.
    #[test]
    fn unparseable_output_is_none_not_a_panic() {
        assert_eq!(parse_high_contrast_flags(""), None);
        assert_eq!(parse_high_contrast_flags("ERROR: The system was unable to find"), None);
        assert_eq!(parse_high_contrast_flags("    Flags    REG_SZ    not-a-number"), None);
    }

    /// The env-override grammar, matching `reduced_motion`'s. We do not mutate
    /// the process-global var in a parallel unit test (it is `unsafe` under
    /// edition 2024 and pollutes siblings) — the pure parser carries the
    /// contract.
    #[test]
    fn truthy_parsing_covers_the_common_forms() {
        for on in ["1", "true", "TRUE", "yes", "on", "  1 "] {
            assert!(is_truthy(on), "{on:?} should be truthy");
        }
        for off in ["", "  ", "0", "false", "False", "no", "off", " OFF "] {
            assert!(!is_truthy(off), "{off:?} should be falsy");
        }
    }

    /// `os_forced_colors()` must never panic on any host — it issues the real
    /// platform query (or hits the `false` fallback on an unknown target).
    #[test]
    fn os_forced_colors_returns_a_bool_without_panicking() {
        let _v: bool = os_forced_colors();
    }

    /// The OS answer is `OnceLock`-cached, so repeated calls must agree.
    #[test]
    fn forced_colors_is_stable_across_calls() {
        let a = forced_colors();
        let b = forced_colors();
        assert_eq!(a, b, "forced_colors must be stable (OnceLock-cached OS read)");
    }
}
