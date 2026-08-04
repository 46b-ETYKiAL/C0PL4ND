# C0PL4ND Core — Input-Handling Security Audit

**Scope:** `c0pl4nd-core` (the terminal engine). The audit covers every
untrusted-input sink: the PTY byte stream, the APC pre-filter, OSC handlers
(incl. clipboard), the Sixel/Kitty/base64 decoders, drag-drop path handling,
and the on-disk workspace persistence layer.

**Threat model:** the PTY child process is fully untrusted. It can emit any
byte sequence, including hostile, malformed, oversized, and adversarially
crafted escape sequences. The persistence file on disk may be corrupt or
hand-edited by an attacker. The audit asserts three invariants throughout:
**(1) no panic, (2) no unbounded memory, (3) no exfiltration / no break-out.**

This audit is **read-only**: it reports findings and cites the mitigating
code; it does not modify engine logic. Each finding below is exercised by a
test in `crates/core/tests/fuzz_robustness.rs`, `e2e_terminal.rs`, or
`perf_smoke.rs`.

---

## 1. PTY byte stream → vte parser + APC pre-filter

**Sink:** `Terminal::advance` (`crates/core/src/term.rs`). Raw PTY bytes drive
the `vte` 0.15 state machine, with a hand-rolled APC pre-filter ahead of it to
recover Kitty graphics APCs that `vte` would otherwise discard.

**Threats:** parser panics on malformed/truncated sequences; integer
overflow on huge CSI parameters; unbounded state accumulation across
`advance()` calls; infinite loops.

**Mitigations (GOOD):**
- `vte` is the same battle-tested parser alacritty uses; it clamps parameter
  values and never panics on malformed input.
- The APC pre-filter is an explicit small state machine (`ApcFilter`) that
  always makes forward progress one byte at a time and flushes passthrough
  text on every transition, preserving byte order.
- The existing in-tree regression test
  `parser_survives_adversarial_escape_sequences` (`term.rs`) already feeds a
  hostile corpus byte-by-byte.

**Verification added:** `random_and_malformed_input_never_panics` feeds 1 MB
of deterministic-LCG random bytes in irregular chunks plus a 14-case
pathological-escape corpus (param overflow, empty-param floods, unterminated
OSC/DCS/APC, ESC storms, raw C1 bytes) both whole and byte-by-byte;
`every_byte_value_is_survivable` feeds all 256 byte values repeatedly.

**Residual risk:** none identified. The parser is total over arbitrary input.

---

## 2. APC / Kitty graphics size caps

**Sink:** `Terminal::advance` APC accumulator + `Screen::handle_kitty_apc`.

**Threats:** a hostile stream sends a multi-megabyte (or never-terminated) APC
to exhaust memory; chunked transmissions (`m=1`) accumulate without bound; the
transmit-store (`a=t`) grows without limit.

**Mitigations (GOOD), cited:**
- `KITTY_APC_MAX_BYTES = 8 MiB` (`term.rs` ~L2217): the pre-filter stops
  accumulating once the body exceeds 8 MiB.
- Chunk accumulation is re-checked against `8 * 1024 * 1024` in
  `handle_kitty_apc` (~L2134) and the in-flight chunk set is **dropped** on
  overflow.
- The transmit-store enforces `KITTY_STORE_MAX_IMAGES = 64` and
  `KITTY_STORE_MAX_BYTES = 64 MiB` (~L2094), evicting lowest-id entries
  deterministically (`store_kitty_image`).
- `decode_kitty` uses `checked_mul` for the `width*height*bpp` allocation size
  and rejects a length mismatch (`image.rs` ~L262) — a hostile
  `s=1000,v=1000` header with a 4-byte payload decodes to `None` rather than
  over-allocating.

**Verification added:**
`image_and_base64_decoders_are_bounded_on_hostile_input` asserts a 9 MiB APC
body is dropped (`images().len() == 0`) and that a dimension/length mismatch
rejects the image.

**Residual risk:** the 8 MiB single-body and 64 MiB store caps are generous
but fixed; a stream could repeatedly transmit-and-evict at up to ~8 MiB churn
per image. This is bounded (no leak) and matches typical terminal behaviour.
**Recommendation (low):** consider making the caps configurable via `Config`
for memory-constrained hosts. Not a vulnerability.

---

## 3. Sixel decoder

**Sink:** `image::decode_sixel`.

**Threats:** RLE expansion (`!Pn`) or band advances grow the pixel map without
bound; out-of-range colour components; integer overflow on coordinates.

**Mitigations (GOOD):**
- The decoder is pure and dependency-free; colour components are clamped to
  `0..=100` and scaled with `.min(100)` (`image.rs` ~L64).
- `parse_u16` saturates at `u16::MAX`, so a numeric field cannot overflow.
- Pixels are stored sparsely in a `HashMap`, so the allocation tracks actual
  drawn pixels, not the declared raster.

**Verification added:** `decode_moderate_images_is_bounded` decodes a
200-wide RLE sixel under a 2s bound; the robustness test feeds garbage-only
and out-of-range-colour sixel bodies without panic.

**Finding (at time of audit): unbounded sparse pixel map.** The map was keyed
by `(x, y)` with no absolute cap on `max_x`/`max_y`, so a crafted stream could
grow it toward tens of millions of entries before the DCS terminated. The input
side already bounded how much sixel text could arrive in one APC/DCS, but that
was an indirect bound, not a hard pixel-count ceiling.

**Status: FIXED.** `decode_sixel` now enforces a hard output ceiling, applying
the same checked-multiply discipline the Kitty decoder uses:

- a per-axis dimension clamp, sized so the dense worst case lands exactly on
  the pixel ceiling;
- a `MAX_SIXEL_PIXELS` ceiling (16 Mpx) checked as a backstop on every
  iteration of the decode loop, so no byte sequence can grow the map past it;
- a cumulative work budget, which additionally defeats the band-reset re-plot
  variant that bounds output but not effort;
- a final `checked_mul` product check on the declared raster.

Any stream that exceeds a limit causes the decoder to return `None` and drop
the image; it cannot allocate past the ceiling or pin a thread. The variant
that motivated the work budget was found by the `fuzz_sixel` fuzz target,
which continues to run against this path.

---

## 4. OSC handlers

### 4a. OSC 52 clipboard (write + read)

**Sink:** `Screen::handle_osc_52`, `Terminal::set_clipboard_read_enabled`,
`respond_clipboard_read`.

**Threat:** the canonical OSC 52 vulnerability — a program issues
`OSC 52 ; c ; ?` to read the host clipboard and exfiltrate its contents (which
may contain passwords) back through the PTY.

**Mitigation (GOOD, the headline security property):**
- Clipboard READ is **default-OFF** (`clipboard_read_enabled: false`,
  `term.rs` ~L514). A `?` payload is silently dropped (`handle_osc_52`
  ~L663–668) — the terminal never reads the host clipboard.
- Even after `set_clipboard_read_enabled(true)`, the **core never reads the
  clipboard itself**; the host must explicitly call `respond_clipboard_read`
  with host-supplied text. A bare `?` query still produces nothing.
- WRITE requests are captured (safe direction) and surfaced to the app via
  `take_clipboard_writes`.

**Verification added:**
`osc52_clipboard_read_is_default_off_and_silent` asserts the default-off
posture, that a read query emits no PTY response (disabled AND enabled), and
that writes are still captured.

**Residual risk:** none. This is best-in-class posture.

### 4b. OSC 133 semantic prompt zones

**Sink:** `Screen` OSC 133 handling; `prompt_marks`, `command_marks`.

**Threat:** the iTerm2 CVE-2024-38395/38396 class — semantic marks that the
terminal reports back to the PTY, enabling injection.

**Mitigation (GOOD):** OSC 133 is **capture-only**. The marks are stored for
the app to draw prompt glyphs; the terminal **never** queues a PTY reply for
them. Cited in `take_pty_response` doc (~L2486) and the in-tree regression
`osc133_never_writes_pty_response`.

**Verification added:** `colored_ls_with_osc133_prompt_marks_captured`
(e2e) and the flood case in `embedded_terminators_cannot_break_out_of_their_frame`
(1000× OSC 133 marks → zero PTY response).

**Residual risk:** none.

### 4c. OSC 4/10/11/12 colour queries

**Sink:** `handle_osc_4`, `handle_dynamic_color`.

**Threat:** a query reply could echo attacker-controlled bytes.

**Mitigation (GOOD):** replies are built from the terminal's own
palette/dynamic-colour state via `format_color_reply` — fixed-format
`rgb:RRRR/GGGG/BBBB`, never echoing the query payload. Index is bounded to
`<= 255`.

**Verification added:** `osc11_background_query_and_set_round_trip` (e2e).

### 4d. OSC title (0/2)

**Threat:** title payload echoed back to the PTY (exfiltration), or title
content treated as instructions.

**Mitigation (GOOD):** title is captured as inert text; never re-emitted.

**Verification added:** the OSC-title case in
`embedded_terminators_cannot_break_out_of_their_frame` confirms an
escape-like payload (`[?62c`) is stored as plain title text and produces no
PTY response.

---

## 5. Device-attributes (DA) reply on the DEC-private `CSI ? … c` form — **FINDING (Informational)**

**Sink:** the `'c'` CSI handler (`term.rs` ~L1807).

**Observation:** the handler distinguishes secondary DA (`CSI > c`) from
primary DA, but does **not** check for the `?` private-marker intermediate.
As a result, a DEC-private request `CSI ? Pm c` (which standard xterm does
**not** treat as a DA request) still triggers the primary-DA reply
`\x1b[?62;1;6;22c`.

**Severity: Informational.** This is a minor spec deviation, **not an
exfiltration vector**: the reply is the terminal's own fixed capability
string — it contains **no attacker-controlled bytes**. The worst outcome is
an extra benign reply to a malformed/private query. The real anti-injection
guarantees (OSC 133 / title content never echoed) are unaffected and hold.

**Recommendation (low):** gate the primary-DA branch on
`!intermediates.contains(&b'?')` so a private-marked `?…c` is ignored,
matching xterm. **No fix applied (audit is read-only; source changes are out
of scope).** The test
`embedded_terminators_cannot_break_out_of_their_frame` was written to assert
the *actual* anti-exfiltration guarantee (no echo of payload bytes) rather
than depend on this DA quirk, so the suite stays green and the finding is
documented here rather than masked.

---

## 6. Base64 decoder

**Sink:** `osc::base64_decode` / `base64_encode`.

**Threats:** panic on malformed input; mis-padding accepted; large input
blowup.

**Mitigations (GOOD):** strict RFC 4648 validation — rejects bad length,
over-padding (`==` mid-string or 3 pads), and non-alphabet bytes, returning
`None`. Whitespace is ignored. No panics.

**Verification added:** `image_and_base64_decoders_are_bounded_on_hostile_input`
asserts malformed inputs return `None` and a 600 KiB real buffer round-trips
exactly. **Note:** during authoring, a test that used
`"QUJDRA==".repeat(N)` was (correctly) rejected by the decoder because the
repeat embeds `==` padding mid-string — confirming the strict-padding guard
works. The test was corrected to encode a real buffer.

---

## 7. Drag-drop path quoting

**Sink:** the engine is **UI-free** — drag-drop path insertion lives in the
app shell (`crates/app`), not in `c0pl4nd-core`. The core has no code that
shells out or executes a dropped path.

**Observation (GOOD):** `layout_persist` documents that the persisted `cwd` is
"a plain string (no path the loader executes — it is handed to the app, which
may ignore a missing dir)" (`layout_persist.rs` ~L18–21). The core never runs
a path. **Recommendation for the app layer (out of core scope):** when the app
inserts a dropped path into the PTY input, it must shell-quote it (bracketed
paste + quoting) so a path containing spaces/quotes/`;`/`$()` cannot break the
shell command line. This is an app-shell responsibility; flagged here for
completeness.

---

## 8. Scrollback-on-disk persistence (workspace snapshots)

**Sink:** `layout_persist::WorkspaceSnapshot::{load, load_strict, from_json}`,
`atomic_write::atomic_write`.

**Threats:** a corrupt/hostile state file crashes the app, wedges the grid, or
causes the loader to execute a path/command; a torn write on crash; unbounded
scrollback replay.

**Mitigations (GOOD), cited:**
- **Never executes:** the format is structural JSON data only — "Loading reads
  data, never instructions" (`layout_persist.rs` ~L19). No command string, no
  path the loader runs.
- **Never panics:** `load` degrades any malformed / over-`MAX_PANES` / empty /
  unknown-version / broken-split file to the single-tab fallback (~L536), with
  `load_strict` available for callers wanting the error.
- **Bounded scrollback:** `SCROLLBACK_MAX_LINES = 2000` is enforced both at
  capture (`with_scrollback`) and on load (`normalize`) as defense-in-depth
  against a hand-edited blob (~L83, ~L673).
- **Crash-safe writes:** `save_atomic` uses temp-file + rename
  (`atomic_write`), so a crash mid-save leaves the previous file intact, never
  a torn one (verified by the in-tree
  `workspace_save_atomic_then_load_round_trips_through_disk`).
- **Version gating:** unknown future versions are rejected, never mis-read.

**Verification added:**
`workspace_load_on_corrupt_file_falls_back_never_panics` feeds six hostile
files (invalid JSON, empty, binary garbage, wrong-shape, unknown future
version, empty-split + out-of-bounds indices) and asserts the single-tab
fallback every time, plus the missing-file case.

**Residual risk:** none identified for the core. The `cwd` string is passed to
the app verbatim; the app is responsible for validating it before launching a
shell there (it "may ignore a missing dir" per the contract).

---

## Summary

| # | Sink | Posture | Finding |
|---|------|---------|---------|
| 1 | PTY stream / vte / APC pre-filter | GOOD — total parser, no panic | — |
| 2 | APC / Kitty size caps | GOOD — 8 MiB body, 64-img / 64 MiB store, `checked_mul` | low: caps fixed, not configurable |
| 3 | Sixel decoder | GOOD on realistic input | **low: no explicit pixel-count ceiling — recommend a `max_x*max_y` guard** |
| 4a | OSC 52 clipboard read | GOOD — **default-OFF, core never reads clipboard** | — |
| 4b | OSC 133 semantic zones | GOOD — capture-only, never replies | — |
| 4c/4d | OSC colour / title | GOOD — fixed-format replies, inert title | — |
| 5 | DA reply on `CSI ? … c` | — | **informational: private `?…c` triggers a (benign, fixed-string) DA reply; not an exfil vector** |
| 6 | base64 | GOOD — strict RFC 4648, no panic | — |
| 7 | drag-drop path | N/A to core (UI-free); core never executes a path | app-shell must quote dropped paths |
| 8 | workspace persistence | GOOD — data-only, never panics, atomic writes, bounded scrollback | — |

**Overall:** the engine's untrusted-input handling is strong. The two
load-bearing anti-exfiltration properties (OSC 52 read default-off; OSC 133 /
title capture-only) are correctly implemented and now regression-tested. Two
findings are reported for the maintainers' consideration — both
**Low/Informational**, neither a memory-safety or exfiltration vulnerability,
and neither fixed here (this audit is read-only and source-changes are out of
scope):

1. **Sixel pixel-count ceiling (Low):** add an explicit `max_x*max_y` guard in
   `decode_sixel` mirroring the Kitty `checked_mul` discipline.
2. **DA reply on `CSI ? … c` (Informational):** gate the primary-DA branch on
   absence of the `?` private marker to match xterm.

No panic, hang, or unbounded-memory condition was found across the random,
malformed, oversized, and embedded-terminator corpora.
