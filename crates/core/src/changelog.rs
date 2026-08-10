//! The release changelog, surfaced inside the app.
//!
//! # Embedded at build time, not read at runtime
//!
//! `CHANGELOG.md` is compiled into the binary with [`include_str!`]. The
//! alternative — locating and reading the file at runtime — was rejected:
//!
//! * **A changelog describes the build you are running.** An embedded copy is
//!   self-consistent by construction: the notes and the code ship as one
//!   artifact and cannot disagree. A runtime read can hand a v0.4.25 binary the
//!   notes for some other version (a stale install dir, a dev tree the process
//!   happened to be launched from, a file edited after install) and present them
//!   as authoritative.
//! * **This repository has already paid for the runtime-read failure mode.** The
//!   terminal-theme loader used to read `assets/themes/*.toml` from disk and
//!   silently fell back to a built-in whenever the file was not found — which,
//!   in every installed launch, it was not. Users experienced it as "the theme
//!   doesn't change". The fix was [`include_str!`] (see
//!   [`crate::theme::Theme::EMBEDDED_THEMES`]), and a changelog panel has the
//!   identical CWD/install-layout exposure.
//! * **The cost is bounded and small.** `CHANGELOG.md` is ~37 KB of text in a
//!   binary that already embeds ~40 theme files. It compresses well and is
//!   dwarfed by the font and GPU-backend surface.
//!
//! The accepted trade-off is stated plainly: **an embedded changelog cannot show
//! entries written after this binary was built.** That is the correct behaviour
//! for a panel answering "what is in the version I am running" — newer entries
//! belong to a version the user does not have. Release announcements for *newer*
//! versions are the updater's job, not this panel's.
//!
//! # The failure path is explicit
//!
//! A changelog panel that silently shows nothing is worse than one that says it
//! could not load. [`entry_for`] therefore always returns a renderable
//! [`ChangelogEntry`]: when the running version has no section it falls back to
//! `[Unreleased]` **with a notice saying so**, and when neither exists it returns
//! an empty body **with a notice naming the version it looked for**. There is no
//! input for which the caller receives a blank panel and no explanation.

/// The changelog source, compiled into the binary. See the module docs for why
/// this is embedded rather than read from disk at runtime.
pub const EMBEDDED: &str = include_str!("../../../CHANGELOG.md");

/// The heading text used for the in-development section of a Keep a Changelog
/// file.
const UNRELEASED: &str = "Unreleased";

/// A changelog entry ready to render, plus an optional notice explaining any
/// fallback or failure. Never "empty with no explanation".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEntry {
    /// Human-readable heading for the panel, e.g. `"v0.4.25"`.
    pub heading: String,
    /// The entry body (Keep a Changelog markdown, without its `##` heading).
    /// Empty only when nothing could be resolved — in which case `notice` says
    /// why.
    pub body: String,
    /// Set whenever the panel is NOT showing the running version's own section:
    /// either a fallback happened, or nothing was found at all. `None` means the
    /// body is exactly the requested version's entry.
    pub notice: Option<String>,
}

impl ChangelogEntry {
    /// Whether a body was resolved at all. `false` means the panel must show
    /// [`notice`](Self::notice) instead of an empty page.
    pub fn is_empty(&self) -> bool {
        self.body.trim().is_empty()
    }
}

/// Extract the body of one `## [<heading>]` section from Keep a Changelog
/// markdown, exclusive of the heading line and of the next `## ` heading.
///
/// Matches `## [0.4.25]`, `## [0.4.25] - 2026-07-03`, and the unbracketed
/// `## 0.4.25` form, so a heading style change does not silently blank the
/// panel. Returns `None` when the section is absent.
///
/// Works line-wise and re-joins with `\n` rather than slicing the input by byte
/// offset. `str::lines` strips a trailing `\r`, so offset arithmetic over it
/// drifts by one byte per line on a CRLF checkout — and this file is text that
/// git may check out either way. Re-joining is immune to that, at the cost of
/// one small allocation per panel open.
///
/// Pure, so the whole parse contract is unit-testable without touching the real
/// changelog.
pub fn section_for(markdown: &str, heading: &str) -> Option<String> {
    let mut lines = markdown.lines();
    // Advance past the opening heading, or report absence.
    lines.by_ref().find(|line| heading_matches(line, heading))?;
    // The body runs to the next `## ` heading (or end of input).
    let body: Vec<&str> = lines
        .take_while(|line| !line.starts_with("## "))
        .collect();
    Some(body.join("\n").trim_matches('\n').to_string())
}

/// Whether `line` is the `## ` heading naming `heading`, in either the
/// bracketed (`## [0.4.25] - 2026-07-03`) or bare (`## 0.4.25`) Keep a Changelog
/// style.
fn heading_matches(line: &str, heading: &str) -> bool {
    let Some(rest) = line.strip_prefix("## ") else {
        return false;
    };
    let rest = rest.trim();
    let name = match rest.strip_prefix('[') {
        // `[0.4.25] - 2026-07-03` → take up to the closing bracket.
        Some(bracketed) => match bracketed.find(']') {
            Some(end) => &bracketed[..end],
            None => return false,
        },
        // `0.4.25 - 2026-07-03` → take up to the date separator.
        None => rest.split(" - ").next().unwrap_or(rest).trim(),
    };
    name.eq_ignore_ascii_case(heading)
}

/// Resolve the entry to show for `version`, with an explicit notice on every
/// path that is not "found the version's own section".
///
/// Three tiers, in order:
/// 1. `## [<version>]` exists → that entry, no notice.
/// 2. otherwise `## [Unreleased]` exists → that entry, with a notice saying the
///    running version has no released section yet (the normal state of a build
///    made from a development tree).
/// 3. otherwise → empty body with a notice naming the version that was sought.
pub fn entry_for(markdown: &str, version: &str) -> ChangelogEntry {
    if let Some(body) = section_for(markdown, version) {
        return ChangelogEntry {
            heading: format!("v{version}"),
            body,
            notice: None,
        };
    }
    if let Some(body) = section_for(markdown, UNRELEASED) {
        return ChangelogEntry {
            heading: format!("{UNRELEASED} (development build)"),
            body,
            notice: Some(format!(
                "No released changelog section exists for v{version} yet — showing \
                 the in-development “{UNRELEASED}” notes instead."
            )),
        };
    }
    ChangelogEntry {
        heading: format!("v{version}"),
        body: String::new(),
        notice: Some(format!(
            "The embedded changelog has no entry for v{version} and no \
             “{UNRELEASED}” section. This is a packaging fault, not an empty \
             release — see CHANGELOG.md in the repository."
        )),
    }
}

/// The entry for the version this binary was built as, from the embedded
/// changelog. The convenience wrapper over [`entry_for`] used by the UI.
pub fn current() -> ChangelogEntry {
    entry_for(EMBEDDED, env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Changelog

Preamble text that is not part of any entry.

## [Unreleased]

### Changed

- An unreleased change.

## [0.4.25]

### Fixed

- A released fix.

## [0.4.24] - 2026-07-01

### Added

- An older addition.
";

    #[test]
    fn extracts_the_requested_version_section_only() {
        let body = section_for(SAMPLE, "0.4.25").expect("0.4.25 section must be found");
        assert!(body.contains("A released fix."));
        // It must stop at the NEXT heading — bleeding into the older entry would
        // silently attribute another version's notes to this one.
        assert!(
            !body.contains("An older addition."),
            "the section must stop at the next `## ` heading, got: {body:?}"
        );
        assert!(!body.contains("An unreleased change."));
        // The `## [..]` heading line itself is excluded — but `### Fixed` legally
        // CONTAINS the substring `"## "`, so this must be checked line-wise
        // against actual `## ` headings, not by substring.
        assert!(
            !body.lines().any(|l| l.starts_with("## ")),
            "no `## ` heading line may survive in the body, got: {body:?}"
        );
        // The nested `###` sub-headings, by contrast, MUST survive — they are the
        // entry's own structure.
        assert!(body.lines().any(|l| l.starts_with("### ")));
    }

    #[test]
    fn the_last_section_runs_to_end_of_input() {
        let body = section_for(SAMPLE, "0.4.24").expect("0.4.24 section must be found");
        assert!(body.contains("An older addition."));
    }

    #[test]
    fn a_dated_heading_still_matches_the_bare_version() {
        // `## [0.4.24] - 2026-07-01` must match the version `0.4.24`.
        assert!(section_for(SAMPLE, "0.4.24").is_some());
    }

    #[test]
    fn the_unbracketed_heading_style_matches_too() {
        let md = "## 1.2.3\n\n- bare style\n";
        let body = section_for(md, "1.2.3").expect("bare `## 1.2.3` must match");
        assert!(body.contains("bare style"));
    }

    #[test]
    fn an_absent_version_is_none_not_a_panic() {
        assert_eq!(section_for(SAMPLE, "9.9.9"), None);
        assert_eq!(section_for("", "0.4.25"), None);
    }

    /// A heading must not match on a prefix: asking for `0.4.2` must not return
    /// the `0.4.25` entry.
    #[test]
    fn version_matching_is_exact_not_a_prefix() {
        assert_eq!(section_for(SAMPLE, "0.4.2"), None);
    }

    /// Tier 1: the running version's own section, and NO notice.
    #[test]
    fn entry_for_a_known_version_carries_no_notice() {
        let e = entry_for(SAMPLE, "0.4.25");
        assert_eq!(e.heading, "v0.4.25");
        assert!(e.body.contains("A released fix."));
        assert_eq!(e.notice, None, "a clean hit must not claim a fallback");
        assert!(!e.is_empty());
    }

    /// Tier 2: no section for the running version → the Unreleased notes, and a
    /// notice saying so. The panel must never present unreleased notes as if
    /// they were the running version's.
    #[test]
    fn a_version_without_a_section_falls_back_to_unreleased_with_a_notice() {
        let e = entry_for(SAMPLE, "0.4.99");
        assert!(e.body.contains("An unreleased change."));
        assert!(e.heading.contains(UNRELEASED));
        let notice = e.notice.expect("the fallback MUST be announced, not silent");
        assert!(notice.contains("0.4.99"), "the notice must name the version sought");
    }

    /// Tier 3: nothing resolvable → empty body, but ALWAYS a notice. This is the
    /// case the module exists to make non-silent.
    #[test]
    fn a_totally_missing_entry_is_empty_but_never_silent() {
        let e = entry_for("# Changelog\n\nnothing here\n", "0.4.25");
        assert!(e.is_empty());
        let notice = e
            .notice
            .expect("an empty changelog panel MUST explain itself");
        assert!(notice.contains("0.4.25"));
    }

    /// The embedded changelog must actually be present and non-trivial —
    /// otherwise every test above passes against a real file that is empty.
    #[test]
    fn the_embedded_changelog_is_present_and_substantial() {
        assert!(
            EMBEDDED.len() > 1_000,
            "the embedded CHANGELOG.md looks truncated ({} bytes)",
            EMBEDDED.len()
        );
        assert!(EMBEDDED.starts_with("# Changelog"));
    }

    /// The shipped binary must resolve a real, non-empty entry — this is the
    /// end-to-end check that the panel is not blank in production. It passes via
    /// the version section or the Unreleased fallback, but NOT via tier 3.
    #[test]
    fn the_current_build_resolves_a_non_empty_entry() {
        let e = current();
        assert!(
            !e.is_empty(),
            "the running build must resolve a changelog body, got notice: {:?}",
            e.notice
        );
    }

    /// The config schema v2 → v3 migration rewrites user state on disk, so it
    /// must be documented in the changelog that ships in the binary — it is
    /// exactly what a user opening this panel needs to know.
    #[test]
    fn the_config_schema_v3_migration_is_documented_in_the_embedded_changelog() {
        assert!(
            EMBEDDED.contains("schema v2 → v3"),
            "the v2 → v3 config-schema migration must be documented in CHANGELOG.md"
        );
    }
}
