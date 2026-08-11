//! In-app verified self-updater backend (the egui Settings → Updates page).
//!
//! A complete, telemetry-free, **verify-before-swap** updater mirroring the
//! sibling SCR1B3 editor, so the app can download + verify + apply a new
//! release in place — no browser, no installer hand-off:
//!
//! - [`net`] — discover the latest release, download the signed manifest +
//!   asset/sig/sha, verify, then extract (blocking I/O; the pure
//!   [`net::check_for_update`] Tier-1 resolver decision is unit-tested offline).
//! - [`verify`] — SHA-256 **then** minisign against an EMBEDDED public key;
//!   fails closed (an unverified binary is NEVER returned).
//! - [`manifest`] — Tier-1 SIGNED release manifest (`latest.json`): verify the
//!   minisign signature over the raw JSON FIRST, then parse; binds product /
//!   schema / version / `release_index` / freshness / per-asset SHA-256 into one
//!   signed document. Fails closed everywhere.
//! - [`update_state`] — persisted monotonic `release_index` high-water mark
//!   (sibling-of-exe, mirroring [`rollback_guard`]); the manifest-native
//!   anti-rollback floor.
//! - [`apply`] — keep-one-prior backup + `self-replace` atomic swap + rollback.
//! - [`rollback_guard`] — anti-rollback (version-downgrade) protection: a
//!   strictly-monotonic version floor re-checked at APPLY time so a
//!   validly-signed but OLDER release (a replayed/MITM'd listing) cannot
//!   downgrade the install. Integrity (verify) ≠ freshness (this).
//! - [`updater`] — the egui-thread [`updater::Updater`] state machine
//!   (`std::thread` + `mpsc`) the Updates page drives.
//!
//! This module is `#[path]`-included by `egui_app::settings` so it resolves
//! identically in the shipping `c0pl4nd` binary AND in the `egui_kittest`
//! integration test binaries. It is kept separate from the legacy CLI `update`
//! module so the egui-only updater backend is compiled exactly once (by the
//! egui surfaces), never by the legacy winit `c0pl4nd-legacy` binary.

pub mod apply;
pub mod manifest;
pub mod net;
pub mod rollback_guard;
pub mod update_state;
pub mod updater;
pub mod verify;

/// GitHub repo coordinates for the Releases API. Public values, shared by the
/// in-app [`net`] / [`updater`] modules and the "View all releases" link.
pub const UPDATE_OWNER: &str = "46b-ETYKiAL";
pub const UPDATE_REPO: &str = "C0PL4ND";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_coordinates_are_the_canonical_release_repo() {
        // These two constants are interpolated directly into the GitHub
        // Releases API URL (`/repos/{owner}/{repo}/releases/latest`) and the
        // "View all releases" link. A wrong value silently points the updater
        // at the wrong repo, so pin the exact expected strings.
        assert_eq!(UPDATE_OWNER, "46b-ETYKiAL");
        assert_eq!(UPDATE_REPO, "C0PL4ND");
    }

    #[test]
    fn repo_coordinates_are_url_path_safe() {
        // Owner/repo are placed in a URL path; neither may contain a slash,
        // whitespace, or be empty, or the constructed API URL is malformed.
        for v in [UPDATE_OWNER, UPDATE_REPO] {
            assert!(!v.is_empty());
            assert!(!v.contains('/'));
            assert!(!v.contains(char::is_whitespace));
        }
    }

    #[test]
    fn releases_api_url_composes_correctly() {
        // Exercise the actual interpolation shape the net module uses, so a
        // mutant that drops/duplicates a path segment is caught.
        let url =
            format!("https://api.github.com/repos/{UPDATE_OWNER}/{UPDATE_REPO}/releases/latest");
        assert_eq!(
            url,
            "https://api.github.com/repos/46b-ETYKiAL/C0PL4ND/releases/latest"
        );
    }
}
