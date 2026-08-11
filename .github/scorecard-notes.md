# OpenSSF Scorecard — remediation notes

This file documents how each OpenSSF Scorecard check is addressed for
`C0PL4ND`, including the findings that are resolved in code, the
ones that resolve automatically, and the ones that require a manual external
action.

> Scorecard findings are produced by the scheduled `scorecard.yml` workflow and
> uploaded to GitHub code scanning. **Alerts do not close instantly** — they
> auto-close on the next scheduled Scorecard scan after the fix lands on the
> default branch (typically within ~1 week, or sooner if the workflow is
> dispatched manually).

## Findings addressed in code

### `Vulnerabilities` (RUSTSEC-2017-0008)

- **What**: The unmaintained [`serial`](https://rustsec.org/advisories/RUSTSEC-2017-0008.html)
  crate is present in the dependency tree.
- **Severity**: RUSTSEC-2017-0008 is an **`unmaintained` informational advisory**,
  not a memory-safety or exploitable-vulnerability advisory. `serial` is no
  longer maintained; it has no known exploitable defect.
- **Root cause**: It is pulled in transitively by `portable-pty 0.8.1`
  (Windows serial-port handling). C0PL4ND does not use the serial-port API
  surface — it uses `portable-pty` only for ConPTY/openpty shell spawning.
- **Why the real upgrade is blocked**: The next `portable-pty` release that
  drops `serial` (for the maintained `serial2`) is `0.9.0` — but **`0.9.0` has
  an open, unfixed Windows regression**: `pty.read` returns garbage/empty
  output on ConPTY, breaking the terminal's core read path on Windows. See
  upstream [wezterm/wezterm#6783](https://github.com/wezterm/wezterm/issues/6783)
  (status: **open**, no fixed release published — `0.9.0` is the latest version
  on crates.io as of this writing). This was reproduced locally: under `0.9.0`
  the PTY round-trip test produces empty output and the child-wait blocks.
  Merging the `0.9.0` bump would trade an informational advisory for a broken
  Windows terminal, which is not an acceptable exchange for the product's
  primary platform.
- **Decision**: **Retain the justified `[advisories] ignore = ["RUSTSEC-2017-0008"]`**
  in `deny.toml` (with the rationale comment), since the advisory is
  informational and the only fix version regresses the core product on Windows.
  This satisfies the "unmaintained-only advisory with no usable fix → justified
  ignore" path. **Re-evaluate when `portable-pty > 0.9.0` ships with the
  wezterm#6783 ConPTY fix** — at that point bump the dependency and remove the
  ignore.
- **Verification**: `cargo audit` reports this advisory among the *allowed
  warnings* (the ignore is honoured); `cargo deny check` passes. No
  memory-safety or exploitable advisory is present.
- **Full ignore list (`deny.toml` `[advisories] ignore`)**: six justified
  *informational, no-usable-fix* advisories are suppressed — `serial`
  (RUSTSEC-2017-0008), `proc-macro-error2` (RUSTSEC-2026-0173), `paste`
  (RUSTSEC-2024-0436), `rsa` (RUSTSEC-2023-0071, no upstream patch yet),
  `ttf-parser` (RUSTSEC-2026-0192), and `bincode` (RUSTSEC-2025-0141). Each is
  either a build-time-only proc-macro, a transitively-pulled unmaintained crate
  with no maintained drop-in, or (for `bincode`) feature-gated out of the build
  graph entirely. The previously-suppressed `anyhow` (RUSTSEC-2026-0190) and
  `memmap2` (RUSTSEC-2026-0186) advisories were **removed** from the ignore list
  once both were bumped to patched versions — they are resolved in the locked
  graph, not suppressed.

### `Fuzzing`

- **What**: Scorecard reports no fuzzing integration.
- **Fix**: Added a `cargo-fuzz` crate (`fuzz/`) with a `vt_parser` target on the
  VT/ANSI/OSC escape-sequence parser — the highest-value untrusted-input
  surface for a terminal emulator (Scorecard detects the `fuzz_target!`
  integration). The crate is `exclude`d from the stable workspace because
  libFuzzer needs a nightly + sanitizer toolchain that must not pollute the
  stable `cargo build`. A dedicated `Fuzz Build` CI job compiles + smoke-runs
  every target on nightly (pinned to the gnu target, since ASAN is incompatible
  with the musl-static `cargo-fuzz` default), and it is also runnable locally
  (`cargo +nightly fuzz run vt_parser`). The crate is structured for OSS-Fuzz
  onboarding (documented in `SECURITY.md`). A
  deterministic adversarial-seed regression test mirrors the fuzz seeds and runs
  in the stable `cargo test` suite on every platform / every PR.

### `Code-Review` / `Branch-Protection`

- **What**: Scorecard rewards enforced PR review on the default branch.
- **Fix**: Branch protection is configured on `main` requiring **1 approving
  review**, with **admin/owner bypass enabled** (this is a solo-maintainer
  project; enforce-on-admins is intentionally left off so the maintainer is not
  locked out). See "Branch protection state" below for the exact applied
  settings. If the GitHub API rejects the configuration (e.g. on a plan that
  does not support branch protection for the repo visibility), the fallback is
  to rely on the required status checks already enforced by the `CI Gate`
  aggregating job.

## Findings that resolve automatically

### `Maintained`

- **What**: An activity heuristic — Scorecard scores a project higher when it
  has had commits / released activity within the trailing 90 days.
- **Action**: **None required.** This project is under active development; the
  ongoing commit and PR activity (including this remediation) satisfies the
  heuristic. The score updates on the next scheduled scan. No code change can
  or should be made for this check.

## Findings deferred to an external manual action

### `CII-Best-Practices` (OpenSSF Best Practices badge)

- **What**: Scorecard awards points when the project holds an
  [OpenSSF Best Practices](https://www.bestpractices.dev/) badge (formerly the
  CII Best Practices badge).
- **Why deferred**: Obtaining the badge requires a **manual, interactive
  self-certification** on an external site that cannot be automated from code or
  CI. The maintainer must sign in and complete the questionnaire.
- **Action (manual, maintainer)**:
  1. Go to <https://www.bestpractices.dev/> and sign in with GitHub.
  2. Add a new project: <https://www.bestpractices.dev/en/projects/new>
  3. Enter the repository URL: `https://github.com/46b-ETYKiAL/C0PL4ND`
  4. Complete the "passing" criteria (most are already satisfied: OSS license,
     `SECURITY.md`, public VCS, automated test suite, static analysis via
     CodeQL + clippy, dependency vetting via `cargo deny`/`cargo audit`).
  5. Once awarded, add the badge markdown to `README.md`.
- **Status**: **Deferred-external.** No in-repo change closes this check; it is
  tracked here so the requirement is not lost.
