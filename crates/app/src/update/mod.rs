//! Telemetry-free update CHECK.
//!
//! The version check runs on launch under the default `notify` mode (and
//! `auto`), when the user invokes `c0pl4nd update`, or when the legacy
//! `[update] check_on_launch = true` flag is set; `manual`/`off` suppress it.
//! The single network surface is the **public GitHub Releases API** for this
//! repository — unauthenticated, zero PII, no custom server, no shipped token,
//! reached over a host-confined https GET ([`confined_api_get`]). The check is
//! offline-graceful: an unreachable API is reported as "could not check", never
//! an error that blocks the app.
//!
//! Scope: this module CHECKS for a newer release and points the user at the
//! download page. It does NOT download-and-apply a binary in place — that
//! ("auto" install) requires cryptographically signed release artifacts (a
//! minisign keypair + a release-workflow signing step) so the updater can
//! verify-before-swap; until that signing infrastructure exists, directing the
//! user to the signed GitHub release is the safe, honest path. The decision
//! logic here ([`parse_tag`], [`pick_channel_tag`], [`is_newer`]) is pure and
//! unit-tested without any network.
//!
//! ## In-app verified self-updater
//!
//! The CLI surface in THIS module is the legacy, browser-pointing path. The
//! richer **verify-before-swap** in-app updater (download + SHA-256/minisign
//! verify + atomic `self-replace`, mirroring the SCR1B3 editor) lives in the
//! sibling `update_engine/` module, which the Settings → Updates page drives.
//! It is kept separate so the egui-only updater backend (and its `eframe`
//! dependency) is compiled exactly once — by the egui app + the egui test
//! binaries — without forcing it onto the legacy winit `c0pl4nd-legacy` binary
//! that also uses this CLI module.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result};

/// Canonical public release repository. (An early value `itasha-corp/c0pl4nd`
/// 404'd — there is no such owner — so every check silently failed. It was then
/// `46b-ETYKiAL/Itasha.Corp_C0PL4ND` until the repository was renamed to
/// `C0PL4ND`; that former name resolves only via GitHub's rename redirect, which
/// this constant must never depend on.)
const REPO: &str = "46b-ETYKiAL/C0PL4ND";

/// `User-Agent` for the one API call. App name + version ONLY — no PII. The
/// GitHub API rejects requests without a User-Agent, so this is mandatory.
const USER_AGENT: &str = concat!("c0pl4nd-updater/", env!("CARGO_PKG_VERSION"));

/// Largest Releases-API JSON body the check will read. A real `releases/latest`
/// or `releases?per_page=20` response is far smaller; the cap stops a hostile or
/// MITM'd endpoint from streaming an unbounded body that could OOM the process.
const MAX_RELEASE_JSON_BYTES: u64 = 4 * 1024 * 1024;

/// Redirect hops followed MANUALLY, each re-confined to https + an allow-listed
/// host. A normal GitHub REST response is a direct 200, so this is headroom.
const MAX_REDIRECTS: usize = 4;

/// The only host this version check may contact. It calls the GitHub REST API
/// and nothing else, so the allow-list is exactly that host — a MITM'd or
/// hostile redirect cannot point the request (and our `User-Agent`) at an
/// arbitrary server. This mirrors the hardened `update_engine::net` allow-list;
/// it is duplicated here rather than shared because this dependency-light module
/// is ALSO compiled into the legacy `c0pl4nd-legacy` binary, which excludes the
/// `update_engine`/eframe stack.
fn host_allowed(host: &str) -> bool {
    host.eq_ignore_ascii_case("api.github.com")
}

/// Refuse any URL that is not `https://` to an allow-listed host. The pure URL
/// plumbing (scheme check, host extraction) is shared with the in-app updater
/// via [`crate::net_confine`]; only the host allow-list ([`host_allowed`]) is
/// caller-specific. Re-checked at every redirect hop by [`confined_api_get`].
fn assert_confined(url: &str) -> Result<()> {
    if !c0pl4nd_core::net_confine::is_https(url) {
        anyhow::bail!("update check blocked: the update link wasn't a secure (https) link");
    }
    match c0pl4nd_core::net_confine::url_host(url) {
        Some(h) if host_allowed(&h) => Ok(()),
        Some(_) => {
            anyhow::bail!("update check blocked: the update link pointed to an unexpected server")
        }
        None => anyhow::bail!("update check blocked: the update link was malformed"),
    }
}

/// Host-confined, redirect-controlled, size-capped GET of a GitHub REST URL.
/// ureq's default agent follows up to 5 redirects to ARBITRARY hosts; this
/// builds a `redirects(0)` agent and walks the chain itself, re-asserting https
/// AND an allow-listed host at EVERY hop, then reads at most
/// [`MAX_RELEASE_JSON_BYTES`]. Connect/read timeouts bound a hung launch thread.
fn confined_api_get(url: &str) -> Result<String> {
    confined_api_get_with(&default_agent(), url)
}

/// The agent the production check runs on: redirects disabled (the walk in
/// [`confined_api_get_with`] follows them MANUALLY so every hop is re-confined)
/// plus connect/read timeouts that bound a hung launch thread.
fn default_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(20))
        .build()
}

/// [`confined_api_get`] over an explicit `agent`. The agent is the ONLY seam:
/// the entry assert, the per-hop re-confinement, the hop cap and the size cap
/// all run identically whoever supplies the transport. Production always passes
/// [`default_agent`]; the tests pass an agent whose middleware short-circuits the
/// socket (see the test module's hermetic transport harness).
fn confined_api_get_with(agent: &ureq::Agent, url: &str) -> Result<String> {
    assert_confined(url)?;
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        // With redirects(0) a 3xx returns Ok (status in 300..400); ureq still
        // maps >=400 to Err(Status). Accept a 3xx from either shape.
        let resp = match agent
            .get(&current)
            .set("User-Agent", USER_AGENT)
            .set("Accept", "application/vnd.github+json")
            .call()
        {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) if (300..400).contains(&code) => r,
            Err(e) => {
                return Err(
                    anyhow::Error::new(e).context("couldn't reach GitHub to check for updates")
                )
            }
        };
        if (300..400).contains(&resp.status()) {
            let loc = resp
                .header("Location")
                .context("update check got an invalid redirect from GitHub")?;
            let next =
                c0pl4nd_core::net_confine::resolve_redirect(&current, loc).map_err(|_| {
                    anyhow::anyhow!("update check blocked: a redirect pointed somewhere unexpected")
                })?;
            assert_confined(&next)?;
            current = next;
            continue;
        }
        let mut body = String::new();
        resp.into_reader()
            .take(MAX_RELEASE_JSON_BYTES)
            .read_to_string(&mut body)
            .context("failed to read release response")?;
        return Ok(body);
    }
    anyhow::bail!("couldn't reach GitHub to check for updates (too many redirects)")
}

/// The running binary's version.
pub fn current_version() -> &'static str {
    c0pl4nd_core::version()
}

/// The human-facing page where a user downloads the latest signed release.
pub fn release_page_url() -> String {
    format!("https://github.com/{REPO}/releases/latest")
}

/// Extract the `tag_name` from a SINGLE-release JSON body (the
/// `…/releases/latest` response), stripping a leading `v`. Pure string scan —
/// no JSON dependency, matching the crate's dependency-light posture.
fn parse_tag(body: &str) -> Option<String> {
    // After splitting on the literal key, the remainder starts at the `:`, so
    // the value is the FIRST quoted token: `:"v0.4.1",…` → split('"') yields
    // [":", "v0.4.1", …], i.e. index 1.
    body.split("\"tag_name\"")
        .nth(1)?
        .split('"')
        .nth(1)
        .map(|s| s.trim_start_matches('v').to_string())
}

/// From a release-LIST JSON body (the `…/releases` response, which GitHub
/// returns newest-first), pick the newest tag whose name contains `channel`
/// (e.g. `beta`/`nightly`), falling back to the newest tag of ANY kind when no
/// release matches the channel. `None` only when the list is empty.
fn pick_channel_tag(body: &str, channel: &str) -> Option<String> {
    let tags: Vec<&str> = body
        .split("\"tag_name\"")
        .skip(1)
        .filter_map(|s| s.split('"').nth(1))
        .collect();
    let channel = channel.to_lowercase();
    tags.iter()
        .find(|tag| tag.to_lowercase().contains(&channel))
        .or_else(|| tags.first())
        .map(|s| s.trim_start_matches('v').to_string())
}

/// Compare dotted numeric versions; `true` when `latest` is strictly newer than
/// `current`. Leading `v` is ignored; a non-numeric suffix on a component
/// (`1.2.0-beta`) compares on the numeric prefix.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .map(|s| s.parse().unwrap_or(0))
            .collect()
    }
    let (l, c) = (parts(latest), parts(current));
    for i in 0..l.len().max(c.len()) {
        let lv = l.get(i).copied().unwrap_or(0);
        let cv = c.get(i).copied().unwrap_or(0);
        if lv != cv {
            return lv > cv;
        }
    }
    false
}

/// Query GitHub Releases for the newest version tag on `channel`. Network call —
/// only invoked on explicit user action or the opt-in launch check. `stable`
/// reads the dedicated `releases/latest` endpoint (newest non-prerelease);
/// other channels scan the full `releases` list for a matching tag.
pub fn latest_version(channel: &str) -> Result<String> {
    if channel.eq_ignore_ascii_case("stable") {
        let body = confined_api_get(&format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))?;
        parse_tag(&body).context("couldn't read the latest version information from GitHub")
    } else {
        let body = confined_api_get(&format!(
            "https://api.github.com/repos/{REPO}/releases?per_page=20"
        ))?;
        pick_channel_tag(&body, channel)
            .context("no releases are available on this update channel yet")
    }
}

/// Check `channel` for a newer release and return a one-line user notice when
/// one exists, else `None`. Offline-graceful: a failed check returns `None`
/// (never an error) so a launch check never blocks or alarms.
///
/// `#[allow(dead_code)]`: this browser-pointing launch notice is the LEGACY
/// winit binary's (`c0pl4nd-legacy` / `main.rs`) on-launch check. The egui
/// binary (`c0pl4nd` / `egui_main.rs`) now drives the SHARED in-app updater's
/// one-click notification banner instead (`egui_app::start_launch_update_check`),
/// so it no longer calls this — but `update.rs` is `mod update`-compiled into
/// BOTH binaries, so this stays live for the legacy binary and is (correctly)
/// unused in the egui one. Not error suppression — a genuinely per-binary path.
#[allow(dead_code)]
pub fn check_for_update(channel: &str) -> Option<String> {
    let latest = latest_version(channel).ok()?;
    if is_newer(&latest, current_version()) {
        Some(format!(
            "C0PL4ND {latest} is available (you have {}). Download: {}",
            current_version(),
            release_page_url()
        ))
    } else {
        None
    }
}

/// `c0pl4nd update` — an explicit, user-initiated check. Prints whether a newer
/// release exists and, if so, the download page. Does NOT install in place (see
/// the module docs): the user downloads the signed release. `channel` selects
/// the release stream.
pub fn run_update(channel: &str) -> Result<()> {
    println!(
        "C0PL4ND updater — current version {} (channel: {channel})",
        current_version()
    );
    match latest_version(channel) {
        Ok(latest) if is_newer(&latest, current_version()) => {
            println!("A newer version is available: {latest}");
            println!("Download the signed release at: {}", release_page_url());
        }
        Ok(latest) => {
            println!("You are up to date ({latest}).");
        }
        Err(e) => {
            // Offline / API unreachable is not a failure of the command. The
            // technical detail (full anyhow chain) goes to the log, never stderr.
            tracing::warn!(target: "c0pl4nd::update", detail = ?e, "update check failed");
            eprintln!("Couldn't check for updates. You may be offline, or GitHub may be");
            eprintln!("unreachable. Nothing was changed.");
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// On-launch check throttle (`[update] check_interval_hours`)
// ----------------------------------------------------------------------------
//
// The default `notify` mode runs a version check on launch. Without a throttle
// that would hit the GitHub REST API on EVERY launch and risk the 60-req/hr
// unauthenticated rate limit. We persist the Unix-seconds timestamp of the last
// launch check in a tiny sibling file next to the config and only re-check once
// `check_interval_hours` have elapsed. The decision ([`is_due`]) is pure and
// unit-tested; the I/O wrappers are best-effort (a read/write failure just means
// the next launch re-checks, which is safe).

/// Path of the file recording the last on-launch check time (Unix seconds).
/// Sibling of the config file so it lives in the same per-user config dir.
fn check_state_path() -> Option<std::path::PathBuf> {
    let cfg = c0pl4nd_core::Config::default_path()?;
    let dir = cfg.parent()?;
    Some(dir.join("last-update-check"))
}

/// Seconds since the Unix epoch, or `None` if the system clock predates it.
fn now_unix() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Read the recorded last-check time (Unix seconds), if present and parseable.
fn read_last_check() -> Option<u64> {
    let path = check_state_path()?;
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Record "a launch check happened now" so the next launch within
/// `check_interval_hours` skips the network. Best-effort: a write failure just
/// means the next launch re-checks (no throttle), which is safe. Called AFTER
/// every launch-check attempt regardless of its outcome, so a transient offline
/// launch does not spam the API on every subsequent start.
pub fn record_check_now() {
    if let (Some(path), Some(now)) = (check_state_path(), now_unix()) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, now.to_string());
    }
}

/// PURE due-ness decision. A check is due when:
/// * there is no recorded last-check (`last == None`), or
/// * the interval is `0` (throttle disabled), or
/// * `last` is in the FUTURE relative to `now` (clock moved backwards — re-check
///   rather than suppress indefinitely), or
/// * at least `interval_hours` have elapsed since `last`.
fn is_due(now: u64, last: Option<u64>, interval_hours: u32) -> bool {
    let Some(last) = last else {
        return true;
    };
    if interval_hours == 0 || last > now {
        return true;
    }
    now - last >= u64::from(interval_hours) * 3600
}

/// Whether an on-launch update check is due now, given the configured interval.
/// A clock error (epoch unreadable) is treated as "due" so a check is never
/// permanently suppressed.
pub fn check_due(interval_hours: u32) -> bool {
    let now = now_unix().unwrap_or(u64::MAX);
    is_due(now, read_last_check(), interval_hours)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("1.2.0", "1.1.9"));
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v1.0.1", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.0", "1.0.0"));
        // Numeric-prefix comparison tolerates a channel suffix on a component.
        assert!(is_newer("1.3.0-beta", "1.2.0"));
    }

    #[test]
    fn current_version_nonempty() {
        assert!(!current_version().is_empty());
    }

    #[test]
    fn release_page_url_points_at_the_real_repo() {
        let url = release_page_url();
        assert!(
            url.contains("46b-ETYKiAL/C0PL4ND"),
            "release URL must target the real repo, not the old 404 path: {url}"
        );
        assert!(
            !url.contains("itasha-corp/c0pl4nd"),
            "must not use the dead path"
        );
    }

    #[test]
    fn parse_tag_extracts_and_strips_v() {
        let body = r#"{"url":"x","tag_name":"v0.4.1","name":"0.4.1"}"#;
        assert_eq!(parse_tag(body).as_deref(), Some("0.4.1"));
        // No tag_name → None (drives the offline-graceful path).
        assert_eq!(parse_tag(r#"{"message":"Not Found"}"#), None);
    }

    /// The SHIPPED default channel must be the one that takes the
    /// prerelease-excluding endpoint.
    ///
    /// This lives here, beside `latest_version` — the only surviving consumer of
    /// `config.update.channel` — rather than in the Settings UI, where it used to
    /// assert that a dropdown offered the default value. That dropdown is gone:
    /// it wrote a field the in-app updater never read, so the property worth
    /// pinning is not "the combo lists it" but "the default routes to the stable
    /// endpoint".
    ///
    /// `latest_version` branches on `eq_ignore_ascii_case("stable")`: that arm
    /// reads `releases/latest`, which EXCLUDES prereleases. Any other value falls
    /// through to scanning the full release list, where a prerelease can match —
    /// and `update_engine::net` then refuses it as a channel-pin violation. So a
    /// default of anything but `stable` would ship an updater that finds
    /// candidates its own security gate rejects.
    #[test]
    fn the_default_channel_takes_the_prerelease_excluding_endpoint() {
        let default_channel = c0pl4nd_core::Config::default().update.channel;
        assert!(
            default_channel.eq_ignore_ascii_case("stable"),
            "the shipped default update channel must be `stable` so checks read \
             the prerelease-excluding releases/latest endpoint; got {default_channel:?}"
        );
    }

    #[test]
    fn pick_channel_tag_prefers_channel_then_falls_back() {
        // Newest-first list with a beta and two stables.
        let body = r#"
            {"tag_name":"v0.5.0-beta.1"},
            {"tag_name":"v0.4.2"},
            {"tag_name":"v0.4.1"}
        "#;
        assert_eq!(
            pick_channel_tag(body, "beta").as_deref(),
            Some("0.5.0-beta.1")
        );
        // A channel with no matching release falls back to the newest tag.
        assert_eq!(
            pick_channel_tag(body, "nightly").as_deref(),
            Some("0.5.0-beta.1")
        );
        // Empty list → None.
        assert_eq!(pick_channel_tag("{}", "beta"), None);
    }

    #[test]
    fn assert_confined_allows_only_https_github_api() {
        // The real endpoints this module uses are accepted.
        assert!(assert_confined("https://api.github.com/repos/x/y/releases/latest").is_ok());
        // Plain http is refused (TLS-downgrade defense).
        assert!(assert_confined("http://api.github.com/repos/x/y/releases/latest").is_err());
        // A different host (e.g. a MITM redirect target) is refused even over https.
        assert!(assert_confined("https://evil.example.com/releases").is_err());
        // github.com (not the API host) is NOT on this module's allow-list.
        assert!(assert_confined("https://github.com/x/y").is_err());
        // Userinfo cannot smuggle a foreign host past the check.
        assert!(assert_confined("https://api.github.com@evil.example.com/").is_err());
        // Malformed (no host) is refused.
        assert!(assert_confined("https:///releases").is_err());
    }

    // =====================================================================
    // Hermetic transport harness for the legacy check's confined GET
    // =====================================================================
    //
    // `assert_confined_allows_only_https_github_api` above proves the PREDICATE.
    // It does NOT prove the predicate is CALLED on redirect hop 2 — the property
    // `confined_api_get`'s doc comment actually claims ("re-asserting https AND
    // an allow-listed host at EVERY hop"). This module's walk is a second, near
    // duplicate of `update_engine::net::confined_get_with`, and it carried the
    // same untested-walk gap.
    //
    // Injection is at the `ureq::Agent` boundary: the middleware never calls
    // `next.handle`, so the chain short-circuits above `unit::connect` and NO
    // socket, DNS lookup or TLS handshake occurs. Only the third-party transport
    // is replaced; the confinement asserts and the walk are the real code.
    //
    // DOES NOT PROVE (socket never opens): TLS, certificate/hostname validation,
    // DNS resolution or rebinding, the connect/read timeouts firing, or real
    // GitHub redirect behaviour. See the equivalent note in `update_engine::net`.

    use std::sync::{Arc, Mutex};

    struct MockTransport {
        seen: Arc<Mutex<Vec<String>>>,
        #[allow(clippy::type_complexity)]
        responder: Box<dyn Fn(usize) -> String + Send + Sync>,
    }

    impl ureq::Middleware for MockTransport {
        fn handle(
            &self,
            request: ureq::Request,
            _next: ureq::MiddlewareNext,
        ) -> Result<ureq::Response, ureq::Error> {
            let idx = {
                let mut log = self.seen.lock().expect("seen log poisoned");
                log.push(request.url().to_string());
                log.len() - 1
            };
            // `_next` is deliberately never handled: that is the short-circuit.
            Ok((self.responder)(idx)
                .parse()
                .expect("test fixture must be a well-formed HTTP response"))
        }
    }

    fn mock_agent(
        responder: impl Fn(usize) -> String + Send + Sync + 'static,
    ) -> (ureq::Agent, Arc<Mutex<Vec<String>>>) {
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

    const API_URL: &str = "https://api.github.com/repos/x/y/releases/latest";

    #[test]
    fn legacy_check_refuses_a_redirect_off_the_allowlist_at_hop_2() {
        let (agent, seen) = mock_agent(|i| match i {
            0 => "HTTP/1.1 302 Found\r\nLocation: https://evil.example.com/x\r\n\r\n".to_string(),
            _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_string(),
        });

        let err = confined_api_get_with(&agent, API_URL).expect_err("hop 2 must be refused");
        assert!(
            err.to_string().contains("unexpected server"),
            "expected the host refusal copy, got: {err}"
        );
        // len==1 proves the walk RAN (an entry-assert bail would be 0) and that
        // the attacker host was never contacted.
        let urls = seen.lock().unwrap().clone();
        assert_eq!(
            urls.len(),
            1,
            "hop 2 must never be dispatched; saw {urls:?}"
        );
        assert!(!urls.iter().any(|u| u.contains("evil.example")));
    }

    #[test]
    fn legacy_check_refuses_a_scheme_downgrade_redirect_at_hop_2() {
        // The redirect target is the ALLOW-LISTED host, so only the per-hop
        // https re-assert can refuse this.
        let (agent, seen) = mock_agent(|i| match i {
            0 => "HTTP/1.1 302 Found\r\nLocation: http://api.github.com/x\r\n\r\n".to_string(),
            _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_string(),
        });

        let err = confined_api_get_with(&agent, API_URL).expect_err("http hop must be refused");
        assert!(
            err.to_string().contains("secure (https) link"),
            "expected the https refusal copy, got: {err}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1, "cleartext hop dispatched");
    }

    #[test]
    fn legacy_check_stops_at_max_redirects() {
        let (agent, seen) = mock_agent(|i| {
            format!("HTTP/1.1 302 Found\r\nLocation: https://api.github.com/hop{i}\r\n\r\n")
        });

        let err =
            confined_api_get_with(&agent, API_URL).expect_err("endless chain must be refused");
        assert!(
            err.to_string().contains("too many redirects"),
            "expected the redirect-cap copy, got: {err}"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            MAX_REDIRECTS + 1,
            "the walk must dispatch exactly MAX_REDIRECTS + 1 requests"
        );
    }

    #[test]
    fn legacy_check_follows_a_confined_redirect_and_returns_the_body() {
        // Control: without this, a `confined_api_get_with` that refused
        // everything would satisfy all three tests above.
        let body = r#"{"tag_name":"v9.9.9"}"#;
        let (agent, seen) = mock_agent(move |i| match i {
            0 => "HTTP/1.1 302 Found\r\nLocation: https://api.github.com/moved\r\n\r\n".to_string(),
            _ => format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        });

        let got = confined_api_get_with(&agent, API_URL).expect("a confined chain must succeed");
        assert_eq!(got, body);
        assert_eq!(seen.lock().unwrap().len(), 2, "both hops dispatched");
        // The body flows into the real pure parser — the check end-to-end.
        assert_eq!(parse_tag(&got).as_deref(), Some("9.9.9"));
    }

    #[test]
    fn legacy_check_truncates_a_body_past_the_size_cap() {
        // The read is `.take(MAX_RELEASE_JSON_BYTES)`, which TRUNCATES rather
        // than refusing. That still bounds memory, and a truncated body fails
        // the downstream parse (fail-closed) — this pins that behaviour so a
        // change to it is noticed.
        let over = (MAX_RELEASE_JSON_BYTES + 1024) as usize;
        let (agent, _seen) = mock_agent(move |_| {
            let body = "a".repeat(over);
            format!("HTTP/1.1 200 OK\r\nContent-Length: {over}\r\n\r\n{body}")
        });

        let got = confined_api_get_with(&agent, API_URL).expect("truncation is not an error");
        assert_eq!(
            got.len() as u64,
            MAX_RELEASE_JSON_BYTES,
            "the body must be truncated to exactly the cap, never read unbounded"
        );
        assert_eq!(
            parse_tag(&got),
            None,
            "a truncated body must not yield a tag (fail-closed)"
        );
    }

    /// The refusal messages are plain user copy — they must NOT echo the
    /// offending URL or host back to the user (inventory C0-034/035/036).
    #[test]
    fn assert_confined_refusal_copy_leaks_no_url_or_host() {
        for url in [
            "http://evil.example.com/secret/path",
            "https://evil.example.com/releases",
            "https:///releases",
        ] {
            let msg = assert_confined(url).unwrap_err().to_string();
            assert!(
                !msg.contains("evil.example.com") && !msg.contains("secret/path"),
                "refusal copy leaked the URL/host for {url:?}: {msg}"
            );
            assert!(
                msg.starts_with("update check blocked"),
                "refusal copy should be plain user copy: {msg}"
            );
        }
    }

    // `url_host` and `resolve_redirect` now live in `crate::net_confine` and are
    // tested there; this module keeps only the allow-list-specific assertion.

    #[test]
    fn is_due_when_never_checked() {
        assert!(
            is_due(1_000_000, None, 24),
            "no recorded check → always due"
        );
    }

    #[test]
    fn is_due_respects_the_interval() {
        let last = 1_000_000u64;
        let day = 24 * 3600;
        // Exactly at the interval boundary → due.
        assert!(is_due(last + day, Some(last), 24));
        // One second past → due.
        assert!(is_due(last + day + 1, Some(last), 24));
        // One second short → NOT due (throttled).
        assert!(!is_due(last + day - 1, Some(last), 24));
        // Same instant → not due.
        assert!(!is_due(last, Some(last), 24));
    }

    #[test]
    fn is_due_when_interval_is_zero_disables_throttle() {
        assert!(
            is_due(1_000_000, Some(1_000_000), 0),
            "interval 0 means check every launch"
        );
    }

    #[test]
    fn is_due_when_clock_moved_backwards() {
        // Recorded time is in the FUTURE relative to now (clock rolled back):
        // re-check rather than suppress forever.
        assert!(is_due(1_000, Some(5_000), 24));
    }

    #[test]
    fn is_newer_handles_differing_component_counts() {
        // A shorter version pads with implicit zeros on the right: 1.2 == 1.2.0
        // (not newer), but 1.2.1 IS newer than 1.2, and 1.2 is newer than 1.1.9.
        assert!(!is_newer("1.2", "1.2.0"), "1.2 == 1.2.0 → not newer");
        assert!(!is_newer("1.2.0", "1.2"), "1.2.0 == 1.2 → not newer");
        assert!(is_newer("1.2.1", "1.2"), "1.2.1 > 1.2 (implicit 1.2.0)");
        assert!(is_newer("1.2", "1.1.9"), "1.2 > 1.1.9");
        // A longer current that is equal on the shared prefix and zero beyond is
        // not newer.
        assert!(!is_newer("1.0", "1.0.0.0"), "trailing zeros do not bump");
    }

    #[test]
    fn is_newer_compares_most_significant_component_first() {
        // The FIRST differing component decides — a huge patch never overrides a
        // larger major/minor (a mutant that compares the wrong index is caught).
        assert!(is_newer("2.0.0", "1.99.99"), "major dominates");
        assert!(
            !is_newer("1.99.99", "2.0.0"),
            "a big patch loses to a bigger major"
        );
        assert!(is_newer("1.3.0", "1.2.99"), "minor dominates patch");
        assert!(!is_newer("1.2.99", "1.3.0"));
    }

    #[test]
    fn is_newer_treats_non_numeric_component_prefix_as_its_number() {
        // A component like `0-beta` compares on its numeric PREFIX (0), and a
        // component with NO leading digits parses as 0 (the `unwrap_or(0)` arm).
        assert!(
            !is_newer("1.0.0-beta", "1.0.0"),
            "1.0.0-beta == 1.0.0 numerically"
        );
        assert!(
            is_newer("1.0.1-beta", "1.0.0"),
            "the numeric prefix still ranks"
        );
        // A wholly non-numeric component is treated as 0.
        assert!(!is_newer("1.x.0", "1.0.0"), "'x' parses to 0 → equal");
        assert!(
            !is_newer("v1.2.0", "v1.2.0"),
            "leading v on both is ignored"
        );
    }

    #[test]
    fn parse_tag_only_reads_the_first_quoted_value_after_the_key() {
        // The value is the FIRST quoted token after `"tag_name"` — a later
        // `"name"` field must not be mistaken for the tag (a mutant that picks
        // the wrong `nth` is caught).
        let body = r#"{"tag_name":"v1.5.0","name":"Release 1.5.0","body":"notes"}"#;
        assert_eq!(parse_tag(body).as_deref(), Some("1.5.0"));
        // A tag with NO leading v is returned verbatim (only a `v` is stripped).
        assert_eq!(
            parse_tag(r#"{"tag_name":"2026.04.01"}"#).as_deref(),
            Some("2026.04.01")
        );
        // Only a single leading `v` is stripped (trim_start_matches strips all).
        assert_eq!(parse_tag(r#"{"tag_name":"vv9"}"#).as_deref(), Some("9"));
        // An empty body or a body without the key → None.
        assert_eq!(parse_tag(""), None);
        assert_eq!(parse_tag("no key here"), None);
    }

    #[test]
    fn pick_channel_tag_is_case_insensitive_on_the_channel() {
        // The channel match folds case on BOTH sides.
        let body = r#"{"tag_name":"v1.0.0-BETA"},{"tag_name":"v0.9.0"}"#;
        assert_eq!(
            pick_channel_tag(body, "beta").as_deref(),
            Some("1.0.0-BETA"),
            "an upper-case tag matches a lower-case channel request"
        );
        assert_eq!(
            pick_channel_tag(body, "BETA").as_deref(),
            Some("1.0.0-BETA"),
            "an upper-case channel request matches a tag"
        );
    }

    #[test]
    fn pick_channel_tag_falls_back_to_the_first_listed_tag() {
        // GitHub returns the list newest-first; with no channel match the
        // fallback is the FIRST (newest) tag, not the last.
        let body = r#"{"tag_name":"v3.0.0"},{"tag_name":"v2.0.0"},{"tag_name":"v1.0.0"}"#;
        assert_eq!(
            pick_channel_tag(body, "nightly").as_deref(),
            Some("3.0.0"),
            "no channel match → newest (first) tag wins, never the oldest"
        );
    }

    #[test]
    fn check_for_update_message_names_both_versions_and_the_page() {
        // The user-facing notice composes correctly: it names the newer version,
        // the current version, and the release page URL. We exercise the pure
        // composition by reproducing the format the function uses (the function
        // itself is network-gated; this locks the message shape it emits).
        let latest = "9.9.9";
        let current = current_version();
        assert!(
            is_newer(latest, current),
            "9.9.9 must be newer than the build"
        );
        let msg = format!(
            "C0PL4ND {latest} is available (you have {current}). Download: {}",
            release_page_url()
        );
        assert!(msg.contains("9.9.9"));
        assert!(msg.contains(current));
        assert!(msg.contains("releases/latest"));
    }
}
