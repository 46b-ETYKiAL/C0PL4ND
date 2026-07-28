//! User-facing error copy — the single place that turns an internal failure
//! into a short, plain-language sentence with a recovery step.
//!
//! ## Contract (non-negotiable)
//!
//! Nothing technical reaches the user. No error chains, OS errno text, host
//! names, URLs, filesystem paths, or internal identifiers appear in any string
//! this module returns. The raw detail is recorded via `tracing` (diagnostic
//! logs only), so support can still see exactly what happened — it just never
//! lands in the UI. This mirrors the Tauri-IPC error-sanitisation discipline
//! already used by `clear_saved_ui_state`.
//!
//! Every user-visible surface that previously interpolated a raw `{e}` / `{url}`
//! / `{path}` routes through one of the helpers here. The helpers are pure apart
//! from the single `tracing::warn!` side effect, so they are unit-testable and
//! the copy can be asserted to be free of leaked detail.
//!
//! Each binary (the egui shell, the legacy winit shell) and each `#[path]`-
//! included test binary uses a different subset of these helpers, so the module
//! carries a crate-level `dead_code` allowance — exactly like the other shared
//! `#[path]`-included modules (`issue_intake`, `reporting`).
#![allow(dead_code)]

use std::fmt::Display;

/// Record `detail` against `context` in the diagnostic log, then return the
/// plain `copy` for display. The single choke point that keeps technical
/// detail in the logs and out of the UI.
fn logged(context: &'static str, detail: impl Display, copy: &str) -> String {
    tracing::warn!(target: "c0pl4nd::user_error", context, detail = %detail);
    copy.to_string()
}

/// A pane's shell could not be launched (missing shell, bad PATH, permission).
/// (Inventory C0-001.)
pub fn shell_spawn_failed(detail: impl Display) -> String {
    logged(
        "shell_spawn",
        detail,
        "Couldn't start the shell in this pane. Check that your shell is \
         installed and on your PATH, then open a new pane.",
    )
}

/// A pane started with an explicit program could not launch. (Inventory C0-002.)
pub fn program_spawn_failed(detail: impl Display) -> String {
    logged(
        "program_spawn",
        detail,
        "Couldn't start that program. Check it is installed and try again.",
    )
}

/// The window / graphics stack failed to initialise at startup. Returns the
/// dialog BODY; the caller supplies the title. (Inventory C0-005 / C0-006.)
pub fn gpu_init_failed(detail: impl Display) -> String {
    logged(
        "gpu_init",
        detail,
        "C0PL4ND couldn't initialize its window or graphics. This is usually a \
         graphics-driver problem. Try updating your graphics driver, or relaunch \
         with WGPU_BACKEND=dx12 (Windows) or WGPU_BACKEND=gl (Linux). If it keeps \
         happening, see TROUBLESHOOTING.",
    )
}

/// Persisting a configuration change to disk failed. `lead` names the change in
/// plain words and starts the sentence — e.g. "Your settings change", "The
/// layout change", "Your reporting choice". (Inventory C0-010 / C0-011 / C0-012.)
pub fn config_save_failed(detail: impl Display, lead: &str) -> String {
    logged(
        "config_save",
        detail,
        &format!(
            "{lead} was applied for now but couldn't be saved. Check that the \
             settings folder is writable and has free space, then try again."
        ),
    )
}

/// The settings file exists but could not be read/parsed on launch; defaults are
/// in use. (Inventory C0-008 — GUI toast; C0-041 — legacy stderr.)
pub fn config_load_failed(detail: impl Display) -> String {
    logged(
        "config_load",
        detail,
        "Your settings file couldn't be read, so defaults are in use. Open \
         Settings and re-save to rewrite it, or see TROUBLESHOOTING.",
    )
}

/// The settings file changed on disk while the app was running, but the new
/// contents could not be parsed. Distinct from [`config_load_failed`]: nothing
/// falls back to defaults here — the settings already running stay in force, so
/// a half-typed TOML line never wipes a live session's setup.
pub fn config_reload_failed(detail: impl Display) -> String {
    logged(
        "config_reload",
        detail,
        "Your settings file changed but couldn't be read, so the settings \
         already running were kept. Fix the file and save again, or see \
         TROUBLESHOOTING.",
    )
}

/// A user-authored theme file exists but failed to parse; fallback colours are
/// in use. `name` is the user's own theme name (their config value, not an
/// internal identifier), so it is safe to echo. (Inventory C0-009.)
pub fn theme_load_failed(detail: impl Display, name: &str) -> String {
    tracing::warn!(
        target: "c0pl4nd::user_error",
        context = "theme_load",
        detail = %detail,
    );
    format!(
        "The theme \"{name}\" couldn't be loaded, so the default colors are in \
         use. Check your theme file for typos, or pick another theme in Settings."
    )
}

/// Map an internal in-app updater failure reason (the raw `UpdateState::Failed`
/// payload) to one of a small set of plain user outcomes. The raw reason is
/// logged for diagnostics and never shown. (Inventory C0-016 + C0-017..C0-031.)
///
/// Ordering is most-specific first so a reason that matches several token sets
/// (e.g. an extraction error that mentions both "archive" and "entries") lands
/// in the right bucket.
pub fn update_failed_user_copy(raw: &str) -> String {
    tracing::warn!(
        target: "c0pl4nd::user_error",
        context = "update_failed",
        detail = %raw,
    );
    let r = raw.to_ascii_lowercase();

    let copy = if r.contains("relaunch failed") {
        // restart-failed (binary swapped, but the new process would not start;
        // the prior version was restored).
        "The update was installed but C0PL4ND couldn't restart, so your previous \
         version was restored. Please close and reopen C0PL4ND."
    } else if r.contains("anti-rollback")
        || r.contains("downgrade")
        || r.contains("older version")
        || r.contains("up to date")
    {
        // downgrade / rollback-blocked.
        "This update was blocked because it would move C0PL4ND to an older \
         version. No changes were made."
    } else if r.contains("a fresh install is required")
        || r.contains("in-place update refused")
        || r.contains("below the manifest minimum_version")
    {
        // too-old-to-update-in-place: this installed build predates the update
        // system's current minimum supported version, so an in-place auto-update
        // is refused BY DESIGN — NOT a transient error. Checked BEFORE the
        // manifest bucket below because the floor-refusal reason string also
        // contains the word "manifest" and would otherwise be mis-mapped to the
        // misleading "try again later" copy. A one-time manual reinstall of the
        // latest release restores automatic updates.
        "This installed version is too old to auto-update in place. Download the \
         latest installer from the official GitHub releases page and reinstall \
         once — automatic updates will work normally afterward."
    } else if r.contains("release json")
        || r.contains("parse release")
        || r.contains("release response")
        || r.contains("tag_name")
        || r.contains("no releases")
        || r.contains("manifest")
    {
        // manifest-could-not-be-verified: the update listing itself was
        // unreadable (checked before the signature/verify bucket so a "manifest
        // parse failed after signature verified" reason lands here, not there).
        "Couldn't confirm the update details from GitHub. Try again later, or \
         download from the official GitHub releases page."
    } else if r.contains("signature")
        || r.contains("checksum")
        || r.contains("sidecar")
        || r.contains("verification")
        || r.contains("refusing")
        || r.contains("non-https")
        || r.contains("allowlist")
        || r.contains("prerelease")
    {
        // verification / safety-stop: the download could not be confirmed
        // genuine, or a safety guard refused it (every guard refusal phrase
        // starts with "refusing …"). Discarded, current build intact.
        "The downloaded update couldn't be verified as authentic and was \
         discarded for your safety. Try again, or download from the official \
         GitHub releases page."
    } else if r.contains("launch the installer") || r.contains("run it manually") {
        // The verified self-elevating installer (the Program-Files fallback) was
        // staged but could not be started at all — NOT a UAC decline (that is
        // swallowed and the prior version relaunches), but a genuine launch
        // failure. The seamless installer path is the DEFAULT for a protected
        // install; this copy only appears when that launch itself fails.
        "C0PL4ND couldn't start the installer to finish updating. Please try again, \
         or download the latest release from GitHub and run it to update."
    } else if r.contains("not writable")
        || r.contains("access is denied")
        || r.contains("permission denied")
        || r.contains("os error 5")
        || r.contains("run once elevated")
        || r.contains("read-only")
        || r.contains("readonly")
    {
        // permission / relocate — the LAST-RESORT dead-end. A protected
        // (Program Files / admin-owned) install now updates SEAMLESSLY by default
        // via the verified self-elevating installer (one permission prompt, then
        // a silent in-place update). This copy is only reached when that path is
        // unavailable — the release ships no installer for this platform (e.g. an
        // admin-extracted install on a platform without a setup.exe) — so the
        // honest relocate/elevate guidance is the right, and only-now-residual,
        // fix. This is NOT a disk-space problem (the old "free disk space" copy
        // was a dead-end); checked BEFORE the disk/unpack bucket so an
        // access-denied `os error 5` lands here.
        "C0PL4ND couldn't update itself because it's installed in a folder this \
         account can't modify (for example Program Files) and there's no installer \
         for your platform. Move C0PL4ND to a folder you own — such as your user \
         folder or Desktop — and try again, or run it once as administrator so the \
         update can install."
    } else if r.contains("staging")
        || r.contains("install failed")
        || r.contains("extract")
        || r.contains("chmod")
        || r.contains("stat ")
        || r.contains("write")
        || r.contains("create")
        || r.contains("tar ")
        || r.contains("zip")
        || r.contains("archive")
        || r.contains("unpack")
    {
        // disk / unpack: the update could not be staged, written, or unpacked
        // for a genuine storage reason (out of space, a broken archive). A
        // permission failure is caught by the bucket ABOVE, so this copy's
        // disk-space guidance is only shown when it is actually apt.
        "The update couldn't be saved or unpacked. Make sure there is free disk \
         space and try again, or download from the official GitHub releases page."
    } else if r.contains("download")
        || r.contains("offline")
        || r.contains("reach")
        || r.contains("read failed")
        || r.contains("redirect")
        || r.contains("network")
        || r.contains("host")
    {
        // no-network: the download or check could not reach GitHub.
        "Couldn't download the update. Check your internet connection and try \
         again, or download the latest release from GitHub."
    } else {
        // Generic fallback (C0-016): an unclassified failure.
        "The update couldn't be completed. Your current version is unchanged. \
         Check your internet connection and try again, or download the latest \
         release from GitHub."
    };
    copy.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokens that must NEVER appear in a user-facing string: a smoke check that
    /// the copy stays free of leaked internals. (We can't catch every possible
    /// leak, but these are the patterns the inventory flagged.)
    fn assert_clean(s: &str) {
        for banned in [
            "{e}", "{e:#}", "{url}", "{path}", "errno", "0x", "://", "\\", "Err(",
        ] {
            assert!(
                !s.contains(banned),
                "user copy leaked an internal token {banned:?}: {s:?}"
            );
        }
        // No raw debug-formatted error chains (": Os {" / "kind:" style).
        assert!(!s.contains("Os {"), "user copy leaked a debug error: {s:?}");
    }

    #[test]
    fn shell_and_program_copy_is_clean_and_actionable() {
        let s = shell_spawn_failed("No such file or directory (os error 2)");
        assert_clean(&s);
        assert!(s.contains("shell"));
        assert!(s.to_lowercase().contains("path"));
        let p = program_spawn_failed("permission denied (os error 13)");
        assert_clean(&p);
        assert!(p.contains("program"));
    }

    #[test]
    fn gpu_and_config_and_theme_copy_is_clean() {
        assert_clean(&gpu_init_failed("wgpu: no compatible adapter at C:\\x"));
        assert_clean(&config_save_failed(
            "Os { code: 5 }",
            "Your settings change",
        ));
        assert_clean(&config_load_failed("expected `=` at line 3 column 1"));
        let t = theme_load_failed("invalid hex `#zz`", "my-theme");
        assert_clean(&t);
        // The user's OWN theme name is allowed to appear.
        assert!(t.contains("my-theme"));
    }

    #[test]
    fn config_save_lead_starts_the_sentence() {
        let s = config_save_failed("disk full", "The layout change");
        assert!(s.starts_with("The layout change was applied for now"));
        assert_clean(&s);
    }

    #[test]
    fn updater_buckets_route_to_the_right_plain_outcome() {
        // restart-failed
        let restart = update_failed_user_copy("relaunch failed, rolled back: os error 2");
        assert!(restart.contains("couldn't restart"));
        assert_clean(&restart);

        // downgrade-blocked
        for raw in [
            "downgrade blocked: 0.4.0 < 0.4.9 (refusing to install an older version)",
            "update refused by anti-rollback gate",
            "already up to date — no newer version to install",
        ] {
            let s = update_failed_user_copy(raw);
            assert!(s.contains("older"), "raw {raw:?} -> {s:?}");
            assert_clean(&s);
        }

        // verification / safety-stop
        for raw in [
            "signature verification failed: bad signature",
            "checksum mismatch",
            "sha256 sidecar was empty",
            "refusing non-https download URL: http://evil",
            "refusing to extract: archive has more than 4096 entries",
            "refusing download: declared size 999 B exceeds cap 50 B",
        ] {
            let s = update_failed_user_copy(raw);
            assert!(s.contains("verified as authentic"), "raw {raw:?} -> {s:?}");
            assert_clean(&s);
        }

        // too-old-to-update-in-place (minimum_version floor). MUST route to the
        // clear "reinstall" copy, NOT the generic "couldn't confirm" manifest
        // bucket — even though the floor-refusal reason ALSO contains "manifest".
        for raw in [
            "installed version 0.4.22 is below the manifest minimum_version 0.4.23 \
             — a fresh install is required (in-place update refused)",
            "in-place update refused: installed build too old",
        ] {
            let s = update_failed_user_copy(raw);
            assert!(s.contains("too old to auto-update"), "raw {raw:?} -> {s:?}");
            assert!(
                !s.contains("Try again later"),
                "a too-old refusal must NOT read as a transient error: {s:?}"
            );
            assert_clean(&s);
        }

        // manifest-could-not-be-verified
        for raw in [
            "failed to parse release JSON: expected value",
            "no tag_name in latest release",
            "manifest parse failed after signature verified: x",
        ] {
            let s = update_failed_user_copy(raw);
            assert!(s.contains("update details"), "raw {raw:?} -> {s:?}");
            assert_clean(&s);
        }

        // permission / relocate (access-denied install dir — NOT disk space).
        // This is the load-bearing fix: the running-exe swap failing with an
        // access-denied `os error 5` (Program Files / admin-owned folder) must
        // route to the actionable relocate/elevate copy, never "free disk space".
        for raw in [
            "install failed: Access is denied. (os error 5)",
            "install location not writable: run once elevated or move the app",
            "swap failed: permission denied",
            "failed to create staging dir: The media is write protected (read-only)",
        ] {
            let s = update_failed_user_copy(raw);
            assert!(
                s.contains("account can't modify") && s.contains("administrator"),
                "raw {raw:?} -> {s:?}"
            );
            assert!(
                !s.contains("free disk space"),
                "a permission failure must NOT be labelled a disk-space problem: {s:?}"
            );
            assert_clean(&s);
        }

        // installer-launch failure (the Program-Files seamless fallback): the
        // verified self-elevating installer was staged but could not be started
        // at all. It must route to the "couldn't start the installer" copy (its
        // own bucket), NOT the relocate/admin copy and NOT a disk-space message.
        for raw in [
            "couldn't launch the installer (os error 2). You can run it manually: \
             C:\\tmp\\c0pl4nd-setup.exe",
            "couldn't launch the installer (The system cannot find the file). \
             You can run it manually: setup.exe",
        ] {
            let s = update_failed_user_copy(raw);
            assert!(
                s.contains("couldn't start the installer"),
                "an installer-launch failure must get its own copy: raw {raw:?} -> {s:?}"
            );
            assert!(
                !s.contains("free disk space"),
                "a launch failure is not a disk-space problem: {s:?}"
            );
            assert_clean(&s);
        }

        // no-installer-for-platform (admin-owned dir, no setup.exe): the residual
        // relocate/elevate dead-end. Still routes to the account-can't-modify copy.
        {
            let raw = "install location not writable: C0PL4ND cannot modify its own \
                       program folder and this release has no installer for your \
                       platform — move it to a folder you own or run once elevated";
            let s = update_failed_user_copy(raw);
            assert!(
                s.contains("account can't modify") && s.contains("administrator"),
                "raw {raw:?} -> {s:?}"
            );
            assert_clean(&s);
        }

        // disk / unpack — a GENUINE storage failure (out of space, corrupt
        // archive), with no permission token, still lands in the disk bucket.
        for raw in [
            "cannot create staging dir: There is not enough space on the disk. (os error 112)",
            "install failed: no space left on device",
            "failed to write extracted bytes: disk full",
            "failed to read tar entries: corrupt",
        ] {
            let s = update_failed_user_copy(raw);
            assert!(s.contains("saved or unpacked"), "raw {raw:?} -> {s:?}");
            assert_clean(&s);
        }

        // no-network
        for raw in [
            "offline",
            "download failed for https://x: connection reset",
            "too many redirects (> 5) fetching https://x",
        ] {
            let s = update_failed_user_copy(raw);
            assert!(s.contains("internet connection"), "raw {raw:?} -> {s:?}");
            assert_clean(&s);
        }

        // generic fallback
        let g = update_failed_user_copy("something nobody anticipated");
        assert!(g.contains("couldn't be completed"));
        assert_clean(&g);
    }
}
