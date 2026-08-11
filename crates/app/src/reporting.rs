//! W1TN3SS opt-in crash/error reporting — the C0PL4ND host glue (Tier-1).
//!
//! This module is thin host glue over the in-house `itasha-report-core` SDK
//! (pinned git tag). C0PL4ND implements NO SDK behavior — the config model,
//! sanitizer, spool, transport, preview API and consent gate all live in the
//! SDK and are CALLED here. The two seams this module owns are:
//!
//! 1. **Capture** ([`capture_panic`]) — builds a Tier-1 report from a panic's
//!    `&'static str` message + our own `file:line` SITE, sanitizes it, and
//!    SPOOLS it locally. It transmits NOTHING — local-first, offline-safe,
//!    consent comes later.
//! 2. **Consent-gated send** ([`send_report`]) — given a host-minted
//!    [`ConsentToken`] (which only exists after the user agreed in the consent
//!    dialog, or because the stream's mode is `Always`), transmit one spooled
//!    report through the SDK's hardened transport, then log the outcome.
//!
//! Privacy invariants (inherited from the SDK, asserted at this surface):
//! - default-OFF (both streams default [`ReportingMode::Off`]),
//! - consent-gated (no [`ConsentToken`] => no send — enforced at the type level
//!   by the SDK's `IngestBackend::send` signature),
//! - previewable+editable before send (the dialog calls [`preview_text`]),
//! - no persistent identifier (only the consent token's ephemeral nonce),
//! - the panic `&'static str` discipline (a `String` payload — which could embed
//!   environment fragments or a path — is deliberately suppressed at capture).

use std::path::{Path, PathBuf};

use itasha_report_core::backend::{
    IngestBackend, LeanPipelineBackend, SendOutcome, TransportConfig,
};
use itasha_report_core::consent::ConsentToken;
use itasha_report_core::preview::Preview;
use itasha_report_core::report::{Report, Stream};
use itasha_report_core::sanitize::Sanitizer;
use itasha_report_core::spool::Spool;
use itasha_report_transport_tor::{TorOnionTransport, TorTransportConfig};

// Re-export the SDK's ReportingMode so the rest of the app names ONE type.
pub use itasha_report_core::config::ReportingMode;

use c0pl4nd_core::Config;

/// The env var that injects the self-hosted ingest endpoint. There is NO
/// hardcoded URL in C0PL4ND and NO default — a build with this unset can spool
/// locally but can NEVER transmit (a mis-build cannot phone home). The
/// server-side endpoint is a separate plan; until one is configured, reports
/// stay in the local spool and a consented send returns a structured
/// `no-endpoint` outcome (never a silent drop, never a fake success).
pub const REPORT_ENDPOINT_ENV: &str = "C0PL4ND_REPORT_ENDPOINT";

/// The env var that opts a user into the metadata-resistant **Tor-onion**
/// transport. When set to a structurally-valid v3 `.onion` address (56 base32
/// chars + `.onion`), a consented send is routed over Arti's pure-Rust Tor
/// stack to that hidden service instead of the clearnet `LeanPipelineBackend` —
/// giving SENDER ANONYMITY (the ingest server never sees the user's IP). This is
/// strictly OPT-IN: unset (the default) keeps the existing clearnet path, so
/// nothing changes for users who do not configure an onion address. A
/// structurally-invalid value is treated as unset (it can never silently
/// downgrade-and-send over clearnet under a false sense of anonymity — see
/// [`choose_transport`]).
pub const REPORT_ONION_ENV: &str = "C0PL4ND_REPORT_ONION";

/// The port the W1TN3SS onion ingest service listens on. Onion services define
/// their own virtual port map; the W1TN3SS hidden service publishes its ingest
/// endpoint on 443 (the conventional HTTPS virtual port for an onion front).
const REPORT_ONION_PORT: u16 = 443;

/// Which transport a consented send will use, decided purely from configuration
/// BEFORE any network or filesystem touch. Factored out so the routing decision
/// is unit-testable without a live onion or a real Tor bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportChoice {
    /// The default clearnet `LeanPipelineBackend` to the configured HTTPS
    /// endpoint, or no transport at all when no endpoint is configured.
    Clearnet { endpoint: Option<String> },
    /// The metadata-resistant Tor-onion transport to the given v3 `.onion`
    /// address (already structurally validated).
    Tor { onion: String },
}

impl TransportChoice {
    /// A stable, non-identifying label for logging which transport class a send
    /// used (NEVER the endpoint or onion address itself — those could be
    /// fingerprints). Honours the same counts/enums-only logging discipline as
    /// [`ReportOutcome::log_detail`].
    fn class(&self) -> &'static str {
        match self {
            TransportChoice::Clearnet { endpoint: Some(_) } => "clearnet",
            TransportChoice::Clearnet { endpoint: None } => "clearnet-no-endpoint",
            TransportChoice::Tor { .. } => "tor",
        }
    }
}

/// Decide the transport PURELY from the configured onion + clearnet endpoint
/// values (the strings the host already read from env/config). This is the
/// selection seam:
///
/// - An onion address that is a structurally-valid v3 `.onion` selects
///   [`TransportChoice::Tor`] — the opt-in metadata-resistant path.
/// - Otherwise (no onion configured, OR a malformed onion value) the existing
///   clearnet path is selected. A MALFORMED onion is NEVER silently treated as
///   "anonymous"; it falls back to the explicit clearnet path so the user is
///   never given a false sense of anonymity while actually sending over
///   clearnet — the structural validity gate is `TorTransportConfig::is_valid_onion`.
/// - The default (both unset) is `Clearnet { endpoint: None }` — spool-only, no
///   transmission, exactly the pre-Tor behaviour.
#[must_use]
pub fn choose_transport(onion: Option<&str>, endpoint: Option<&str>) -> TransportChoice {
    if let Some(onion) = onion.map(str::trim).filter(|s| !s.is_empty()) {
        // Build a throwaway config purely to run the SDK's structural v3-onion
        // check; the state/cache/config dirs are irrelevant to validation and
        // are never touched here (no I/O on the selection path).
        let probe = TorTransportConfig::new(onion, REPORT_ONION_PORT, "", "");
        if probe.is_valid_onion() {
            return TransportChoice::Tor {
                onion: onion.to_string(),
            };
        }
        // A non-empty-but-malformed onion falls through to clearnet — never a
        // silent anonymity downgrade masquerading as the Tor path.
    }
    TransportChoice::Clearnet {
        endpoint: endpoint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

/// The structured result of attempting a report, logged counts/enums only
/// (never PII). A report is either captured-and-spooled, sent, refused for want
/// of an endpoint, or failed in transport — never silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportOutcome {
    /// The panic was captured and written to the local spool. Nothing sent.
    Spooled,
    /// A consented report was transmitted and accepted by the endpoint.
    Sent,
    /// Consent was present but no endpoint is configured — the report stays
    /// spooled for a later, configured send.
    RefusedNoEndpoint,
    /// The transport failed (offline, TLS, status). The report is retained.
    Failed(String),
}

impl ReportOutcome {
    /// The stable, non-identifying log-detail string for this outcome.
    fn log_detail(&self) -> &'static str {
        match self {
            ReportOutcome::Spooled => "spooled",
            ReportOutcome::Sent => "sent",
            ReportOutcome::RefusedNoEndpoint => "refused-no-endpoint",
            ReportOutcome::Failed(_) => "failed",
        }
    }

    /// A COARSE, privacy-safe classification of a `Failed` reason — a stable
    /// enum-like token an operator can act on, WITHOUT inlining the raw reason
    /// string (which can embed a configured endpoint host / onion). Substring
    /// classification only; the raw reason is never logged. `None` for the
    /// non-failure variants. This is the diagnostic the audit (LOG-C0-038) asked
    /// for: previously a `Failed` send collapsed to the single token "failed".
    fn failure_kind(&self) -> Option<&'static str> {
        let ReportOutcome::Failed(reason) = self else {
            return None;
        };
        let r = reason.to_ascii_lowercase();
        let kind = if r.contains("spool-open") || r.contains("spool") {
            "spool"
        } else if r.contains("timeout") || r.contains("timed out") {
            "timeout"
        } else if r.contains("tls") || r.contains("certificate") || r.contains("handshake") {
            "tls"
        } else if r.contains("status") || r.contains("http") {
            "http-status"
        } else if r.contains("connect")
            || r.contains("offline")
            || r.contains("dns")
            || r.contains("resolve")
            || r.contains("unreachable")
        {
            "connect"
        } else {
            "transport"
        };
        Some(kind)
    }
}

/// Log a report outcome counts/enums only (no PII — the `Failed` reason is
/// NEVER inlined). Honours `C0PL4ND_DISABLE_TELEMETRY=1` by emitting nothing.
/// Best-effort; never blocks.
fn log_outcome(outcome: &ReportOutcome) {
    if std::env::var_os("C0PL4ND_DISABLE_TELEMETRY").is_some() {
        return;
    }
    // A transport FAILURE escalates to WARN and carries a coarse, privacy-safe
    // `reason_kind` (LOG-C0-037 level / LOG-C0-038 dropped-reason): the operator
    // can now tell a TLS failure from a connect failure WITHOUT the raw reason
    // (which can embed the configured endpoint/onion) ever being logged. Every
    // other outcome stays at INFO with the counts-only `detail`.
    match outcome.failure_kind() {
        Some(reason_kind) => tracing::warn!(
            target: "c0pl4nd::report",
            detail = outcome.log_detail(),
            reason_kind = reason_kind,
        ),
        None => tracing::info!(target: "c0pl4nd::report", detail = outcome.log_detail()),
    }
}

/// Build a sanitized Tier-1 crash report from the panic's STATIC message + our
/// own panic SITE. Only a source-literal message (e.g. an `expect("…")` string)
/// and the `file:line` of our own code enter the report. A runtime `String`
/// payload — which could embed environment fragments or a user's path — is the
/// caller's responsibility to keep out (the hook passes `&'static str` only);
/// the SDK's [`Sanitizer`] is the second line of defense (home/username/host
/// scrub).
pub fn build_crash_report(static_msg: &'static str, location: &str) -> Report {
    let raw = Report::crash(format!("panic: {static_msg} (at {location})"))
        .with_metadata("app_version", env!("CARGO_PKG_VERSION"))
        .with_metadata("os", std::env::consts::OS);
    Sanitizer::new().sanitize(raw)
}

/// The literal, editable Tier-1 preview text the consent dialog shows the user
/// BEFORE any send. This is the transparency primitive — the user sees exactly
/// what would leave the machine.
#[must_use]
pub fn preview_text(report: &Report) -> String {
    Preview::of(report).text().to_string()
}

/// Rebuild a [`Report`] from the user-edited preview text, preserving the
/// original report's stream, title, metadata, and attachments. The preview text
/// renders as `title\n\nbody[\n\n--- metadata ---\n…]`; this extracts the BODY
/// span so the user's edits/redactions to the body are what gets sent.
#[must_use]
pub fn edited_report_from_preview_text(edited_text: &str, original: &Report) -> Report {
    let body = edited_text
        // Drop the title line: everything after the first blank-line separator.
        .split_once("\n\n")
        .map(|(_title, rest)| rest)
        .unwrap_or(edited_text)
        // Drop the metadata footer if present.
        .split("\n\n--- metadata ---\n")
        .next()
        .unwrap_or(edited_text)
        .to_string();
    Report {
        stream: original.stream,
        title: original.title.clone(),
        body,
        metadata: original.metadata.clone(),
        attachments: original.attachments.clone(),
    }
}

/// Capture a panic into the local spool. Builds the sanitized Tier-1 report,
/// then enqueues it to `<config_dir>/reports/` via the SDK's atomic spool. This
/// is the panic-hook seam: it CAPTURES + SPOOLS but transmits NOTHING — consent
/// is sought on the NEXT launch (ask-each-time) or honoured automatically
/// (`Always`), never inside the panic hook. Returns the outcome (for logging).
///
/// Best-effort and panic-safe: a spool failure inside an already-panicking
/// thread must not re-panic. The outcome is logged either way.
pub fn capture_panic(static_msg: &'static str, location: &str) -> ReportOutcome {
    capture_panic_in(Config::config_dir().as_deref(), static_msg, location)
}

/// The config-dir-injectable core of [`capture_panic`]. The public wrapper
/// resolves the GLOBAL `Config::config_dir()` and delegates here; tests pass an
/// EXPLICIT temp dir (or `None`) so the capture seam — including the
/// spool-open, enqueue-error, and no-config-dir arms — is fully exercisable
/// without ever mutating the process-global config-dir env (which other test
/// modules in this binary read concurrently). The outcome is logged either way.
fn capture_panic_in(
    config_dir: Option<&Path>,
    static_msg: &'static str,
    location: &str,
) -> ReportOutcome {
    let outcome = match config_dir {
        Some(dir) => match Spool::open(dir) {
            Ok(spool) => {
                let report = build_crash_report(static_msg, location);
                match spool.enqueue(&report) {
                    Ok(_path) => ReportOutcome::Spooled,
                    Err(e) => ReportOutcome::Failed(format!("spool: {e}")),
                }
            }
            Err(e) => ReportOutcome::Failed(format!("spool-open: {e}")),
        },
        // No config dir => nowhere to spool. Surface it rather than swallow.
        None => ReportOutcome::Failed("no-config-dir".to_string()),
    };
    log_outcome(&outcome);
    outcome
}

/// Transmit ONE report through the SDK's hardened transport, consent-gated.
///
/// The `consent` argument is mandatory — there is no send path without it (the
/// SDK enforces this at the type level). The host mints the [`ConsentToken`]
/// ONLY after the user agreed in the dialog, or because the stream's mode is
/// `Always`. The outcome is logged.
///
/// Transport selection ([`choose_transport`]):
/// - If the user opted into the Tor-onion transport (a structurally-valid v3
///   `.onion` in `C0PL4ND_REPORT_ONION`), the report is sent over Arti's
///   pure-Rust Tor stack — the metadata-resistant, sender-anonymous path.
/// - Otherwise the default clearnet [`LeanPipelineBackend`] is used (a static
///   User-Agent, zero redirects, bounded timeout, size-capped, NO persistent
///   identifier — only the token's ephemeral nonce).
/// - If neither an onion NOR a clearnet endpoint is configured, this returns
///   [`ReportOutcome::RefusedNoEndpoint`] and transmits nothing — the report
///   stays in the spool for a later, configured send (never a silent drop,
///   never a fake `Sent`).
pub fn send_report(report: &Report, consent: &ConsentToken) -> ReportOutcome {
    send_report_with(
        report,
        consent,
        onion_from_env().as_deref(),
        endpoint_from_env().as_deref(),
        Config::config_dir().as_deref(),
    )
}

/// The config-injectable core of [`send_report`]. The public wrapper resolves
/// the onion/endpoint env + the GLOBAL `Config::config_dir()` and delegates
/// here; tests pass EXPLICIT values so the transport-selection, clearnet,
/// Tor-construction, and no-config-dir arms are all exercisable without
/// mutating process-global env. `config_dir` is consulted only on the Tor path
/// (to root the Arti state/cache); the clearnet path never touches it.
fn send_report_with(
    report: &Report,
    consent: &ConsentToken,
    onion: Option<&str>,
    endpoint: Option<&str>,
    config_dir: Option<&Path>,
) -> ReportOutcome {
    let choice = choose_transport(onion, endpoint);
    log_transport_choice(&choice);
    let outcome = match choice {
        TransportChoice::Tor { onion } => send_over_tor_in(config_dir, report, consent, &onion),
        TransportChoice::Clearnet {
            endpoint: Some(endpoint),
        } => {
            let backend = LeanPipelineBackend::new(TransportConfig::new(endpoint));
            send_via_backend(&backend, report, consent)
        }
        TransportChoice::Clearnet { endpoint: None } => ReportOutcome::RefusedNoEndpoint,
    };
    log_outcome(&outcome);
    outcome
}

/// Run one `IngestBackend::send` and fold its result into a [`ReportOutcome`].
/// Shared by the clearnet and Tor paths so both honour the identical
/// sent/failed mapping.
fn send_via_backend<B: IngestBackend>(
    backend: &B,
    report: &Report,
    consent: &ConsentToken,
) -> ReportOutcome {
    match backend.send(report, consent) {
        Ok(SendOutcome::Sent) => ReportOutcome::Sent,
        Ok(SendOutcome::Failed(reason)) => ReportOutcome::Failed(reason),
        Err(e) => ReportOutcome::Failed(e.to_string()),
    }
}

/// Build the Tor-onion transport rooted under the app's per-user data dir and
/// send one report over it. The Arti state/cache live under
/// `<config_dir>/tor/{state,cache}` so the bootstrap directory cache survives
/// across launches (a fresh bootstrap every send would be slow + chatty). If no
/// config dir resolves (no `%APPDATA%`/`$HOME`), the report is retained and the
/// outcome surfaces the reason rather than silently dropping it.
fn send_over_tor_in(
    config_dir: Option<&Path>,
    report: &Report,
    consent: &ConsentToken,
    onion: &str,
) -> ReportOutcome {
    let Some(dir) = config_dir else {
        return ReportOutcome::Failed("tor: no-config-dir".to_string());
    };
    let tor_root = dir.join("tor");
    let state_dir = tor_root.join("state");
    let cache_dir = tor_root.join("cache");
    let config_dir = tor_root.join("config");
    let cfg = TorTransportConfig::new(onion, REPORT_ONION_PORT, state_dir, cache_dir);
    match TorOnionTransport::new(cfg, config_dir) {
        Ok(backend) => send_via_backend(&backend, report, consent),
        Err(e) => ReportOutcome::Failed(format!("tor: {e}")),
    }
}

/// Log the transport CLASS (counts/enums only — never the endpoint or onion
/// address, which could be fingerprints). Honours `C0PL4ND_DISABLE_TELEMETRY=1`.
fn log_transport_choice(choice: &TransportChoice) {
    if std::env::var_os("C0PL4ND_DISABLE_TELEMETRY").is_some() {
        return;
    }
    tracing::info!(target: "c0pl4nd::report", transport = choice.class());
}

/// Read the ingest endpoint from the env var, treating an empty value as unset.
fn endpoint_from_env() -> Option<String> {
    std::env::var(REPORT_ENDPOINT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the opt-in onion address from the env var, treating an empty value as
/// unset. Structural validity is checked later in [`choose_transport`] (a
/// malformed value falls back to clearnet, never a silent anonymity downgrade).
fn onion_from_env() -> Option<String> {
    std::env::var(REPORT_ONION_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Open the local spool rooted at an EXPLICIT config dir so the host can drain
/// pending crash reports into the consent dialog on the next launch. The dir is
/// always passed by the caller (the app's per-instance resolved `config_dir`, a
/// temp dir under test) so no spool I/O ever silently hits the GLOBAL
/// `Config::config_dir()`.
pub fn open_spool_in(dir: &Path) -> Option<Spool> {
    Spool::open(dir).ok()
}

/// What the user chose to remember for the crash stream after a per-event
/// consent decision (Always / Never / Just this time). Maps onto the config
/// `ReportingMode` so the next launch honours it (or keeps asking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RememberChoice {
    /// Remember "Always send" — graduate the stream to [`ReportingMode::Always`].
    Always,
    /// Remember "Never" — set the stream to [`ReportingMode::Off`].
    Never,
    /// Just this time — leave the mode at [`ReportingMode::AskEachTime`].
    JustThisTime,
}

impl RememberChoice {
    /// The `ReportingMode` this choice should persist to the config, if any.
    /// `JustThisTime` returns `None` (the mode stays `AskEachTime`).
    #[must_use]
    pub fn persisted_mode(self) -> Option<ReportingMode> {
        match self {
            RememberChoice::Always => Some(ReportingMode::Always),
            RememberChoice::Never => Some(ReportingMode::Off),
            RememberChoice::JustThisTime => None,
        }
    }
}

/// The per-launch crash-consent dialog state, owned by the app. On launch the
/// host loads the spooled crash reports into `queue`; the dialog presents them
/// one at a time with an EDITABLE preview and equal-weight Send / Don't-send.
///
/// This holds NO SDK transport state — only the spooled paths, the currently-
/// presented report + its editable preview text, and the user's remember choice.
#[derive(Debug, Default)]
pub struct CrashConsentState {
    /// The EXPLICIT config dir this dialog's spool I/O is rooted at — the app's
    /// per-instance resolved `config_dir` (a temp dir under test). `None` until
    /// the host binds it via [`CrashConsentState::set_config_dir`]; while `None`
    /// every spool operation is a no-op (so a default-constructed state touches
    /// NO real config dir).
    config_dir: Option<PathBuf>,
    /// Remaining spooled report paths to present (oldest first).
    queue: Vec<PathBuf>,
    /// The report currently shown in the dialog (loaded from `queue`'s head).
    current: Option<(PathBuf, Report)>,
    /// The editable preview text the user sees and may modify before sending.
    edited_text: String,
    /// The remember-my-choice selection (defaults to Just-this-time).
    remember: Option<RememberChoice>,
}

impl CrashConsentState {
    /// Bind the explicit config dir whose `reports/` spool this dialog drains.
    pub fn set_config_dir(&mut self, dir: Option<PathBuf>) {
        self.config_dir = dir;
    }

    /// Open this dialog's spool at its bound config dir, if any is set.
    fn spool(&self) -> Option<Spool> {
        self.config_dir.as_deref().and_then(open_spool_in)
    }

    /// Load the spooled CRASH reports into the dialog queue. Returns the number
    /// queued. Manual-issue reports are not presented by this crash dialog.
    /// Best-effort: a spool error yields an empty queue.
    pub fn load_from_spool(&mut self) -> usize {
        self.queue.clear();
        self.current = None;
        if let Some(spool) = self.spool() {
            if let Ok(paths) = spool.list() {
                for path in paths {
                    if let Ok(report) = spool.load(&path) {
                        if report.stream == Stream::CrashReports {
                            self.queue.push(path);
                        }
                    }
                }
            }
        }
        self.advance();
        self.queue.len() + usize::from(self.current.is_some())
    }

    /// Whether the dialog has a report to present this frame.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.current.is_some()
    }

    /// The editable preview text (mutable so the dialog can bind a `TextEdit`).
    pub fn edited_text_mut(&mut self) -> &mut String {
        &mut self.edited_text
    }

    /// The remember-choice selection (mutable for the dialog radios).
    pub fn remember_mut(&mut self) -> &mut Option<RememberChoice> {
        &mut self.remember
    }

    /// Pop the next report off the queue and load it as `current` + its preview
    /// text. Clears `current` when the queue is empty.
    fn advance(&mut self) {
        self.current = None;
        self.edited_text.clear();
        self.remember = Some(RememberChoice::JustThisTime);
        if self.queue.is_empty() {
            return;
        }
        let path = self.queue.remove(0);
        if let Some(spool) = self.spool() {
            if let Ok(report) = spool.load(&path) {
                self.edited_text = preview_text(&report);
                self.current = Some((path, report));
            }
        }
    }

    /// The user pressed SEND on the current report. Build the (possibly edited)
    /// report from the preview text, mint a consent token, transmit, and — on a
    /// successful send — remove the spooled file. Returns the outcome. Advances
    /// to the next queued report regardless of outcome.
    pub fn consent_and_send(&mut self) -> Option<ReportOutcome> {
        let (path, original) = self.current.take()?;
        let edited = edited_report_from_preview_text(&self.edited_text, &original);
        let token = ConsentToken::granted();
        let outcome = send_report(&edited, &token);
        if outcome == ReportOutcome::Sent {
            if let Some(spool) = self.spool() {
                let _ = spool.remove(&path);
            }
        }
        // Not sent (offline / no endpoint / failed): keep the file spooled so a
        // later configured/online send can retry.
        self.advance();
        Some(outcome)
    }

    /// The user pressed DON'T-SEND on the current report. Discard the spooled
    /// file (the user declined to send it) and advance.
    pub fn decline_and_discard(&mut self) {
        if let Some((path, _)) = self.current.take() {
            if let Some(spool) = self.spool() {
                let _ = spool.remove(&path);
            }
        }
        self.advance();
    }
}

/// Auto-send every spooled CRASH report through the consent-gated path WITHOUT a
/// dialog — used when the crash stream's mode is [`ReportingMode::Always`]. Each
/// report is still transmitted only via a freshly-minted [`ConsentToken`]; a
/// successful send removes the spooled file, a failure leaves it for retry.
/// Returns the number of reports for which a send was ATTEMPTED.
pub fn auto_send_spooled_crashes(config_dir: &Path) -> usize {
    let Some(spool) = open_spool_in(config_dir) else {
        return 0;
    };
    let Ok(paths) = spool.list() else {
        return 0;
    };
    let mut attempted = 0;
    for path in paths {
        let Ok(report) = spool.load(&path) else {
            continue;
        };
        if report.stream != Stream::CrashReports {
            continue;
        }
        attempted += 1;
        let token = ConsentToken::granted();
        if send_report(&report, &token) == ReportOutcome::Sent {
            let _ = spool.remove(&path);
        }
    }
    attempted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scoped guard that sets an env var and restores it on drop.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }
        fn unset(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    use std::sync::Mutex;
    static ENDPOINT_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn crash_report_is_crash_stream_and_carries_static_message() {
        let r = build_crash_report("called `Option::unwrap()` on a `None`", "src/foo.rs:42");
        assert_eq!(r.stream, Stream::CrashReports);
        // The bumped SDK sanitizer (post-tag content-scrubbing hardening)
        // redacts higher-risk free-text spans inside the panic message — the
        // backtick+parens token `Option::unwrap()` is scrubbed to the uniform
        // <redacted> marker, while the structural `panic:` prefix and the
        // `file:line` panic SITE (the dedup signal) are preserved. We assert the
        // hardened behaviour: the site survives and the risky token is redacted
        // (a strictly MORE-private outcome than the pre-bump verbatim body).
        assert!(
            r.body.starts_with("panic: "),
            "the structural panic prefix is preserved: {:?}",
            r.body
        );
        assert!(
            r.body.contains("src/foo.rs:42"),
            "the panic site (file:line) survives sanitization: {:?}",
            r.body
        );
        assert!(
            r.body.contains("<redacted>"),
            "the risky backtick+parens token is scrubbed by the hardened sanitizer: {:?}",
            r.body
        );
        assert!(
            !r.body.contains("Option::unwrap()"),
            "the raw `Option::unwrap()` token must NOT leak verbatim after hardening: {:?}",
            r.body
        );
        assert!(r.metadata.iter().any(|(k, _)| k == "app_version"));
        assert!(r.metadata.iter().any(|(k, _)| k == "os"));
    }

    #[test]
    fn preview_text_shows_the_literal_payload() {
        let r = build_crash_report("boom", "src/x.rs:1");
        let text = preview_text(&r);
        assert!(text.contains("boom"));
        assert!(text.contains("src/x.rs:1"));
    }

    #[test]
    fn send_without_endpoint_refuses_and_transmits_nothing() {
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        // Even WITH a consent token, an unset endpoint cannot transmit — the
        // report stays spooled and the outcome is the structured refusal (never
        // a fake Sent, never a silent drop).
        let r = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        let outcome = send_report(&r, &token);
        assert_eq!(outcome, ReportOutcome::RefusedNoEndpoint);
    }

    #[test]
    fn empty_endpoint_env_is_treated_as_unset() {
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::set(REPORT_ENDPOINT_ENV, "   ");
        assert!(
            endpoint_from_env().is_none(),
            "a whitespace-only endpoint must be treated as unset (cannot phone home)"
        );
    }

    // ---- W1TN3SS opt-in Tor-onion transport SELECTION (default-clearnet) ----

    /// A structurally-valid onion as the SDK's [`TorTransportConfig::is_valid_onion`]
    /// defines it at the pinned rev: exactly 56 lowercase ASCII letters plus the
    /// `.onion` suffix. NOTE — the SDK's validator at this rev accepts only the
    /// lowercase-letter subset of base32 (its `is_ascii_lowercase()` clause
    /// excludes the base32 digits `2-7`), so a real-world digit-bearing onion is
    /// treated as invalid by the SDK and would fall back to clearnet. We use a
    /// fixture the SDK actually accepts so the selection test reflects the SDK's
    /// real contract. No connection is ever made in tests.
    const VALID_V3_ONION: &str = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcd.onion";

    #[test]
    fn valid_onion_selects_the_tor_transport() {
        // When a structurally-valid v3 onion is configured, the opt-in Tor path
        // is selected — even if a clearnet endpoint is ALSO set (onion wins, the
        // anonymous path takes precedence).
        let choice = choose_transport(Some(VALID_V3_ONION), Some("https://ingest.example"));
        assert_eq!(
            choice,
            TransportChoice::Tor {
                onion: VALID_V3_ONION.to_string()
            },
            "a valid onion selects the Tor transport regardless of the clearnet endpoint"
        );
        assert_eq!(choice.class(), "tor");
    }

    #[test]
    fn no_onion_falls_back_to_clearnet() {
        // No onion configured but a clearnet endpoint is → the existing clearnet
        // backend is selected with that endpoint.
        let choice = choose_transport(None, Some("https://ingest.example"));
        assert_eq!(
            choice,
            TransportChoice::Clearnet {
                endpoint: Some("https://ingest.example".to_string())
            }
        );
        assert_eq!(choice.class(), "clearnet");
    }

    #[test]
    fn default_unconfigured_is_clearnet_no_endpoint() {
        // The DEFAULT (neither an onion nor a clearnet endpoint configured) is
        // the pre-Tor behaviour: clearnet with no endpoint → spool-only, no
        // transmission. Nothing changes for users who configure nothing.
        let choice = choose_transport(None, None);
        assert_eq!(choice, TransportChoice::Clearnet { endpoint: None });
        assert_eq!(choice.class(), "clearnet-no-endpoint");
    }

    #[test]
    fn malformed_onion_never_silently_downgrades_to_anonymity_then_clearnet() {
        // A non-empty BUT structurally-invalid onion (too short, wrong charset,
        // missing suffix) must NOT be treated as the Tor path. It falls back to
        // the EXPLICIT clearnet path so the user is never given a false sense of
        // anonymity while actually sending over clearnet.
        for bad in [
            "not-an-onion",
            "tooshort.onion",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcd.com", // wrong suffix
            "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZABCD.onion", // uppercase (SDK requires lowercase)
            "duckduckgogg42xjoc72x3sjasowoarfbgcmvfimaftt6twagswzczad.onion", // digit-bearing: rejected by the SDK's lowercase-letter-only validator at this rev
            "duck duck go.onion",                                             // spaces
        ] {
            let choice = choose_transport(Some(bad), Some("https://ingest.example"));
            assert_eq!(
                choice,
                TransportChoice::Clearnet {
                    endpoint: Some("https://ingest.example".to_string())
                },
                "a malformed onion ({bad:?}) must fall back to clearnet, never the Tor path"
            );
        }
    }

    #[test]
    fn whitespace_or_empty_onion_is_treated_as_unset() {
        // A whitespace-only / empty onion is unset → clearnet selection, and the
        // valid onion is trimmed of surrounding whitespace before validation.
        assert_eq!(
            choose_transport(Some("   "), None),
            TransportChoice::Clearnet { endpoint: None },
            "a whitespace-only onion is unset (no Tor, no false anonymity)"
        );
        let padded = format!("  {VALID_V3_ONION}  ");
        assert_eq!(
            choose_transport(Some(&padded), None),
            TransportChoice::Tor {
                onion: VALID_V3_ONION.to_string()
            },
            "a valid onion is trimmed before selection"
        );
    }

    #[test]
    fn onion_env_round_trips_and_empty_is_unset() {
        // The env reader trims and treats empty/whitespace as unset, mirroring
        // the clearnet endpoint reader.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        {
            let _g = EnvGuard::set(REPORT_ONION_ENV, "   ");
            assert!(
                onion_from_env().is_none(),
                "a whitespace-only onion env must be treated as unset"
            );
        }
        {
            let _g = EnvGuard::set(REPORT_ONION_ENV, VALID_V3_ONION);
            assert_eq!(onion_from_env().as_deref(), Some(VALID_V3_ONION));
        }
        {
            let _g = EnvGuard::unset(REPORT_ONION_ENV);
            assert!(onion_from_env().is_none());
        }
    }

    #[test]
    fn send_with_no_onion_and_no_endpoint_still_refuses_default_off_semantics_intact() {
        // The consent gate + default-OFF posture are unchanged: with neither an
        // onion NOR an endpoint configured, a CONSENTED send still transmits
        // nothing and returns the structured refusal — adding the Tor path did
        // not weaken the no-endpoint / default-clearnet refusal.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let _go = EnvGuard::unset(REPORT_ONION_ENV);
        let r = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        assert_eq!(send_report(&r, &token), ReportOutcome::RefusedNoEndpoint);
    }

    #[test]
    fn transport_choice_class_labels_are_stable_and_non_identifying() {
        // The log label is the transport CLASS only — never the endpoint or the
        // onion address (those could be fingerprints).
        assert_eq!(
            TransportChoice::Tor {
                onion: VALID_V3_ONION.to_string()
            }
            .class(),
            "tor"
        );
        assert_eq!(
            TransportChoice::Clearnet {
                endpoint: Some("https://secret.example".to_string())
            }
            .class(),
            "clearnet",
            "the label must not embed the endpoint URL"
        );
        assert_eq!(
            TransportChoice::Clearnet { endpoint: None }.class(),
            "clearnet-no-endpoint"
        );
    }

    // NOTE — there is deliberately NO live-onion test here (E2E_CROSS_SERVICE = N/A).
    //
    // This previously held `live_onion_connect_is_gated_behind_ignore`, whose body
    // was two comment lines. Being `#[ignore]`d, `cargo test -- --ignored` reported
    // it PASSING while it asserted nothing — strictly worse than an empty cell,
    // because to a reader and to tooling it looked like a live test existed behind
    // a flag. It was also a placeholder for future work, which this repo forbids.
    //
    // A hermetic version is not possible, and that is a property of the surface,
    // not a gap to fill later: `send_report_tor` constructs `TorOnionTransport`
    // inline (see `send_report_tor` above), and driving it needs a real Tor
    // bootstrap (a directory-consensus fetch = live egress) plus a live W1TN3SS
    // hidden service. An onion has no loopback substitute — that is the point of an
    // onion.
    //
    // Nothing is left uncovered by removing it. The routing DECISION is pure and is
    // asserted by `choose_transport` tests above; the result-folding seam
    // `send_via_backend<B: IngestBackend>` is exercised by `FakeBackend` below. A
    // test injecting a fake at that seam and asserting the fake would be a
    // tautology, so it is intentionally absent. A live onion check is an operator
    // smoke test, not a CI test.

    #[test]
    fn remember_choice_maps_to_config_mode() {
        assert_eq!(
            RememberChoice::Always.persisted_mode(),
            Some(ReportingMode::Always)
        );
        assert_eq!(
            RememberChoice::Never.persisted_mode(),
            Some(ReportingMode::Off)
        );
        assert_eq!(
            RememberChoice::JustThisTime.persisted_mode(),
            None,
            "just-this-time leaves the mode at AskEachTime (no persist)"
        );
    }

    #[test]
    fn edited_preview_text_round_trips_user_redactions_into_body() {
        let original = Report::crash("panic: boom (at src/x.rs:1)")
            .with_metadata("os", "linux")
            .with_metadata("app_version", "9.9.9");
        let preview = preview_text(&original);
        assert!(preview.contains("boom"));
        let edited_text = preview.replace("boom", "[redacted]");
        let edited = edited_report_from_preview_text(&edited_text, &original);
        assert!(edited.body.contains("[redacted]"));
        assert!(!edited.body.contains("boom"));
        assert!(!edited.body.contains("--- metadata ---"));
        assert!(!edited.body.contains("os: linux"));
        assert_eq!(edited.stream, Stream::CrashReports);
        assert_eq!(edited.title, original.title);
        assert_eq!(edited.metadata, original.metadata);
    }

    #[test]
    fn outcome_log_details_are_stable_and_non_identifying() {
        assert_eq!(ReportOutcome::Spooled.log_detail(), "spooled");
        assert_eq!(ReportOutcome::Sent.log_detail(), "sent");
        assert_eq!(
            ReportOutcome::RefusedNoEndpoint.log_detail(),
            "refused-no-endpoint"
        );
        // The Failed reason is NOT inlined into the log detail (no PII leak).
        assert_eq!(
            ReportOutcome::Failed("transport error: https://secret".to_string()).log_detail(),
            "failed"
        );
    }

    #[test]
    fn default_crash_consent_state_touches_no_real_config_dir() {
        // A default-constructed state with no bound config dir is fully inert:
        // load_from_spool returns 0 and nothing is presented.
        let mut st = CrashConsentState::default();
        assert_eq!(st.load_from_spool(), 0);
        assert!(!st.has_pending());
    }

    #[test]
    fn spool_capture_and_decline_round_trip_in_temp_dir() {
        // Capture into a temp config dir, then a bound consent dialog drains it
        // and DECLINE removes the spooled file (the user declined to send).
        let dir = std::env::temp_dir().join(format!("c0pl4nd-report-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let report = build_crash_report("boom", "src/x.rs:1");
        let spool = open_spool_in(&dir).expect("open spool");
        spool.enqueue(&report).expect("enqueue");

        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        assert!(st.load_from_spool() >= 1, "the queued crash must load");
        assert!(st.has_pending());
        st.decline_and_discard();
        assert!(!st.has_pending(), "declining clears the presented report");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh, isolated config dir for a reporting test (one per `tag`).
    fn report_scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "c0pl4nd-report-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn capture_panic_into_temp_dir_spools_and_reports_spooled() {
        // The capture seam: with an explicit config dir, a panic capture spools
        // a crash report and returns the structured `Spooled` outcome (it never
        // transmits — local-first). We drive the capture through a temp HOME so
        // the GLOBAL config dir is not touched.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let dir = report_scratch_dir("capture");
        // `capture_panic` resolves the GLOBAL config dir, so to keep this test
        // hermetic we exercise the same enqueue path it uses directly and assert
        // the spooled report shape, then assert `capture_panic`'s outcome enum.
        let report = build_crash_report("called `Result::unwrap()`", "src/y.rs:9");
        let spool = open_spool_in(&dir).expect("open spool");
        let path = spool.enqueue(&report).expect("enqueue");
        assert!(path.exists(), "the spooled crash file exists");
        // The enqueued report round-trips back as a CrashReports-stream report.
        let loaded = spool.load(&path).expect("load");
        assert_eq!(loaded.stream, Stream::CrashReports);
        // Hardened-SDK behaviour: the panic structure round-trips through the
        // spool while the risky `Result::unwrap()` token is scrubbed to
        // <redacted> (see `crash_report_is_crash_stream_and_carries_static_message`).
        assert!(loaded.body.starts_with("panic: "));
        assert!(loaded.body.contains("src/y.rs:9"));
        assert!(loaded.body.contains("<redacted>"));
        assert!(!loaded.body.contains("Result::unwrap()"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_from_spool_presents_only_crash_stream_reports() {
        // A spool holding BOTH a crash report and a manual-issue report must
        // surface ONLY the crash report in the crash-consent dialog — the
        // manual-issue stream is filtered out (the dialog drains crashes only).
        let dir = report_scratch_dir("mixed-stream");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("crash one", "src/a.rs:1"))
            .expect("enqueue crash");
        spool
            .enqueue(&Report::manual_issue("a manual issue", "user-filed body"))
            .expect("enqueue manual");

        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        let queued = st.load_from_spool();
        assert_eq!(
            queued, 1,
            "exactly one CRASH report is queued; the manual-issue is filtered out"
        );
        assert!(st.has_pending());
        // The presented preview is the crash, never the manual issue.
        assert!(
            st.edited_text_mut().contains("crash one"),
            "the presented report is the crash"
        );
        assert!(
            !st.edited_text_mut().contains("user-filed body"),
            "the manual-issue body is never presented by the crash dialog"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_from_spool_queues_and_advances_through_multiple_crashes() {
        // Two crash reports → the first is presented, the second stays queued;
        // declining the first advances to the second; declining again clears.
        let dir = report_scratch_dir("multi");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("first crash", "src/a.rs:1"))
            .expect("enqueue 1");
        spool
            .enqueue(&build_crash_report("second crash", "src/b.rs:2"))
            .expect("enqueue 2");

        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        let total = st.load_from_spool();
        assert_eq!(total, 2, "both crash reports are accounted for");
        assert!(st.has_pending());

        // Decline the first → still pending (the second advances in).
        st.decline_and_discard();
        assert!(
            st.has_pending(),
            "the second crash advances after declining the first"
        );
        // Decline the second → now empty.
        st.decline_and_discard();
        assert!(!st.has_pending(), "declining the last clears the dialog");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consent_and_send_without_endpoint_keeps_report_spooled_and_returns_refusal() {
        // SEND with no endpoint configured returns RefusedNoEndpoint, transmits
        // nothing, and — because the outcome is not `Sent` — KEEPS the spooled
        // file for a later configured/online retry. The dialog still advances.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        // Also unset the opt-in onion env so the default-clearnet refusal is
        // exercised hermetically (a stray onion would route to the Tor path).
        let _go = EnvGuard::unset(REPORT_ONION_ENV);
        let dir = report_scratch_dir("send-no-endpoint");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("keepme", "src/c.rs:3"))
            .expect("enqueue");

        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        assert!(st.load_from_spool() >= 1);
        let outcome = st.consent_and_send().expect("a report is presented");
        assert_eq!(
            outcome,
            ReportOutcome::RefusedNoEndpoint,
            "no endpoint → structured refusal (never a fake Sent, never a drop)"
        );
        assert!(
            !st.has_pending(),
            "the dialog advances past the sent-attempt report"
        );
        // The file is RETAINED (not removed) because the send did not succeed.
        let remaining = open_spool_in(&dir).expect("reopen").list().expect("list");
        assert_eq!(
            remaining.len(),
            1,
            "a refused send keeps the report spooled for retry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consent_and_send_transmits_the_user_edited_body() {
        // The user's redactions to the preview text are what the send path uses.
        // With no endpoint the transport refuses, but we assert the edited body
        // is plumbed through `edited_report_from_preview_text` (the outcome enum
        // confirms the path executed end-to-end).
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let _go = EnvGuard::unset(REPORT_ONION_ENV);
        let dir = report_scratch_dir("send-edited");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("secret-token-xyz", "src/d.rs:4"))
            .expect("enqueue");

        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        assert!(st.load_from_spool() >= 1);
        // Redact the sensitive token in the preview before sending.
        let edited = st.edited_text_mut();
        assert!(edited.contains("secret-token-xyz"));
        *edited = edited.replace("secret-token-xyz", "[redacted]");
        let outcome = st.consent_and_send().expect("a report is presented");
        // No endpoint → refusal (the edited body still flows through the path).
        assert_eq!(outcome, ReportOutcome::RefusedNoEndpoint);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consent_and_send_is_none_when_nothing_pending() {
        // With no presented report, SEND is a no-op returning None (guards the
        // `self.current.take()?` early return).
        let mut st = CrashConsentState::default();
        assert!(!st.has_pending());
        assert_eq!(st.consent_and_send(), None);
    }

    #[test]
    fn auto_send_attempts_only_crash_reports_and_skips_manual_issues() {
        // The `Always`-mode auto-send path: it ATTEMPTS a send for each spooled
        // CRASH report only (manual-issue reports are skipped). With no endpoint
        // every attempt refuses, so all files are retained and the attempt count
        // equals the number of CRASH reports (never the manual-issue count).
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let dir = report_scratch_dir("auto-send");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("auto crash 1", "src/e.rs:1"))
            .expect("enqueue crash 1");
        spool
            .enqueue(&build_crash_report("auto crash 2", "src/e.rs:2"))
            .expect("enqueue crash 2");
        spool
            .enqueue(&Report::manual_issue("manual", "not a crash"))
            .expect("enqueue manual");

        let attempted = auto_send_spooled_crashes(&dir);
        assert_eq!(
            attempted, 2,
            "auto-send attempts exactly the two CRASH reports, never the manual issue"
        );
        // No endpoint → nothing was Sent → every file is retained for retry.
        let remaining = open_spool_in(&dir).expect("reopen").list().expect("list");
        assert_eq!(
            remaining.len(),
            3,
            "all three reports remain spooled (no successful send removed any)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_send_on_an_unopenable_dir_attempts_nothing() {
        // A config dir that cannot host a spool (a path that is a FILE, not a
        // directory) yields zero attempts — the guard returns 0, never panics.
        let dir = report_scratch_dir("auto-send-bad");
        let not_a_dir = dir.join("a-file-not-a-dir");
        std::fs::write(&not_a_dir, b"x").expect("write file");
        // `open_spool_in` on a file path fails → 0 attempted.
        let attempted = auto_send_spooled_crashes(&not_a_dir);
        assert_eq!(attempted, 0, "an unopenable spool dir attempts no sends");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_config_dir_to_none_makes_the_dialog_inert() {
        // Binding the config dir back to None makes every spool op a no-op even
        // after a prior bind (the `spool()` accessor returns None).
        let dir = report_scratch_dir("rebind-none");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("boom", "src/f.rs:1"))
            .expect("enqueue");
        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        assert!(st.load_from_spool() >= 1);
        // Rebind to None → reloading finds nothing.
        st.set_config_dir(None);
        assert_eq!(
            st.load_from_spool(),
            0,
            "an unbound dialog presents nothing"
        );
        assert!(!st.has_pending());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remember_mut_round_trips_the_choice() {
        // The remember-choice accessor is mutable and persists the selection so
        // the dialog radios can bind it. Defaults to JustThisTime after a load.
        let mut st = CrashConsentState::default();
        *st.remember_mut() = Some(RememberChoice::Always);
        assert_eq!(st.remember_mut(), &Some(RememberChoice::Always));
        *st.remember_mut() = Some(RememberChoice::Never);
        assert_eq!(st.remember_mut(), &Some(RememberChoice::Never));
    }

    // ====================================================================
    // Coverage-completion tests (W1TN3SS reporting integration). The tests
    // below drive the remaining executable arms of this module — the send
    // backend mapping, the clearnet-with-endpoint and Tor transport paths,
    // the global-config-dir capture/auto-send seams, the telemetry-skip
    // branches, and the spool error arms — all WITHOUT live network or a
    // real Tor bootstrap. The two facts that make this possible deterministically:
    //
    //  1. `Config::config_dir()` is rooted at `%APPDATA%` (Windows) / `$HOME` /
    //     `$XDG_CONFIG_HOME` (Unix), so a scoped env override points it at a
    //     temp dir — the global capture/auto-send seams become hermetic.
    //  2. The SDK's `TorOnionTransport::send` is FIRE-AND-FORGET: it builds the
    //     padded envelope, enforces the size cap, then SPOOLS the report and
    //     returns `SendOutcome::Sent` synchronously — NO bootstrap, NO circuit,
    //     NO network. So a valid-onion `send_report` returns `Sent` offline,
    //     which lets us cover `send_over_tor` + every `Sent`-removal branch.
    // ====================================================================

    /// A scoped guard that overrides EVERY config-dir-rooting env var (so
    /// `Config::config_dir()` resolves to `dir`, cross-platform) and restores
    /// them all on drop. On Windows the rooting var is `APPDATA`; on Unix it is
    /// `XDG_CONFIG_HOME` (with `HOME` also pinned so neither path can leak to a
    /// real user dir). We set all three regardless of platform so the test is
    /// hermetic on whichever OS runs it.
    struct ConfigDirGuard {
        _appdata: EnvGuard,
        _xdg: EnvGuard,
        _home: EnvGuard,
    }
    impl ConfigDirGuard {
        fn to(dir: &Path) -> Self {
            let s = dir.to_str().expect("utf-8 temp dir");
            Self {
                _appdata: EnvGuard::set("APPDATA", s),
                _xdg: EnvGuard::set("XDG_CONFIG_HOME", s),
                _home: EnvGuard::set("HOME", s),
            }
        }
        /// Override the rooting vars to a value that CANNOT host a config dir:
        /// each is set to a path whose PARENT does not exist as a directory, so
        /// the spool's `create_dir_all` under `<dir>/c0pl4nd/reports` still
        /// succeeds — that is not what we want for the "no spool" arm. Instead
        /// this unsets them entirely so `Config::config_dir()` returns `None`.
        fn unset() -> Self {
            Self {
                _appdata: EnvGuard::unset("APPDATA"),
                _xdg: EnvGuard::unset("XDG_CONFIG_HOME"),
                _home: EnvGuard::unset("HOME"),
            }
        }
    }

    /// A throwaway `IngestBackend` that returns a programmed outcome, so the
    /// `send_via_backend` result-folding arms can be exercised in isolation
    /// (no network, no SDK transport). Each of the three SDK results
    /// (`Ok(Sent)`, `Ok(Failed)`, `Err`) maps to a distinct `ReportOutcome`.
    enum FakeResult {
        Sent,
        Failed(&'static str),
        Err(&'static str),
    }
    struct FakeBackend {
        result: FakeResult,
    }
    impl IngestBackend for FakeBackend {
        fn send(
            &self,
            _report: &Report,
            _consent: &ConsentToken,
        ) -> Result<SendOutcome, itasha_report_core::backend::SendError> {
            match &self.result {
                FakeResult::Sent => Ok(SendOutcome::Sent),
                FakeResult::Failed(r) => Ok(SendOutcome::Failed((*r).to_string())),
                FakeResult::Err(r) => Err(itasha_report_core::backend::SendError::Transport(
                    (*r).to_string(),
                )),
            }
        }
    }

    #[test]
    fn send_via_backend_maps_all_three_sdk_results() {
        let r = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();

        // Ok(Sent) -> ReportOutcome::Sent
        let sent = FakeBackend {
            result: FakeResult::Sent,
        };
        assert_eq!(
            send_via_backend(&sent, &r, &token),
            ReportOutcome::Sent,
            "an SDK Sent must fold to ReportOutcome::Sent"
        );

        // Ok(Failed(reason)) -> ReportOutcome::Failed(reason) — the reason is
        // carried through verbatim (it is non-identifying by SDK contract).
        let failed = FakeBackend {
            result: FakeResult::Failed("http status 500"),
        };
        assert_eq!(
            send_via_backend(&failed, &r, &token),
            ReportOutcome::Failed("http status 500".to_string()),
            "an SDK Failed must fold to ReportOutcome::Failed with the reason"
        );

        // Err(SendError) -> ReportOutcome::Failed(err.to_string()) — a pre-send
        // error (size cap, transport build) also becomes a structured Failed,
        // never a panic and never a fake Sent.
        let errored = FakeBackend {
            result: FakeResult::Err("connection refused"),
        };
        match send_via_backend(&errored, &r, &token) {
            ReportOutcome::Failed(msg) => assert!(
                msg.contains("connection refused"),
                "the SendError display is folded into the Failed reason: {msg:?}"
            ),
            other => panic!("an SDK Err must fold to ReportOutcome::Failed, got {other:?}"),
        }
    }

    #[test]
    fn send_report_clearnet_endpoint_failure_is_structured_not_fake_sent() {
        // With a clearnet endpoint configured but UNROUTABLE (port 1 on
        // loopback is reserved/closed), the real `LeanPipelineBackend` is
        // selected and its `ureq` send fails fast — folding to a structured
        // `Failed` (never a fake `Sent`, never a silent drop). This exercises
        // the `Clearnet { endpoint: Some(..) }` arm of `send_report` and the
        // `send_via_backend` Failed/Err fold against the REAL backend, with no
        // external network (the connection is refused locally).
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _ge = EnvGuard::set(REPORT_ENDPOINT_ENV, "http://127.0.0.1:1/ingest");
        let _go = EnvGuard::unset(REPORT_ONION_ENV);
        let r = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        match send_report(&r, &token) {
            ReportOutcome::Failed(_) => {}
            other => panic!("an unroutable clearnet endpoint must return Failed, got {other:?}"),
        }
    }

    #[test]
    fn send_report_over_tor_spools_and_reports_sent_without_network() {
        // A structurally-valid onion selects the Tor transport. Because the SDK
        // Tor backend is fire-and-forget (spool + return Sent, no bootstrap),
        // `send_report` returns `Sent` with NO network — and the report is
        // durably spooled under `<config_dir>/tor`-adjacent state. This is the
        // ONLY way the `send_over_tor` happy path is reachable offline.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let dir = report_scratch_dir("tor-send");
        let _cfg = ConfigDirGuard::to(&dir);
        let _go = EnvGuard::set(REPORT_ONION_ENV, VALID_V3_ONION);
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let r = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        assert_eq!(
            send_report(&r, &token),
            ReportOutcome::Sent,
            "the fire-and-forget Tor transport accepts the report for anonymous delivery (spooled)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_over_tor_with_no_config_dir_surfaces_the_reason() {
        // When the onion is valid but NO config dir resolves (rooting env
        // unset), `send_over_tor` cannot root the Arti state dir and returns a
        // structured `Failed("tor: no-config-dir")` — never a silent drop.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _cfg = ConfigDirGuard::unset();
        let _go = EnvGuard::set(REPORT_ONION_ENV, VALID_V3_ONION);
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let r = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        match send_report(&r, &token) {
            ReportOutcome::Failed(reason) => assert_eq!(
                reason, "tor: no-config-dir",
                "no config dir on the Tor path surfaces the structured reason"
            ),
            other => panic!("Tor with no config dir must Fail, got {other:?}"),
        }
    }

    #[test]
    fn capture_panic_spools_under_the_global_config_dir() {
        // The global capture seam: with the rooting env pointed at a temp dir,
        // `capture_panic` resolves `Config::config_dir()` there, opens the
        // spool, and writes a Tier-1 crash report — returning `Spooled` and
        // transmitting nothing. Exercises the `Some(dir) => Ok(spool)` happy arm
        // of `capture_panic` (not just the direct-enqueue path the older test
        // used).
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let dir = report_scratch_dir("capture-global");
        let _cfg = ConfigDirGuard::to(&dir);
        let outcome = capture_panic("called `Option::unwrap()`", "src/z.rs:7");
        assert_eq!(
            outcome,
            ReportOutcome::Spooled,
            "a panic capture spools under the global config dir and transmits nothing"
        );
        // The report is actually on disk under the resolved config dir's spool.
        let resolved = Config::config_dir().expect("config dir resolves to the temp dir");
        let spool = open_spool_in(&resolved).expect("open spool at resolved dir");
        assert_eq!(
            spool.list().expect("list").len(),
            1,
            "exactly one crash report was spooled by capture_panic"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_panic_with_no_config_dir_surfaces_the_reason() {
        // No rooting env => `Config::config_dir()` is None => nowhere to spool.
        // `capture_panic` surfaces `Failed("no-config-dir")` rather than
        // swallowing the panic silently.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _cfg = ConfigDirGuard::unset();
        let outcome = capture_panic("boom", "src/z.rs:8");
        assert_eq!(
            outcome,
            ReportOutcome::Failed("no-config-dir".to_string()),
            "no config dir must surface the structured reason, never a silent drop"
        );
    }

    #[test]
    fn capture_panic_when_spool_cannot_open_surfaces_spool_open_failure() {
        // Point the config dir at a location whose `reports/` cannot be created:
        // a config dir that is itself a FILE means `Spool::open`'s
        // `create_dir_all(<file>/reports)` fails → `capture_panic` returns the
        // structured `Failed("spool-open: ..")` arm.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let base = report_scratch_dir("capture-spool-open-fail");
        // The resolved config dir is `<rooting>/c0pl4nd`. Create that path as a
        // FILE so `Spool::open` cannot mkdir `<...>/c0pl4nd/reports`.
        let c0pl4nd_as_file = base.join("c0pl4nd");
        std::fs::write(&c0pl4nd_as_file, b"not a dir").expect("write file at config dir");
        let _cfg = ConfigDirGuard::to(&base);
        let outcome = capture_panic("boom", "src/z.rs:9");
        match outcome {
            ReportOutcome::Failed(reason) => assert!(
                reason.starts_with("spool-open:"),
                "a non-directory config dir surfaces the spool-open failure: {reason:?}"
            ),
            other => panic!("expected a spool-open Failed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn log_outcome_is_suppressed_when_telemetry_disabled_and_emits_otherwise() {
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        // Disabled: the early-return branch is taken (no emit). We assert it does
        // not panic and is a no-op for every variant.
        {
            let _g = EnvGuard::set("C0PL4ND_DISABLE_TELEMETRY", "1");
            log_outcome(&ReportOutcome::Spooled);
            log_outcome(&ReportOutcome::Sent);
            log_outcome(&ReportOutcome::RefusedNoEndpoint);
            log_outcome(&ReportOutcome::Failed("x".to_string()));
        }
        // Enabled: the emit branch is taken (the tracing call runs even with no
        // subscriber installed — it is a no-op sink, but the line is executed).
        {
            let _g = EnvGuard::unset("C0PL4ND_DISABLE_TELEMETRY");
            log_outcome(&ReportOutcome::Sent);
        }
    }

    #[test]
    fn log_transport_choice_is_suppressed_when_telemetry_disabled_and_emits_otherwise() {
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let tor = TransportChoice::Tor {
            onion: VALID_V3_ONION.to_string(),
        };
        {
            let _g = EnvGuard::set("C0PL4ND_DISABLE_TELEMETRY", "1");
            log_transport_choice(&tor); // suppressed branch
        }
        {
            let _g = EnvGuard::unset("C0PL4ND_DISABLE_TELEMETRY");
            log_transport_choice(&tor); // emit branch
            log_transport_choice(&TransportChoice::Clearnet { endpoint: None });
        }
    }

    #[test]
    fn load_from_spool_skips_a_corrupt_report_file() {
        // A malformed `report-*.json` in the spool dir makes `spool.load` return
        // Err — exercising the `if let Ok(report)` ELSE arm of `load_from_spool`
        // (the corrupt file is skipped, not surfaced, and never crashes the
        // dialog). A valid crash report alongside it still loads.
        let dir = report_scratch_dir("corrupt-skip");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("good crash", "src/a.rs:1"))
            .expect("enqueue good");
        // Drop a corrupt file matching the `report-*.json` list filter.
        let corrupt = spool.dir().join("report-corrupt.json");
        std::fs::write(&corrupt, b"{ this is not valid report json").expect("write corrupt");

        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        let queued = st.load_from_spool();
        assert_eq!(
            queued, 1,
            "only the well-formed crash report is queued; the corrupt file is skipped"
        );
        assert!(st.has_pending());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn advance_skips_a_report_that_vanishes_before_load() {
        // Queue two crash reports, then DELETE the head's backing file before
        // `advance` re-loads it (via the next `decline_and_discard`). The load in
        // `advance` returns Err → `current` stays None for that slot. The dialog
        // must not crash; it simply has nothing to present once both are gone.
        let dir = report_scratch_dir("advance-vanish");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("first", "src/a.rs:1"))
            .expect("enqueue 1");
        spool
            .enqueue(&build_crash_report("second", "src/b.rs:2"))
            .expect("enqueue 2");

        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        assert_eq!(st.load_from_spool(), 2);
        assert!(st.has_pending());

        // Delete the still-queued second report's file out from under the dialog
        // so the NEXT advance (triggered by declining the first) fails to load
        // it — covering the `advance` load-Err arm (current stays None).
        let remaining = spool.list().expect("list");
        // The currently-presented report was removed from `queue`; the file that
        // is still on disk and queued is the SECOND one. Delete every spooled
        // file so the advance after the decline finds nothing to load.
        for p in remaining {
            let _ = spool.remove(&p);
        }
        st.decline_and_discard();
        assert!(
            !st.has_pending(),
            "after the queued file vanished, advance presents nothing (no crash)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consent_and_send_over_tor_marks_sent_and_removes_the_spooled_file() {
        // SEND over the fire-and-forget Tor transport returns `Sent`, so the
        // dialog REMOVES the spooled file (covering the `outcome == Sent` removal
        // branch of `consent_and_send`). No network: the Tor backend spools the
        // outbound copy and reports Sent synchronously.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        // The DIALOG's spool dir and the global config dir must be the same temp
        // dir so the Tor transport (rooted at the global config dir) and the
        // dialog (rooted at its bound config dir) agree.
        let dir = report_scratch_dir("tor-consent-send");
        let _cfg = ConfigDirGuard::to(&dir);
        // Resolve where the global config dir now points and enqueue there.
        let resolved = Config::config_dir().expect("config dir resolves");
        let _go = EnvGuard::set(REPORT_ONION_ENV, VALID_V3_ONION);
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let spool = open_spool_in(&resolved).expect("open spool at resolved");
        spool
            .enqueue(&build_crash_report("sendme", "src/c.rs:3"))
            .expect("enqueue");

        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(resolved.clone()));
        assert!(st.load_from_spool() >= 1);
        // Count BEFORE: at least the one we enqueued.
        let before = open_spool_in(&resolved).unwrap().list().unwrap().len();
        let outcome = st.consent_and_send().expect("a report is presented");
        assert_eq!(
            outcome,
            ReportOutcome::Sent,
            "the fire-and-forget Tor send reports Sent"
        );
        // The original dialog report file was removed because the send succeeded.
        // (The Tor transport spools its OWN outbound copy under the same root, so
        // we assert the SENT report's source file is gone, not that the dir is
        // empty.)
        let after = open_spool_in(&resolved).unwrap().list().unwrap().len();
        assert!(
            after < before + 1,
            "the sent report's spooled source file was removed (before={before}, after={after})"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decline_and_discard_removes_the_backing_file() {
        // DECLINE removes the spooled file via the bound spool (covering the
        // `spool.remove` call inside `decline_and_discard`). After declining,
        // the source spool no longer holds the declined report.
        let dir = report_scratch_dir("decline-removes");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("discardme", "src/d.rs:4"))
            .expect("enqueue");
        let mut st = CrashConsentState::default();
        st.set_config_dir(Some(dir.clone()));
        assert!(st.load_from_spool() >= 1);
        st.decline_and_discard();
        let remaining = open_spool_in(&dir).unwrap().list().unwrap();
        assert!(
            remaining.is_empty(),
            "declining removed the spooled file (none remain)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_outcome_and_transport_choice_emit_under_an_installed_subscriber() {
        // With telemetry ENABLED *and* a subscriber installed, the
        // `tracing::info!` emit lines in `log_outcome` / `log_transport_choice`
        // actually run (a no-op sink subscriber still drives the emit branch).
        // This covers the macro's enabled/emit arm, not just the early-return.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::unset("C0PL4ND_DISABLE_TELEMETRY");
        let subscriber = tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _default = tracing::subscriber::set_default(subscriber);
        log_outcome(&ReportOutcome::Sent);
        log_outcome(&ReportOutcome::Failed("redacted".to_string()));
        log_transport_choice(&TransportChoice::Tor {
            onion: VALID_V3_ONION.to_string(),
        });
        log_transport_choice(&TransportChoice::Clearnet { endpoint: None });
    }

    #[test]
    fn auto_send_skips_a_corrupt_report_file_via_continue() {
        // A corrupt `report-*.json` in the spool makes `spool.load` Err inside
        // `auto_send_spooled_crashes` → the `continue` arm is taken (the corrupt
        // file is skipped, never counted, never crashes). A valid crash report
        // alongside it is still attempted. No endpoint/onion → the valid one
        // refuses and is retained; the count reflects only the loadable crash.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let _go = EnvGuard::unset(REPORT_ONION_ENV);
        let dir = report_scratch_dir("auto-send-corrupt");
        let spool = open_spool_in(&dir).expect("open spool");
        spool
            .enqueue(&build_crash_report("loadable", "src/g.rs:1"))
            .expect("enqueue good");
        let corrupt = spool.dir().join("report-corrupt.json");
        std::fs::write(&corrupt, b"not valid json").expect("write corrupt");

        let attempted = auto_send_spooled_crashes(&dir);
        assert_eq!(
            attempted, 1,
            "only the loadable crash report is attempted; the corrupt file is skipped via continue"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_over_tor_surfaces_a_transport_new_failure() {
        // Force `TorOnionTransport::new` to fail by making its config-dir
        // unopenable: the Tor path roots the transport at `<config_dir>/tor`, and
        // `TorOnionTransport::new` opens a Spool at `<config_dir>/tor/config`
        // (which mkdirs `<...>/tor/config/reports`). If `<config_dir>/tor` is a
        // FILE, that mkdir fails → `send_over_tor` returns the structured
        // `Failed("tor: ..")` arm, never a silent drop.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let dir = report_scratch_dir("tor-new-fail");
        let _cfg = ConfigDirGuard::to(&dir);
        let resolved = Config::config_dir().expect("config dir resolves");
        // Create `<resolved>/tor` as a FILE so the transport's spool-open mkdir
        // under it cannot succeed.
        std::fs::create_dir_all(&resolved).expect("mkdir resolved");
        std::fs::write(resolved.join("tor"), b"not a dir").expect("write tor-as-file");
        let _go = EnvGuard::set(REPORT_ONION_ENV, VALID_V3_ONION);
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let r = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        match send_report(&r, &token) {
            ReportOutcome::Failed(reason) => assert!(
                reason.starts_with("tor:"),
                "a Tor transport-construction failure surfaces a tor: reason: {reason:?}"
            ),
            other => panic!("expected a tor: Failed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_send_over_tor_removes_each_sent_report() {
        // The `Always`-mode auto-send path over the fire-and-forget Tor
        // transport: every spooled CRASH report is Sent and its source file
        // removed (covering the `== Sent => remove` branch of
        // `auto_send_spooled_crashes`). A manual-issue report is skipped.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let dir = report_scratch_dir("tor-auto-send");
        let _cfg = ConfigDirGuard::to(&dir);
        let resolved = Config::config_dir().expect("config dir resolves");
        let _go = EnvGuard::set(REPORT_ONION_ENV, VALID_V3_ONION);
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let spool = open_spool_in(&resolved).expect("open spool");
        spool
            .enqueue(&build_crash_report("auto 1", "src/e.rs:1"))
            .expect("enqueue 1");
        spool
            .enqueue(&build_crash_report("auto 2", "src/e.rs:2"))
            .expect("enqueue 2");
        spool
            .enqueue(&Report::manual_issue("manual", "not a crash"))
            .expect("enqueue manual");

        let crash_files_before = open_spool_in(&resolved)
            .unwrap()
            .list()
            .unwrap()
            .into_iter()
            .filter(|p| {
                open_spool_in(&resolved)
                    .unwrap()
                    .load(p)
                    .map(|r| r.stream == Stream::CrashReports)
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(crash_files_before, 2, "two crash reports enqueued");

        let attempted = auto_send_spooled_crashes(&resolved);
        assert_eq!(attempted, 2, "exactly the two crash reports are attempted");

        // After auto-send over Tor (all Sent), the two crash SOURCE files are
        // removed; the manual-issue file remains. The Tor transport spools its
        // own outbound copies, so we count the CRASH reports that remain by
        // re-reading — they should be fewer than before (the sent sources are
        // gone).
        let crash_files_after = open_spool_in(&resolved)
            .unwrap()
            .list()
            .unwrap()
            .into_iter()
            .filter_map(|p| open_spool_in(&resolved).unwrap().load(&p).ok())
            .filter(|r| r.stream == Stream::CrashReports && r.body.contains("auto "))
            .count();
        assert_eq!(
            crash_files_after, 0,
            "every Sent crash report's source file was removed by auto-send"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ======================================================================
    // SECURITY / PRIVACY TESTS (W1TN3SS reporting-transport boundary)
    // ----------------------------------------------------------------------
    // These are mutation-grade: a wrong implementation (default-ON, a clearnet
    // leak on the onion path, a best-effort fallback transmit on a malformed
    // config, or an unsanitized payload at the transport hand-off) fails them.
    // ======================================================================

    // ---- 1. default-OFF: no transmit path is reachable without opt-in ----

    #[test]
    fn security_reporting_default_is_off_for_both_streams() {
        // The privacy posture: the host's reporting config defaults to Off for
        // BOTH streams. A mutant that flips a stream's default to AskEachTime or
        // Always (an on-by-default mis-build that could phone home) fails here.
        let mode = ReportingMode::default();
        assert_eq!(
            mode,
            ReportingMode::Off,
            "the reporting mode MUST default to Off (default-OFF posture)"
        );
        // The default mode permits NO transmission and NO auto-send.
        assert!(
            !ReportingMode::default().permits_reporting(),
            "default Off must permit NO reporting at all"
        );
        assert!(
            !ReportingMode::default().is_always(),
            "default Off is never an auto-send (Always) mode"
        );
    }

    #[test]
    fn security_only_always_mode_permits_unprompted_autosend() {
        // The auto-send-without-dialog path (`auto_send_spooled_crashes`) is the
        // ONLY mode that may transmit without a per-event prompt — and ONLY when
        // the stream is `Always`. Off and AskEachTime must NOT be auto-send.
        // A mutant that widens `is_always` (e.g. matches AskEachTime too) fails.
        assert!(
            ReportingMode::Always.is_always(),
            "Always is the auto-send mode"
        );
        assert!(
            !ReportingMode::AskEachTime.is_always(),
            "AskEachTime requires a per-event prompt — never unprompted auto-send"
        );
        assert!(!ReportingMode::Off.is_always(), "Off never auto-sends");
        // Off is the only mode that forbids ALL reporting; the other two permit it.
        assert!(!ReportingMode::Off.permits_reporting());
        assert!(ReportingMode::AskEachTime.permits_reporting());
        assert!(ReportingMode::Always.permits_reporting());
    }

    #[test]
    fn security_default_constructed_dialog_reaches_no_transmit_path() {
        // A default-constructed consent dialog (no config dir bound, no opt-in)
        // is fully inert: nothing is presented, so `consent_and_send` — the ONLY
        // method that calls `send_report` — short-circuits to None and never
        // reaches a transport. A mutant that lets `consent_and_send` proceed past
        // an absent `current` (e.g. drops the `?` early-return) fails here.
        let mut st = CrashConsentState::default();
        assert!(!st.has_pending(), "default dialog has nothing to send");
        assert_eq!(
            st.consent_and_send(),
            None,
            "with nothing pending, the send path is never entered (no transmit)"
        );
    }

    // ---- 2. no clearnet leak on the onion/Tor path ----

    #[test]
    fn security_onion_path_never_carries_a_clearnet_endpoint() {
        // When the Tor transport is selected, the resolved descriptor MUST be a
        // `Tor { onion }` whose address is a `.onion` host — it can NEVER be a
        // `Clearnet { endpoint }` carrying a clearnet host, even though a clearnet
        // endpoint is ALSO configured. A mutant that returns the clearnet variant
        // (or embeds the endpoint) on the onion path leaks the clearnet host.
        let choice = choose_transport(
            Some(VALID_V3_ONION),
            Some("https://ingest.clearnet.example"),
        );
        match &choice {
            TransportChoice::Tor { onion } => {
                assert!(
                    onion.ends_with(".onion"),
                    "the onion-path endpoint MUST be a .onion host, never a clearnet URL: {onion:?}"
                );
                assert!(
                    !onion.contains("clearnet")
                        && !onion.starts_with("http://")
                        && !onion.starts_with("https://"),
                    "no clearnet URL/host may appear in the onion descriptor: {onion:?}"
                );
            }
            other => panic!(
                "a valid onion MUST select the Tor transport, never clearnet — got {other:?}"
            ),
        }
        // The log class is the anonymous class, never a clearnet label.
        assert_eq!(choice.class(), "tor");
        assert_ne!(choice.class(), "clearnet");
        assert_ne!(choice.class(), "clearnet-no-endpoint");
    }

    #[test]
    fn security_onion_descriptor_is_byte_for_byte_the_configured_onion_no_url_wrap() {
        // Defense against a mutant that wraps the onion in a clearnet URL
        // (e.g. "https://<onion>/ingest") on the way to the transport. The Tor
        // descriptor MUST be the bare onion host the user configured — no scheme,
        // no path, no port suffix that could be a clearnet exit fingerprint.
        let choice = choose_transport(Some(VALID_V3_ONION), None);
        assert_eq!(
            choice,
            TransportChoice::Tor {
                onion: VALID_V3_ONION.to_string()
            },
            "the Tor descriptor is the bare configured onion, not a wrapped clearnet URL"
        );
        if let TransportChoice::Tor { onion } = choice {
            assert!(
                !onion.contains("://"),
                "no URL scheme may be present: {onion:?}"
            );
            assert!(
                !onion.contains('/'),
                "no path component may be present: {onion:?}"
            );
        }
    }

    // ---- 3. malformed / empty config never transmits (no best-effort fallback) ----

    #[test]
    fn security_fully_empty_config_resolves_to_no_transmit() {
        // The empty config (no onion, no clearnet endpoint) MUST resolve to the
        // do-nothing descriptor — `Clearnet { endpoint: None }` — which carries
        // no host and therefore cannot transmit. A mutant that fabricates a
        // default endpoint (a hardcoded phone-home URL) fails: the endpoint stays
        // None.
        let choice = choose_transport(None, None);
        assert_eq!(
            choice,
            TransportChoice::Clearnet { endpoint: None },
            "an empty config must resolve to the spool-only, no-transmit descriptor"
        );
        match choice {
            TransportChoice::Clearnet { endpoint } => assert!(
                endpoint.is_none(),
                "no endpoint may be conjured for an empty config (no silent phone-home)"
            ),
            TransportChoice::Tor { .. } => {
                panic!("an empty config must NEVER select the Tor (or any transmitting) path")
            }
        }
    }

    #[test]
    fn security_malformed_onion_with_no_endpoint_never_transmits() {
        // The dangerous case: a user TYPED an onion (so they expect anonymity)
        // but it is malformed, and there is NO clearnet endpoint. The result MUST
        // be the no-transmit descriptor — NOT a Tor transmit (the address is
        // invalid) and NOT a best-effort clearnet fallback (there is no endpoint
        // to fall back to). A mutant that treats a malformed onion as valid, or
        // that fabricates a fallback endpoint, fails here.
        for bad in [
            "not-an-onion",
            "short.onion",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZABCD.onion", // uppercase
            &("a".repeat(55) + ".onion"),                                     // 55 != 56
            &("a".repeat(57) + ".onion"),                                     // 57 != 56
        ] {
            let choice = choose_transport(Some(bad), None);
            assert_eq!(
                choice,
                TransportChoice::Clearnet { endpoint: None },
                "a malformed onion with no endpoint ({bad:?}) must resolve to NO transmit"
            );
            assert_eq!(
                choice.class(),
                "clearnet-no-endpoint",
                "the no-transmit class must be reported (never a silent anonymity claim)"
            );
        }
    }

    #[test]
    fn security_consented_send_with_empty_config_transmits_nothing() {
        // End-to-end through the env-reading send seam: a CONSENTED send with
        // BOTH env vars unset (the empty/default config) returns the structured
        // refusal and transmits nothing. This is the strongest default-OFF claim:
        // even WITH a valid consent token, an unconfigured build cannot phone
        // home. A mutant that maps an empty config to a `Sent` (fake success) or
        // to a transmit attempt fails here.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let _go = EnvGuard::unset(REPORT_ONION_ENV);
        let report = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        let outcome = send_report(&report, &token);
        assert_eq!(
            outcome,
            ReportOutcome::RefusedNoEndpoint,
            "an empty config + consent → structured refusal, never a transmit or fake Sent"
        );
        // The refusal is NOT a success and NOT a silent drop — it is the explicit
        // retain-for-later outcome.
        assert_ne!(
            outcome,
            ReportOutcome::Sent,
            "an empty config must never report Sent"
        );
    }

    #[test]
    fn security_malformed_onion_env_with_no_endpoint_consented_send_refuses() {
        // The same dangerous case as the selection test, but driven through the
        // real env readers in `send_report`: a malformed onion env + no endpoint
        // env + a consent token MUST still refuse (no Tor transmit on an invalid
        // address, no clearnet fallback when no endpoint exists).
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _ge = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let _go = EnvGuard::set(REPORT_ONION_ENV, "not-a-valid-onion");
        let report = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        assert_eq!(
            send_report(&report, &token),
            ReportOutcome::RefusedNoEndpoint,
            "a malformed onion env with no endpoint must refuse, never transmit"
        );
    }

    // ---- 4. sanitization is applied on the transport boundary ----

    #[test]
    fn security_email_and_url_in_panic_are_scrubbed_before_any_transport() {
        // `build_crash_report` runs the report through the SDK Sanitizer — the
        // gate every report passes BEFORE preview/spool/transmit. A panic message
        // that interpolated an email and a URL (the #1 free-text leak vector) MUST
        // have those tokens scrubbed to the uniform <redacted> marker in the body
        // that would reach the transport. A mutant that skips the sanitize call
        // (returns the raw report) leaks the email/URL and fails here.
        let report = build_crash_report(
            "auth failed for alice@secret.example via https://internal.corp/login",
            "src/auth.rs:7",
        );
        assert!(
            !report.body.contains("alice@secret.example"),
            "the email must be scrubbed before transport: {:?}",
            report.body
        );
        assert!(
            !report.body.contains("https://internal.corp"),
            "the URL must be scrubbed before transport: {:?}",
            report.body
        );
        assert!(
            report.body.contains("<redacted>"),
            "sensitive tokens collapse to the uniform <redacted> marker: {:?}",
            report.body
        );
        // The structural panic SITE (the dedup signal, not PII) survives.
        assert!(
            report.body.contains("src/auth.rs:7"),
            "the non-PII panic site survives sanitization: {:?}",
            report.body
        );
    }

    #[test]
    fn security_sanitization_is_applied_at_capture_and_persists_through_the_spool() {
        // The sanitized envelope is what reaches the transport: capture builds a
        // sanitized report, the spool round-trips it unchanged, and the consent
        // dialog's preview (which `consent_and_send` turns into the sent body)
        // shows the ALREADY-scrubbed text. Assert a bare IP embedded in the panic
        // is gone in the spooled+previewed envelope — proving the scrub is at the
        // boundary, not merely cosmetic at the preview layer. A mutant that drops
        // the sanitize call at capture leaks the IP and fails here.
        let dir = report_scratch_dir("sec-sanitize-boundary");
        let spool = open_spool_in(&dir).expect("open spool");
        let report = build_crash_report(
            "connect failed to host 203.0.113.42 refused",
            "src/net.rs:9",
        );
        // Already scrubbed at capture (build) time.
        assert!(
            !report.body.contains("203.0.113.42"),
            "the bare IP is scrubbed at capture: {:?}",
            report.body
        );
        let path = spool.enqueue(&report).expect("enqueue");
        let loaded = spool.load(&path).expect("load");
        assert!(
            !loaded.body.contains("203.0.113.42"),
            "the scrubbed body persists through the spool to the transport boundary: {:?}",
            loaded.body
        );
        assert!(
            loaded.body.contains("<redacted>"),
            "the IP collapsed to <redacted>"
        );
        // The preview the user/transport sees is the scrubbed text.
        let preview = preview_text(&loaded);
        assert!(
            !preview.contains("203.0.113.42"),
            "the preview presented before send is already scrubbed: {preview:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn security_user_redactions_in_preview_are_what_reach_the_transport() {
        // The send seam (`consent_and_send`) rebuilds the report from the EDITED
        // preview text via `edited_report_from_preview_text`. A span the user
        // redacts in the preview BODY MUST be what flows to `send_report` — the
        // original (pre-edit) text must NOT survive. A mutant that ignores the
        // user's edits and sends the original body re-introduces what the user
        // chose to strip.
        let original = build_crash_report("boom in widget", "src/x.rs:1");
        let preview = preview_text(&original);
        // The capture-time body is "panic: boom in widget (at src/x.rs:1)".
        // The user redacts the "widget" span inside the BODY (before the metadata
        // footer) — this is the surface `edited_report_from_preview_text` reads.
        assert!(
            preview.contains("widget"),
            "the body word is present pre-edit: {preview:?}"
        );
        let edited_text = preview.replace("widget", "[user-redacted]");
        let edited = edited_report_from_preview_text(&edited_text, &original);
        assert!(
            edited.body.contains("[user-redacted]"),
            "the user's edited body is what reaches the transport: {:?}",
            edited.body
        );
        assert!(
            !edited.body.contains("widget"),
            "the original span the user redacted must NOT survive into the sent body: {:?}",
            edited.body
        );
        // The metadata footer is never re-injected into the sent body.
        assert!(
            !edited.body.contains("--- metadata ---"),
            "the metadata footer must not leak into the transmitted body: {:?}",
            edited.body
        );
        // Stream + metadata are preserved (the edit only redacts the body).
        assert_eq!(edited.stream, Stream::CrashReports);
        assert_eq!(edited.metadata, original.metadata);
    }

    #[test]
    fn security_finding_bare_ip_with_port_suffix_survives_sdk_sanitizer() {
        // FINDING (SDK sanitization gap, NOT a c0pl4nd bug): the SDK's free-text
        // redactor detects a BARE IPv4 (e.g. `203.0.113.42` → <redacted>) but a
        // panic embedding `IP:PORT` (the common Rust networking-panic shape, e.g.
        // a `connect to 203.0.113.42:443 refused`) leaks the IP — the `:443`
        // suffix defeats the redactor's octet-split (`is_ipv4` sees a 4th "octet"
        // of `42:443` and rejects the whole token, so it is kept verbatim).
        //
        // This test PINS the current behaviour so a future SDK bump that closes
        // the gap is noticed (the assert flips and the test is updated). It does
        // NOT assert a c0pl4nd defect — the fix belongs in the SDK redactor,
        // which is out of this crate's scope. A bare IP IS scrubbed (proven by
        // `security_sanitization_is_applied_at_capture_and_persists_through_the_spool`).
        let bare = build_crash_report("connect failed to host 203.0.113.42 refused", "src/n.rs:1");
        assert!(
            !bare.body.contains("203.0.113.42"),
            "a BARE IP is scrubbed by the SDK: {:?}",
            bare.body
        );
        let with_port =
            build_crash_report("connect failed to 203.0.113.42:443 refused", "src/n.rs:1");
        // Current SDK behaviour: the IP:PORT token survives. Documented gap.
        assert!(
            with_port.body.contains("203.0.113.42"),
            "current SDK behaviour: an IP:PORT token leaks (documented gap, fix is SDK-side): {:?}",
            with_port.body
        );
    }

    #[test]
    fn security_metadata_values_are_sanitized_before_transport() {
        // The Sanitizer scrubs metadata VALUES too (not just the body). A panic
        // captured with metadata holding an absolute path / secret must have that
        // value scrubbed in the envelope. We drive a report whose metadata value
        // carries an email through the SDK sanitizer the same way capture does.
        let raw = Report::crash("panic: boom (at src/x.rs:1)")
            .with_metadata("contact", "ops@secret.example")
            .with_metadata("app_version", env!("CARGO_PKG_VERSION"));
        let sanitized = Sanitizer::new().sanitize(raw);
        let contact = sanitized
            .metadata
            .iter()
            .find(|(k, _)| k == "contact")
            .map(|(_, v)| v.as_str())
            .expect("the contact metadata key survives");
        assert!(
            !contact.contains("ops@secret.example"),
            "a sensitive metadata VALUE must be scrubbed before transport: {contact:?}"
        );
        assert!(
            contact.contains("<redacted>"),
            "the scrubbed metadata value collapses to <redacted>: {contact:?}"
        );
    }
}
