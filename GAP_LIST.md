# C0PL4ND Prioritized Gap List (audit + best-in-class research)

Synthesized from `RESEARCH_TERMINAL_SPEC.md` (80+ features, 7-terminal matrix), `RESEARCH_WINDOW_UX.md` (window/UX + winit/wgpu patterns), and a twice-verified current-state structural audit. Comparison set: Ghostty, WezTerm, kitty, Alacritty, iTerm2, Windows Terminal, Konsole.

Legend: **P0** = expected-everywhere / correctness or breaks real apps · **P1** = strongly wanted · **P2** = power-user · **P3** = niche. Status: ✅ built · ⚠️ partial · ❌ absent.

---

## Already built (confirmed by audit — NOT gaps)
- ✅ Frameless custom titlebar (min/max/close), double-click-maximize, edge-resize + hover cursor (PR #23)
- ✅ Splits/panes (H/V, grid, focus-nav, resize, close) — `core/src/layout/`
- ✅ Tabs (per-cell tab strips, new/close/switch)
- ✅ Command palette (25 actions), fuzzy filter
- ✅ Themes + theme files (`assets/themes/*.toml`), opacity, CRT effects
- ✅ TOML config w/ line-error surfacing, zero-config defaults
- ✅ Truecolor / 256 / 16-color SGR (foreground)
- ✅ OSC 0/2 (title), OSC 7 (cwd), OSC 8 (hyperlinks), OSC 133 (prompt marks; deliberately never reports back — anti-CVE)
- ✅ Sixel graphics (8 MiB cap)
- ✅ Scrollback (10k default) + search
- ✅ Workspace save/restore, sessions
- ✅ Font size hotkeys, fallback chain; cursor style/blink in config
- ✅ Auto-update (opt-in), neofetch splash

---

## P0 — Correctness / breaks real apps (HIGHEST VALUE)

| ID | Gap | Status | Evidence | Chosen fix | Source |
|----|-----|--------|----------|-----------|--------|
| E1 | **Background color + inverse video not rendered** — SGR bg/reverse are parsed but `leaf_spans` resolves fg only. Every app using colored backgrounds (ls, grep --color, syntax bg, htop bars, selections, vim themes) renders wrong. | ❌ render | AUDIT term.rs / leaf_spans | Render per-cell bg quads (reuse `pane_render::ChromeRenderer` ColorRect) behind text; honor reverse-video by swapping fg/bg; honor default-bg. | term-spec §Color; audit |
| E2 | **Alternate screen buffer** (DEC ?1049/?47/?1047) | ❌ | AUDIT: no DEC private modes | Add alt-screen grid; switch on ?1049h/l; restore primary + cursor. Enables vim/less/htop/tmux without trashing scrollback. | term-spec §Misc VT |
| E3 | **Bracketed paste** (DEC ?2004) + paste sanitization | ❌ | AUDIT | Track ?2004; wrap pasted text in ESC[200~..201~; strip embedded 201~ + C0/C1 (anti-injection). | term-spec §Paste (P0 security) |
| E4 | **DEC private mode framework** (?25 cursor visibility, ?1049, ?2004, ?1000/1002/1003/1006 mouse, ?2026 sync) | ❌ | AUDIT | Central CSI ? h/l handler dispatching to mode flags; foundation for E2/E3/E6. | term-spec §Mouse/Misc |
| E5 | **Cursor visibility (DECTCEM ?25)** | ❌ | AUDIT | Hide/show cursor; many TUIs hide it. | term-spec |

## P1 — Strongly wanted

| ID | Gap | Status | Chosen fix | Source |
|----|-----|--------|-----------|--------|
| E6 | **Mouse reporting** (1000/1002/1003 + SGR 1006 + focus 1004) | ❌ | Encode mouse events per active mode; report to PTY. Enables mouse in vim/tmux/htop. | term-spec §Mouse |
| E7 | **DECSCUSR cursor-shape escape** (`CSI Ps SP q`) | ❌ | Let apps set bar/underline/block + blink (nvim mode cursor). | term-spec |
| E8 | **Live config reload** (doc-claimed, unimplemented) | ❌ | `notify` file-watcher on config path → reload + re-apply (no restart). | audit; window-ux §12 |
| E9 | **URL detection + Ctrl/Cmd-click open** (OSC 8 already parsed) | ⚠️ | Heuristic URL scan + hover underline + ctrl-click → open. | term-spec §Hyperlinks |
| E10 | **OSC 52 clipboard write** (read default-OFF) | ❌ | Honor OSC 52 write (remote yank); never auto-read. | term-spec §Clipboard |
| E11 | **OSC 4 / 10 / 11 color query+set** | ❌ | TUI bg-detection theming; respond on PTY. | term-spec §Color |
| E12 | **Synchronized output (DEC ?2026)** | ❌ | Buffer frame between begin/end; flicker-free TUI redraw. | term-spec |
| E13 | **cwd inheritance on split/new-tab** (via OSC 7) | ⚠️ | New pane/tab starts in active pane's OSC7 cwd. | window-ux §4 |
| E14 | **Tab auto-title from OSC 0/1/2** + Ctrl+1..9 switch | ⚠️ | Title tabs from running cmd; numeric switch. | window-ux §5 |
| E15 | **Render-on-change (damage) gating** | ⚠️ | Gate request_redraw + glyphon prepare on dirty flag (perf/battery). | window-ux §9 |
| E16 | **Graceful PTY-exit handling** | ⚠️ | Close pane/tab cleanly on shell exit; show status. | window-ux §15 |

## D — Named deferred items
- ✅ D1 hover backplates (f9efb7a + borrow fix 2334b74)
- ✅ D2 window geometry persistence (652c44b config + c424634 window)
- ✅ D3 in-app settings panel (613cee7 + import fix 7d3f9c2)
- ✅ D4 Win+arrow Aero snap (Win32 custom-frame subclass; PR #34)

## VT-core (merged 21e3b70) — provides API for E-integration
- ✅ DEC private-mode framework, alt screen, DECSCUSR cursor shape, bracketed-paste/cursor-visibility getters, encode_mouse() (818542e; 177 core tests pass)
- 🔄 App-side wiring (E1/E5/E7/E2/E3/E6) in flight on app-correctness branch

## P2 / P3 — deferred with justification (out of "ship LESS" taste or niche)
- P2: programming ligatures (HarfBuzz), kitty graphics protocol (Sixel already covers inline images), `.itermcolors` import, quake/dropdown mode, leader/prefix keybinds, copy-mode/vi keys, block/rectangular selection, title stack (XTWINOPS 22/23).
- P3: BiDi/RTL (even Ghostty/Alacritty punt), background image, animated cursor trail, base16 framework, detached daemon, screen-reader UIA, iTerm2 inline images (OSC 1337).

---

## Execution decision
1. **D1–D4** — impl agent (running).
2. **Phase E (this pass): E1, E5, E4, E2, E3** — the P0 correctness set (bg/inverse rendering first = most visible; cursor visibility; DEC mode framework; alt screen; bracketed paste). These make the terminal render correctly and run TUIs. Then **E7 (DECSCUSR)** and **E6 (mouse)** if clean.
3. **Remaining P1 (E8–E16)** and all P2/P3 — documented backlog; implement opportunistically / future pass with explicit justification (no silent deferral; this list IS the tracked record).

Rationale: a terminal that renders background colors wrong and can't run vim is failing its core job — these P0s outrank chrome polish. All term.rs P0s touch the same files, so they're done sequentially (one commit each, with tests) after D1–D4 frees `window.rs`.

---

## Major correctness + UX cycle (2026-05-31)

A large cycle landed terminal-emulation correctness, the keystone convenience
features, session restore, and a full test suite. **Shipped & merged:**

**VT/ANSI correctness (apps that were broken now work — vim/htop/less/nano/mc):**
- **P0** — DA1/DA2 device attributes, DSR/CPR, IL/DL/ICH/DCH/ECH, save/restore
  cursor (`esc_dispatch`), DECSTBM scroll region, DEC line-drawing charset,
  East-Asian **wide-cell width** (`unicode-width`), ED/EL submodes.
- **P1** — **reflow/rewrap on resize** (history + grid), settable tab stops,
  focus-report core; bonus RI/IND/NEL, RIS/DECSTR, CHA/VPA/CNL/CPL, SU/SD, REP.
- **P2/P3** — styled underlines + colour (SGR 4:x / 58), IRM/DECOM/DECSCNM,
  OSC 9;4 progress, combining marks + VS15/16, OSC 133 C/D, XTGETTCAP, DECRQM.
- **Keyboard encoder** — F1–F12, Home/End/Ins/Del/PgUp/PgDn, DECCKM-aware
  arrows (SS3 vs CSI), Alt-as-Meta. **Focus reporting** wired app-side (`?1004`).
- **Renderer** — draws styled underlines + strikeout; DECSCNM reverse-screen.

**Convenience/UX:** mouse **text selection + copy** (was entirely absent),
copy-on-select (opt-in), **paste-safety** confirm (multi-line), drag-and-drop
file path (quoted, never executed), jump-to-prompt (Ctrl+Shift+PgUp/Dn),
**OSC 9/777 notification** taskbar flash.

**Session restore:** `WorkspaceSnapshot` v2 + crash-safe atomic writes (core);
per-pane **cwd capture + restore** + auto-save-on-close (app). Reopen → same
panes in the same directories (fresh shells; live processes not preserved —
that needs a tmux-style daemon, an explicit non-goal).

**Quality:** E2E + performance + security/fuzz test suites + `SECURITY_AUDIT.md`;
sixel decoder pixel-count ceiling added (audit finding).

### ✅ Previously-remaining items — now all shipped (2026-05-31)
- **Live font zoom (Ctrl +/−/0, Ctrl+scroll)** — DONE. Grid cell dims +
  glyph metrics route through `cell_w()`/`cell_h()` (single source of truth);
  scale 1.0 is byte-identical, and a round-trip unit test proves render↔hit-test
  alignment at every scale. Chrome/titlebar stays fixed size.
- **Configurable content padding** — DONE. `config.window.padding` drives the
  grid left-inset at all 9 `leaf_text_origin` sites in lockstep.
- **True curly undercurl** — DONE. SGR 4:3 draws a 1px zigzag.
- **Multi-window-tab persistence** — DONE. All tabs persist via the core
  multi-tab `WorkspaceSnapshot` v2 (auto-save on close, restore-all on startup).

The entire GAP_LIST is now closed: no remaining items.

## Shipped Status (final update — master 2026-05-30)

This section is the authoritative as-shipped ledger; earlier rows above are the original audit.

### ✅ Shipped & merged to master
- **D1** caption-button hover/press backplates
- **D2** remembered window size/position/maximized (multi-monitor + DPI clamp)
- **D3** in-app settings panel (font/theme/cursor/scrollback/opacity/startup; live-apply + TOML write-back)
- **VT-core parser**: DEC private modes, alternate screen, DECSCUSR cursor shape, bracketed-paste/cursor-visibility getters, `encode_mouse()`, `cursor_position()`
- **E1** per-cell background color + inverse-video rendering
  - **Correction (2026-07-21):** this was true of the legacy winit/wgpu renderer
    only; the *shipping* egui renderer regressed E1 (its `ColorRun` was fg-only and
    `paint_grid_native` emitted no background quads), so `ls`/`grep --color`/`git
    diff`/htop/vim backgrounds rendered flat. E1 is now genuinely shipped in the
    egui binary: `RunStyle` carries the background, `paint_grid_native` emits bg
    quads (skipping the window default), reverse video renders visibly, and the
    styled underline / strikethrough / dim attributes draw. Verified by the render
    snapshot tests, not just the parser tests.
- **E3** clipboard paste + bracketed-paste wrapping (dependency-free shell-out)
- **E5/E7** terminal cursor draw (block/bar/underline, blink, visibility)
- **E6** mouse reporting → PTY (DEC 1000/1002/1003 + SGR 1006)
- **E14** Ctrl+1-9 window-tab switching
- **OSC suite (core)**: OSC 52 (write; read default-off), OSC 4/10/11/12 query+set, OSC 104 reset, OSC 9/777 notifications, title stack (CSI 22/23 t), `.itermcolors` import, ligatures config flag, PTY-response channel
- **OSC app-wiring (E10/E11)**: `pump_terminal_io()` drains query replies → PTY, clipboard writes → OS clipboard (`write_os_clipboard`), color sets → live theme, notifications → log
- **E16** graceful PTY exit (close pane on shell exit; exit app when last pane closes)
- **E9** URL detection + Ctrl-click to open (heuristic row-scan for `http(s)://`|`file://` under the cursor → `open_path`, wired into the mouse-press handler) — PR #33
- **D4** Win+Arrow Aero Snap — Win32 custom-frame subclass on the frameless window (`WS_THICKFRAME`/`WS_CAPTION` + `WM_NCCALCSIZE`/`NCHITTEST`/`GETMINMAXINFO`), `windows` crate target-gated to `cfg(windows)` — PR #34
- **Programming ligatures** — the `ligatures` config flag is now functional: grid shaping gates `Shaping::Advanced` (ligatures, opt-in) vs `Shaping::Basic` (strict monospace, default) — PR #35
- **BiDi/RTL** — per-row visual reordering via the Unicode Bidirectional Algorithm (`unicode-bidi`), ASCII fast-path, per-glyph colour preserved through the permutation — PR #36
- **Full Kitty graphics protocol** — APC pre-filter ahead of `vte` (which silently discards APC), decode f=32 RGBA / f=24 RGB / f=100 PNG, actions transmit/display/store/delete, chunked `m=1`…`m=0` transfers — PR #37

### ✅ Previously "deferred (offline-blocked)" — now shipped (2026-05-31)

D4, BiDi/RTL, programming ligatures, and full Kitty graphics were previously
marked deferred with the reason "requires a crate that cannot be fetched (no
crates.io access)". On re-check, crates.io **was** reachable and the required
crates (`windows`, `unicode-bidi`) were already present in the lock graph, so
all four were implemented and merged (PRs #34–#37 above). The environmental
deferral reason is resolved.

## egui-shell parity (2026-06-11)

The earlier rows above track features against the **legacy `window.rs` (winit)**
shell. The `PR #166` refactor stood up the new **egui shell** (the shipping
`c0pl4nd` binary) but did not port the legacy per-frame terminal-effect draining
or the pointer→PTY mouse path, so the egui binary silently regressed several
already-shipped features. A whole-app audit found exactly **5 P0 egui-path gaps
(and no others)**; all five are now wired, plus mouse reporting and mouse-wheel
scrollback:

- ✅ **OSC PTY-response draining** (DA/DSR/cursor/colour queries) → written back to the pane's PTY each frame (`pump_host_effects`). Was silently dropped → TUIs misdetect/hang.
- ✅ **OSC 52 clipboard-write** → OS clipboard (`ctx.copy_text`).
- ✅ **OSC 4/10/11/12/104 colour-set** → live theme (`apply_color_set`).
- ✅ **OSC 9/777 notification** → taskbar attention while unfocused (text never read — privacy).
- ✅ **OSC 9;4 progress drain** → bounded-growth guard (the un-drained queues were also a memory leak).
- ✅ **E6 mouse reporting → PTY** (`report_mouse`, gated on `mouse_mode()`): mouse in vim/tmux/htop/less.
- ✅ **Mouse-wheel scrollback** (`scroll_view`): scroll up into history when no program has grabbed the mouse.

Coverage: deterministic `pane_term` unit tests (`pump_host_effects_drains_every_queue`,
`report_mouse_gates_on_mouse_mode`, `scroll_view_moves_the_scrollback_offset`).
See `docs/control-test-ledger.md` § Milestone 2.2.

## Whole-app audit-and-fix program (2026-06-12)

A 6-dimension read-only audit (VT-correctness, security/DoS, egui↔legacy parity,
panics/overflow/locks, error-handling/lifecycle, coverage/perf) drove eight
shipped PRs (#169–#175 + #168):

**Core VT correctness + DoS hardening** (PR #169): wide-glyph continuation-bitset
desync in scroll/insert/delete (corrupted CJK/emoji on edit); CUU/CUD now honour
DECSTBM margins; SU feeds scrollback on a full region; DSR/notification/clipboard/
progress/title-stack/kitty-chunks queues capped; `frame_paste` strips C0/C1.

**Security** (PR #170): the legacy shell's `file://` URL auto-detection (ctrl-click
→ `cmd /C start` executable launch) removed — http(s) only, matching the egui shell.

**egui silent errors** (PR #171): config-write + live-PTY-write failures now logged.

**egui parity** — features the egui shell had dropped/lacked: PTY-reply/clipboard/
colour/notification/progress draining + mouse reporting + scrollback (#168); font
zoom, drag-and-drop, focus-reporting `?1004`, jump-to-prompt (#172); window-mgmt
keyboard shortcuts + honest read-only keybindings + greyed startup-panel (#173);
**mouse text selection + copy** (#174 — the P0 baseline feature).

**Coverage + perf** (PR #175): unit tests for the previously-untested
`egui_key_to_logical` / `byte_to_col` / `tint_rgba`; deduped the per-frame
grid-text clone shared by the find-highlight + hyperlink span builders.

All eight PRs landed with regression tests, `clippy --workspace -D warnings`,
`fmt`, full `cargo test`, and `cargo deny` green.

### ❌ Remaining honest limitations (shipped features, documented gaps)
- **BiDi cursor/selection stay logical-order**, and per-cell background quads are not reordered — only the displayed text run is reordered. The common RTL line (default background) is correct; an explicit per-cell background highlight on RTL text is the one divergence. Most grid terminals (Ghostty/Alacritty) omit BiDi entirely, so this is strictly ahead.
- **Ligatures default OFF** (`Shaping::Basic`) to preserve strict monospace cell fidelity; opt-in via `ligatures = true`. This is intended behaviour, not a gap.
- **Kitty formats** beyond f=24/32/100 are unsupported (return `None`); placement is at the cursor cell (no z-index/relative-placement extensions). Sixel remains the dependency-free fallback.
