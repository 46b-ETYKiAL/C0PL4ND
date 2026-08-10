# Changelog

All notable changes to C0PL4ND are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Full per-release artifacts (signed binaries, SBOMs, provenance) are on the
[GitHub Releases](https://github.com/46b-ETYKiAL/C0PL4ND/releases)
page.

## [Unreleased]

### Changed — config schema v2 → v3 (one-time, rewrites `config.toml`)

- **The GitHub repository was renamed (`Itasha.Corp_C0PL4ND` → `C0PL4ND`), and a
  one-time config migration re-points the stored issue-intake repository.** This
  is user-visible because it rewrites state on disk, so the mechanism is worth
  stating plainly:
  - Every field of `config.toml` is `#[serde(default)]`, so a value *stored* in
    your file always beats the source default. `Config::save_to` serializes every
    field (there is no `skip_serializing_if`) and runs on ordinary actions such
    as persisting window geometry — so essentially every existing user already
    has the old repository name written into their file. Changing the compiled
    default alone would therefore never have reached them, and their "Report an
    issue" link would keep depending on GitHub's rename redirect indefinitely.
  - On first load under this build, a config below schema v3 is stamped to v3 and
    its `reporting.issue_intake.repo` is re-pointed **only when it is exactly the
    old shipped default** `46b-ETYKiAL/Itasha.Corp_C0PL4ND`. **Any other value —
    an operator pointing the issue form at their own fork — is preserved
    verbatim.** The step is one-shot: once `schema_version` reaches 3 it never
    runs again, so a value you set deliberately afterwards is never re-touched.
  - Known and accepted limitation: a config that *deliberately* pins the old
    name is byte-identical to one that merely persisted the old default, so it is
    indistinguishable and will be re-pointed once. Re-set it after upgrading and
    it will stick.
  - Because a migration marks the config dirty, the next save rewrites the file.
    Keys that this build no longer recognises (notably the retired
    `transparency_enabled` / `window_mode` / `acrylic` transparency model,
    collapsed into `opacity` in v0.4.21) are dropped from the file at that point.
    They were already inert on load; the rewrite is when they stop being stored.
- **A config written by a *newer* build is left alone.** A `schema_version` ahead
  of this build's is never lowered and never migrated down.

### Fixed

- **The one-line installer died silently when a release asset was missing.**
  `packaging/linux/install.sh` fetched the release archive with no failure
  handler, so under `set -eu` a 404 killed the script immediately after printing
  `Downloading …` — no cause, no next step, indistinguishable from a hang. (The
  checksum and signature downloads beside it always had that handler; the archive
  download did not.) It now names the URL and says what to check. This is the
  failure shape that made an earlier asset-contract change look like a hung
  terminal rather than a broken install.
- **The Homebrew cask pointed at DMG assets that never existed.** The formula
  asked for `c0pl4nd-<tag>-aarch64-apple-darwin.dmg`, but the release workflow
  publishes `c0pl4nd-<tag>-aarch64.dmg` (arch, not the full target triple), so
  both URLs 404'd — verified against the live release. A correct repository name
  pointing at a nonexistent asset is the same broken install in a new place.
- **The release-drift check could fail without saying why, and could pass
  without checking.** Two defects in one step: (a) under `set -euo pipefail`, a
  `Cargo.toml` with no top-level `version` line made `grep` exit non-zero and the
  failing command substitution killed the step *on the assignment* — before the
  explicit "could not read the workspace version" error could run, so the step
  exited 1 having printed nothing at all and the guard was unreachable code;
  (b) `gh release view … 2>/dev/null || true` collapsed *every* failure mode
  (expired auth, rate limit, network, egress policy, `gh` absent) into the
  "no releases published yet, nothing to compare" vacuous pass. Since the job
  runs on a schedule, a persistent auth fault would have made it silently inert
  while still logging a green premise that was false. Release-query failures now
  fail loudly, matching the tag-existence probe in the same file, which already
  failed closed.

### Added

- **An end-to-end installer check (`installer-e2e.yml`).** It runs the real
  `install.sh` against the real latest published release on a clean runner and
  then executes the installed binary, because verifying that release *assets*
  exist and verify — which `post-release-verify.yml` already does — cannot catch
  an installer that is unable to consume them. It carries a permanent negative
  control that asserts the installer refuses a nonexistent release *with a
  diagnosis*, so the job proves it can fail on every run rather than on the day
  someone checked by hand.

## [0.4.25]

### Fixed

- **Clear "too old — reinstall" update message.** When an installed build is
  below the update manifest's `minimum_version` floor, the updater refuses the
  in-place hop by design. That refusal was being shown with the generic
  "Couldn't confirm the update details from GitHub. Try again later…" copy —
  misleadingly implying a transient error. It now shows a clear, actionable
  message: *"This installed version is too old to auto-update in place. Download
  the latest installer from the official GitHub releases page and reinstall once —
  automatic updates will work normally afterward."*

## [0.4.24]

### Changed — release assets trimmed to the signed-manifest set (~13)

- **Dropped the per-binary `.minisig` and `.sha256` sidecars.** A binary's
  authenticity now rests on the minisign-signed `latest.json` manifest (which
  pins each asset's SHA-256 — what the in-app updater checks) and the signed
  `SHA256SUMS` (for manual verification). The release asset list drops from ~27
  to ~13.
- **Raised the manifest `minimum_version` to 0.4.23** (the first sidecar-tolerant
  updater). A client older than that — whose updater still requires the per-binary
  sidecars — is now told "a fresh install is required" (a graceful message shown
  *before* it looks for a now-absent sidecar) instead of a cryptic failure.
  Clients on v0.4.23+ verify via the signed-manifest hash and auto-update
  seamlessly. **If you are on an older build, download the latest installer once
  from the releases page; auto-update works normally from v0.4.23 onward.**
- `post-release-verify` now verifies each published asset via the signed-manifest
  SHA-256, tolerating an absent per-asset `.minisig` (mirroring the updater).

## [0.4.23]

### Changed — leaner release assets + auto-update verification

- **Auto-update now verifies against the signed manifest hash, tolerating an
  absent per-artifact `.minisig`/`.sha256` sidecar.** The `latest.json` manifest
  is minisign-verified and pins each asset's SHA-256; the updater treats that pin
  as the authoritative integrity+authenticity gate and verifies the per-artifact
  `.minisig` only when present (a present-but-invalid signature still fails
  closed). This loses no security — a substitution is impossible without a SHA-256
  preimage, and a forged manifest is impossible without the release key — and lets
  future releases ship a smaller asset set. Deployed clients are unaffected (the
  sidecars are still published).
- **Trimmed the release asset list.** Dropped the redundant `*.sha256.minisig`
  (a signature over a checksum — the signed `SHA256SUMS` already covers it) and
  the SBOM / build-provenance `.minisig` (covered by the release's SLSA
  attestation), and consolidated the three per-crate SBOMs into the single
  top-level `c0pl4nd.cdx.json` (the dependency superset). The per-binary
  `.minisig` + `.sha256` remain for now so already-deployed clients keep
  auto-updating; they are removed in a later release once the manifest-hash
  updater has aged in.

## [0.4.22]

### Added

- **Always-on-top window toggle.** A new window option keeps C0PL4ND pinned
  above other windows (SCR1B3 parity).

### Changed — dependencies

- Batched dependabot updates (codeql-action 4.37.0, pollster 1.0, and the cargo
  minor-patch group: regex 1.13.1, toml 1.1.3, wgpu 29.0.4, …). The GPU stack
  (eframe / egui / wgpu / winit / glyphon) is deliberately held as one
  coordinated set — glyphon `< 0.12` and wgpu `< 30` — so a same-generation
  patch never splits the single shared `wgpu::Device`.

### Internal — test & CI honesty hardening

- Raised coverage (core ≈97%, app line floor 78 → 80) and made three silently
  vacuous gates real: cargo-mutants now reads its config (relocated to
  `.cargo/mutants.toml`), nextest fails rerun-masked flakes (`flaky-result =
  "fail"`, proven by a live bidirectional canary), and the reporting-coverage
  job measures the right target (`--lib`, not `--bin`).
- The `CI Gate` aggregator now transitively requires the flaky canary and the
  visual-QA render job, so a future silent disarm of either blocks the merge.
- Reconciled `docs/control-test-ledger.md`: repointed seven ✅ rows that cited
  non-existent test names to the real tests, and replaced a vacuous
  equivalent-mutant test with an honestly-documented defensive one.

## [0.4.21]

### Added — frosted glass

- **Software "frosted glass" wash.** A new Settings → Appearance → Frosted glass
  toggle adds an adjustable diffuse frost over the window, with a Frost amount
  slider (0–100%, capped so text stays legible), a frost colour picker (defaults
  to the theme background), and an optional procedural Grain texture. It is
  independent of the Opacity slider (works at any opacity). Honest by design: it
  tints/diffuses the window; it does not blur the desktop behind the window (a
  real backdrop blur is not possible on this hardware).

### Changed — window look

- **Opacity is now clean and linear.** The terminal background was being painted
  twice and the two opacity alphas compounded (≈opacity²), so a mid-opacity window
  looked far hazier than the slider suggested and never went truly clear. The
  background is now painted once, so the Opacity slider is linear — clear glass at
  low values, solid at 100%.
- **Tint works at any opacity.** The colour tint no longer fades with the Opacity
  slider — it colours the see-through glass regardless of how transparent the
  window is. Opacity (glass clarity), Tint (colour), and Frost (diffuse wash) are
  now three independent controls; a fully-clear window is tint-off + frost-off.
- **All Settings dropdowns are now fixed-width with an up/down stepper.** Every
  combo (theme, fonts, cursor, graphics, update, …) is a constant width with a
  compact ▲/▼ spinner to its right that cycles the options with wrap-around — the
  dropdown and its stepper no longer move as the selected value's length changes.

### Changed — window transparency simplified to one effect

- **One Opacity slider, no mode selector.** The window is now always
  transparent-capable and a single **Opacity** control (0% = fully see-through,
  100% = solid) drives it. The `WindowMode` dropdown and every extra mode —
  Opaque, Dim, Glass, Mica, Vibrancy, and the `acrylic` backdrop — were removed:
  on hybrid-GPU (Optimus) laptops the OS blur backdrops never composited (they
  looked identical to plain Transparent), Dim rendered black, and the separate
  Opaque mode rendered black. The one portable per-pixel effect that works is now
  the only one. Opacity applies live.
- **Opacity 0 is now maximally see-through.** The resting chrome (toolbar
  buttons, tab chips, title bar) now fades with the opacity alpha along with the
  panes, so at 0% only the glyph text remains over the desktop. Hover/active
  states, the scrollbar handle, popups, tooltips, and the Settings window stay
  opaque for feedback and legibility. (Ports the SCR1B3 v0.4.59 fade-chrome
  behaviour.)
- **Old configs keep loading.** The retired keys (`transparency_enabled`,
  `window_mode`, `acrylic`) are ignored on load; the retained `opacity` value
  carries the see-through level.

### Fixed

- **UI-scale slider no longer runs away.** Dragging Settings → Appearance →
  Interface scale used to flicker small↔big and shoot to the 3.0 maximum, leaving
  the UI gigantic: the slider applied the zoom live every frame, rescaling itself
  under the cursor. The scale now applies only when you release the slider (or on
  a click / keyboard step), so the drag stays controllable.
- **Opacity 0 is now genuinely clear (no frosted wash).** With a tint enabled, the
  colour wash painted at a fixed alpha regardless of opacity, so a maximally
  see-through window (opacity 0) still showed a uniform frosted haze. The tint —
  and the ambient background effects (wired mesh / VHS / flicker) — now fade
  proportionally with the window opacity, so at 0% only the terminal glyph text
  remains over the desktop.
- **Ambient effects no longer cover popups.** The wired-mesh and other motion
  overlays rendered on the same layer order as popups and painted over the tint
  colour-picker; they now render strictly behind all popups, menus,
  color-pickers, and tooltips.

## [0.4.14] - 2026-07-03

### Fixed — rendering

- **Terminal-grid glyph garble eliminated.** Intermittently on some NVIDIA DX12
  drivers the grid text rendered as garbled or blank glyphs — a DX12 font-atlas
  `write_texture`→sample hazard (wgpu#1306 / #6829, DX12-only). Two-part fix: an
  atlas-warmup GPU fence that guarantees the font atlas is uploaded and resident
  before the grid draws (including across the off-thread system-font swap), and —
  the definitive fix — **Vulkan is now the default GPU backend on Windows**, which
  is immune to the hazard. DX12 remains one setting away (Settings → Window →
  Graphics backend, or `WGPU_BACKEND=dx12`) for anyone who hits a Vulkan
  overlay-layer issue.
- **"Make panes symmetrical" now produces an even grid.** It rebuilds the layout
  into a uniform grid for any pane count, instead of only rebalancing shares
  within existing splits (which left a nested/asymmetric layout uneven).

### Added — customizable toolbar

- **Settings → Toolbar.** The top-bar quick-action buttons (view-toggle,
  equalize, shell-switcher, script-launcher) are now fully customizable: place
  each on the LEFT (after the +), on the RIGHT (next to the settings gear), or in
  an overflow "…" menu — or hide it — and reorder within a zone. The script
  launcher now sits on the right by the gear by default.

### Changed — performance

- **Instant multi-pane close.** Closing the window no longer stalls while shells
  tear down: every pane's shell is killed in one pass and the blocking
  `ClosePseudoConsole` teardown is skipped on exit, with a Windows Job Object
  (`KILL_ON_JOB_CLOSE`) guaranteeing no orphaned `conhost`/`cmd` processes.
- Font-zoom (Ctrl+scroll) config writes are now debounced into a single write,
  and terminal synchronized-output (DEC `?2026`) is honored so full-screen TUIs
  repaint without tearing.

### Fixed — window transparency & tint

- **Transparency now covers the whole window.** The titlebar and status bar were
  opaque even with transparency on, so only the pane backgrounds went
  see-through. They now fold in the same opacity as the panes, so the entire
  window — chrome included — is translucent.
- **The color tint no longer discolors text or the Settings window.** It was a
  full-window overlay painted over everything; it is now a single wash on the
  background layer that shows through the panes, gaps, and top-bar buttons
  uniformly, while the terminal text and the (opaque) Settings window stay their
  true colors.
- **Opacity can go fully transparent.** The opacity slider floored at 15%; it now
  runs the whole 0–100% range for a much more see-through background (the text
  stays readable at its own opacity).
- **Top-bar buttons fit the bar again.** Once the titlebar went translucent, every
  control kept its own opaque background and read as a floating chip. The chrome
  buttons are now flat — no idle background, with a subtle fill only on hover or
  press — the standard for a translucent titlebar (Windows Terminal, VS Code,
  macOS vibrancy, libadwaita `.flat`), so they sit on the bar instead of over it.
- **Pane dividers now respect transparency and tint.** The seams between split
  terminals were painted as opaque bars that stayed solid regardless of the
  window opacity/tint. The divider is now negative space — the gap shows the same
  translucent, tinted background as the panes — and each pane's border folds in
  the window opacity, so a focused pane still reads clearly while the seams blend
  into the see-through window.
- **No more doubled window buttons (Windows).** With transparency on, the OS drew
  its own native minimize/maximize/close over the app's custom ones — a second,
  offset set. The native minimize/maximize are now suppressed at window creation
  and the native close button is removed, so only the app's own controls show.
  Alt+F4 and the taskbar "Close window" still close the window as usual.
- **The maximize button now reflects the window state.** It showed the same
  "maximize" square whether the window was maximized or not; it now switches to a
  "restore" glyph while maximized, matching the standard caption-button behavior.
- **Restoring from maximized returns to a normal, centered window.** Un-maximizing
  used to snap the window to nearly the full monitor size (so you had to shrink it
  by hand to move it); it now restores to your last un-maximized size — or a sane
  default on first use — and re-centers on the monitor.

### Changed — settings layout

- **The Settings window is clearer to read and use.** Every on/off row is now a
  real checkbox instead of click-to-toggle text (it was not obvious the labels
  were clickable), and the longer sections are split into labelled sub-groups —
  each with a heading, a one-line description, and a divider — so related options
  (Theme, Transparency & tint, Interface scale, Shell & scrollback, Clipboard,
  window Layout, Graphics) group together instead of running into one list.
- **Every settings tab now lines up the same way.** All pages — including Privacy,
  which previously used a different, looser layout — share one three-column grid
  (label · control · reset), so labels, controls, and the ↺ revert buttons align
  consistently as you move between tabs.
- **The Settings window is resizable, and remembers its size and position.** Drag
  any edge to resize it or the title area to move it; both the size and the
  position are saved and restored on the next launch. It also no longer gets
  shifted or clipped when the main window is maximized.
- **Settings now opens centered.** The first time you open it (before you've moved
  it), it appears dead center over the app instead of tucked into the top-left
  corner; after that it reopens wherever you last left it.

### Added — window management

- **The main window resizes from every edge and corner.** The frameless window
  gave no way to drag-resize it before. You can now grab any edge or corner: the
  right/bottom edges grow the window in place, and the left/top edges (and their
  corners) move the opposite side so the window grows toward your pointer. A
  minimum size keeps it from collapsing, and the size is remembered across
  launches.
- **The app name is now a window drag-handle.** Dragging the two-tone "C0PL4ND"
  wordmark in the top-left used to highlight the text; it now moves the window
  like the rest of the titlebar (double-click still maximizes / restores), while
  still showing the name in its brand colors.

### Changed — tabs & CRT effect

- **The CRT scanline effect is calmer.** It used to sweep a bright white bar down
  each pane, which read as a distracting flash. The crisp scanlines stay, but the
  whole field now drifts down slowly for a gentle CRT shimmer (matching the SCR1B3
  editor's effect) instead of the bright rolling bar.
- **Tab overflow now uses ‹ › step arrows instead of a scrollbar.** When you have
  more tabs than fit, chevron buttons appear on either side of the strip and step
  the active tab to the previous/next one, scrolling it into view — clearer than
  a thin horizontal scrollbar.
- **Hovering a tab previews its terminal.** Rest the pointer on a tab to see the
  last few lines of that pane's output in a small popup, so you can tell inactive
  panes apart without clicking into each one.

### Added — motion & visual effects (SCR1B3 parity)

- **A new "Motion" settings category** collects every animation and retro
  post-effect in one place, mirroring the SCR1B3 editor: a master **"Enable
  animations"** switch (turn it off for a fully static UI) with an animation-speed
  slider, plus the CRT scanlines and chromatic-aberration controls (moved here
  from Appearance) and the new effects below. Everything is gated behind the
  master switch and off by default, so the shipped look is unchanged until you opt
  in.
- **New optional CRT/ambient effects ported from SCR1B3:** a subtle screen
  **flicker**, **VHS tracking lines** that sweep down the window, an animated
  **wired node-mesh** ambient background (with a density slider), a **cursor
  ghost-trail** that echoes the terminal cursor as it moves, and a one-shot
  **boot-glitch** sweep on launch. All are GPU-free, honor reduced-motion, and are
  off by default.

### Changed — window controls & settings organization

- **Window buttons now light up on hover like SCR1B3's.** The minimize and
  maximize/restore buttons brighten on hover and the close button turns
  Windows-standard red with a white ✕, so C0PL4ND and SCR1B3 share one
  window-control language. The resting look is unchanged (flat, translucent).
- **Settings categories match SCR1B3's order and naming.** "Font" is now "Fonts",
  the new "Motion" category sits between Toolbar and Keybindings, and Updates now
  comes before Privacy — so the two apps' Settings read the same way.

### Added — bundled fonts (SCR1B3 parity)

- **C0PL4ND now ships SCR1B3's full font set** — the monospace faces
  (JetBrains Mono, Fira Mono, IBM Plex Mono, Source Code Pro, Space Mono, and more)
  plus the lore/influence-inspired display faces (Michroma, Syncopate, Wallpoet,
  Zen Dots, Chakra Petch, Rajdhani, Teko, Major Mono Display, Doto, Saira) and a
  Japanese fallback. Every face is bundled (all-OFL/open-license) and picks in
  Settings → Fonts → Family regardless of what's installed on the machine, so both
  apps offer the same typography.

### Changed — visual-effect polish

- **CRT scanlines now read as SCR1B3's clean drifting lines** rather than a shifting
  shadow-film — the dark bands were thinned so distinct lines slide down the pane.
- **The animation-speed slider now visibly governs every motion effect.** It scales
  the drift clock of the scanlines, wired mesh, VHS tracking, and flicker (from
  frozen at 0 to full speed at 1); the cursor trail and boot sweep keep their
  event-anchored timing.
- **The wired node-mesh is now actually visible** on every pane (it previously hid
  behind opaque pane backgrounds), and raising the density visibly thickens the web.
- **The cursor ghost-trail is bolder and gains an intensity slider** — tune it from
  a faint flick to a pronounced comet tail (Motion → Cursor-trail intensity).
- **Tab step-arrows light up on hover** and the tab strip's horizontal scrollbar is
  hidden (the ‹ › arrows handle overflow), and the right-side top-bar buttons now
  sit at a uniform spacing so no pair looks squished.

### Changed — motion range & live preview

- **The motion sliders now reach much further.** Animation speed goes up to 2×
  (above 1× only accelerates the drift effects — the UI fades stay snappy), the
  screen flicker and node-mesh density each run their full range, and the
  cursor-trail intensity extends to a long, unmistakable comet tail. The shipped
  defaults are unchanged.
- **Motion effects now preview live while Settings is open.** The flicker, VHS
  lines, wired mesh, and cursor trail used to switch off entirely whenever a
  centered panel (Settings, command palette, paste confirm) was open — so tuning a
  Motion slider looked like it did nothing until you closed Settings. They now
  paint everywhere *except* the open panel, so you see the effect change on the
  terminal in real time while the panel itself stays clean.

### Added — appearance controls

- **Node-mesh brightness slider (Motion).** A new brightness control sits next to
  the mesh density: dim the wired lattice toward invisible or brighten it so it
  clearly pops, independent of how many nodes it draws.
- **The app logo now tints with the theme.** The "4ND" half of the two-tone
  C0PL4ND wordmark follows the active theme's accent (the "C0PL" half stays the
  fixed brand purple), so the top-bar logo picks up the palette you pick. On a
  theme without an accent it keeps the original brand green.
- **Theme up/down step arrows.** Small ⏶/⏷ buttons next to the theme picker step
  through the built-in themes without opening the dropdown — the same quick-step
  pattern as the tab arrows.

## [0.4.13] - 2026-06-30

### Added — macOS and ARM64 builds

- **Releases now ship for all six desktop targets**: Windows, Linux, and macOS,
  each in x64 and ARM64. macOS builds are not yet Apple-notarized (a certificate
  is pending) — see the README/TROUBLESHOOTING for the one-time Gatekeeper step.
  Update security is unchanged: every asset and the update manifest are
  minisign-signed and SLSA-attested, and verified before installing.

### Changed — supply-chain hygiene

- Resolved two dependency advisories at the source instead of suppressing them:
  `anyhow` is on the patched 1.0.103 (RUSTSEC-2026-0190) and `memmap2` was
  bumped to the patched 0.9.11 (RUSTSEC-2026-0186); both ignore entries dropped.

### Fixed — docs + internals

- Documented the GPU requirement (no software-render fallback) and the
  unsigned-installer/SmartScreen and macOS-Gatekeeper steps.
- Internal: the frame-scheduling policy (render-on-damage vs continuous) is now
  the typed `FramePolicy` contract shared with the renderer crate. No
  user-visible change.

## [0.4.12] - 2026-06-30

### Added — update diagnostics

- **Structured local diagnostics on the updater's security path.** The in-app
  updater now records, to the local diagnostic log only, what happened when an
  update is checked, downloaded, verified, and applied — including the specific
  reason an update was refused (checksum mismatch, signature failure, an
  insecure transport, or a downgrade attempt). These records carry only
  non-identifying detail (no URLs, tokens, or payload contents) and are never
  sent anywhere — C0PL4ND remains telemetry-free.

### Fixed — release verification

- The independent post-release verification job now runs automatically on every
  published release (previously it had to be started by hand). Each release's
  signed manifest, per-asset signatures, and checksum bindings are re-checked
  against the embedded key as soon as the release publishes.

### Changed — dependencies

- `anyhow` updated to 1.0.103; pinned GitHub Actions kept current. `egui` 0.35 /
  `egui_tiles` 0.16 are intentionally held until the icon-font dependency
  supports egui 0.35.

## [0.4.11] - 2026-06-30

### Changed — clearer error messages

- **User-visible error messages rewritten** to be plain-language and actionable,
  and to never expose internal details. Errors across the terminal, settings,
  updater, config/theme loading, and the `update` CLI now say what happened and
  what you can do about it, with the technical detail kept in local diagnostic
  logs instead of the message. No raw error chains, file paths, hostnames, or
  internal identifiers appear in any error you see.

### Added — release-pipeline hardening (Tier 2)

- Build-provenance (SLSA) attestation now covers **every** published asset,
  including the signed update manifest. An independent post-release verification
  job re-downloads the published assets and re-checks every signature, checksum,
  and the manifest binding against the embedded key — so a release can never
  publish an artifact the deployed app cannot verify.

## [0.4.10] - 2026-06-29

### Changed — update-security hardening

- **Signed update manifest (identity binding).** Each release now publishes a
  signed `latest.json` declaring, per platform, `{asset_name, sha256, size}` plus
  `{version, release_index, minimum_version, valid_until}`. The in-app updater
  verifies the manifest against the embedded key and binds every download to the
  **signed** SHA-256 — so an update's identity is a signed hash, not an unsigned
  field. The updater **requires** a verified manifest and refuses (rather than
  installing) if one is absent or unverifiable — there is no weaker path.
- **Downgrade, freeze, and rollback protection.** The updater refuses an older or
  equal version, a stale manifest (past its `valid_until`), a replayed
  `release_index`, and an install below the signed `minimum_version` floor.
- **Fail-closed release self-verification.** The release workflow verifies every
  signed artifact against the embedded public key (crypto + bare-filename
  identity + digest + count-parity + tamper test) before publishing, so a build
  can never ship an artifact the deployed client cannot verify.

Internal to the updater + release pipeline; no change to how you use C0PL4ND.
Updating from an earlier version works as it did before.

## [0.4.9] - 2026-06-25

### Fixed

- **In-app auto-update was rejecting the download** with *"signature
  trusted-comment file mismatch: signed for `release/c0pl4nd-…`, expected
  `c0pl4nd-…`"*. The release workflow signed each artifact by its `release/<name>`
  path, which minisign records verbatim in the signature's trusted comment
  (`file:release/<name>`); the fail-closed updater binds that token to the **bare**
  asset name and so refused the otherwise-valid, correctly-checksummed artifact —
  breaking auto-update for every deployed client. The release now signs **bare
  filenames** (trusted comment `file:<name>`), so every existing client can verify
  and install the update; a CI guard fails the release if any signature carries a
  path prefix. The updater's trusted-comment binding was additionally hardened to
  compare basenames on both sides (defence-in-depth for future builds). If you are
  on an older version, update to v0.4.9 — it installs cleanly.

## [0.4.8] - 2026-06-25

A best-in-class interaction wave bringing the egui shell to parity with
mainstream terminals — selection, navigation, and pane management — followed by
a production-scale QA pass (full feature inventory, risk-based edge cases, and
the complete issue-finding tool sweep).

### Added

- **Scrollback navigation chords.** `mod+shift+Home` / `mod+shift+End` jump to
  the top / bottom of the scrollback; `mod+shift+PageUp` / `mod+shift+PageDown`
  jump to the previous / next shell-prompt mark (OSC 133). All are consumed
  before reaching the shell, so the chord never leaks a control byte to the PTY.
- **Word and line selection.** Double-click selects the word under the cursor
  (its run includes path / URL / identifier punctuation); triple-click selects
  the whole line.
- **Block (rectangular) selection.** Hold `Alt` while dragging to clip every row
  to the same column range instead of the line-wise span.
- **Right-click pane context menu.** Copy (when a selection exists), Clear
  scrollback, Split right, Split down, New tab, and Close pane. (Paste is offered
  via the keyboard shortcut.)
- **Zoom-pane toggle (`mod+shift+Z`).** Render only the focused pane full-size,
  siblings hidden; toggle again to restore the exact prior layout.
- **Directional pane focus (`mod+shift+Arrow`).** Move keyboard focus to the
  geometrically adjacent pane.
- **Hover-URL affordance.** A detected URL underlines and shows the hand cursor
  on plain hover (no modifier), signalling it is `mod`-clickable to open.

### Fixed

- **Jump-to-prompt-mark chords now fire on Windows and Linux.** The
  `mod+shift+PageUp/PageDown` chords were silently dead off-macOS because the
  matcher required the platform `command` modifier; they now use the same
  explicit ctrl-or-command discipline as the other chords.
- **Zoom no longer shows a stale pane after focus moves.** Switching tabs (or
  opening a new tab) while zoomed kept the old pane on screen while keystrokes
  routed to the now-focused hidden pane; a per-frame reconcile drops the zoom
  when focus diverges, so the focused pane is always the one shown.

### Documentation

- `KEYBINDINGS.md` corrected to reflect that the shell's shortcuts are currently
  fixed (not yet user-rebindable) — matching the Settings panel — and a mouse-
  gestures reference table was added.

## [0.4.7] - 2026-06-17

### Changed

- **New app icon: a monochrome phosphor-teal daemon-sigil.** The app icon is
  redrawn as a single-hue (`#1ad6c0`) occult summoning seal — a double
  containment ring with radial seal-ticks, a faint kamea grid, inward goetic
  bind-rune spikes, and cardinal pommel dots — wrapped around the bold `>_`
  shell prompt mark, with a `fork()` trident as the daemon signature at the
  base. This replaces the previous multi-colour cyan/violet/red icon and
  realigns the mark to C0PL4ND's phosphor-teal brand voice. The new art ships
  in the embedded Windows `.ico`, the runtime window icon, and the full icon
  source family (`assets/svg/app-icon*.svg`, `logomark.svg`). Legibility is
  preserved down to 16px via a size-tiered render (the clean prompt mark at
  small sizes, the full seal at large sizes).

## [0.4.6] - 2026-06-15

### Changed

- **Update checks now default to `notify` (previously `manual`).** On launch,
  C0PL4ND performs a single **read-only** check against the public GitHub
  Releases API and shows a passive toast if a newer version exists. The check is
  throttled to at most once per `check_interval_hours` (default 24h) via a
  persisted `last-update-check` timestamp, and it **sends zero identifiers**.
  This is a privacy-relevant default change: a fresh install now makes one
  on-launch network connection. To keep update checks but make **no on-launch
  network connection**, set `mode = "manual"`; to disable all network access,
  set `mode = "off"`. See [PRIVACY.md](PRIVACY.md).

### Fixed

- **Wide (CJK / emoji) glyphs render cell-accurately again.** Rendering is now
  per-cell — each glyph is painted at its computed grid column rather than from
  accumulated font advances — so a wide or fallback glyph can never shift
  neighbouring cells. This properly fixes the launch-time "scattered glyphs"
  regression that was reverted in 0.4.5.
- Copying a selection across a wide glyph no longer inserts a stray space.
- Rows mixing right-to-left script with wide CJK/emoji glyphs keep correct
  cell-column alignment through BiDi reordering.

### Security

- The on-launch and `c0pl4nd update` CLI version checks now use a host-confined
  HTTPS GET — no arbitrary-host redirects (manual redirect walk re-asserting the
  `api.github.com` allow-list at every hop), a response size cap, and
  connect/read timeouts — matching the in-app updater's hardened network path.
- `check_interval_hours` is now enforced (a persisted last-check timestamp), so
  the default `notify` check cannot exceed GitHub's unauthenticated rate limit.

### Tests

- Added property-based tests (proptest), a VT/ANSI conformance corpus, an
  explicit accessibility (AccessKit) suite, additional fuzz targets
  (config-TOML / `.itermcolors` / OSC), and grid-reflow + search benchmarks.
  Filled previously-untested modules (palette, update-engine constants, DLL
  hardening, screenshot math). Wide-glyph cell positioning is now unit-tested
  without a display via a pure positioning function.

## [0.4.5] - 2026-06-14

### Fixed

- Reverted the 0.4.4 wide-glyph rendering change, which scattered glyphs on
  launch when a proportional fallback font loaded before the monospace font.
  (0.4.6 reintroduces the fix correctly via per-cell rendering.)

## [0.4.4] - 2026-06-14

### Changed

- First attempt at cell-accurate wide-glyph rendering (superseded; see 0.4.5 /
  0.4.6).

## [0.4.3] - 2026-01-17

Earlier releases (0.1.0 – 0.4.3) predate this changelog; see the GitHub Releases
page for their notes and signed artifacts.

[0.4.6]: https://github.com/46b-ETYKiAL/C0PL4ND/releases/tag/v0.4.6
[0.4.5]: https://github.com/46b-ETYKiAL/C0PL4ND/releases/tag/v0.4.5
[0.4.4]: https://github.com/46b-ETYKiAL/C0PL4ND/releases/tag/v0.4.4
[0.4.3]: https://github.com/46b-ETYKiAL/C0PL4ND/releases/tag/v0.4.3
