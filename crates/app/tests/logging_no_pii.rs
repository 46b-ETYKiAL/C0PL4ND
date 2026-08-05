//! Log-capture harness for the in-app self-updater's security-critical paths.
//!
//! ## Why this suite exists
//!
//! The entire `update_engine` chain (check → download → verify → apply →
//! rollback) historically emitted ZERO `tracing` events: every refusal or
//! failure funnelled only into `UpdateState::Failed(String)` for the UI, so an
//! operator diagnosing a field update-failure from the local `C0PL4ND_LOG`
//! subscriber was blind. This suite proves the now-added structured events:
//!
//! 1. each security REFUSAL (checksum / signature / non-https / non-allowlisted
//!    host / anti-rollback downgrade) EMITS a log at the right level naming the
//!    gate that refused, and
//! 2. a NEGATIVE control (a fully-valid verify) emits NO refusal, and
//! 3. a NO-SECRET control: a planted secret token driven through a refusal path
//!    is NEVER echoed by any captured log line — the logs carry host + version +
//!    gate-name + sizes only, never a full URL/query, key, or raw payload.
//!
//! Telemetry-free discipline: these events go ONLY to the local `tracing`
//! subscriber. This harness installs a per-test in-memory capture layer (no
//! network, no file sink) and asserts against what the subscriber received.
//!
//! The production `update_engine` module is compiled into this test binary via
//! `#[path]` (the app crate is binary-only, with no `lib.rs`), mirroring the
//! other `crates/app/tests/*.rs` integration suites.

#![allow(dead_code)] // The `#[path]`-included module has production entry points
                     // this suite does not all exercise.

use c0pl4nd::egui_app::settings::update_engine;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

use update_engine::net::{download_verify_extract, ReleaseInfo};
use update_engine::updater::{UpdateState, Updater};
use update_engine::verify::{sha256_hex, verify_artifact_bound};

// ---------------------------------------------------------------------------
// In-memory tracing capture layer
// ---------------------------------------------------------------------------

/// One captured `tracing` event, flattened to the fields this suite asserts on.
#[derive(Clone, Debug)]
struct Captured {
    level: Level,
    target: String,
    message: String,
    fields: BTreeMap<String, String>,
}

impl Captured {
    /// Every field value + the message, joined — the surface a no-secret scan
    /// must search (so a planted token cannot hide in any field OR the message).
    fn searchable(&self) -> String {
        let mut s = self.message.clone();
        for (k, v) in &self.fields {
            s.push('\u{1f}');
            s.push_str(k);
            s.push('=');
            s.push_str(v);
        }
        s
    }

    fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: BTreeMap<String, String>,
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let v = format!("{value:?}");
        if field.name() == "message" {
            // `Debug` messages arrive quoted; strip the wrapping quotes so the
            // stored message reads like the source format string.
            self.message = v.trim_matches('"').to_string();
        } else {
            self.fields.insert(field.name().to_string(), v);
        }
    }
}

/// A `tracing` layer that records every event into a shared buffer.
struct CaptureLayer {
    logs: Arc<Mutex<Vec<Captured>>>,
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut v = FieldVisitor::default();
        event.record(&mut v);
        let meta = event.metadata();
        self.logs.lock().unwrap().push(Captured {
            level: *meta.level(),
            target: meta.target().to_string(),
            message: v.message,
            fields: v.fields,
        });
    }
}

/// Run `f` with an isolated capture subscriber installed on the current thread,
/// returning every event the updater emitted. `with_default` is thread-local, so
/// concurrent test threads never cross-contaminate.
fn capture<F: FnOnce()>(f: F) -> Vec<Captured> {
    let logs: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
    let layer = CaptureLayer { logs: logs.clone() };
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, f);
    let out = logs.lock().unwrap().clone();
    out
}

/// Find the first event with `event=<event_kind>` AND `gate=<gate>`.
fn find_refusal<'a>(logs: &'a [Captured], event_kind: &str, gate: &str) -> Option<&'a Captured> {
    logs.iter().find(|c| {
        c.target == "c0pl4nd::update"
            && c.field("event") == Some(event_kind)
            && c.field("gate") == Some(gate)
    })
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A `ReleaseInfo` whose three download URLs are all `url` (so a single planted
/// host/scheme drives the gate under test). Versions/sha are placeholders — the
/// security gates run BEFORE any byte is fetched, so they never matter here.
fn release_info_with_urls(url: &str) -> ReleaseInfo {
    ReleaseInfo {
        version: semver::Version::parse("9.9.9").unwrap(),
        tag: "v9.9.9".to_string(),
        asset_url: url.to_string(),
        asset_name: "c0pl4nd-v9.9.9-x86_64-pc-windows-msvc.zip".to_string(),
        sig_url: url.to_string(),
        sha_url: url.to_string(),
        html_url: "https://github.com/o/r".to_string(),
        pinned_sha256: "deadbeef".to_string(),
        release_index: None,
        installer: None,
    }
}

fn updater_ready(version: &str) -> Updater {
    let mut u = Updater::default();
    u.state = UpdateState::ReadyToApply {
        staged: std::path::PathBuf::from("nonexistent-staged-binary"),
        version: version.to_string(),
        release_index: None,
    };
    u
}

// ---------------------------------------------------------------------------
// Per-path "the refusal EMITS a log at the right level / gate" tests
// ---------------------------------------------------------------------------

#[test]
fn checksum_mismatch_emits_warn_with_checksum_gate() {
    let logs = capture(|| {
        // Good-looking bytes but a wrong expected digest -> checksum gate refuses
        // BEFORE the signature is even checked.
        let err = verify_artifact_bound(
            b"the downloaded bytes",
            "deadbeef",
            "untrusted comment: x\nbogus",
            update_engine::verify::EMBEDDED_PUBLIC_KEYS,
            "c0pl4nd-v9.9.9-x86_64-pc-windows-msvc.zip",
        )
        .unwrap_err();
        assert_eq!(err, "checksum mismatch");
    });

    let ev = find_refusal(&logs, "verify_refused", "checksum")
        .expect("a checksum mismatch must emit a verify_refused/checksum event");
    assert_eq!(ev.level, Level::WARN, "an integrity refusal is WARN");
    assert_eq!(
        ev.field("asset"),
        Some("c0pl4nd-v9.9.9-x86_64-pc-windows-msvc.zip")
    );
    // The actual digest is logged (a public integrity value), letting an operator
    // compare it against the expected pinned hash.
    assert_eq!(
        ev.field("actual_sha256"),
        Some(sha256_hex(b"the downloaded bytes").as_str())
    );
}

#[test]
fn signature_failure_emits_warn_with_signature_gate() {
    // A signature failure: the checksum must MATCH first so the gate reaches the
    // signature branch, then a bogus signature is rejected.
    let data = b"correctly-checksummed but unsigned bytes";
    let sha = sha256_hex(data);
    let logs = capture(|| {
        let err = verify_artifact_bound(
            data,
            &sha,
            "untrusted comment: x\nbogus-signature-line",
            update_engine::verify::EMBEDDED_PUBLIC_KEYS,
            "c0pl4nd.zip",
        )
        .unwrap_err();
        assert!(!err.is_empty());
    });

    let ev = find_refusal(&logs, "verify_refused", "signature")
        .expect("a signature failure must emit a verify_refused/signature event");
    assert_eq!(ev.level, Level::WARN, "a tampering refusal is WARN");
    assert_eq!(ev.field("asset"), Some("c0pl4nd.zip"));
    // The clean WARN must NOT carry the raw verifier error; that lives at DEBUG.
    assert!(
        !ev.message.to_ascii_lowercase().contains("bogus"),
        "the WARN message must stay clean of raw verifier detail: {:?}",
        ev.message
    );
}

#[test]
fn non_https_url_emits_warn_with_https_gate() {
    // A staging dir is created before the gate runs; use a real temp dir.
    let staging = tempfile::tempdir().unwrap();
    let logs = capture(|| {
        let info = release_info_with_urls("http://github.com/o/r/releases/download/v9/c0pl4nd.zip");
        let err = download_verify_extract(&info, staging.path(), |_, _| {}).unwrap_err();
        assert!(err.contains("non-https"), "{err}");
    });

    let ev = find_refusal(&logs, "download_refused", "https")
        .expect("a non-https URL must emit a download_refused/https event");
    assert_eq!(ev.level, Level::WARN);
    // Host is logged (diagnostic); the full URL is NOT.
    assert_eq!(ev.field("host"), Some("github.com"));
}

#[test]
fn non_allowlisted_host_emits_warn_with_host_gate() {
    let staging = tempfile::tempdir().unwrap();
    let logs = capture(|| {
        // All-https so the https gate passes; the host gate then refuses.
        let info = release_info_with_urls("https://attacker.example/c0pl4nd.zip");
        let err = download_verify_extract(&info, staging.path(), |_, _| {}).unwrap_err();
        assert!(err.contains("non-allowlisted host"), "{err}");
    });

    let ev = find_refusal(&logs, "download_refused", "host_allowlist")
        .expect("a non-allowlisted host must emit a download_refused/host_allowlist event");
    assert_eq!(ev.level, Level::WARN);
    assert_eq!(ev.field("host"), Some("attacker.example"));
}

#[test]
fn anti_rollback_downgrade_emits_warn_with_anti_rollback_gate() {
    // A staged version BELOW the running build is a downgrade. `apply_and_restart`
    // refuses it at the anti-rollback gate, BEFORE any swap.
    let ctx = egui::Context::default();
    let logs = capture(|| {
        let mut u = updater_ready("0.0.1");
        u.apply_and_restart(&ctx);
        match &u.state {
            UpdateState::Failed(msg) => assert!(msg.contains("downgrade")),
            other => panic!("expected Failed(downgrade), got {other:?}"),
        }
    });

    let ev = find_refusal(&logs, "update_refused", "anti_rollback")
        .expect("a downgrade must emit an update_refused/anti_rollback event");
    assert_eq!(ev.level, Level::WARN);
    assert_eq!(ev.field("candidate_version"), Some("0.0.1"));
    assert!(
        ev.field("reason").is_some(),
        "the refusal names the gate's reason"
    );
}

// ---------------------------------------------------------------------------
// Negative control: a VALID artifact emits no refusal
// ---------------------------------------------------------------------------

#[test]
fn valid_artifact_emits_no_refusal() {
    // Sign with the dev-only `minisign` crate; verify via the production path.
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk_box = kp.pk.to_box().unwrap().to_string();
    let data = b"a genuine, correctly-signed c0pl4nd binary";
    let sig = minisign::sign(
        Some(&kp.pk),
        &kp.sk,
        std::io::Cursor::new(&data[..]),
        Some("c0pl4nd v9.9.9"), // no `file:` token -> asset binding is a no-op
        Some("comment"),
    )
    .unwrap()
    .to_string();
    let sha = sha256_hex(data);

    let logs = capture(|| {
        verify_artifact_bound(data, &sha, &sig, &[pk_box.as_str()], "c0pl4nd.zip")
            .expect("a correctly-signed, correctly-hashed artifact verifies");
    });

    assert!(
        find_refusal(&logs, "verify_refused", "checksum").is_none()
            && find_refusal(&logs, "verify_refused", "signature").is_none(),
        "a valid artifact must emit NO verify_refused event, got: {logs:?}"
    );
}

// ---------------------------------------------------------------------------
// No-secret control: a planted token is never echoed by any captured log
// ---------------------------------------------------------------------------

#[test]
fn no_captured_log_echoes_a_planted_url_secret() {
    const PLANTED: &str = "PLANTED_SECRET_TOKEN_DO_NOT_LOG_a1b2c3d4";
    let staging = tempfile::tempdir().unwrap();
    let logs = capture(|| {
        // Plant the secret in the URL query of a non-allowlisted host download.
        // The host gate refuses it; the log must record the HOST only, never the
        // full URL (which carries the planted query token).
        let url = format!("https://attacker.example/c0pl4nd.zip?sig={PLANTED}");
        let info = release_info_with_urls(&url);
        let _ = download_verify_extract(&info, staging.path(), |_, _| {});
    });

    // The refusal fired (so the path was actually exercised)...
    assert!(
        find_refusal(&logs, "download_refused", "host_allowlist").is_some(),
        "the host-refusal path must have been exercised"
    );
    // ...and NOT ONE captured event (message or any field) echoes the token.
    for c in &logs {
        assert!(
            !c.searchable().contains(PLANTED),
            "a captured log echoed the planted secret token: {c:?}"
        );
    }
}

#[test]
fn no_captured_log_echoes_a_planted_payload_secret() {
    // A "secret" planted into the artifact bytes themselves must not be echoed by
    // a verify-refusal log (the logs carry sizes + digests, never raw payload).
    const PLANTED: &str = "PLANTED_PAYLOAD_SECRET_xyz789";
    let mut data = b"binary-prefix-".to_vec();
    data.extend_from_slice(PLANTED.as_bytes());
    let logs = capture(|| {
        let _ = verify_artifact_bound(
            &data,
            "deadbeef", // wrong digest -> checksum refusal
            "untrusted comment: x\nbogus",
            update_engine::verify::EMBEDDED_PUBLIC_KEYS,
            "c0pl4nd.zip",
        );
    });
    assert!(
        find_refusal(&logs, "verify_refused", "checksum").is_some(),
        "the checksum-refusal path must have been exercised"
    );
    for c in &logs {
        assert!(
            !c.searchable().contains(PLANTED),
            "a captured log echoed the planted payload secret: {c:?}"
        );
    }
}
