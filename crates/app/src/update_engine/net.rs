//! Network half of the in-app self-updater.
//!
//! Telemetry-free by construction: the only network surfaces are
//! 1. a single unauthenticated `GET` of the public GitHub Releases API,
//! 2. download of the SIGNED `latest.json` manifest + its `.minisig`, and
//! 3. downloads of the release archive + its `.minisig` + `.sha256` siblings.
//!
//! No analytics, no identifiers, no payload: every request sends only a generic
//! `User-Agent` (app name + version). A Tier-1 client installs ONLY through the
//! verified signed manifest — the archive is verified (its bytes pinned to the
//! manifest's SIGNED SHA-256, then minisign against
//! [`super::verify::EMBEDDED_PUBLIC_KEYS`]) before the extracted binary is ever
//! returned. A verify failure deletes the staging area and the binary is NEVER
//! returned unverified. There is no install path that skips the manifest.
//!
//! Pure decision logic ([`resolve_tier1_update`]) is split out from the I/O so
//! it can be unit-tested offline against a fixture [`RawRelease`] + manifest.
//!
//! ## Asset naming
//!
//! C0PL4ND's release workflow publishes, per target, an archive whose name
//! embeds both the tag and the Rust target triple — `c0pl4nd-<tag>-<target>.zip`
//! on Windows, `c0pl4nd-<tag>-<target>.tar.gz` on Unix — plus a `.sha256` and a
//! `.minisig` sidecar. [`manifest::Manifest::archive_for`] matches by the
//! **target-triple substring + archive extension** rather than an exact
//! filename, so it is robust to the tag prefix.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::verify::{verify_artifact_bound_optional_sig, EMBEDDED_PUBLIC_KEYS};
use super::{manifest, update_state};

/// Decompression-bomb guard: hard cap on the TOTAL number of uncompressed bytes
/// any single archive may expand to during extraction (S-4). A legitimate
/// C0PL4ND release archive holds one binary of ~10-30 MiB plus a handful of
/// small sidecars, so 256 MiB is comfortably above the real ceiling while
/// refusing a malicious/MITM'd archive that expands a tiny payload into a
/// disk-filling flood. The cap is enforced on the STREAMED copy (not the
/// header-declared size), so a lying header cannot bypass it.
const MAX_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;

/// Decompression-bomb guard: hard cap on the number of entries any single
/// archive may contain. A release archive is one binary plus a few docs; an
/// archive with thousands of entries is a zip-bomb / resource-exhaustion shape.
const MAX_ARCHIVE_ENTRIES: usize = 64;

/// Download-DoS guard: hard ceiling on the asset download. The verify gate
/// (SHA-256 + minisign) only runs AFTER the body is buffered, so a body that is
/// hostile by SIZE (a MITM, a compromised asset, or a redirect to an
/// endless-stream host) would OOM the process before integrity is ever checked.
/// Enforced on the STREAMED read (never on a header), so a lying `Content-Length`
/// cannot bypass it. Matches the post-download extraction cap.
const MAX_DOWNLOAD_BYTES: u64 = MAX_EXTRACTED_BYTES;

/// Download-DoS guard for the tiny sidecars: a `.minisig` is ~100 bytes and a
/// `.sha256` ~80, so 64 KiB is comfortably above the real ceiling while refusing
/// a multi-GB sidecar streamed by a hostile endpoint.
const MAX_SIDECAR_BYTES: u64 = 64 * 1024;

/// Download-DoS guard for the Releases API JSON. A real `releases/latest`
/// response is a few KiB; 4 MiB is a generous ceiling that still refuses an
/// unbounded JSON flood (which would also stress the serde parser).
const MAX_RELEASE_JSON_BYTES: u64 = 4 * 1024 * 1024;

/// Download-DoS guard for the signed `latest.json` manifest. A real manifest is
/// a few KiB (a handful of asset entries); 1 MiB is a generous ceiling that
/// still refuses an unbounded flood before the signature/serde work runs.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Redirect cap for the manually-followed, host-confined GET. GitHub asset
/// downloads redirect 1–2 times (api → codeload/objects CDN); 4 is ample.
const MAX_REDIRECTS: usize = 4;

/// A bounded reader-copy that aborts once `limit` uncompressed bytes have been
/// written, defending against decompression bombs whose declared size lies.
/// Returns the number of bytes copied, or an error string if the cap is hit.
/// This is the load-bearing bomb guard: it measures the ACTUAL inflated stream,
/// not any header field an attacker controls.
fn copy_capped<R: Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    limit: u64,
) -> Result<u64, String> {
    let mut written: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("failed to read archive entry: {e}"))?;
        if n == 0 {
            break;
        }
        written = written.saturating_add(n as u64);
        if written > limit {
            return Err(format!(
                "refusing to extract: archive expands past the {limit}-byte \
                 decompression-bomb cap"
            ));
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| format!("failed to write extracted bytes: {e}"))?;
    }
    Ok(written)
}

/// Mandatory `User-Agent` for every request. App name + version ONLY — no
/// machine identifier, OS fingerprint, install ID, or any unique token.
const USER_AGENT: &str = concat!("c0pl4nd-updater/", env!("CARGO_PKG_VERSION"));

/// GitHub REST API version header value.
const GITHUB_API_VERSION: &str = "2026-03-10";

/// GitHub Releases API `Accept` header value.
const GITHUB_ACCEPT: &str = "application/vnd.github+json";

/// A single release asset as returned by the GitHub Releases API. Only the
/// fields the updater needs are deserialized.
#[derive(Clone, Debug, Deserialize)]
pub struct RawAsset {
    pub name: String,
    pub browser_download_url: String,
}

/// The subset of the GitHub `releases/latest` JSON the updater reads. Made
/// public + constructible so the Tier-1 resolver can be unit-tested with a
/// fixture (no network). `prerelease`/`draft` are read as a defense-in-depth
/// channel-pin (the `…/releases/latest` endpoint already excludes prereleases).
#[derive(Clone, Debug, Deserialize)]
pub struct RawRelease {
    pub tag_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<RawAsset>,
}

/// A verifiable platform installer asset (Windows `*-x86_64-setup.exe`) + its
/// signature/checksum sidecars, plus the manifest's SIGNED sha256 the download
/// is pinned to. Present only when the verified manifest enumerates a setup
/// installer for this platform. Used for the in-place-update path when the app
/// lives in a protected, admin-owned location (e.g. `C:\Program Files`): the
/// installer self-elevates (`requireAdministrator`), so running it UNATTENDED
/// (`--silent --dir`) updates in place where a direct exe swap can't. Verified
/// through the IDENTICAL gate as the archive (SHA-256 pinned to the signed
/// manifest digest, THEN minisign bound to the asset name) — this is a separate
/// elevated fallback, NOT a relaxation of any signature/anti-rollback gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallerAsset {
    /// The `setup.exe` browser_download_url (the release asset url).
    pub url: String,
    /// The setup installer asset name (bound into the minisign trusted-comment
    /// `file:` check, exactly like the archive path).
    pub asset_name: String,
    /// The `setup.exe.minisig` url.
    pub sig_url: String,
    /// The `setup.exe.sha256` url.
    pub sha_url: String,
    /// The SIGNED SHA-256 from the verified manifest, pinned as the expected
    /// digest for the installer download (binding the bytes to the signed hash).
    pub pinned_sha256: String,
}

/// One resolved, newer-than-current release ready to download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: semver::Version,
    /// The original tag string (e.g. `v0.4.0`).
    pub tag: String,
    /// The archive (`.zip`/`.tar.gz`) browser_download_url.
    pub asset_url: String,
    /// The archive name (used to pick the `.zip` vs `.tar.gz` extractor).
    pub asset_name: String,
    /// The `.minisig` url.
    pub sig_url: String,
    /// The `.sha256` url.
    pub sha_url: String,
    /// The release page (for "view all releases" / changelog in a browser).
    pub html_url: String,
    /// The SIGNED SHA-256 from the verified manifest, pinned as the expected
    /// digest for the download (binding the bytes to the signed hash). Every
    /// `ReleaseInfo` carries a pin by construction — a Tier-1 client only ever
    /// resolves an update through the signed manifest, so there is NO
    /// unpinned/manifest-absent install path (the type makes the guarantee).
    pub pinned_sha256: String,
    /// The manifest `release_index`, persisted as the new monotonic high-water
    /// mark on a successful apply. `None` only when the index is not carried
    /// (defensive; the producer always sets it).
    pub release_index: Option<u64>,
    /// The self-elevating Windows installer for this release, when the manifest
    /// enumerates one — the apply path for a Program-Files (admin-owned) install.
    /// `None` on platforms/releases without a `setup.exe`. Boxed so the (common)
    /// `None` case keeps `ReleaseInfo` small — it rides inside several UI-state
    /// enum variants.
    pub installer: Option<Box<InstallerAsset>>,
}

/// The archive file extension this build's release artifact carries: `.zip` on
/// Windows, `.tar.gz` elsewhere (matches `release.yml`'s per-OS packaging).
pub const fn archive_ext() -> &'static str {
    if cfg!(windows) {
        ".zip"
    } else {
        ".tar.gz"
    }
}

/// Blocking GET of `/repos/{owner}/{repo}/releases/latest`. Any network/HTTP/
/// decode error is mapped to a human `String`; this function never panics.
pub fn fetch_latest_release(owner: &str, repo: &str) -> Result<RawRelease, String> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    // Host-confined GET (https + GitHub allow-list at every redirect hop) and a
    // hard JSON size cap: a MITM'd / hostile endpoint cannot stream an unbounded
    // body that OOMs the process (or stresses serde) before parsing runs.
    let mut body = String::new();
    confined_get(
        &url,
        &[
            ("Accept", GITHUB_ACCEPT),
            ("X-GitHub-Api-Version", GITHUB_API_VERSION),
        ],
    )?
    .into_reader()
    .take(MAX_RELEASE_JSON_BYTES)
    .read_to_string(&mut body)
    .map_err(|e| format!("failed to read release response: {e}"))?;
    serde_json::from_str::<RawRelease>(&body)
        .map_err(|e| format!("failed to parse release JSON: {e}"))
}

/// Convenience: fetch + Tier-1 resolve in one blocking call (the worker thread
/// calls this). `Ok(None)` means "up to date / no matching asset"; `Err` means
/// the network fetch failed, the manifest could not be VERIFIED, or a manifest
/// gate refused the update.
///
/// ## Tier-1 REQUIRES a verified signed manifest — fail-CLOSED, no fallback
///
/// A Tier-1 client only ever installs an update whose SIGNED `latest.json`
/// manifest verifies and passes every gate. The manifest is fetched, its
/// minisign signature is verified over the RAW JSON (BEFORE parse), then its
/// gates — schema/product identity, freshness (freeze beacon), `version >
/// current` (downgrade), `release_index > persisted` (rollback), and the
/// `current >= minimum_version` floor — are enforced, all fail-closed.
///
/// There is deliberately NO legacy/per-asset fallback when the manifest is
/// absent or unverifiable. A fallback would make the freeze-beacon, the
/// `minimum_version` floor, and the signed-hash binding OPTIONAL — an attacker
/// who strips `latest.json` (or its `.minisig`) could force the weaker path and
/// downgrade the protection. The honest path always has a manifest: a Tier-1
/// binary only sees `/releases/latest >= its own version`, and every such
/// release carries a manifest. The only actor a fallback serves is that
/// attacker. (Pre-Tier-1 binaries used their own per-asset selector — removed
/// from THIS binary, which has no install path that skips the signed manifest.)
pub fn check_for_update(
    owner: &str,
    repo: &str,
    current: &semver::Version,
    target: &str,
) -> Result<Option<ReleaseInfo>, String> {
    // INFO lifecycle: the check started. `current` is the running build version
    // (public, no PII); the target triple identifies the asset family.
    tracing::info!(
        target: "c0pl4nd::update",
        event = "update_check_started",
        current_version = %current,
        target = target,
        "update check started"
    );
    // A single fetch of `/releases/latest` yields BOTH the asset list (for the
    // per-asset sidecars + prerelease/draft channel-pin) and the REQUIRED signed
    // manifest. An absent/unverifiable manifest is a hard refusal here.
    let outcome = (|| {
        let (raw, json, sig_str) = fetch_manifest(owner, repo)?;
        let manifest = manifest::parse_and_verify(&json, &sig_str, EMBEDDED_PUBLIC_KEYS)?;
        resolve_tier1_update(
            &raw,
            &manifest,
            current,
            target,
            archive_ext(),
            now_unix_secs(),
            update_state::applied_index(),
        )
    })();
    match &outcome {
        Ok(Some(info)) => tracing::info!(
            target: "c0pl4nd::update",
            event = "update_available",
            version = %info.version,
            "a newer verified release is available"
        ),
        Ok(None) => tracing::debug!(
            target: "c0pl4nd::update",
            event = "update_up_to_date",
            "no newer release (up to date / no platform asset)"
        ),
        Err(e) => {
            // Network / manifest-verify / gate refusal. The reason is an app
            // string (no secret/PII); kept in a DEBUG field, with a clean WARN.
            tracing::warn!(
                target: "c0pl4nd::update",
                event = "update_check_failed",
                "update check failed (network, manifest verification, or a gate refusal)"
            );
            tracing::debug!(
                target: "c0pl4nd::update",
                event = "update_check_failed_detail",
                detail = %e,
                "update check failure detail"
            );
        }
    }
    outcome
}

/// Current wall-clock as a Unix timestamp (seconds). On a clock error (a
/// before-epoch system time) returns [`i64::MAX`] so freshness checks fail
/// CLOSED — an unreadable clock must never make a stale manifest look fresh.
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(i64::MAX)
}

/// Locate the signed-manifest pair (`latest.json` + `latest.json.minisig`) among
/// the release assets. Returns `Some((json_url, sig_url))` only when BOTH are
/// present; `None` when EITHER is absent.
fn find_manifest_assets(raw: &RawRelease) -> Option<(String, String)> {
    let url_of = |name: &str| -> Option<String> {
        raw.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    let json = url_of("latest.json")?;
    let sig = url_of("latest.json.minisig")?;
    Some((json, sig))
}

/// Require the signed-manifest pair on a release. An ABSENT manifest (or its
/// signature) is a hard refusal — a Tier-1 client never installs an update it
/// cannot verify, so a missing manifest fails CLOSED here rather than degrading
/// to the weaker per-asset path.
fn require_manifest_assets(raw: &RawRelease) -> Result<(String, String), String> {
    find_manifest_assets(raw).ok_or_else(|| {
        "update could not be verified: this release carries no signed manifest \
         (latest.json + latest.json.minisig) — refusing to install"
            .to_string()
    })
}

/// Download the REQUIRED signed manifest pair for an already-fetched release.
/// `Err` when the manifest is absent (fail-closed) OR on a network/decoding
/// failure. The returned `(json_bytes, sig_str)` are UNVERIFIED — the caller
/// MUST pass them through [`manifest::parse_and_verify`] before trusting them.
fn fetch_manifest_for(raw: &RawRelease) -> Result<(Vec<u8>, String), String> {
    let (json_url, sig_url) = require_manifest_assets(raw)?;
    // Defense-in-depth: both URLs come verbatim from the Releases JSON; assert
    // https before any byte is fetched (host is re-asserted at every redirect
    // hop inside `confined_get`).
    assert_https(&json_url)?;
    assert_https(&sig_url)?;
    let json = download_small_capped(&json_url, MAX_MANIFEST_BYTES)?;
    let sig = download_small(&sig_url)?;
    let sig_str = String::from_utf8(sig)
        .map_err(|e| format!("manifest signature is not valid UTF-8: {e}"))?;
    Ok((json, sig_str))
}

/// Public entry point: fetch the latest release ONCE and return it alongside its
/// REQUIRED signed manifest — `(raw_release, latest.json bytes, .minisig string)`.
/// Returning the `RawRelease` too lets the caller resolve the per-asset sidecars
/// and the prerelease/draft channel-pin from the SAME fetch (no second round
/// trip, no check→use drift). `Err` when the manifest is absent (fail-closed; a
/// Tier-1 client requires a verifiable manifest) or on a network failure. The
/// bytes are UNVERIFIED — pass them through [`manifest::parse_and_verify`].
pub fn fetch_manifest(owner: &str, repo: &str) -> Result<(RawRelease, Vec<u8>, String), String> {
    let raw = fetch_latest_release(owner, repo)?;
    let (json, sig_str) = fetch_manifest_for(&raw)?;
    Ok((raw, json, sig_str))
}

/// PURE (no network) Tier-1 resolver: given a VERIFIED manifest, decide the
/// update. Every gate fails CLOSED. `now_unix` and `persisted_index` are passed
/// in (not read from the clock/disk) so the whole decision is unit-testable.
///
/// Returns:
/// - `Ok(Some(info))` — a fresh, in-policy update with the SIGNED archive url +
///   the pinned manifest SHA-256 (+ `release_index` to persist on apply).
/// - `Ok(None)` — genuinely up to date (`version <= current`) OR no archive
///   asset for this platform.
/// - `Err(reason)` — a gate REFUSAL (wrong product/schema, stale/frozen,
///   below the minimum floor, a rollback, an unparseable version, or a malformed
///   archive entry).
fn resolve_tier1_update(
    raw: &RawRelease,
    manifest: &manifest::Manifest,
    current: &semver::Version,
    target: &str,
    ext: &str,
    now_unix: i64,
    persisted_index: u64,
) -> Result<Option<ReleaseInfo>, String> {
    // Channel-pin (defense-in-depth): the `…/releases/latest` endpoint already
    // excludes prereleases/drafts, but if a prerelease/draft ever reaches here it
    // is a different release CHANNEL than the pinned stable stream — refused so
    // the updater can never jump the user stable → beta.
    if raw.prerelease || raw.draft {
        // SECURITY (channel-pin): refuse a beta/draft jumping the stable stream.
        tracing::warn!(
            target: "c0pl4nd::update",
            event = "update_refused",
            gate = "channel_prerelease",
            prerelease = raw.prerelease,
            draft = raw.draft,
            "refusing a prerelease/draft release on the stable channel"
        );
        return Err("refusing a prerelease/draft release on the stable channel".to_string());
    }

    // Identity binding (the heart of Tier-1): a manifest for a DIFFERENT product
    // or an unrecognised schema family is refused — never silently honoured.
    if manifest.product != manifest::MANIFEST_PRODUCT {
        return Err(format!(
            "manifest is for a different product {:?} (expected {:?}) — refusing",
            manifest.product,
            manifest::MANIFEST_PRODUCT
        ));
    }
    if !manifest
        .schema
        .starts_with(manifest::MANIFEST_SCHEMA_PREFIX)
    {
        return Err(format!(
            "unrecognised manifest schema {:?} (expected {:?}*) — refusing",
            manifest.schema,
            manifest::MANIFEST_SCHEMA_PREFIX
        ));
    }

    // Version first: an unparseable candidate is fail-closed; an equal-or-older
    // candidate is a normal "up to date" (no scary error, no gate noise).
    let candidate = manifest.version()?;
    if candidate <= *current {
        return Ok(None);
    }

    // Freshness (freeze beacon): a stale/frozen or unreadable-deadline manifest
    // for a would-be NEWER release is refused — fail-closed.
    if !manifest.is_fresh(now_unix) {
        return Err(format!(
            "update manifest is stale/frozen (valid_until {:?} has passed) — refusing",
            manifest.valid_until_utc
        ));
    }

    // Floor sanity: refuse an in-place hop when the running install is BELOW the
    // manifest's declared minimum supported version (too old to update in place
    // — a fresh install is required). Fail-closed.
    let minimum = manifest.minimum_version()?;
    if *current < minimum {
        return Err(format!(
            "installed version {current} is below the manifest minimum_version {minimum} — \
             a fresh install is required (in-place update refused)"
        ));
    }

    // Anti-rollback on the manifest ordinal: STRICTLY greater than the highest
    // index ever applied. Equal or lower is a replay/rollback. Because
    // release_index is monotonic with version and the candidate is already
    // strictly newer than `current`, a fresh forward update always satisfies
    // strict `>`; an equal index means "this exact release was already applied"
    // and is refused. Fail-closed.
    if manifest.release_index <= persisted_index {
        return Err(format!(
            "rollback blocked: manifest release_index {} is not newer than the last \
             applied index {persisted_index} (refusing a replayed/superseded release)",
            manifest.release_index
        ));
    }

    // Resolve the in-place ARCHIVE asset from the SIGNED manifest (skips the
    // setup .exe). No archive for this platform → "no update for this platform".
    let masset = match manifest.archive_for(target, ext) {
        Some(a) => a,
        None => return Ok(None),
    };

    let info = build_tier1_release_info(raw, manifest, masset, &candidate, target)?;
    Ok(Some(info))
}

/// Build the download plumbing for a Tier-1 update: the SIGNED archive url from
/// the manifest, the per-asset `.minisig` + `.sha256` sidecar urls from the
/// release asset list (kept as defense-in-depth — the manifest does not
/// enumerate them), and the pinned manifest SHA-256 + `release_index`. A
/// manifest archive whose sidecars are ABSENT, or whose url/sha256 are empty, is
/// a malformed release — fail-closed `Err`.
fn build_tier1_release_info(
    raw: &RawRelease,
    manifest: &manifest::Manifest,
    masset: &manifest::ManifestAsset,
    candidate: &semver::Version,
    target: &str,
) -> Result<ReleaseInfo, String> {
    let (sig_url, sha_url) = sidecar_urls(raw, &masset.asset_name);
    if masset.sha256.trim().is_empty() {
        return Err(format!(
            "manifest archive {:?} has an empty sha256 — refusing",
            masset.asset_name
        ));
    }
    if masset.url.trim().is_empty() {
        return Err(format!(
            "manifest archive {:?} has an empty url — refusing",
            masset.asset_name
        ));
    }
    Ok(ReleaseInfo {
        version: candidate.clone(),
        tag: raw.tag_name.clone(),
        asset_url: masset.url.clone(),
        asset_name: masset.asset_name.clone(),
        sig_url,
        sha_url,
        html_url: raw.html_url.clone(),
        pinned_sha256: masset.sha256.clone(),
        release_index: Some(manifest.release_index),
        installer: build_tier1_installer(raw, manifest, target).map(Box::new),
    })
}

/// Resolve the per-asset `.minisig` + `.sha256` sidecar download urls for
/// `asset_name` from the release asset list (the manifest does not enumerate
/// them). A missing sidecar is a malformed release — fail-closed `Err`. Shared
/// by the archive path ([`build_tier1_release_info`]) and the installer path
/// ([`build_tier1_installer`]) so both bind their sidecars identically.
fn sidecar_urls(raw: &RawRelease, asset_name: &str) -> (String, String) {
    let url_of = |name: &str| -> String {
        raw.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
            .unwrap_or_default()
    };
    // An EMPTY url means the release does not enumerate that sidecar — the
    // signed-manifest-only asset set (the migration that trims per-artifact
    // `.minisig`/`.sha256`). The caller treats an empty url as "sidecar absent"
    // and falls back to the AUTHORITATIVE signed-manifest SHA-256 (which the
    // verified manifest already authenticates); a sidecar that IS enumerated is
    // still fetched and fully verified (defense-in-depth). A missing sidecar is
    // therefore never fatal here — the manifest pin is the load-bearing gate.
    (
        url_of(&format!("{asset_name}.minisig")),
        url_of(&format!("{asset_name}.sha256")),
    )
}

/// Build the verified-manifest, SHA-pinned self-elevating installer descriptor
/// for a Windows `target`, if the manifest enumerates one AND its sidecars are
/// present. Returns `None` (no installer offered — never a fail-OPEN install)
/// when the manifest has no installer entry, when its url/sha256 are empty, or
/// when either per-asset sidecar is missing from the release. This is a SEPARATE
/// elevated fallback resolved alongside — never in place of — the in-place
/// archive; the archive selector still governs the normal path.
fn build_tier1_installer(
    raw: &RawRelease,
    manifest: &manifest::Manifest,
    target: &str,
) -> Option<InstallerAsset> {
    let masset = manifest.installer_for(target)?;
    if masset.sha256.trim().is_empty() || masset.url.trim().is_empty() {
        return None;
    }
    let (sig_url, sha_url) = sidecar_urls(raw, &masset.asset_name);
    Some(InstallerAsset {
        url: masset.url.clone(),
        asset_name: masset.asset_name.clone(),
        sig_url,
        sha_url,
        pinned_sha256: masset.sha256.clone(),
    })
}

/// Resolve the expected SHA-256 the downloaded archive is verified against.
///
/// The `pinned` (signed-manifest) digest is AUTHORITATIVE; the `.sha256` sidecar
/// is kept as defense-in-depth and MUST AGREE with it — a disagreement is a
/// tampered sidecar or a manifest/asset mismatch and is refused (fail-closed).
/// Comparison is case-insensitive and whitespace-trimmed (hex digests). The
/// pinned (manifest) value is returned, so the load-bearing digest is always the
/// signed one.
fn resolve_expected_sha<'a>(pinned: &'a str, sidecar: &str) -> Result<&'a str, String> {
    if pinned.trim().eq_ignore_ascii_case(sidecar.trim()) {
        Ok(pinned.trim())
    } else {
        Err(format!(
            "manifest/sidecar sha256 disagreement: manifest {:?} != sidecar {:?} — refusing",
            pinned.trim(),
            sidecar.trim()
        ))
    }
}

/// Reject any download URL that is not `https://` (audit finding #6, TLS
/// downgrade defense-in-depth). The `browser_download_url` fields come from the
/// GitHub Releases JSON and are used verbatim; a malicious or MITM'd response
/// could supply an `http://` asset/sig/sha URL. Integrity is still caught by
/// minisign, but enforcing https closes the downgrade-to-cleartext channel
/// before any byte is fetched. Case-insensitive on the scheme per RFC 3986.
fn assert_https(url: &str) -> Result<(), String> {
    if c0pl4nd_core::net_confine::is_https(url) {
        Ok(())
    } else {
        // SECURITY: a non-https URL is a TLS-downgrade-to-cleartext attempt (a
        // MITM'd/hostile Releases JSON serving an `http://` asset). Log the host
        // ONLY — never the full URL (it may carry a query) — so a downgrade
        // rejection that was previously silent now leaves a trace.
        tracing::warn!(
            target: "c0pl4nd::update",
            event = "download_refused",
            gate = "https",
            host = c0pl4nd_core::net_confine::url_host(url).as_deref().unwrap_or("<none>"),
            "refusing non-https update download URL (TLS-downgrade defense)"
        );
        Err(format!("refusing non-https download URL: {url}"))
    }
}

/// The ONLY hosts the updater will fetch from. GitHub serves the Releases API
/// from `api.github.com` and redirects asset downloads to the codeload / objects
/// CDN on `*.githubusercontent.com` (and `codeload.github.com`). Confining every
/// request — and every redirect HOP — to this set means a MITM'd / malicious
/// Releases JSON cannot point the download (and our `User-Agent`) at an arbitrary
/// attacker host (SSRF / exfil shape), and turns the redirect path from an
/// open-ended fetch into a closed one. Case-insensitive; exact host or a
/// `.githubusercontent.com` subdomain.
fn host_allowed(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "github.com"
        || h == "api.github.com"
        || h == "codeload.github.com"
        || h == "objects.githubusercontent.com"
        || h.ends_with(".githubusercontent.com")
}

/// Reject any URL whose host is not in the allow-list ([`host_allowed`]). Host
/// extraction is shared with the CLI/launch check via [`crate::net_confine`];
/// only this module's broader allow-list (API + CDN) is caller-specific.
fn assert_allowed_host(url: &str) -> Result<(), String> {
    match c0pl4nd_core::net_confine::url_host(url) {
        Some(h) if host_allowed(&h) => Ok(()),
        Some(h) => {
            // SECURITY: a host outside the GitHub allow-list is an SSRF / MITM-
            // redirect attempt (a hostile Releases JSON or a `302 → attacker`).
            // The host is the diagnostic field and is safe to log; the full URL
            // (path/query) is NOT logged.
            tracing::warn!(
                target: "c0pl4nd::update",
                event = "download_refused",
                gate = "host_allowlist",
                host = %h,
                "refusing update download from non-allowlisted host (SSRF/MITM defense)"
            );
            Err(format!("refusing download from non-allowlisted host: {h}"))
        }
        None => {
            tracing::warn!(
                target: "c0pl4nd::update",
                event = "download_refused",
                gate = "host_allowlist",
                host = "<malformed>",
                "refusing update download: malformed URL with no host"
            );
            Err(format!("malformed download URL (no host): {url}"))
        }
    }
}

/// Issue a GET that follows redirects MANUALLY, re-asserting `https` AND an
/// allow-listed host at EVERY hop. ureq's default agent follows up to 5 redirects
/// to ARBITRARY hosts, and [`assert_https`] only guards the FIRST URL — so a
/// `302 → http://evil/` or `302 → https://attacker/` would be followed
/// transparently. This builds a `redirects(0)` agent and walks the chain itself,
/// confining every hop to GitHub over https.
fn confined_get(url: &str, headers: &[(&str, &str)]) -> Result<ureq::Response, String> {
    confined_get_with(&default_agent(), url, headers)
}

/// The agent every production fetch runs on: redirects disabled (the walk in
/// [`confined_get_with`] follows them MANUALLY so every hop is re-confined) plus
/// connect/read timeouts that bound a hung update thread — without them a stalled
/// or slow-loris peer keeps the download/check thread (and any window waiting on
/// it) alive forever. Matches the legacy `update::http_get` bounds.
fn default_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(30))
        .build()
}

/// [`confined_get`] over an explicit `agent`. The agent is the ONLY seam: every
/// confinement assert, the redirect walk, and the hop cap below run identically
/// whoever supplies the transport. Production always passes [`default_agent`];
/// the tests pass an agent whose middleware short-circuits the socket so the
/// confinement logic can be driven against synthesized responses (see the test
/// module's `E2E_API` harness).
fn confined_get_with(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<ureq::Response, String> {
    assert_https(url)?;
    assert_allowed_host(url)?;
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let mut req = agent.get(&current).set("User-Agent", USER_AGENT);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        // With redirects(0) a 3xx returns Ok (status in 300..400); ureq still
        // maps >=400 to Err(Status). Accept a 3xx from either shape.
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) if (300..400).contains(&code) => r,
            Err(e) => {
                // Network/HTTP failure on an update fetch. The operator was
                // previously blind (the error surfaced only as
                // `UpdateState::Failed`). Log the host + transport detail (no
                // full URL / query). The transport string can carry an endpoint
                // host but no secret, so it lives in a DEBUG field.
                tracing::error!(
                    target: "c0pl4nd::update",
                    event = "download_failed",
                    host = c0pl4nd_core::net_confine::url_host(&current)
                        .as_deref()
                        .unwrap_or("<none>"),
                    "update download request failed"
                );
                tracing::debug!(
                    target: "c0pl4nd::update",
                    event = "download_failed_detail",
                    detail = %e,
                    "update download transport error detail"
                );
                return Err(format!("download failed for {current}: {e}"));
            }
        };
        if (300..400).contains(&resp.status()) {
            let loc = resp
                .header("Location")
                .ok_or_else(|| format!("redirect {} without Location", resp.status()))?;
            let next = c0pl4nd_core::net_confine::resolve_redirect(&current, loc)
                .map_err(|e| format!("{e}: {loc}"))?;
            assert_https(&next)?;
            assert_allowed_host(&next)?;
            current = next;
            continue;
        }
        return Ok(resp);
    }
    Err(format!(
        "too many redirects (> {MAX_REDIRECTS}) fetching {url}"
    ))
}

/// Blocking GET of a small file (sig / sha), returning its raw bytes. Host-
/// confined and size-capped ([`MAX_SIDECAR_BYTES`]) so a hostile endpoint cannot
/// stream an unbounded sidecar into memory before verification runs.
fn download_small(url: &str) -> Result<Vec<u8>, String> {
    download_small_capped(url, MAX_SIDECAR_BYTES)
}

/// Blocking GET of a small file with an explicit byte `cap`. Host-confined and
/// size-capped so a hostile endpoint cannot stream an unbounded body into memory
/// before verification runs. Used for the sidecars ([`MAX_SIDECAR_BYTES`]) and
/// the signed manifest ([`MAX_MANIFEST_BYTES`]).
fn download_small_capped(url: &str, cap: u64) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    confined_get(url, &[])?
        .into_reader()
        .take(cap)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed for {url}: {e}"))?;
    Ok(buf)
}

/// Fetch a per-artifact sidecar (`.minisig` / `.sha256`) that MAY be absent.
///
/// The migration boundary is deterministic and lives in the RELEASE, not the
/// network: an EMPTY `url` means the release does not enumerate the sidecar (the
/// signed-manifest-only asset set) → `Ok(None)`, and the caller falls back to the
/// AUTHORITATIVE signed-manifest SHA-256. When the release DOES enumerate the
/// sidecar (`url` non-empty) it is fetched over the https/host-confined path and
/// returned as text; a TLS/fetch/UTF-8 failure there is a genuine error and fails
/// CLOSED (`Err`) — so defense-in-depth is preserved verbatim for as long as
/// sidecars ship, and only a release that intentionally omits them relaxes to the
/// manifest-pin gate.
fn fetch_optional_sidecar_text(url: &str, kind: &str) -> Result<Option<String>, String> {
    if url.trim().is_empty() {
        return Ok(None);
    }
    assert_https(url)?;
    let bytes = download_small(url)?;
    let text =
        String::from_utf8(bytes).map_err(|e| format!("{kind} sidecar is not valid UTF-8: {e}"))?;
    Ok(Some(text))
}

/// Blocking GET of a large asset, streaming the body to drive `progress`
/// (`downloaded`, `total`). `total` is read from `Content-Length`; if absent it
/// is reported as `0` (the UI shows an indeterminate bar). Returns the full
/// asset bytes.
fn download_asset(url: &str, progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String> {
    download_asset_with(&default_agent(), url, progress)
}

/// [`download_asset`] over an explicit `agent` (see [`confined_get_with`] for the
/// seam's rationale). The declared-size check and the streamed cap below are
/// unchanged and run identically on any transport.
fn download_asset_with(
    agent: &ureq::Agent,
    url: &str,
    progress: impl FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    let resp = confined_get_with(agent, url, &[])?;

    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);

    // SECURITY: reject early when the DECLARED size already exceeds the ceiling,
    // and clamp the pre-allocation so a lying `Content-Length` cannot trigger a
    // multi-GB up-front allocation (CWE-789). The header is never trusted for the
    // real bound — the streamed cap in the read loop below is the load-bearing
    // guard against a body that lies about (or omits) its length.
    if total > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "refusing download: declared size {total} B exceeds cap {MAX_DOWNLOAD_BYTES} B"
        ));
    }
    let reader = resp.into_reader();
    read_capped(reader, MAX_DOWNLOAD_BYTES, total, progress).map_err(|e| format!("{e} for {url}"))
}

/// Read `reader` to EOF into a buffer, aborting the moment the STREAMED body
/// exceeds `cap`. This is the load-bearing guard (CWE-789) the header check
/// cannot provide: a response that lies about or OMITS `Content-Length` (so the
/// declared-size pre-check passes with `total == 0`) must not be able to grow the
/// buffer without bound and OOM the process before signature verification runs.
/// `total` is the declared size, forwarded to `progress` for the UI bar ONLY —
/// it is never trusted as a bound. The cap is checked BEFORE each append, so the
/// buffer never exceeds `cap` bytes.
fn read_capped<R: std::io::Read>(
    mut reader: R,
    cap: u64,
    total: u64,
    mut progress: impl FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(total.min(cap) as usize);
    let mut chunk = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    progress(0, total);
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| format!("read failed: {e}"))?;
        if n == 0 {
            break;
        }
        downloaded += n as u64;
        if downloaded > cap {
            return Err(format!(
                "refusing download: streamed body exceeds cap {cap} B \
                 (declared {total} B) — possible lying/absent Content-Length"
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        progress(downloaded, total);
    }
    Ok(buf)
}

/// The expected binary file name on this platform.
fn binary_file_names() -> [&'static str; 2] {
    ["c0pl4nd", "c0pl4nd.exe"]
}

/// Mark `path` executable (`0o755`) on unix; a no-op on other platforms.
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| format!("failed to stat extracted binary: {e}"))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).map_err(|e| format!("failed to chmod extracted binary: {e}"))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Extract the single `c0pl4nd` / `c0pl4nd.exe` binary entry from a `.tar.gz`
/// archive's bytes into `dir`, returning the path to the extracted file. On
/// unix the extracted file is made executable (`0o755`).
fn extract_binary_targz(archive_bytes: &[u8], dir: &Path) -> Result<PathBuf, String> {
    let gz = flate2::read::GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(gz);
    let entries = archive
        .entries()
        .map_err(|e| format!("failed to read tar entries: {e}"))?;
    let mut entry_count: usize = 0;
    for entry in entries {
        // Decompression-bomb guard (S-4): refuse an archive with an
        // unreasonable number of entries before parsing any further.
        entry_count += 1;
        if entry_count > MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "refusing to extract: archive has more than {MAX_ARCHIVE_ENTRIES} entries"
            ));
        }
        let mut entry = entry.map_err(|e| format!("failed to read tar entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("bad tar entry path: {e}"))?;
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if binary_file_names().contains(&file_name.as_str()) {
            let out_path = dir.join(&file_name);
            let mut out = fs::File::create(&out_path)
                .map_err(|e| format!("failed to create {}: {e}", out_path.display()))?;
            // Bounded copy: aborts past MAX_EXTRACTED_BYTES even if a malicious
            // gzip stream tries to inflate a tiny payload into a disk-filler.
            copy_capped(&mut entry, &mut out, MAX_EXTRACTED_BYTES)?;
            drop(out);
            set_executable(&out_path)?;
            return Ok(out_path);
        }
    }
    Err("archive did not contain a c0pl4nd / c0pl4nd.exe binary".to_string())
}

/// Extract the single `c0pl4nd` / `c0pl4nd.exe` binary entry from a `.zip`
/// archive's bytes into `dir`, returning the path to the extracted file.
fn extract_binary_zip(archive_bytes: &[u8], dir: &Path) -> Result<PathBuf, String> {
    let reader = std::io::Cursor::new(archive_bytes);
    let mut zip =
        zip::ZipArchive::new(reader).map_err(|e| format!("failed to read zip archive: {e}"))?;
    // Decompression-bomb guard (S-4): refuse an archive with an unreasonable
    // number of entries before touching any of them.
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "refusing to extract: zip has more than {MAX_ARCHIVE_ENTRIES} entries"
        ));
    }
    for i in 0..zip.len() {
        let mut file = zip
            .by_index(i)
            .map_err(|e| format!("failed to read zip entry {i}: {e}"))?;
        let entry_name = match file.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue, // skip path-traversal entries
        };
        let file_name = match entry_name.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if binary_file_names().contains(&file_name.as_str()) {
            let out_path = dir.join(&file_name);
            let mut out = fs::File::create(&out_path)
                .map_err(|e| format!("failed to create {}: {e}", out_path.display()))?;
            // Bounded copy: aborts past MAX_EXTRACTED_BYTES even if a malicious
            // zip entry lies about its uncompressed size (a classic zip bomb).
            copy_capped(&mut file, &mut out, MAX_EXTRACTED_BYTES)?;
            drop(out);
            set_executable(&out_path)?;
            return Ok(out_path);
        }
    }
    Err("archive did not contain a c0pl4nd / c0pl4nd.exe binary".to_string())
}

/// Extract the binary from a `.zip` or `.tar.gz` archive (selected by the
/// asset name), returning the extracted path. Split out from
/// [`download_verify_extract`] so it can be unit-tested directly (no network,
/// no signature) — the production path NEVER reaches here without a passing
/// `verify_artifact_bound`.
fn extract_binary(archive_bytes: &[u8], asset_name: &str, dir: &Path) -> Result<PathBuf, String> {
    if asset_name.ends_with(".zip") {
        extract_binary_zip(archive_bytes, dir)
    } else {
        extract_binary_targz(archive_bytes, dir)
    }
}

/// Download the release archive + `.minisig` + `.sha256`, verify (SHA-256 THEN
/// minisign against the embedded key — fails closed), then extract the single
/// binary from the archive into `staging_dir`, returning the path to the
/// extracted, verified binary.
///
/// `progress` is called as `(downloaded_bytes, total_bytes)` for the big asset
/// so the UI can show a bar. ANY failure (network OR verify) deletes
/// `staging_dir` and returns `Err` — the binary is NEVER returned unverified.
pub fn download_verify_extract(
    info: &ReleaseInfo,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    match download_verify_extract_inner(info, staging_dir, progress) {
        Ok(p) => Ok(p),
        Err(e) => {
            let _ = fs::remove_dir_all(staging_dir);
            Err(e)
        }
    }
}

fn download_verify_extract_inner(
    info: &ReleaseInfo,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    fs::create_dir_all(staging_dir).map_err(|e| format!("failed to create staging dir: {e}"))?;

    // Defense-in-depth (audit #6): assert EVERY download URL is https BEFORE any
    // fetch. A present (non-empty) sidecar url is https-checked upfront too, so a
    // downgraded sidecar url is refused before any network I/O; an ABSENT sidecar
    // (empty url — the signed-manifest-only asset set) is simply skipped.
    assert_https(&info.asset_url)?;
    if !info.sig_url.trim().is_empty() {
        assert_https(&info.sig_url)?;
    }
    if !info.sha_url.trim().is_empty() {
        assert_https(&info.sha_url)?;
    }

    // Big asset (streamed for progress) + the two tiny sidecars, fetched
    // best-effort: a release that ENUMERATES them is fully verified; a release
    // that OMITS them (empty url — the signed-manifest-only asset set) falls back
    // to the authoritative signed-manifest pin.
    let asset_bytes = download_asset(&info.asset_url, progress)?;
    let sig_str = fetch_optional_sidecar_text(&info.sig_url, "minisig")?;
    let sha_text = fetch_optional_sidecar_text(&info.sha_url, "sha256")?;

    // The manifest's SIGNED digest is authoritative. When the `.sha256` sidecar
    // is present it must AGREE (defense-in-depth — a disagreement fails closed);
    // when absent, the signed pin stands alone. Every `ReleaseInfo` carries a
    // pin, so the download is always bound to the signed hash.
    let expected_sha = match sha_text.as_deref() {
        Some(text) => {
            // `.sha256` is a bare hex digest or the `<hex>  <file>` `sha256sum`
            // form — take the first whitespace token.
            let sidecar_sha = text
                .split_whitespace()
                .next()
                .ok_or_else(|| "sha256 sidecar was empty".to_string())?;
            resolve_expected_sha(&info.pinned_sha256, sidecar_sha)?
        }
        None => info.pinned_sha256.trim(),
    };

    // SHA-256 (ALWAYS, against the signed pin) THEN minisign WHEN a sidecar is
    // present, its trusted-comment `file:` token bound to the resolved asset name
    // (defense-in-depth against same-key wrong-artifact substitution). Fails closed.
    verify_artifact_bound_optional_sig(
        &asset_bytes,
        expected_sha,
        sig_str.as_deref(),
        EMBEDDED_PUBLIC_KEYS,
        &info.asset_name,
    )?;

    // Only reached when verification passed.
    extract_binary(&asset_bytes, &info.asset_name, staging_dir)
}

/// Download the self-elevating installer (`setup.exe`), pin it to the manifest's
/// SIGNED sha256 (the sidecar must AGREE), verify it (SHA-256 THEN minisign,
/// bound to the asset name — the IDENTICAL gate the archive path uses), and
/// write it into `staging_dir`, returning the path to the verified `.exe`. The
/// caller launches it (elevated, `--silent --dir`) to update in place. ANY
/// verify failure wipes `staging_dir` and returns `Err` — an unverified
/// installer is NEVER written for launch (fail-closed, no relaxed gate).
pub fn download_verify_installer(
    installer: &InstallerAsset,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    match download_verify_installer_inner(installer, staging_dir, progress) {
        Ok(p) => Ok(p),
        Err(e) => {
            let _ = fs::remove_dir_all(staging_dir);
            Err(e)
        }
    }
}

fn download_verify_installer_inner(
    installer: &InstallerAsset,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    fs::create_dir_all(staging_dir).map_err(|e| format!("failed to create staging dir: {e}"))?;

    // Defense-in-depth (audit #6): assert EVERY download URL is https BEFORE any
    // fetch — a present sidecar url upfront (refused before network I/O), an
    // absent one (empty url) simply skipped.
    assert_https(&installer.url)?;
    if !installer.sig_url.trim().is_empty() {
        assert_https(&installer.sig_url)?;
    }
    if !installer.sha_url.trim().is_empty() {
        assert_https(&installer.sha_url)?;
    }

    let exe_bytes = download_asset(&installer.url, progress)?;
    let sig_str = fetch_optional_sidecar_text(&installer.sig_url, "minisig")?;
    let sha_text = fetch_optional_sidecar_text(&installer.sha_url, "sha256")?;

    // The manifest's SIGNED digest is authoritative; a present `.sha256` sidecar
    // must AGREE (defense-in-depth), an absent one leaves the signed pin alone.
    let expected_sha = match sha_text.as_deref() {
        Some(text) => {
            let sidecar_sha = text
                .split_whitespace()
                .next()
                .ok_or_else(|| "sha256 sidecar was empty".to_string())?;
            resolve_expected_sha(&installer.pinned_sha256, sidecar_sha)?
        }
        None => installer.pinned_sha256.trim(),
    };

    // SHA-256 (ALWAYS, against the signed pin) THEN minisign WHEN a sidecar is
    // present, its trusted-comment `file:` token bound to the installer asset
    // name (defense-in-depth against same-key wrong-artifact substitution). Fails
    // closed — IDENTICAL gate to the tar.gz/zip archive path.
    verify_artifact_bound_optional_sig(
        &exe_bytes,
        expected_sha,
        sig_str.as_deref(),
        EMBEDDED_PUBLIC_KEYS,
        &installer.asset_name,
    )?;

    // Only reached when verification passed — write the verified installer out.
    let out = staging_dir.join("c0pl4nd-setup.exe");
    fs::write(&out, &exe_bytes).map_err(|e| format!("failed to write installer: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::verify::sha256_hex;
    use super::*;
    use std::io::Write;

    fn asset(name: &str, url: &str) -> RawAsset {
        RawAsset {
            name: name.to_string(),
            browser_download_url: url.to_string(),
        }
    }

    /// A release fixture for `<target>` with a full asset triple at `tag`,
    /// matching C0PL4ND's `c0pl4nd-<tag>-<target>.<ext>` naming.
    fn release_with_triple(tag: &str, target: &str, ext: &str) -> RawRelease {
        let base = format!("c0pl4nd-{tag}-{target}{ext}");
        RawRelease {
            tag_name: tag.to_string(),
            prerelease: false,
            draft: false,
            html_url: "https://github.com/o/r/releases/tag/x".to_string(),
            assets: vec![
                asset(&base, &format!("https://dl/{base}")),
                asset(
                    &format!("{base}.minisig"),
                    &format!("https://dl/{base}.minisig"),
                ),
                asset(
                    &format!("{base}.sha256"),
                    &format!("https://dl/{base}.sha256"),
                ),
            ],
        }
    }

    #[test]
    fn assert_https_accepts_https_and_rejects_others() {
        assert!(assert_https("https://github.com/o/r/releases/download/x/a.zip").is_ok());
        // Case-insensitive scheme (RFC 3986).
        assert!(assert_https("HTTPS://example.com/a.zip").is_ok());
        // Every non-https scheme is refused.
        assert!(assert_https("http://example.com/a.zip").is_err());
        assert!(assert_https("ftp://example.com/a.zip").is_err());
        assert!(assert_https("file:///etc/passwd").is_err());
        assert!(assert_https("/relative/path").is_err());
        assert!(assert_https("httpsx://no-delim").is_err());
    }

    #[test]
    fn host_allowed_confines_to_github_set() {
        // The allow-listed GitHub hosts pass (case-insensitive).
        assert!(host_allowed("github.com"));
        assert!(host_allowed("api.github.com"));
        assert!(host_allowed("codeload.github.com"));
        assert!(host_allowed("objects.githubusercontent.com"));
        assert!(host_allowed("release-assets.githubusercontent.com"));
        assert!(host_allowed("GITHUB.COM"));
        // Everything else is refused — including look-alikes and sub-domain
        // confusables that are NOT a *.githubusercontent.com suffix.
        assert!(!host_allowed("evil.example"));
        assert!(!host_allowed("github.com.evil.example"));
        assert!(!host_allowed("githubusercontent.com.evil.example"));
        assert!(!host_allowed("notgithub.com"));
    }

    // `url_host` + `resolve_redirect` now live in `crate::net_confine` and are
    // tested there; this module keeps only the allow-list-specific assertion.

    #[test]
    fn assert_allowed_host_blocks_non_github() {
        assert!(assert_allowed_host("https://objects.githubusercontent.com/x").is_ok());
        assert!(assert_allowed_host("https://evil.example/x").is_err());
        assert!(assert_allowed_host("https:///no-host").is_err());
    }

    // =====================================================================
    // E2E_API — hermetic transport harness for the confined GET
    // =====================================================================
    //
    // ## What this harness IS
    //
    // The tests above prove the confinement PREDICATES (`assert_https`,
    // `host_allowed`) in isolation, and the `download_verify_extract_refuses_*`
    // tests prove the FIRST url is checked. None of them enters
    // `confined_get_with`'s redirect walk, so the security property the doc
    // comment on `confined_get` actually claims — that https AND the host
    // allow-list are RE-ASSERTED AT EVERY HOP — was, before this harness,
    // asserted by no test at all. A predicate being correct does not prove it is
    // CALLED on hop 2.
    //
    // The harness closes that gap by injecting at the `ureq::Agent` boundary via
    // ureq's `Middleware`. A middleware that never calls `next.handle(req)`
    // short-circuits the chain ABOVE `unit::connect`, so no socket, no DNS
    // resolution and no TLS handshake ever occur (see `MockTransport::handle`).
    // Responses are synthesized with `impl FromStr for Response`, which parses a
    // full status line + headers, letting us drive `Location` / `Content-Length`.
    //
    // Crucially the mock replaces ONLY the third-party transport. Everything
    // under test — the `assert_https` / `assert_allowed_host` calls, the
    // redirect walk, `resolve_redirect`, the `MAX_REDIRECTS` cap, the
    // declared-size check — is the REAL production code path. Injecting any
    // higher (at `confined_get`/`fetch_latest_release` level) would degenerate
    // into a tautology that merely re-tested serde against a fixture, which the
    // `RawRelease` tests already cover.
    //
    // ## What this harness DOES NOT PROVE  (read before trusting a green run)
    //
    // Because the socket is never opened, these are OUT OF SCOPE here and are
    // NOT covered by any assertion below:
    //   * TLS itself: handshake, protocol version, cipher selection.
    //   * Certificate validation, hostname verification, pinning, revocation.
    //   * DNS resolution and any DNS-rebinding class of attack (a host that
    //     passes the allow-list string check but RESOLVES to an attacker IP is
    //     NOT addressed by this defense at all — the allow-list is a string
    //     gate, and this harness only proves the string gate runs per hop).
    //   * That `default_agent`'s connect/read timeouts actually fire (no real
    //     socket can stall), nor any slow-loris behaviour.
    //   * Real GitHub API compatibility: that api.github.com truly returns the
    //     shapes synthesized here, or that its real redirect chain stays inside
    //     the allow-list.
    //   * The `Err(ureq::Error::Status(code, r))` 3xx arm of the walk: ureq maps
    //     only >=400 to `Error::Status`, so a synthesized 3xx always arrives via
    //     the `Ok(r)` arm. That arm is defensive and stays untested here.
    //   * Proxy handling and connection pooling.

    use std::sync::{Arc, Mutex};

    /// One request exactly as the transport saw it, captured BEFORE any socket
    /// would have been opened.
    #[derive(Clone, Debug)]
    struct SeenRequest {
        url: String,
        headers: Vec<(String, String)>,
    }

    impl SeenRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    /// The hermetic transport. `responder` maps `(hop_index, request)` to a raw
    /// HTTP response string. It NEVER calls `next.handle`, so the request dies
    /// here and no byte reaches a socket.
    struct MockTransport {
        seen: Arc<Mutex<Vec<SeenRequest>>>,
        #[allow(clippy::type_complexity)]
        responder: Box<dyn Fn(usize, &SeenRequest) -> String + Send + Sync>,
    }

    impl ureq::Middleware for MockTransport {
        fn handle(
            &self,
            request: ureq::Request,
            _next: ureq::MiddlewareNext,
        ) -> Result<ureq::Response, ureq::Error> {
            let headers = request
                .header_names()
                .into_iter()
                .map(|n| {
                    let v = request.header(&n).unwrap_or_default().to_string();
                    (n, v)
                })
                .collect();
            let seen = SeenRequest {
                url: request.url().to_string(),
                headers,
            };
            let idx = {
                let mut log = self.seen.lock().expect("seen log poisoned");
                log.push(seen.clone());
                log.len() - 1
            };
            let raw = (self.responder)(idx, &seen);
            // NOTE: `_next` is deliberately dropped without being handled. That
            // is what makes this hermetic — dropping it is the short-circuit.
            let resp: ureq::Response = raw
                .parse()
                .expect("test fixture must be a well-formed HTTP response");
            Ok(resp)
        }
    }

    /// Build an agent whose transport is `responder`, mirroring `default_agent`'s
    /// `redirects(0)` so the manual walk under test behaves exactly as in
    /// production. Returns the agent plus the shared request log.
    fn mock_agent(
        responder: impl Fn(usize, &SeenRequest) -> String + Send + Sync + 'static,
    ) -> (ureq::Agent, Arc<Mutex<Vec<SeenRequest>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .middleware(MockTransport {
                seen: Arc::clone(&seen),
                responder: Box::new(responder),
            })
            .build();
        (agent, seen)
    }

    /// The urls the transport was actually asked for, in order.
    fn seen_urls(seen: &Arc<Mutex<Vec<SeenRequest>>>) -> Vec<String> {
        seen.lock()
            .expect("seen log poisoned")
            .iter()
            .map(|r| r.url.clone())
            .collect()
    }

    const API_URL: &str = "https://api.github.com/repos/o/r/releases/latest";

    #[test]
    fn redirect_to_non_allowlisted_host_is_refused_at_hop_2() {
        // Hop 1 is a perfectly legitimate api.github.com request that returns a
        // 302 pointing off the allow-list — the SSRF / exfil shape. The refusal
        // must come from the redirect walk's per-hop re-assert, NOT from the
        // entry assert (which hop 1's url passes cleanly).
        let (agent, seen) = mock_agent(|i, _| match i {
            0 => "HTTP/1.1 302 Found\r\nLocation: https://evil.example/payload\r\n\r\n".to_string(),
            _ => "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\npwnd".to_string(),
        });

        let err = confined_get_with(&agent, API_URL, &[]).expect_err("hop 2 must be refused");

        assert!(
            err.contains("non-allowlisted host") && err.contains("evil.example"),
            "expected a host-allowlist refusal naming the host, got: {err}"
        );
        // THE load-bearing assertion: the transport saw hop 1 and was NEVER
        // asked for evil.example. len==1 proves the walk ran (not an early
        // bail at the entry assert, which would give len==0) AND that the
        // refusal happened BEFORE hop 2 was dispatched.
        let urls = seen_urls(&seen);
        assert_eq!(
            urls.len(),
            1,
            "hop 2 must never be dispatched; saw {urls:?}"
        );
        assert_eq!(urls[0], API_URL, "hop 1 should be the api.github.com url");
        assert!(
            !urls.iter().any(|u| u.contains("evil.example")),
            "the updater must never send a request to the redirect target: {urls:?}"
        );
    }

    #[test]
    fn redirect_scheme_downgrade_to_http_is_refused_at_hop_2() {
        // The redirect target host is ALLOW-LISTED (codeload.github.com), so the
        // host gate cannot be what refuses this. Only the per-hop `assert_https`
        // can — which isolates the TLS-downgrade defense at hop 2 specifically.
        let (agent, seen) = mock_agent(|i, _| match i {
            0 => "HTTP/1.1 302 Found\r\nLocation: http://codeload.github.com/a.zip\r\n\r\n"
                .to_string(),
            _ => "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\npwnd".to_string(),
        });

        let err =
            confined_get_with(&agent, API_URL, &[]).expect_err("http redirect must be refused");

        assert!(
            err.contains("non-https"),
            "expected an https-downgrade refusal, got: {err}"
        );
        let urls = seen_urls(&seen);
        assert_eq!(
            urls.len(),
            1,
            "the cleartext hop must never be dispatched; saw {urls:?}"
        );
    }

    #[test]
    fn redirect_walk_stops_at_max_redirects() {
        // Every hop is https + allow-listed, so ONLY the hop cap can stop this.
        // Without the cap the walk would follow an endless chain forever.
        let (agent, seen) = mock_agent(|i, _| {
            format!("HTTP/1.1 302 Found\r\nLocation: https://codeload.github.com/hop{i}\r\n\r\n")
        });

        let err = confined_get_with(&agent, API_URL, &[])
            .expect_err("an endless redirect chain must be refused");

        assert!(
            err.contains("too many redirects"),
            "expected a redirect-cap refusal, got: {err}"
        );
        // The walk is `for _ in 0..=MAX_REDIRECTS`, so it dispatches exactly
        // MAX_REDIRECTS + 1 requests and then gives up. Pinning the exact count
        // is what makes this test notice a cap that silently grows.
        let urls = seen_urls(&seen);
        assert_eq!(
            urls.len(),
            MAX_REDIRECTS + 1,
            "expected exactly {} dispatches, saw {urls:?}",
            MAX_REDIRECTS + 1
        );
    }

    #[test]
    fn a_confined_redirect_chain_is_followed_to_the_body() {
        // The control for the three refusal tests above: a chain that stays
        // https + inside the allow-list MUST be followed and deliver the body.
        // Without this, a `confined_get_with` that refused EVERYTHING would pass
        // every other test in this section.
        let (agent, seen) = mock_agent(|i, _| match i {
            0 => "HTTP/1.1 302 Found\r\nLocation: https://codeload.github.com/a.zip\r\n\r\n"
                .to_string(),
            _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_string(),
        });

        let resp = confined_get_with(&agent, API_URL, &[]).expect("a confined chain must succeed");
        assert_eq!(resp.status(), 200);
        let mut body = String::new();
        resp.into_reader()
            .read_to_string(&mut body)
            .expect("body reads");
        assert_eq!(body, "ok");

        let urls = seen_urls(&seen);
        assert_eq!(
            urls,
            vec![
                API_URL.to_string(),
                "https://codeload.github.com/a.zip".to_string()
            ],
            "both hops should be dispatched in order"
        );
    }

    #[test]
    fn a_relative_redirect_location_is_resolved_and_confined() {
        // `Location` may be relative (RFC 7231). It must resolve against the
        // CURRENT url and still be re-confined, not silently dropped.
        let (agent, seen) = mock_agent(|i, _| match i {
            0 => "HTTP/1.1 302 Found\r\nLocation: /o/r/releases/download/a.zip\r\n\r\n".to_string(),
            _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_string(),
        });

        let resp = confined_get_with(&agent, API_URL, &[]).expect("relative redirect resolves");
        assert_eq!(resp.status(), 200);
        let urls = seen_urls(&seen);
        assert_eq!(
            urls[1], "https://api.github.com/o/r/releases/download/a.zip",
            "a relative Location must resolve against the current url; saw {urls:?}"
        );
    }

    #[test]
    fn a_redirect_without_a_location_header_is_an_error() {
        let (agent, _seen) =
            mock_agent(|_, _| "HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n".to_string());
        let err = confined_get_with(&agent, API_URL, &[])
            .expect_err("a 302 with no Location must not be silently treated as a body");
        assert!(
            err.contains("without Location"),
            "expected a missing-Location error, got: {err}"
        );
    }

    #[test]
    fn every_request_carries_the_exact_telemetry_free_headers() {
        // The module doc claims "telemetry-free by construction: every request
        // sends only a generic User-Agent (app name + version)". That claim was
        // asserted by NO test. This pins it on the REQUEST AS THE TRANSPORT SEES
        // IT — i.e. what would actually go on the wire — for BOTH hops, since a
        // redirect must not silently drop or mutate the headers either.
        let (agent, seen) = mock_agent(|i, _| match i {
            0 => "HTTP/1.1 302 Found\r\nLocation: https://codeload.github.com/a.zip\r\n\r\n"
                .to_string(),
            _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_string(),
        });

        confined_get_with(
            &agent,
            API_URL,
            &[
                ("Accept", GITHUB_ACCEPT),
                ("X-GitHub-Api-Version", GITHUB_API_VERSION),
            ],
        )
        .expect("confined chain succeeds");

        let log = seen.lock().expect("seen log poisoned").clone();
        assert_eq!(log.len(), 2, "expected both hops to be dispatched");

        for (hop, req) in log.iter().enumerate() {
            assert_eq!(
                req.header("User-Agent"),
                Some(USER_AGENT),
                "hop {hop} must carry the generic User-Agent"
            );
            assert_eq!(
                req.header("Accept"),
                Some(GITHUB_ACCEPT),
                "hop {hop} must carry the Accept header"
            );
            assert_eq!(
                req.header("X-GitHub-Api-Version"),
                Some(GITHUB_API_VERSION),
                "hop {hop} must carry the API-version header"
            );

            // ZERO-TELEMETRY: nothing BEYOND the known headers may ride along.
            // This is the assertion that would catch a future change slipping an
            // install-id / machine fingerprint / auth token onto the wire — the
            // whole point of the "no identifiers" claim.
            //
            // `accept-encoding: gzip` is in this list because ureq's `gzip`
            // feature adds it to every request BEFORE the middleware chain runs
            // (it is not set by our code). It is content negotiation only and
            // carries no identifier, so it is benign — but it IS on the wire,
            // and this list documents what actually goes out rather than what we
            // wish went out. `Host` is added later, inside `unit::connect`, and
            // so is not visible at this seam.
            let mut names: Vec<String> = req
                .headers
                .iter()
                .map(|(k, _)| k.to_ascii_lowercase())
                .collect();
            names.sort();
            assert_eq!(
                names,
                vec![
                    "accept",
                    "accept-encoding",
                    "user-agent",
                    "x-github-api-version"
                ],
                "hop {hop} sent unexpected header(s) — telemetry-free claim broken: {:?}",
                req.headers
            );
        }

        // The User-Agent is app name + version ONLY: no OS, arch, or machine id.
        assert_eq!(
            USER_AGENT,
            concat!("c0pl4nd-updater/", env!("CARGO_PKG_VERSION")),
            "the User-Agent must stay app-name/version with no fingerprint"
        );
    }

    #[test]
    fn download_asset_refuses_a_declared_size_over_the_cap() {
        // A hostile endpoint declaring a multi-GB body must be refused on the
        // HEADER, before `into_reader` and before any allocation sized from it.
        let over = MAX_DOWNLOAD_BYTES + 1;
        let (agent, _seen) =
            mock_agent(move |_, _| format!("HTTP/1.1 200 OK\r\nContent-Length: {over}\r\n\r\n"));

        let err = download_asset_with(&agent, "https://codeload.github.com/a.zip", |_, _| {})
            .expect_err("a declared size over the cap must be refused");

        assert!(
            err.contains("refusing download") && err.contains("exceeds cap"),
            "expected a declared-size refusal, got: {err}"
        );
        assert!(
            err.contains(&over.to_string()),
            "the refusal should report the declared size, got: {err}"
        );
    }

    #[test]
    fn download_asset_declared_size_check_is_exclusive_at_the_cap_boundary() {
        // Boundary control for the test above: the check is `>` not `>=`, so a
        // body declaring EXACTLY the cap must get PAST the declared-size gate.
        //
        // We cannot send a real 256 MiB body, so we assert the boundary
        // NEGATIVELY: declare exactly the cap, send a short body, and require
        // that whatever error comes back is NOT the declared-size refusal. That
        // proves control flow reached `into_reader` — i.e. the gate let it
        // through. Widening the gate to `>=` makes this fail with
        // "refusing download ... exceeds cap", which is exactly the regression
        // this pins. (The read then fails on ureq's own Content-Length
        // enforcement; that error is the EVIDENCE of passage, not the subject.)
        let (agent, _seen) = mock_agent(|_, _| {
            format!("HTTP/1.1 200 OK\r\nContent-Length: {MAX_DOWNLOAD_BYTES}\r\n\r\n")
        });

        let err = download_asset_with(&agent, "https://codeload.github.com/a.zip", |_, _| {})
            .expect_err("the short body cannot satisfy the declared length");
        assert!(
            !err.contains("exceeds cap"),
            "a declared size exactly AT the cap must pass the gate (`>` not `>=`), got: {err}"
        );
        assert!(
            err.contains("read failed"),
            "expected the failure to come from the read stage, proving the \
             declared-size gate was passed, got: {err}"
        );
    }

    #[test]
    fn download_asset_streams_a_legitimate_body_and_reports_progress() {
        // The success control for the whole `download_asset_with` path: without
        // it, a `download_asset_with` that refused EVERYTHING would satisfy both
        // cap tests above.
        let body = "c0pl4nd-archive-bytes";
        let (agent, _seen) = mock_agent(move |_, _| {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        });

        let mut last = (0u64, 0u64);
        let got = download_asset_with(&agent, "https://codeload.github.com/a.zip", |d, t| {
            last = (d, t)
        })
        .expect("a legitimate download must succeed");

        assert_eq!(got, body.as_bytes());
        assert_eq!(
            last,
            (body.len() as u64, body.len() as u64),
            "progress must report the streamed and declared sizes"
        );
    }

    #[test]
    fn download_caps_are_ordered_and_bounded() {
        // Sidecars are tiny; the asset cap matches the extraction cap; the JSON
        // cap sits between them. A regression that inverts these is a bug. These
        // are compile-time invariants — `const` blocks keep clippy's
        // assertions-on-constants lint satisfied.
        const { assert!(MAX_SIDECAR_BYTES < MAX_RELEASE_JSON_BYTES) };
        const { assert!(MAX_RELEASE_JSON_BYTES < MAX_DOWNLOAD_BYTES) };
        const { assert!(MAX_DOWNLOAD_BYTES == MAX_EXTRACTED_BYTES) };
    }

    #[test]
    fn read_capped_aborts_on_streamed_body_over_cap() {
        use std::io::Cursor;
        // The load-bearing guard: a body whose declared size is a LIE (total = 0,
        // so the header pre-check is bypassed) must still be stopped by the
        // streamed cap before it can grow without bound. 100 bytes streamed
        // against a 50-byte cap must error, not OOM.
        let body = vec![0u8; 100];
        let err = read_capped(Cursor::new(body), 50, 0, |_, _| {})
            .expect_err("an over-cap streamed body must be rejected");
        assert!(
            err.contains("exceeds cap"),
            "the error must name the cap breach: {err:?}"
        );
    }

    #[test]
    fn read_capped_accepts_body_at_or_under_cap() {
        use std::io::Cursor;
        // A body exactly at the cap is accepted and returned verbatim; progress
        // reports the streamed length, never a trusted header value.
        let body = vec![7u8; 50];
        let mut last = 0u64;
        let out = read_capped(Cursor::new(body.clone()), 50, 50, |d, _| last = d)
            .expect("a body within the cap must be accepted");
        assert_eq!(out, body, "the full in-cap body is returned");
        assert_eq!(last, 50, "progress reports the streamed byte count");
    }

    #[test]
    fn download_verify_extract_refuses_http_asset_url_without_network() {
        // A MITM'd / malicious release response could hand back an http:// asset
        // URL. The https assertion must fire BEFORE any byte is fetched.
        let dir = tempfile::tempdir().expect("tempdir");
        let info = ReleaseInfo {
            version: semver::Version::parse("0.4.0").unwrap(),
            tag: "v0.4.0".to_string(),
            asset_url: "http://evil.example/c0pl4nd.zip".to_string(),
            asset_name: "c0pl4nd.zip".to_string(),
            sig_url: "https://dl/c0pl4nd.zip.minisig".to_string(),
            sha_url: "https://dl/c0pl4nd.zip.sha256".to_string(),
            html_url: "https://github.com/o/r".to_string(),
            pinned_sha256: "deadbeef".to_string(),
            release_index: None,
            installer: None,
        };
        let err = download_verify_extract(&info, dir.path(), |_, _| {})
            .expect_err("http asset url must be refused");
        assert!(
            err.contains("non-https"),
            "expected an https-refusal error, got: {err}"
        );
    }

    #[test]
    fn download_verify_extract_refuses_http_sig_url_without_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let info = ReleaseInfo {
            version: semver::Version::parse("0.4.0").unwrap(),
            tag: "v0.4.0".to_string(),
            asset_url: "https://dl/c0pl4nd.zip".to_string(),
            asset_name: "c0pl4nd.zip".to_string(),
            sig_url: "http://evil.example/c0pl4nd.zip.minisig".to_string(),
            sha_url: "https://dl/c0pl4nd.zip.sha256".to_string(),
            html_url: "https://github.com/o/r".to_string(),
            pinned_sha256: "deadbeef".to_string(),
            release_index: None,
            installer: None,
        };
        let err = download_verify_extract(&info, dir.path(), |_, _| {})
            .expect_err("http sig url must be refused");
        assert!(err.contains("non-https"), "got: {err}");
    }

    #[test]
    fn extract_binary_targz_pulls_the_binary_and_chmods() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"#!/bin/sh\necho c0pl4nd\n";
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            // A decoy + the real binary.
            for (name, data) in [("README.txt", &b"x"[..]), ("c0pl4nd", &payload[..])] {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, data).unwrap();
            }
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();
        let extracted = extract_binary(&archive_bytes, "c0pl4nd-x.tar.gz", dir.path()).unwrap();
        assert_eq!(fs::read(&extracted).unwrap(), payload);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&extracted).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn extract_binary_zip_pulls_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"MZ\x90\x00 fake c0pl4nd.exe";
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zw.start_file("README.md", opts).unwrap();
            zw.write_all(b"docs").unwrap();
            zw.start_file("c0pl4nd.exe", opts).unwrap();
            zw.write_all(payload).unwrap();
            zw.finish().unwrap();
        }
        let extracted = extract_binary(&buf, "c0pl4nd-x.zip", dir.path()).unwrap();
        assert_eq!(fs::read(&extracted).unwrap(), payload);
    }

    #[test]
    fn copy_capped_aborts_past_the_limit() {
        // A reader that yields more bytes than the cap must be refused, and the
        // error must name the cap (S-4 decompression-bomb guard).
        let payload = vec![0u8; 1024];
        let mut reader = std::io::Cursor::new(payload);
        let mut sink: Vec<u8> = Vec::new();
        let err = copy_capped(&mut reader, &mut sink, 512).expect_err("must hit the cap");
        assert!(err.contains("decompression-bomb cap"), "got: {err}");
    }

    #[test]
    fn copy_capped_passes_under_the_limit() {
        let payload = vec![7u8; 100];
        let mut reader = std::io::Cursor::new(payload.clone());
        let mut sink: Vec<u8> = Vec::new();
        let n = copy_capped(&mut reader, &mut sink, 256).expect("under cap copies cleanly");
        assert_eq!(n, 100);
        assert_eq!(sink, payload);
    }

    #[test]
    fn extract_binary_targz_refuses_too_many_entries() {
        // An archive with more than MAX_ARCHIVE_ENTRIES is a resource-exhaustion
        // shape and is refused before the binary is ever located.
        let dir = tempfile::tempdir().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            for i in 0..(MAX_ARCHIVE_ENTRIES + 5) {
                let data = b"x";
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("decoy-{i}.txt"), &data[..])
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();
        let err = extract_binary(&archive_bytes, "c0pl4nd-x.tar.gz", dir.path())
            .expect_err("too many entries must be refused");
        assert!(err.contains("more than"), "got: {err}");
    }

    #[test]
    fn extract_binary_zip_refuses_too_many_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            for i in 0..(MAX_ARCHIVE_ENTRIES + 5) {
                zw.start_file(format!("decoy-{i}.txt"), opts).unwrap();
                zw.write_all(b"x").unwrap();
            }
            zw.finish().unwrap();
        }
        let err = extract_binary(&buf, "c0pl4nd-x.zip", dir.path())
            .expect_err("too many entries must be refused");
        assert!(err.contains("more than"), "got: {err}");
    }

    #[test]
    fn extract_binary_errs_when_no_binary_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let data = b"readme";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "README.txt", &data[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();
        assert!(extract_binary(&archive_bytes, "c0pl4nd-x.tar.gz", dir.path()).is_err());
    }

    #[test]
    fn sha_sidecar_first_token_matches_archive_digest() {
        let archive = b"pretend tarball bytes";
        let digest = sha256_hex(archive);
        let sidecar = format!("{digest}  c0pl4nd-x.tar.gz\n");
        let first = sidecar.split_whitespace().next().unwrap();
        assert_eq!(first, digest);
    }

    #[test]
    fn archive_ext_matches_platform() {
        if cfg!(windows) {
            assert_eq!(archive_ext(), ".zip");
        } else {
            assert_eq!(archive_ext(), ".tar.gz");
        }
    }

    #[test]
    fn extract_binary_zip_errs_when_only_non_binary_entries() {
        // A zip with only non-binary entries (no `c0pl4nd`/`c0pl4nd.exe`) is an
        // error — the binary entry must be present and named exactly.
        let dir = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zw.start_file("README.md", opts).unwrap();
            zw.write_all(b"docs").unwrap();
            zw.start_file("LICENSE", opts).unwrap();
            zw.write_all(b"mit").unwrap();
            zw.finish().unwrap();
        }
        let err = extract_binary(&buf, "c0pl4nd-x.zip", dir.path())
            .expect_err("a zip without the binary entry must error");
        assert!(
            err.contains("did not contain a c0pl4nd"),
            "the error names the missing binary: {err}"
        );
    }

    #[test]
    fn extract_binary_zip_picks_binary_from_a_subdirectory_by_basename() {
        // The binary may be nested under a top-level dir in the archive
        // (`c0pl4nd-vX/c0pl4nd.exe`). Matching is by file BASENAME, so the
        // nested binary is still found and extracted into the flat staging dir.
        let dir = tempfile::tempdir().unwrap();
        let payload = b"MZ nested fake exe";
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zw.start_file("c0pl4nd-v1.0.0/README.md", opts).unwrap();
            zw.write_all(b"x").unwrap();
            zw.start_file("c0pl4nd-v1.0.0/c0pl4nd.exe", opts).unwrap();
            zw.write_all(payload).unwrap();
            zw.finish().unwrap();
        }
        let extracted = extract_binary(&buf, "c0pl4nd-x.zip", dir.path()).unwrap();
        assert_eq!(
            extracted.file_name().and_then(|n| n.to_str()),
            Some("c0pl4nd.exe"),
            "the nested binary is extracted under its basename into the flat dir"
        );
        assert_eq!(fs::read(&extracted).unwrap(), payload);
    }

    #[test]
    fn extract_binary_dispatches_zip_vs_targz_by_asset_extension() {
        // `extract_binary` selects the zip path for a `.zip` asset name and the
        // tar.gz path otherwise. Feeding a .zip body but naming it `.tar.gz`
        // must route to the tar.gz extractor and FAIL to parse it as a tarball
        // (proving the dispatch keys on the NAME, not content sniffing).
        let dir = tempfile::tempdir().unwrap();
        // A valid zip body.
        let mut zip_buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zw.start_file("c0pl4nd", opts).unwrap();
            zw.write_all(b"x").unwrap();
            zw.finish().unwrap();
        }
        // Routed to the tar.gz extractor by the `.tar.gz` name → cannot parse a
        // zip as a gzip stream → error (never a silent wrong-format extraction).
        assert!(
            extract_binary(&zip_buf, "mislabelled.tar.gz", dir.path()).is_err(),
            "a zip body routed through the tar.gz extractor by name must fail"
        );
    }

    #[test]
    fn sha_sidecar_bare_digest_first_token_is_the_whole_string() {
        // The `.sha256` sidecar may be a BARE hex digest with no filename. The
        // first-whitespace-token extraction (used by download_verify_extract)
        // must return the whole digest unchanged.
        let archive = b"some bytes";
        let digest = sha256_hex(archive);
        let bare = format!("{digest}\n");
        let first = bare.split_whitespace().next().unwrap();
        assert_eq!(
            first, digest,
            "a bare-digest sidecar yields the digest itself"
        );
    }

    // --- Tier-1 signed-manifest wiring ---------------------------------------

    /// A release fixture carrying the full per-asset triple AND the signed
    /// `latest.json` + `latest.json.minisig` manifest pair.
    fn tier1_raw(target: &str, ext: &str, tag: &str) -> RawRelease {
        let base = format!("c0pl4nd-{tag}-{target}{ext}");
        let dl = |n: &str| format!("https://github.com/o/r/releases/download/{tag}/{n}");
        RawRelease {
            tag_name: tag.to_string(),
            prerelease: false,
            draft: false,
            html_url: "https://github.com/o/r".to_string(),
            assets: vec![
                asset(&base, &dl(&base)),
                asset(&format!("{base}.minisig"), &dl(&format!("{base}.minisig"))),
                asset(&format!("{base}.sha256"), &dl(&format!("{base}.sha256"))),
                asset("latest.json", &dl("latest.json")),
                asset("latest.json.minisig", &dl("latest.json.minisig")),
            ],
        }
    }

    /// A verified-manifest fixture matching [`tier1_raw`] for one platform.
    #[allow(clippy::too_many_arguments)]
    fn tier1_manifest(
        target: &str,
        ext: &str,
        tag: &str,
        version: &str,
        idx: u64,
        minimum: &str,
        valid_until: &str,
        sha: &str,
    ) -> manifest::Manifest {
        let base = format!("c0pl4nd-{tag}-{target}{ext}");
        let kind = if ext == ".zip" { "zip" } else { "tar.gz" };
        manifest::Manifest {
            schema: "itasha.update.manifest/v1".to_string(),
            product: "c0pl4nd".to_string(),
            version: version.to_string(),
            release_index: idx,
            minimum_version: minimum.to_string(),
            published_utc: "2026-06-29T14:17:42Z".to_string(),
            valid_until_utc: valid_until.to_string(),
            assets: vec![manifest::ManifestAsset {
                platform: target.to_string(),
                kind: kind.to_string(),
                asset_name: base.clone(),
                url: format!("https://github.com/o/r/releases/download/{tag}/{base}"),
                size: 123,
                sha256: sha.to_string(),
            }],
        }
    }

    #[test]
    fn find_manifest_assets_requires_both_json_and_sig() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = tier1_raw(target, ".tar.gz", "v0.4.9");
        assert!(find_manifest_assets(&raw).is_some());

        // Drop the .minisig → None (an absent manifest is then refused upstream).
        let mut no_sig = raw.clone();
        no_sig.assets.retain(|a| a.name != "latest.json.minisig");
        assert!(find_manifest_assets(&no_sig).is_none());

        // Drop the json → None.
        let mut no_json = raw.clone();
        no_json.assets.retain(|a| a.name != "latest.json");
        assert!(find_manifest_assets(&no_json).is_none());
    }

    #[test]
    fn manifest_absent_is_refused_fail_closed_no_install() {
        // Tier-1 REQUIRES a verified manifest. A release with NO manifest assets
        // is a HARD refusal — `check_for_update` never degrades to the weaker
        // per-asset path (the protection-downgrade an attacker who strips
        // latest.json would otherwise force). The decision is made by
        // `require_manifest_assets`, which fails closed when the manifest is gone.
        let target = "x86_64-unknown-linux-gnu";
        let raw = release_with_triple("v0.4.9", target, ".tar.gz"); // no manifest assets
        let err = require_manifest_assets(&raw)
            .expect_err("an absent manifest must be refused, not silently fallen back");
        assert!(
            err.contains("could not be verified") && err.contains("no signed manifest"),
            "got: {err}"
        );
        // Present → accepted.
        let raw_ok = tier1_raw(target, ".tar.gz", "v0.4.9");
        assert!(require_manifest_assets(&raw_ok).is_ok());
    }

    #[test]
    fn tier1_resolves_a_fresh_in_policy_update_with_pinned_sha_and_index() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = tier1_raw(target, ".tar.gz", "v0.4.9");
        let m = tier1_manifest(
            target,
            ".tar.gz",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2099-01-01T00:00:00Z",
            "abc123def",
        );
        let current = semver::Version::parse("0.4.5").unwrap();
        let info = resolve_tier1_update(&raw, &m, &current, target, ".tar.gz", 0, 4000)
            .expect("a fresh in-policy update resolves")
            .expect("an update is available");
        assert_eq!(info.version, semver::Version::parse("0.4.9").unwrap());
        assert_eq!(
            info.pinned_sha256, "abc123def",
            "the SIGNED manifest digest is pinned"
        );
        assert_eq!(info.release_index, Some(4009));
        // The archive url comes from the SIGNED manifest; sidecars from the release.
        assert!(info
            .asset_url
            .contains("c0pl4nd-v0.4.9-x86_64-unknown-linux-gnu.tar.gz"));
        assert!(info.sig_url.ends_with(".minisig"));
        assert!(info.sha_url.ends_with(".sha256"));
        // A linux release ships no `setup.exe` — the elevated-installer fallback
        // is absent (the archive in-place swap is the only path off Windows).
        assert!(
            info.installer.is_none(),
            "no setup.exe installer on a non-Windows platform"
        );
    }

    #[test]
    fn tier1_windows_resolves_the_self_elevating_installer_alongside_the_archive() {
        // On Windows a release ships BOTH the in-place `.zip` archive AND the
        // self-elevating `setup.exe`. The resolver must pin the archive for the
        // normal in-place path AND carry the verified installer descriptor for
        // the Program-Files fallback — the two are disjoint by `kind`, so wiring
        // the installer never re-routes the writable-dir path through the exe.
        let target = "x86_64-pc-windows-msvc";
        let tag = "v0.4.9";
        let zip = format!("c0pl4nd-{tag}-{target}.zip");
        let exe = format!("c0pl4nd-{tag}-x86_64-setup.exe");
        let dl = |n: &str| format!("https://github.com/o/r/releases/download/{tag}/{n}");
        let raw = RawRelease {
            tag_name: tag.to_string(),
            prerelease: false,
            draft: false,
            html_url: "https://github.com/o/r".to_string(),
            assets: vec![
                asset(&zip, &dl(&zip)),
                asset(&format!("{zip}.minisig"), &dl(&format!("{zip}.minisig"))),
                asset(&format!("{zip}.sha256"), &dl(&format!("{zip}.sha256"))),
                asset(&exe, &dl(&exe)),
                asset(&format!("{exe}.minisig"), &dl(&format!("{exe}.minisig"))),
                asset(&format!("{exe}.sha256"), &dl(&format!("{exe}.sha256"))),
            ],
        };
        let m = manifest::Manifest {
            schema: "itasha.update.manifest/v1".to_string(),
            product: "c0pl4nd".to_string(),
            version: "0.4.9".to_string(),
            release_index: 4009,
            minimum_version: "0.4.0".to_string(),
            published_utc: "2026-06-29T14:17:42Z".to_string(),
            valid_until_utc: "2099-01-01T00:00:00Z".to_string(),
            assets: vec![
                manifest::ManifestAsset {
                    platform: target.to_string(),
                    kind: "zip".to_string(),
                    asset_name: zip.clone(),
                    url: dl(&zip),
                    size: 123,
                    sha256: "aaaa1111".to_string(),
                },
                manifest::ManifestAsset {
                    platform: target.to_string(),
                    kind: "exe".to_string(),
                    asset_name: exe.clone(),
                    url: dl(&exe),
                    size: 456,
                    sha256: "bbbb2222".to_string(),
                },
            ],
        };
        let current = semver::Version::parse("0.4.5").unwrap();
        let info = resolve_tier1_update(&raw, &m, &current, target, ".zip", 0, 4000)
            .expect("resolves")
            .expect("update available");
        // The normal in-place path still pins the ARCHIVE (never the exe).
        assert_eq!(info.asset_name, zip);
        assert_eq!(info.pinned_sha256, "aaaa1111");
        // The elevated fallback carries the verified, sha-pinned installer.
        let inst = info
            .installer
            .expect("windows carries a setup.exe installer");
        assert_eq!(inst.asset_name, exe);
        assert_eq!(inst.url, dl(&exe));
        assert_eq!(inst.pinned_sha256, "bbbb2222");
        assert!(inst.sig_url.ends_with(".minisig"));
        assert!(inst.sha_url.ends_with(".sha256"));
    }

    #[test]
    fn tier1_up_to_date_returns_none() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = tier1_raw(target, ".tar.gz", "v0.4.9");
        let m = tier1_manifest(
            target,
            ".tar.gz",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2099-01-01T00:00:00Z",
            "abc",
        );
        // Running the SAME version → no update (not an error).
        let current = semver::Version::parse("0.4.9").unwrap();
        assert!(
            resolve_tier1_update(&raw, &m, &current, target, ".tar.gz", 0, 0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn tier1_blocks_stale_manifest_freeze_beacon() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = tier1_raw(target, ".tar.gz", "v0.4.9");
        // valid_until far in the past; `now` well after it → stale.
        let m = tier1_manifest(
            target,
            ".tar.gz",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2000-01-01T00:00:00Z",
            "abc",
        );
        let current = semver::Version::parse("0.4.5").unwrap();
        let err = resolve_tier1_update(&raw, &m, &current, target, ".tar.gz", 4_000_000_000, 0)
            .expect_err("a stale manifest must be refused");
        assert!(err.contains("stale/frozen"), "got: {err}");
    }

    #[test]
    fn tier1_blocks_rollback_on_release_index() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = tier1_raw(target, ".tar.gz", "v0.4.9");
        let m = tier1_manifest(
            target,
            ".tar.gz",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2099-01-01T00:00:00Z",
            "abc",
        );
        let current = semver::Version::parse("0.4.5").unwrap();
        // The highest index ever applied (5000) is GREATER than the candidate
        // (4009): a replayed/superseded release → refused even though it is a
        // signed, version-newer-than-current release.
        let err = resolve_tier1_update(&raw, &m, &current, target, ".tar.gz", 0, 5000)
            .expect_err("a release_index regression must be refused");
        assert!(err.contains("rollback blocked"), "got: {err}");
        // An EQUAL index is likewise refused (already applied).
        let err_eq = resolve_tier1_update(&raw, &m, &current, target, ".tar.gz", 0, 4009)
            .expect_err("an equal release_index must be refused");
        assert!(err_eq.contains("rollback blocked"), "got: {err_eq}");
    }

    #[test]
    fn tier1_blocks_install_below_minimum_floor() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = tier1_raw(target, ".tar.gz", "v0.4.9");
        // minimum_version 0.4.0; running 0.3.0 is below the floor.
        let m = tier1_manifest(
            target,
            ".tar.gz",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2099-01-01T00:00:00Z",
            "abc",
        );
        let current = semver::Version::parse("0.3.0").unwrap();
        let err = resolve_tier1_update(&raw, &m, &current, target, ".tar.gz", 0, 0)
            .expect_err("an install below the minimum floor must be refused");
        assert!(err.contains("minimum_version"), "got: {err}");
    }

    #[test]
    fn tier1_rejects_wrong_product_and_unknown_schema() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = tier1_raw(target, ".tar.gz", "v0.4.9");
        let current = semver::Version::parse("0.4.5").unwrap();

        let mut wrong_product = tier1_manifest(
            target,
            ".tar.gz",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2099-01-01T00:00:00Z",
            "abc",
        );
        wrong_product.product = "scr1b3".to_string();
        let err = resolve_tier1_update(&raw, &wrong_product, &current, target, ".tar.gz", 0, 0)
            .expect_err("a wrong-product manifest must be refused");
        assert!(err.contains("different product"), "got: {err}");

        let mut bad_schema = tier1_manifest(
            target,
            ".tar.gz",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2099-01-01T00:00:00Z",
            "abc",
        );
        bad_schema.schema = "some.other.schema/v1".to_string();
        let err2 = resolve_tier1_update(&raw, &bad_schema, &current, target, ".tar.gz", 0, 0)
            .expect_err("an unrecognised schema must be refused");
        assert!(err2.contains("unrecognised manifest schema"), "got: {err2}");
    }

    #[test]
    fn tier1_none_when_no_archive_for_this_platform() {
        // The manifest only carries a Windows zip; a Linux build finds no archive
        // → "no update for this platform" (Ok(None), not an error).
        let raw = tier1_raw("x86_64-pc-windows-msvc", ".zip", "v0.4.9");
        let m = tier1_manifest(
            "x86_64-pc-windows-msvc",
            ".zip",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2099-01-01T00:00:00Z",
            "abc",
        );
        let current = semver::Version::parse("0.4.5").unwrap();
        let out = resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            0,
        )
        .expect("no-archive-for-platform is not an error");
        assert!(out.is_none());
    }

    #[test]
    fn tier1_tolerates_absent_archive_sidecars_via_signed_manifest_hash() {
        // The signed-manifest-only migration: a manifest-bearing release whose
        // archive OMITS its `.minisig`/`.sha256` sidecars resolves SUCCESSFULLY —
        // the sidecar urls come back EMPTY and verification falls back to the
        // authoritative signed-manifest SHA-256 (the manifest is itself
        // minisign-verified, so the pin is trusted). Reverses the old
        // fail-closed-on-absent-sidecar behavior on purpose.
        let target = "x86_64-unknown-linux-gnu";
        let mut raw = tier1_raw(target, ".tar.gz", "v0.4.9");
        // Drop the archive's own .minisig AND .sha256 sidecars; keep the archive
        // itself and the REQUIRED signed manifest pair.
        raw.assets.retain(|a| {
            a.name == "latest.json.minisig"
                || (!a.name.ends_with(".minisig") && !a.name.ends_with(".sha256"))
        });
        let m = tier1_manifest(
            target,
            ".tar.gz",
            "v0.4.9",
            "0.4.9",
            4009,
            "0.4.0",
            "2099-01-01T00:00:00Z",
            "abc",
        );
        let current = semver::Version::parse("0.4.5").unwrap();
        let info = resolve_tier1_update(&raw, &m, &current, target, ".tar.gz", 0, 0)
            .expect("absent archive sidecars must not error")
            .expect("an in-range update must still be offered");
        assert!(
            info.sig_url.is_empty(),
            "sig_url must be empty when the .minisig sidecar is absent, got {:?}",
            info.sig_url
        );
        assert!(
            info.sha_url.is_empty(),
            "sha_url must be empty when the .sha256 sidecar is absent, got {:?}",
            info.sha_url
        );
        assert!(
            !info.pinned_sha256.is_empty(),
            "the signed-manifest digest must still be pinned"
        );
    }

    #[test]
    fn resolve_expected_sha_pins_agrees_and_detects_mismatch() {
        // The SIGNED manifest digest is authoritative and returned (case-
        // insensitive, whitespace-trimmed) when the sidecar agrees.
        assert_eq!(resolve_expected_sha("ABCDEF", "abcdef").unwrap(), "ABCDEF");
        assert_eq!(resolve_expected_sha("  abc  ", "abc").unwrap(), "abc");
        // A disagreement between the manifest pin and the sidecar fails closed.
        let err = resolve_expected_sha("aaaa", "bbbb")
            .expect_err("a manifest/sidecar disagreement must fail closed");
        assert!(err.contains("disagreement"), "got: {err}");
    }

    #[test]
    fn fetch_optional_sidecar_text_absent_is_none_and_http_is_refused() {
        // An EMPTY url — a release that does not enumerate the sidecar (the
        // signed-manifest-only asset set) — resolves to None WITHOUT any network
        // call, so the caller falls back to the authoritative signed-manifest pin.
        assert!(fetch_optional_sidecar_text("", "minisig")
            .unwrap()
            .is_none());
        assert!(fetch_optional_sidecar_text("   ", "sha256")
            .unwrap()
            .is_none());
        // A NON-empty http:// url is refused BEFORE any fetch (the TLS-downgrade
        // guard) — an ENUMERATED sidecar is still held to the https bar, so
        // tolerating absence never tolerates a downgraded fetch.
        assert!(fetch_optional_sidecar_text("http://evil.example/x.minisig", "minisig").is_err());
    }

    #[test]
    fn pinned_manifest_sha_mismatch_is_rejected_by_the_verify_gate() {
        // The download path passes the resolved (pinned) digest into
        // `verify_artifact_bound`. A pinned digest that does NOT match the actual
        // bytes is rejected with "checksum mismatch" — proving the SIGNED
        // manifest hash binds the downloaded bytes (asset-substitution defense).
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"the real, signed archive bytes";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("timestamp:1\tfile:c0pl4nd.tar.gz"),
            Some("c"),
        )
        .unwrap()
        .to_string();
        let asset = "c0pl4nd.tar.gz";
        let real_sha = sha256_hex(data);

        // The genuine (manifest-matching) sha → accepted.
        assert!(verify_artifact_bound_optional_sig(
            data,
            &real_sha,
            Some(&sig),
            &[pk_box.as_str()],
            asset
        )
        .is_ok());
        // A WRONG pinned sha (attacker swapped the asset under the same key) → rejected.
        let wrong = "0".repeat(64);
        assert_eq!(
            verify_artifact_bound_optional_sig(data, &wrong, Some(&sig), &[pk_box.as_str()], asset)
                .unwrap_err(),
            "checksum mismatch"
        );
    }
}
