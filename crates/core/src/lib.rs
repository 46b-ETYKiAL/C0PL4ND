//! C0PL4ND core engine.
//!
//! Houses the platform-agnostic terminal substrate: the [`pty`] abstraction,
//! the VT/grid model, configuration, and theming. The UI shell ([`c0pl4nd`])
//! and the GPU renderer (`c0pl4nd-renderer`) build on top of this crate. The
//! seam is deliberately UI-free so a platform-native shell can be layered on
//! later without re-architecting the engine.

pub mod atomic_write;
pub mod command_history;
pub mod config;
pub mod fetch;
pub mod fs_perms;
pub mod fuzzy;
pub mod grid;
pub mod image;
pub mod layout;
pub mod layout_persist;
pub mod net_confine;
pub mod paste_guard;
pub mod plugin;
pub mod pty;
pub mod reduced_motion;
pub mod search;
pub mod session;
pub mod term;
pub mod theme;
pub mod win_process;

pub use config::Config;
pub use grid::{Cell, CellFlags, Color, Grid};
pub use session::{Session, WakeFn};
pub use term::Terminal;
pub use theme::Theme;

/// Product display name.
pub const PRODUCT_NAME: &str = "C0PL4ND";

/// Product tagline.
pub const TAGLINE: &str = "the operator's shell into the wired";

/// Returns the crate semantic version, surfaced in `--version` output.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_identity_is_stable() {
        assert_eq!(PRODUCT_NAME, "C0PL4ND");
        assert!(!version().is_empty());
    }

    #[test]
    fn tagline_is_the_brand_tagline() {
        // The tagline is surfaced in startup / about UI; pin the exact brand copy
        // so a silent edit is caught (it is a user-visible brand string).
        assert_eq!(TAGLINE, "the operator's shell into the wired");
    }

    #[test]
    fn version_matches_the_crate_package_version() {
        // version() must return the compiled-in CARGO_PKG_VERSION verbatim — not a
        // hard-coded literal that could drift from Cargo.toml.
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        // Sanity: a semantic version has at least major.minor (one dot).
        assert!(
            version().contains('.'),
            "version {:?} should look like a semver",
            version()
        );
    }
}
