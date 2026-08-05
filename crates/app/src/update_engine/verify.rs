//! Update artifact verification: SHA-256 checksum + minisign (ed25519)
//! signature. Defense in depth — the checksum catches corruption, the
//! signature catches tampering. An update is applied ONLY if BOTH pass.
//!
//! The public key is embedded in the binary at build time (a PUBLIC value,
//! safe to commit); the matching secret key lives OUTSIDE the repo as a CI
//! secret (`MINISIGN_SECRET_KEY`), never committed. Only the holder of that
//! secret can produce a signature this gate accepts — so a downloaded binary
//! a MITM or a compromised release asset cannot forge will fail closed.

use sha2::{Digest, Sha256};

/// The embedded minisign public key (full box form) for the C0PL4ND release
/// signing key. PUBLIC — safe to commit. The in-app updater verifies every
/// downloaded artifact against it; only the holder of the matching secret key
/// (the `MINISIGN_SECRET_KEY` GitHub Actions secret, never committed) can
/// produce an accepted signature.
///
/// Generated 2026-06-04 with `rsign generate -W` (rsign2 — the SAME tool the
/// release workflow signs with, mirroring the sibling SCR1B3 editor's
/// `packaging/signing.md`). The passwordless secret key was written to a
/// gitignored path OUTSIDE the repo and is added to CI as the
/// `MINISIGN_SECRET_KEY` repository secret. Until a signed release is cut,
/// verification has no signed artifact to accept and the UI reports "no
/// verified update available" — it NEVER installs an unsigned binary.
pub const EMBEDDED_PUBLIC_KEY: &str = "untrusted comment: minisign public key: A8D869E2B4DD3FD9\nRWTZP9204mnYqKT/TK6OfYG70QwFoHF5WuuxODg8tgPU+WdLRJYt6iNN";

/// EVERY key this build trusts to sign an update. The production verification
/// path takes this SET, never a single key.
///
/// # Why this is a set, and why it had to be one before shipping
///
/// Verification happens inside the ALREADY-INSTALLED client, against keys
/// compiled into that binary. A client built to trust exactly one key can only
/// ever accept that one key — for the rest of its life, on the user's disk,
/// beyond our reach.
///
/// So if the release key is ever lost, compromised, or simply rotated, releases
/// signed with the new key are rejected by every installed client, and the
/// in-app updater — the mechanism that would deliver the fix — is precisely the
/// thing that stops working. The remaining recovery is "ask every user to
/// re-download by hand", which for a compromise means asking them to do so
/// through the same channel the attacker now controls.
///
/// The capability to rotate cannot be retrofitted into copies already on disk;
/// it has to be in the binary BEFORE it ships. That is why this is a set today
/// while it still holds exactly one key. Rotating later is then an ordinary
/// change: append the new key here, ship a release signed by the OLD key so
/// existing clients accept it, and once that build is widely deployed, drop the
/// old key and switch signing to the new one.
///
/// This is not theoretical for this fleet: the sibling SCR1B3 editor has
/// already used that escape hatch once (`rotate to maintainer-owned signing
/// key`), and could only do so because its updater verified against a set.
///
/// Order is irrelevant — acceptance requires a FULL cryptographic verification
/// against at least one entry, so a longer list weakens nothing.
pub const EMBEDDED_PUBLIC_KEYS: &[&str] = &[EMBEDDED_PUBLIC_KEY];

/// Hex-encoded SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Case-insensitive hex comparison of the SHA-256 of `bytes` against the
/// expected digest (the `.sha256` sidecar's first whitespace token).
pub fn verify_checksum(bytes: &[u8], expected_hex: &str) -> bool {
    sha256_hex(bytes).eq_ignore_ascii_case(expected_hex.trim())
}

/// Verify a minisign signature (`sig_str` = the `.minisig` file contents)
/// against `bytes` using the given public-key box string.
pub fn verify_signature(bytes: &[u8], sig_str: &str, public_key_box: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::decode(public_key_box)
        .map_err(|e| format!("bad public key: {e}"))?;
    let sig =
        minisign_verify::Signature::decode(sig_str).map_err(|e| format!("bad signature: {e}"))?;
    pk.verify(bytes, &sig, false)
        .map_err(|e| format!("signature verification failed: {e}"))
}

/// Verify a minisign signature against a SET of candidate public keys, accepting
/// if ANY of them verifies. The key-rotation-safe form of [`verify_signature`]
/// (see [`EMBEDDED_PUBLIC_KEYS`]).
///
/// SECURITY: acceptance requires a FULL cryptographic verification against at
/// least one key. minisign embeds an 8-byte key id in both the public key and
/// the signature, but that id is only a routing HINT and is attacker-controlled,
/// so it is never trusted on its own: `minisign_verify` rejects a key-id
/// mismatch before the Ed25519 check, and a key-id MATCH still requires the
/// signature itself to verify. Trying more keys therefore cannot turn a bad
/// signature into an accepted one — it can only find the right key for a
/// signature that was already valid.
///
/// Fails closed on an empty set: no trusted keys means nothing is trusted, not
/// that everything is.
pub fn verify_any_signature(
    bytes: &[u8],
    sig_str: &str,
    public_key_boxes: &[&str],
) -> Result<(), String> {
    if public_key_boxes.is_empty() {
        return Err("no trusted public keys configured".to_string());
    }
    let mut last_err = None;
    for pk in public_key_boxes {
        match verify_signature(bytes, sig_str, pk) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| "signature did not verify against any trusted key".to_string()))
}

/// Defense-in-depth (audit P3-#1): assert the signature's *trusted comment* binds
/// to the asset we believe we downloaded. The trusted comment is part of the
/// cryptographically-signed blob (minisign signs `signature || trusted_comment`),
/// so a same-key signature produced over a DIFFERENT asset (e.g. signing the
/// Windows archive and serving it as the Linux one) carries that asset's name in
/// its trusted comment and is caught here — a layer above the `.sha256` sidecar +
/// target-triple asset matching that already constrain this.
///
/// `rsign2`/`minisign` write a default trusted comment of the form
/// `timestamp:<unix>\tfile:<basename>[\tprehashed]` when no explicit `-t` is
/// given (the C0PL4ND release workflow signs exactly this way). We bind on the
/// `file:` token: if present, its value MUST equal `expected_asset` (the asset
/// basename the updater resolved from the target-triple-named release asset).
///
/// Conservative-by-construction so a legitimate update is never broken: if the
/// trusted comment carries NO `file:` token (a hand-rolled custom comment), the
/// binding is skipped rather than failed — the cryptographic signature + checksum
/// remain the load-bearing gates and this is purely additive. A `file:` token
/// that is PRESENT but MISMATCHED is a hard failure (fail-closed).
pub fn verify_signature_bound(
    bytes: &[u8],
    sig_str: &str,
    public_keys: &[&str],
    expected_asset: &str,
) -> Result<(), String> {
    // Cryptographic check first — a bad signature is rejected before we even
    // look at the (now-trusted) comment. Multi-key, so a rotated signing key is
    // still accepted by already-installed clients (see `EMBEDDED_PUBLIC_KEYS`).
    verify_any_signature(bytes, sig_str, public_keys)?;

    let sig =
        minisign_verify::Signature::decode(sig_str).map_err(|e| format!("bad signature: {e}"))?;
    trusted_comment_binds_asset(sig.trusted_comment(), expected_asset)
}

/// The basename of a path-or-filename: the final component after any `/` or `\`
/// separator (so `release/foo.zip` and `foo.zip` both yield `foo.zip`).
fn basename(s: &str) -> &str {
    let s = s.trim();
    s.rsplit(['/', '\\']).next().unwrap_or(s).trim()
}

/// Bind a minisign trusted comment to the asset the updater resolved. The
/// comment is `\t`-separated `key:value` fields; if a `file:` token is present
/// its BASENAME must equal the BASENAME of `expected_asset` (case-insensitive).
///
/// Both sides are reduced to their basename so a signer invoked with a path
/// argument (e.g. `rsign sign release/foo.zip`, which writes
/// `file:release/foo.zip`) still matches the bare downloaded `foo.zip`. Without
/// this, a correctly-signed artifact was rejected with "signature trusted-comment
/// file mismatch" — the `release/`-prefix in-app-updater failure this fixes
/// (alongside the release workflow now signing bare filenames).
///
/// Conservative-by-construction: a comment with NO `file:` token returns `Ok`
/// (the binding is purely additive over the cryptographic signature + checksum);
/// a `file:` token PRESENT but with a mismatched basename is a hard failure.
fn trusted_comment_binds_asset(trusted: &str, expected_asset: &str) -> Result<(), String> {
    if let Some(signed_file) = trusted
        .split('\t')
        .find_map(|field| field.trim().strip_prefix("file:"))
    {
        let signed = basename(signed_file);
        let expected = basename(expected_asset);
        if !signed.eq_ignore_ascii_case(expected) {
            return Err(format!(
                "signature trusted-comment file mismatch: signed for {signed:?}, \
                 expected {expected:?}"
            ));
        }
    }
    Ok(())
}

/// Full gate (fails closed): an artifact is acceptable IFF its checksum matches
/// (SHA-256 first — catches corruption) AND its signature verifies against the
/// embedded public key with the signed trusted-comment `file:` token bound to
/// `expected_asset` (then minisign — catches tampering + same-key wrong-artifact
/// substitution). Returns `Err` the moment any check fails; the caller NEVER
/// returns an unverified binary. The `_bound` suffix denotes the trusted-comment
/// asset binding added in audit P3-#1 (see [`verify_signature_bound`]); the
/// updater always knows the asset name it resolved, so this is the only gate it
/// needs.
pub fn verify_artifact_bound(
    bytes: &[u8],
    expected_sha256: &str,
    sig_str: &str,
    public_keys: &[&str],
    expected_asset: &str,
) -> Result<(), String> {
    verify_artifact_bound_optional_sig(
        bytes,
        expected_sha256,
        Some(sig_str),
        public_keys,
        expected_asset,
    )
}

/// Verify an artifact against the SIGNED-manifest digest (`expected_sha256`,
/// ALWAYS enforced) and, WHEN a `.minisig` sidecar is present, its minisign
/// signature bound to `expected_asset` (defense-in-depth).
///
/// The checksum gate is authoritative: the digest is pinned by the manifest,
/// which is itself minisign-verified against the embedded key BEFORE any asset
/// is fetched. A bit-for-bit substitution is therefore impossible without a
/// SHA-256 preimage, and a forged manifest is impossible without the release
/// private key.
///
/// `sig_str == None` means the release ships NO per-artifact `.minisig` sidecar —
/// the migration toward the signed-manifest-only asset set (fewer release
/// assets). Verification then rests on the checksum gate ALONE, which loses no
/// security because the manifest signature already authenticates that digest. A
/// sidecar that is PRESENT but whose signature is invalid or bound to the WRONG
/// asset is a tampering signal and is REJECTED (fail-closed). The checksum gate
/// runs FIRST in every case, so a mismatching artifact never reaches the
/// signature step.
pub fn verify_artifact_bound_optional_sig(
    bytes: &[u8],
    expected_sha256: &str,
    sig_str: Option<&str>,
    public_keys: &[&str],
    expected_asset: &str,
) -> Result<(), String> {
    if !verify_checksum(bytes, expected_sha256) {
        // SECURITY: a checksum mismatch means the downloaded bytes are NOT the
        // artifact the signed manifest pinned — corruption or substitution. Log
        // the integrity refusal (SHA-256 digests are public values, safe to log;
        // no secret/PII). Naming the gate ("checksum") lets an operator localise
        // the failure that otherwise only surfaced as `UpdateState::Failed`.
        tracing::warn!(
            target: "c0pl4nd::update",
            event = "verify_refused",
            gate = "checksum",
            asset = expected_asset,
            artifact_bytes = bytes.len(),
            expected_sha256 = expected_sha256.trim(),
            actual_sha256 = %sha256_hex(bytes),
            "update artifact rejected: SHA-256 checksum mismatch"
        );
        return Err("checksum mismatch".to_string());
    }
    let Some(sig_str) = sig_str else {
        // No per-artifact sidecar shipped: the signed-manifest checksum gate
        // above is authoritative. Record the fall-back so a skipped
        // defense-in-depth layer is OBSERVABLE (never a silent swallow).
        tracing::info!(
            target: "c0pl4nd::update",
            event = "verify_sidecarless",
            asset = expected_asset,
            artifact_bytes = bytes.len(),
            "no .minisig sidecar present; verified against the signed-manifest SHA-256"
        );
        return Ok(());
    };
    if let Err(e) = verify_signature_bound(bytes, sig_str, public_keys, expected_asset) {
        // SECURITY: a minisign verification failure means the bytes were not
        // signed by the embedded release key (tampering, a forged sidecar, or a
        // same-key wrong-artifact substitution caught by the trusted-comment
        // binding). The WARN message stays clean; the raw verifier error goes to
        // a DEBUG field (it is a minisign classification string — no key material
        // or PII — but DEBUG keeps the WARN line free of raw detail).
        tracing::warn!(
            target: "c0pl4nd::update",
            event = "verify_refused",
            gate = "signature",
            asset = expected_asset,
            artifact_bytes = bytes.len(),
            "update artifact rejected: minisign signature verification failed"
        );
        tracing::debug!(
            target: "c0pl4nd::update",
            event = "verify_refused_detail",
            gate = "signature",
            detail = %e,
            "minisign verification failure detail"
        );
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        // SHA-256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn trusted_comment_binding_compares_basenames() {
        let asset = "c0pl4nd-v0.4.8-x86_64-pc-windows-msvc.zip";

        // Bare filename in the trusted comment (the intended form) → matches.
        assert!(
            trusted_comment_binds_asset(&format!("timestamp:123\tfile:{asset}"), asset).is_ok()
        );

        // REGRESSION (the in-app-update failure): the signer was invoked with a
        // `release/` PATH prefix, so the trusted comment carries
        // `file:release/<asset>`. The basename comparison must still accept it.
        assert!(
            trusted_comment_binds_asset(&format!("timestamp:123\tfile:release/{asset}"), asset)
                .is_ok(),
            "a release/-prefixed trusted comment must bind to the bare asset"
        );
        // Windows-style separator too.
        assert!(trusted_comment_binds_asset(
            &format!("timestamp:123\tfile:release\\{asset}"),
            asset
        )
        .is_ok());

        // Case-insensitive on the filename.
        assert!(trusted_comment_binds_asset(
            &format!("timestamp:123\tfile:{}", asset.to_uppercase()),
            asset
        )
        .is_ok());

        // A genuinely DIFFERENT asset (wrong-artifact substitution) → hard fail.
        let err = trusted_comment_binds_asset(
            "timestamp:123\tfile:c0pl4nd-v0.4.8-x86_64-unknown-linux-gnu.tar.gz",
            asset,
        )
        .unwrap_err();
        assert!(
            err.contains("mismatch"),
            "wrong asset must be rejected: {err}"
        );

        // No `file:` token (hand-rolled comment) → binding skipped (additive).
        assert!(trusted_comment_binds_asset("timestamp:123", asset).is_ok());
    }

    #[test]
    fn checksum_match_and_mismatch() {
        let data = b"c0pl4nd release artifact";
        let good = sha256_hex(data);
        assert!(verify_checksum(data, &good));
        assert!(verify_checksum(data, &good.to_uppercase())); // case-insensitive
        assert!(verify_checksum(data, &format!("  {good}  "))); // trims whitespace
        assert!(!verify_checksum(data, "deadbeef"));
    }

    #[test]
    fn signature_roundtrip_accepts_valid_rejects_tampered() {
        // Sign with the dev-only `minisign` crate, verify with the production
        // `minisign-verify` path. Proves the verify path accepts a real sig and
        // rejects tampered data — the verify-before-swap contract.
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"the new c0pl4nd binary bytes";
        let sig_box = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("c0pl4nd v9.9.9"),
            Some("comment"),
        )
        .unwrap();
        let sig_str = sig_box.to_string();

        // Valid signature over the exact bytes -> accepted.
        assert!(verify_signature(data, &sig_str, &pk_box).is_ok());

        // Tampered bytes -> rejected.
        let tampered = b"the new c0pl4nd binary bytez";
        assert!(verify_signature(tampered, &sig_str, &pk_box).is_err());
    }

    #[test]
    fn verify_artifact_requires_both_checksum_and_signature() {
        // A full round-trip through the combined gate: correct sha + valid sig
        // accepts; a wrong sha rejects BEFORE the signature is even checked. Uses
        // a trusted comment with no `file:` token, so the asset binding is a
        // no-op here and this exercises the checksum+signature path in isolation.
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"c0pl4nd-x86_64-pc-windows-msvc.tar.gz bytes";
        let sig_box = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("c0pl4nd"),
            Some("c"),
        )
        .unwrap();
        let sig_str = sig_box.to_string();
        let sha = sha256_hex(data);
        let asset = "c0pl4nd-x86_64-pc-windows-msvc.tar.gz";

        assert!(verify_artifact_bound(data, &sha, &sig_str, &[pk_box.as_str()], asset).is_ok());
        // Wrong checksum fails closed even with a valid signature.
        assert_eq!(
            verify_artifact_bound(data, "deadbeef", &sig_str, &[pk_box.as_str()], asset)
                .unwrap_err(),
            "checksum mismatch"
        );
        // Right checksum but a bogus signature also fails closed.
        assert!(verify_artifact_bound(
            data,
            &sha,
            "untrusted comment: x\nbogus",
            &[pk_box.as_str()],
            asset
        )
        .is_err());
    }

    #[test]
    fn optional_sig_absent_verifies_via_pin_but_never_relaxes_integrity() {
        // The signed-manifest-only migration: when NO `.minisig` sidecar ships
        // (`sig = None`) the artifact is accepted on the AUTHORITATIVE
        // signed-manifest SHA-256 ALONE — but a wrong/ tampered hash is still
        // rejected, and a PRESENT-but-bogus signature still fails closed. Proves
        // absence relaxes ONLY the redundant signature layer, never integrity.
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"signed-manifest-only archive bytes";
        let sha = sha256_hex(data);
        let asset = "c0pl4nd-x86_64-unknown-linux-gnu.tar.gz";

        // No sidecar + correct signed hash → accepted (verified via the pin).
        assert!(
            verify_artifact_bound_optional_sig(data, &sha, None, &[pk_box.as_str()], asset).is_ok(),
            "an absent sidecar must verify against the signed-manifest SHA-256"
        );
        // No sidecar + WRONG hash → rejected (the integrity gate always runs).
        assert_eq!(
            verify_artifact_bound_optional_sig(data, "deadbeef", None, &[pk_box.as_str()], asset)
                .unwrap_err(),
            "checksum mismatch"
        );
        // A tampered archive carrying the OLD pinned hash → rejected even without
        // a sidecar (preimage resistance of the signed pin catches substitution).
        let tampered = b"signed-manifest-only archive bytes (TAMPERED)";
        assert_eq!(
            verify_artifact_bound_optional_sig(tampered, &sha, None, &[pk_box.as_str()], asset)
                .unwrap_err(),
            "checksum mismatch"
        );
        // Sidecar PRESENT but bogus, correct hash → still fails closed (a present
        // signature is ALWAYS enforced; absence is the only relaxation).
        assert!(
            verify_artifact_bound_optional_sig(
                data,
                &sha,
                Some("untrusted comment: x\nbogus"),
                &[pk_box.as_str()],
                asset
            )
            .is_err(),
            "a present-but-invalid signature must be rejected"
        );
        // Sidecar present + valid signature + correct hash → accepted (parity
        // with the strict `verify_artifact_bound` path).
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("timestamp:1\tfile:c0pl4nd-x86_64-unknown-linux-gnu.tar.gz"),
            Some("c"),
        )
        .unwrap()
        .to_string();
        assert!(verify_artifact_bound_optional_sig(
            data,
            &sha,
            Some(&sig),
            &[pk_box.as_str()],
            asset
        )
        .is_ok());
    }

    #[test]
    fn trusted_comment_binding_accepts_match_rejects_mismatch() {
        // The release workflow signs with rsign2's DEFAULT trusted comment,
        // which embeds `file:<basename>`. We bind on that token: a signature
        // whose trusted comment names a DIFFERENT asset is rejected even though
        // it verifies cryptographically against the same key (the same-key
        // wrong-artifact substitution this layer defends against).
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"the linux archive bytes";

        // Sign with a trusted comment naming the LINUX asset (mirrors the
        // rsign2 default `file:<basename>` shape).
        let sig_linux = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("timestamp:1700000000\tfile:c0pl4nd-v1.0.0-x86_64-unknown-linux-gnu.tar.gz"),
            Some("comment"),
        )
        .unwrap()
        .to_string();

        // Matching asset name -> accepted.
        assert!(verify_signature_bound(
            data,
            &sig_linux,
            &[pk_box.as_str()],
            "c0pl4nd-v1.0.0-x86_64-unknown-linux-gnu.tar.gz",
        )
        .is_ok());

        // Same key, same bytes, but we expected the WINDOWS asset -> the
        // trusted-comment `file:` token mismatches -> rejected.
        let err = verify_signature_bound(
            data,
            &sig_linux,
            &[pk_box.as_str()],
            "c0pl4nd-v1.0.0-x86_64-pc-windows-msvc.zip",
        )
        .unwrap_err();
        assert!(err.contains("trusted-comment file mismatch"), "{err}");

        // A staging-dir-prefixed expected name still binds on the basename.
        assert!(verify_signature_bound(
            data,
            &sig_linux,
            &[pk_box.as_str()],
            "/tmp/staging/c0pl4nd-v1.0.0-x86_64-unknown-linux-gnu.tar.gz",
        )
        .is_ok());
    }

    #[test]
    fn trusted_comment_binding_is_skipped_when_no_file_token() {
        // Conservative-by-construction: a custom trusted comment with NO `file:`
        // token does NOT break verification — the binding is purely additive on
        // top of the load-bearing checksum + signature gates.
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"archive bytes with custom comment";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("c0pl4nd v1.2.3"), // no `file:` token
            Some("comment"),
        )
        .unwrap()
        .to_string();
        // Binding skipped -> any expected asset name passes (crypto still gates).
        assert!(verify_signature_bound(data, &sig, &[pk_box.as_str()], "anything.tar.gz").is_ok());
        // But tampered bytes still fail closed.
        assert!(
            verify_signature_bound(b"tampered", &sig, &[pk_box.as_str()], "anything.tar.gz")
                .is_err()
        );
    }

    #[test]
    fn verify_artifact_bound_requires_checksum_signature_and_asset_binding() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"c0pl4nd-v2.0.0-x86_64-pc-windows-msvc.zip bytes";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("timestamp:1700000001\tfile:c0pl4nd-v2.0.0-x86_64-pc-windows-msvc.zip"),
            Some("c"),
        )
        .unwrap()
        .to_string();
        let sha = sha256_hex(data);
        let asset = "c0pl4nd-v2.0.0-x86_64-pc-windows-msvc.zip";

        // Correct sha + valid sig + matching asset name -> accepted.
        assert!(verify_artifact_bound(data, &sha, &sig, &[pk_box.as_str()], asset).is_ok());
        // Wrong checksum fails closed BEFORE the signature is even checked.
        assert_eq!(
            verify_artifact_bound(data, "deadbeef", &sig, &[pk_box.as_str()], asset).unwrap_err(),
            "checksum mismatch"
        );
        // Right sha + valid sig but the WRONG expected asset -> rejected.
        assert!(
            verify_artifact_bound(data, &sha, &sig, &[pk_box.as_str()], "wrong-asset.tar.gz")
                .is_err()
        );
    }

    #[test]
    fn embedded_key_decodes() {
        // The committed embedded key must be a well-formed minisign public key
        // (so a real release signature CAN verify against it).
        assert!(minisign_verify::PublicKey::decode(EMBEDDED_PUBLIC_KEY).is_ok());
    }

    #[test]
    fn embedded_key_rejects_bogus_signatures() {
        // The embedded release key must reject a malformed / forged signature
        // (it only accepts artifacts signed by the matching secret key) — this
        // is the "fails closed until a signed release exists" guarantee.
        assert!(
            verify_signature(b"x", "untrusted comment: x\nbogus", EMBEDDED_PUBLIC_KEY).is_err()
        );
    }

    /// The embedded SET must be non-empty and every entry a decodable key.
    ///
    /// An empty set would make `verify_any_signature` reject everything — the
    /// updater would fail closed rather than open, but it would be permanently
    /// broken, so this is a real (if benign-direction) failure worth pinning.
    #[test]
    fn embedded_key_set_is_non_empty_and_all_entries_decode() {
        assert!(
            !EMBEDDED_PUBLIC_KEYS.is_empty(),
            "an empty trusted-key set rejects every update forever"
        );
        for pk in EMBEDDED_PUBLIC_KEYS {
            assert!(
                minisign_verify::PublicKey::decode(pk).is_ok(),
                "every embedded key must be a well-formed minisign public key"
            );
        }
        assert!(
            EMBEDDED_PUBLIC_KEYS.contains(&EMBEDDED_PUBLIC_KEY),
            "the current release key must be in the trusted set"
        );
    }

    /// THE ROTATION PROPERTY — the reason the set exists.
    ///
    /// An artifact signed by a key that is in the set but is NOT the first entry
    /// must still verify. This is exactly the state during a key rotation: the
    /// installed client carries {old, new} and the release is signed by one of
    /// them. Without it, rotating the signing key bricks in-app updates for
    /// every client already on disk, and that cannot be fixed after the fact
    /// because the fix would have to arrive through the updater it broke.
    #[test]
    fn a_signature_from_any_trusted_key_verifies() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"a release signed by the ROTATED key";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("c0pl4nd v9.9.9"),
            Some("comment"),
        )
        .unwrap()
        .to_string();

        // The rotated key is SECOND; the embedded release key is first and does
        // not match this signature. A single-key verifier fails here.
        let rotated_set = [EMBEDDED_PUBLIC_KEY, pk_box.as_str()];
        assert!(
            verify_any_signature(data, &sig, &rotated_set).is_ok(),
            "a signature from ANY trusted key must verify — this is the whole \
             point of the set"
        );

        // …and trying several keys must NOT launder a bad signature. A set that
        // accepted anything once it held more than one key would be worse than
        // the single-key form it replaced.
        assert!(
            verify_any_signature(b"different bytes", &sig, &rotated_set).is_err(),
            "multi-key must not weaken verification: tampered bytes still fail \
             against every key"
        );
    }

    /// Fail-closed on an empty set: no trusted keys means nothing is trusted.
    #[test]
    fn an_empty_key_set_rejects_everything() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let data = b"payload";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            None,
            None,
        )
        .unwrap()
        .to_string();
        assert!(
            verify_any_signature(data, &sig, &[]).is_err(),
            "an empty trusted set must reject a signature that is otherwise valid"
        );
    }

    /// The full artifact gate must carry the rotation property too — a caller
    /// going through `verify_artifact_bound` (the path the updater uses) must
    /// accept a non-first trusted key.
    #[test]
    fn the_artifact_gate_accepts_a_non_first_trusted_key() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"artifact bytes signed by the rotated key";
        let sha = sha256_hex(data);
        let asset = "c0pl4nd-x86_64-pc-windows-msvc.zip";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some(&format!("timestamp:0\tfile:{asset}")),
            Some("comment"),
        )
        .unwrap()
        .to_string();

        let rotated_set = [EMBEDDED_PUBLIC_KEY, pk_box.as_str()];
        assert!(
            verify_artifact_bound(data, &sha, &sig, &rotated_set, asset).is_ok(),
            "the artifact gate must accept a signature from any trusted key"
        );
    }
}
