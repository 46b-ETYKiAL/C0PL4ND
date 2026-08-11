# Security Policy

C0PL4ND is a cross-platform, GPU-accelerated terminal emulator written in Rust
(`crates/core` engine + `crates/app` eframe/egui + wgpu UI), shipping for
Windows, Linux, and macOS under `MIT OR Apache-2.0`. It builds two binaries:
`c0pl4nd` (the canonical egui app) and `c0pl4nd-legacy` (the original winit
renderer).

This document describes our supported versions, how to report a vulnerability,
the threat model we hardened against, the deliberate anti-RCE design choices,
the verified-self-updater security model (including key rotation), and our
supply-chain controls.

---

## Supported versions

Security fixes are provided for the latest released version. We recommend always
running the most recent release.

| Version | Supported |
| --- | --- |
| Latest release | ✅ |
| Previous minor | ⚠️ Critical fixes only |
| Older | ❌ |

---

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
discussions, or pull requests.**

Report privately using **GitHub Security Advisories** — open the repository's
**Security → Advisories → Report a vulnerability** form
(<https://github.com/46b-ETYKiAL/C0PL4ND/security/advisories/new>).
A private advisory is visible only to you and the maintainers.

Please include, as far as you can:

- A description of the issue and its potential impact.
- Steps to reproduce, or a proof-of-concept.
- Affected version(s) and platform(s) (Windows / Linux / macOS).
- Any suggested mitigation.

### Coordinated disclosure

We practice coordinated disclosure:

- **Acknowledgement** within a few business days.
- A good-faith effort to validate, triage, and develop a fix.
- We will work with you on disclosure timing and credit you in the advisory
  (unless you prefer to remain anonymous).

We ask that you give us a reasonable opportunity to release a fix before any
public disclosure. We will not pursue legal action against researchers who
report in good faith and avoid privacy violations, data destruction, and
service disruption.

---

## Threat model & security posture

### A terminal spawns shells with your full privileges — by design

C0PL4ND is a terminal emulator. Its purpose is to run a shell (and the programs
that shell launches) with **your** user privileges. That is the product, not a
vulnerability:

- **Sandboxing the spawned shell is out of scope.** The shell you run inside
  C0PL4ND can read your files, make network connections, and execute code — just
  as it would in any other terminal. We do **not** confine the child process with
  seccomp, AppContainer, Landlock, or a similar sandbox, because a confined shell
  is not a usable terminal. Proposals to sandbox the spawned process are
  therefore not in scope.
- **Programs you run inside the terminal are out of scope.** Vulnerabilities in
  third-party shells, CLIs, or OS components that you execute inside C0PL4ND are
  the responsibility of those programs, not of the emulator.

### What we actually hardened: the untrusted-byte surfaces

The interesting attack surface in a terminal emulator is the code that processes
**bytes it did not produce and that do not need shell privileges to cause harm.**
A hostile or compromised program — or a remote host over SSH — can emit arbitrary
bytes into the terminal. Those bytes flow into three surfaces, which are the ones
we deliberately harden:

1. **The VT / ANSI / OSC escape-sequence parser** (`crates/core/src/term.rs`,
   `crates/core/src/term/osc.rs`). It consumes every byte the shell prints. This
   is the highest-value untrusted-input surface and is continuously fuzzed (see
   [Fuzzing](#fuzzing)).
2. **The inline-image decoders** (`crates/core/src/image.rs`) — a dependency-free
   Sixel decoder and the Kitty graphics protocol decoder. Both are pure,
   `unsafe`-free functions that turn attacker-controlled bytes into RGBA pixels.
   They cap the decoded pixel count (`MAX_SIXEL_PIXELS` = 16 Mpx), use
   `checked_mul` to reject integer-overflow dimensions, and validate the declared
   payload length against the actual bytes. The Kitty transmit-store is capped at
   `KITTY_STORE_MAX_IMAGES` (64) and `KITTY_STORE_MAX_BYTES` (64 MiB) so an `a=t`
   upload stream cannot exhaust memory.
3. **The updater's network fetch** (`crates/app/src/update_engine/`) — the only
   place C0PL4ND reaches the network. It verifies every downloaded archive before
   anything is executed (see [Updater security](#updater-security)) and guards
   extraction against decompression bombs (`MAX_EXTRACTED_BYTES` = 256 MiB
   measured on the streamed output, `MAX_ARCHIVE_ENTRIES` = 64).

The egui application UI (`crates/app/src/egui_main.rs`,
`crates/app/src/egui_app/mod.rs`) is built with `#![forbid(unsafe_code)]`. The
core engine and a few platform modules contain small, audited `unsafe` blocks
for OS FFI (ConPTY / Win32 / winit / env-var mutation in tests) — the image and
OSC parsers themselves are `unsafe`-free.

---

## Anti-RCE design choices (what is deliberately absent)

A recurring class of terminal vulnerabilities lets a program that merely *prints*
to the terminal trick the emulator into *typing back* into the shell — turning
"display some text" into "execute a command." C0PL4ND closes that class by
**never** letting untrusted output drive an automatic reply into the PTY, and by
omitting the features that historically enabled it. The following are verifiable
in `crates/core/src/term.rs` and `crates/core/src/term/osc.rs`:

- **No answerback / ENQ reply.** There is no answerback string and no handler for
  the `ENQ` (`0x05`) control byte. A hostile program cannot make the terminal
  emit a canned string back into the shell.
- **No window-title reporting.** OSC `0` / `2` set an *internal* title that is
  shown only as the in-app tab/pane label
  (`crates/app/src/egui_app/pane_term.rs`). It is **never** reported back to the
  PTY, and it is **never** propagated to the OS window title — the OS window title
  is the fixed string `C0PL4ND` (`crates/app/src/egui_main.rs`). This removes the
  classic "set the title to a command, then ask the terminal to print it back"
  injection.
- **OSC 52 clipboard read is default-off.** OSC 52 *writes* are accepted (and
  handled carefully — see [Privacy](PRIVACY.md)), but a clipboard **read** request
  (`OSC 52 ; … ; ?`) is treated as a host-clipboard-exfiltration vector and is
  **dropped** unless the application explicitly opts in (`clipboard_read_enabled`
  defaults to `false`). The terminal never auto-responds with the contents of
  your clipboard.
- **OSC 133 semantic prompt marks are capture-only.** Shell-integration marks
  (`OSC 133 A/B/C/D`) are recorded for prompt navigation and command-status
  glyphs but are **never** reported back into the PTY — closing the
  iTerm2 CVE-2024-38395 / CVE-2024-38396 injection class.
- **Device replies are fixed constants.** Responses to capability/status queries
  (DA1, DA2, DSR/CPR, DECRQM, OSC color queries, XTGETTCAP) reflect only
  C0PL4ND's own fixed capabilities; none of them echo attacker-supplied data
  back as an executable command.
- **A global PTY-reply byte cap.** Every reply C0PL4ND queues toward the PTY
  flows through one bounded queue (`PTY_RESPONSE_MAX` = 64 KiB between drains). A
  hostile program that floods the terminal with status queries to amplify reply
  bytes (a write-amplification DoS) is capped: once the queue is full, further
  replies are dropped wholesale (never truncated mid-sequence, which would emit a
  malformed escape).
- **OSC 8 hyperlinks are scheme-restricted and capture-only.** The core accepts
  OSC 8 links only with `http` / `https` / `file` schemes (rejecting
  `javascript:`, `data:`, etc.) and caps each URI at 2 KiB. Links are *captured*,
  not auto-activated. In the egui app, the only links made clickable are plain
  `http://` / `https://` URLs detected in grid text
  (`crates/app/src/egui_app/hyperlink.rs`), and they open only on **Ctrl-click**
  via the OS opener.

---

## Updater security

The opt-in self-updater (`crates/app/src/update_engine/`) is the only component
that reaches the network. It is built to **fail closed**: a binary is applied
only after it passes both an integrity check and a signature check.

### Verify-before-swap

1. **Discovery.** A single unauthenticated `GET` to the public GitHub Releases
   API (`https://api.github.com/repos/{owner}/{repo}/releases/latest`) finds the
   newest release. Every download URL is asserted to be `https://`
   (`assert_https`) — an `http://` asset / signature / checksum URL is rejected.
2. **Checksum (catches corruption).** The downloaded archive's SHA-256 is
   compared against the release's `.sha256` sidecar (`verify_checksum`).
3. **Signature (catches tampering).** The archive is verified against a
   **minisign / ed25519** signature (`.minisig` sidecar) using the
   `EMBEDDED_PUBLIC_KEY` compiled into the binary
   (`crates/app/src/update_engine/verify.rs`). The matching secret key is a CI
   secret (`MINISIGN_SECRET_KEY`), never committed.
4. **Both must pass.** `verify_artifact` returns an error the moment either check
   fails; an unverified binary is never returned. On any failure the per-run
   staging directory is deleted.
5. **Atomic swap.** Only a verified binary is installed, via `self-replace`
   (which handles the Windows locked-running-executable rename-aside), keeping a
   one-prior backup for rollback (`crates/app/src/update_engine/apply.rs`).

The download is extracted into a **freshly created, per-run, owner-only staging
directory** created with `tempfile::Builder` (0700 on Unix via `mkdtemp`; an
owner-only location on Windows), which is removed after apply — closing a TOCTOU
window on a shared temp directory.

Until a release is signed with the matching secret key, the updater has no
signed artifact to accept and the UI reports that no verified update is
available — **it never installs an unsigned binary.** (If a release is published
without the `MINISIGN_SECRET_KEY` secret set, the release workflow ships
checksummed but **unsigned** artifacts, which the in-app updater rejects by
design.)

### Signed update manifest (Tier-1)

On top of the per-asset SHA-256 + minisign checks above, every release ships a
single **signed manifest** — `latest.json` plus a `latest.json.minisig`
signature produced by the **same** embedded key
(`crates/app/src/update_engine/manifest.rs`). The manifest is verified
**signature-first**: the minisign signature is checked over the raw JSON bytes
**before** the document is parsed, so a tampered or forged `latest.json` never
reaches the deserializer (`parse_and_verify`). It binds the release's identity —
product, schema, version, a strictly-monotonic `release_index`, and the expected
SHA-256 of every platform asset — into one signed document, catching attacks the
per-asset gates cannot (a MITM/compromised CDN that swaps in an older-but-genuine
asset, or replays a stale listing). It adds two further fail-closed gates:

1. **Anti-rollback floor (`minimum_version` + `release_index`).** An in-place
   update is refused when the running version is below the manifest's
   `minimum_version`, and the persisted high-water `release_index`
   (`update_state`) refuses any manifest that is not strictly newer — so a
   downgrade to an older signed release is rejected.
2. **Freshness window (`valid_until_utc`).** Each manifest carries a freshness
   deadline; after that instant it is treated as a frozen/stale beacon and
   refused — the anti-freeze defense against an attacker pinning a client on a
   vulnerable version. An unparseable date is never "fresh."

### Key-rotation runbook (minisign)

The signing public key is **embedded in the binary** at build time
(`EMBEDDED_PUBLIC_KEY`). Because an already-installed copy can only verify
updates against the key it was built with, rotating the key naively would brick
auto-update for every existing install (their binaries would reject everything
signed by the new key). To rotate without bricking auto-update:

1. **Generate the new keypair** with `rsign generate -W` (rsign2 — the same tool
   the release workflow signs with). Keep the new secret key outside the repo.
2. **Ship a transitional release signed by BOTH keys.** Publish a release whose
   binary embeds the **new** public key but whose release **artifacts are signed
   by the OLD key** (the key every currently-installed copy already trusts).
   Existing installs verify that release with the old key, accept it, and swap in
   a binary that now trusts the new key.
3. **Cut over.** Every subsequent release is signed by the **new key only**.
   Installs that took the transitional release now verify against the new key;
   the old key can be retired.
4. **Update CI and source.** Replace the `MINISIGN_SECRET_KEY` Actions secret
   with the new secret, and update `EMBEDDED_PUBLIC_KEY` in `verify.rs` to the
   new public key in the same release that introduces the transitional build.

### Key-compromise response

If the signing secret key is believed compromised:

1. **Revoke and rotate immediately** — generate a fresh keypair and treat the
   compromised key as untrusted from that point.
2. **Cut a new signed release** using the transitional dual-sign procedure above
   so existing installs can move to the new key. Where the old key cannot be
   trusted to sign the transitional release, users must reinstall from a freshly
   downloaded, manually verified build (verify SHA-256 + signature by hand
   against an out-of-band channel).
3. **Publish a security advisory** describing the affected versions, the new
   public-key fingerprint, and the recommended action.
4. **Rotate the CI secret** and audit release-workflow access.

---

## Supply chain & build provenance

C0PL4ND's production dependencies are exact-version-pinned (see
`crates/app/Cargo.toml`), and the following gates run in CI
(`.github/workflows/`):

| Gate | Workflow | What it enforces |
| --- | --- | --- |
| Build + test matrix | `ci.yml` | `cargo nextest` across Windows / Linux / macOS; `cargo clippy -D warnings`; `cargo fmt --check`. |
| `cargo-deny` | `ci.yml` | Advisory, license, and ban checks (`deny.toml`). Yanked crates are denied. |
| `cargo-audit` | `ci.yml` | RustSec advisory scan of the dependency tree. |
| CodeQL | `codeql.yml` | Static analysis for code-level vulnerabilities. |
| OpenSSF Scorecard | `scorecard.yml` | Repository security-posture scoring. |
| Dependency review | `dependency-review.yml` | Flags vulnerable / disallowed-license dependency changes on PRs. |
| No-network gate | `no-network-gate.yml` | A source grep that **fails the build** if any network call site (`ureq`, `reqwest`, raw sockets, `std::net`, `tokio::net`) appears outside the two opt-in updater modules — enforcing the zero-egress invariant. |
| Fuzz bitrot + smoke | `fuzz.yml` | Compiles the `cargo-fuzz` targets on nightly and runs a short smoke campaign so the harness cannot rot. |

### Build provenance

- **SLSA build provenance** — every shipped binary and installer is attested via
  `actions/attest-build-provenance` (SLSA v1 Build L2 on the public-good Sigstore
  instance), verifiable with `gh attestation verify` (`release.yml`).
- **CycloneDX SBOM** — a workspace SBOM is generated with `cargo-cyclonedx` and
  published per release (`release.yml`).
- **Signed release artifacts** — each release asset is signed with minisign
  (rsign2) when the signing key is provisioned, plus a `.sha256` sidecar; the
  in-app updater verifies both before applying.

The Windows installer is additionally signed via SignPath Foundation when the
`SIGNPATH_ORG_ID` repository variable is configured (unsigned otherwise).

---

## Fuzzing

The highest-value untrusted-input surface is the **VT / ANSI / OSC
escape-sequence parser**, which consumes arbitrary bytes from any program running
inside the terminal. C0PL4ND fuzzes it (and the persisted-state JSON loader)
continuously.

- **Harness:** [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz)
  (libFuzzer). The fuzz crate lives in [`fuzz/`](fuzz/) and is `exclude`d from the
  default workspace so the stable CI matrix is unaffected.
- **Targets:**
  - `vt_parser` ([`fuzz/fuzz_targets/vt_parser.rs`](fuzz/fuzz_targets/vt_parser.rs))
    drives `c0pl4nd_core::Terminal::advance` — the same entry point the live PTY
    reader uses — then reads back the public surface to catch state
    inconsistencies.
  - `state_json` ([`fuzz/fuzz_targets/state_json.rs`](fuzz/fuzz_targets/state_json.rs))
    fuzzes the persisted-state JSON loader.
- **CI:** `fuzz.yml` compiles both targets on a nightly toolchain (bitrot guard)
  and runs a short smoke campaign (`-max_total_time=10`) per target on a
  schedule. A deterministic seed-corpus regression test
  (`parser_survives_adversarial_escape_sequences` in `crates/core/src/term.rs`)
  runs in the ordinary `cargo test` suite on every platform, so a crash
  regression is caught even without the nightly toolchain.

Run a campaign locally (requires a nightly toolchain):

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run vt_parser
cargo +nightly fuzz run state_json
```

---

## Scope

In scope:

- Memory-safety issues, crashes, or undefined behavior in the terminal core.
- Escape-sequence / image-decoder handling that can lead to code execution, file
  access, or data exfiltration.
- Any path by which terminal contents or input could leave the device
  unexpectedly.
- Updater-integrity bypasses (signature / checksum) or privilege-escalation
  vectors introduced by C0PL4ND.

Out of scope:

- Vulnerabilities in third-party shells, programs, or OS components run inside
  the terminal.
- The fact that the spawned shell runs with the user's privileges (this is the
  product's purpose; see [Threat model](#threat-model--security-posture)).
- Social-engineering or physical-access attacks.

---

Thank you for helping keep C0PL4ND and its users safe.
