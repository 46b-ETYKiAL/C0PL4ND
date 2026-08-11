# Troubleshooting

Common issues and how to resolve them. If your problem isn't here, please open
an issue on the [tracker](https://github.com/46b-ETYKiAL/C0PL4ND/issues)
and include your OS, the `c0pl4nd --version` output, and (if relevant) the log
output described under [Diagnostic logging](#diagnostic-logging) below.

## Colours look wrong, or a TUI program won't use colour

Symptoms: `vim`, `tmux`, `less`, `htop`, or another `ncurses`/terminfo program
shows no colour, the wrong colours, or a monochrome fallback — even though
C0PL4ND itself renders full 256-colour and truecolor.

Cause: terminfo-based programs key their capabilities off the `TERM`
environment variable (and `COLORTERM` for truecolor).

C0PL4ND sets these for the child shell **automatically**: every spawned shell
gets `TERM=xterm-256color` and `COLORTERM=truecolor` (matching the
`xterm-256color`-class terminal C0PL4ND advertises over the wire — its
Device-Attributes / `XTGETTCAP` replies in `crates/core/src/term.rs`). So colour
works out of the box even when C0PL4ND is launched from a GUI shortcut (Start
menu, Explorer, a desktop launcher), where the launching process usually has no
`TERM` at all.

C0PL4ND never clobbers a `TERM`/`COLORTERM` you exported yourself before
launching — a deliberately-set value is honoured. To verify what the child
actually sees, run:

```sh
c0pl4nd --diagnostics
```

which prints the `TERM` / `COLORTERM` / `TERM_PROGRAM` values in effect (among
other environment facts).

If you need a different terminfo identity (e.g. `screen-256color` under a
multiplexer), set the `term` key in your config:

```toml
term = "screen-256color"
```

(`COLORTERM` stays `truecolor` — C0PL4ND renders 24-bit colour.) An empty value
falls back to the `xterm-256color` default. After changing it, restart the pane
so the new child shell picks up the value.

## CJK / IME input: Japanese, Chinese, or Korean text won't type

Symptoms: pressing an IME-composed key produces nothing, or only the committed
romaji, and the OS candidate window does not appear over the terminal.

Cause: the terminal grid is a custom-painted surface rather than a standard
text input, so the operating system's input-method editor (IME) candidate
window is not yet wired to the terminal caret. Latin (direct) input works
normally; composed CJK / complex-script input does not.

Workarounds:

- Compose the text in another application (a text editor, the OS IME pad, or a
  browser field), then **paste** it into C0PL4ND with the paste binding
  (`Ctrl+Shift+V` / `Cmd+Shift+V`). Paste delivers the committed Unicode text
  directly. If the paste contains a newline, a confirm overlay appears first —
  this is the multi-line-paste safety guard (see below).
- For shells that support it, type the codepoint directly (e.g. a shell's
  Unicode escape) rather than via the OS IME.

This is a known limitation tracked for the input layer; East-Asian *width*
(wide characters occupying two cells) is already handled correctly, so pasted
CJK text displays with the right column widths.

## GPU, transparency, or the window appears as a solid dark box

Symptoms: the window is set see-through (`opacity` below `1.0`) but renders
fully opaque; or the app crashes at startup on a machine with a third-party GPU
overlay layer installed.

Cause and fixes:

- **Transparency requires Vulkan on Windows.** A wgpu swapchain bound to a
  Win32 window through DX12/DXGI cannot per-pixel alpha-composite with the
  desktop, so a reduced `opacity` is a silent no-op under DX12. The window is
  always transparent-capable and C0PL4ND automatically selects the Vulkan
  backend for it. If the window still won't go see-through, lower the `opacity`
  slider (Settings → Appearance) below 100% and check `<config_dir>/gpu-diag.log`
  for the adapter its surface `alpha_modes` chose.
- **A Vulkan overlay layer crashes startup.** Some third-party Vulkan overlay
  layers (e.g. game/store overlays) corrupt the Vulkan instance and crash the
  renderer. Force the more robust DX12 backend by setting the `WGPU_BACKEND`
  environment variable before launching — this trades transparency for
  stability:

  ```sh
  WGPU_BACKEND=dx12 c0pl4nd      # Linux/macOS shells
  ```

  ```powershell
  $env:WGPU_BACKEND = "dx12"; c0pl4nd   # PowerShell
  ```

  Other accepted values include `vulkan` and `gl`. `WGPU_BACKEND` always
  overrides the automatic choice.
- **A working GPU is required — there is no software-render fallback.** C0PL4ND
  is GPU-accelerated (wgpu) and needs a graphics adapter exposing one of:
  **Vulkan** (Linux, Windows), **DirectX 12** (Windows 10+), or **Metal**
  (macOS); it falls back to **OpenGL 3.3+** only as a best effort. If no usable
  adapter is found (a headless server, a VM without GPU passthrough, or a broken
  driver), startup fails cleanly with a **"C0PL4ND couldn't start"** dialog and
  the app exits — it does **not** silently render on the CPU. To run without a
  hardware GPU, either enable GPU passthrough for the VM, update/repair the
  graphics driver, or install a software rasterizer and point wgpu at it — e.g.
  Mesa **lavapipe** (a software Vulkan device) on Linux, then:

  ```sh
  WGPU_BACKEND=vulkan c0pl4nd      # picks the lavapipe software adapter
  ```

## Animations / the CRT effect cause discomfort (reduced motion)

C0PL4ND honours a reduced-motion preference via the `C0PL4ND_REDUCED_MOTION`
environment variable. Set it (to any value) before launching to suppress
animated chrome such as the drag-ghost glide:

```sh
C0PL4ND_REDUCED_MOTION=1 c0pl4nd
```

The CRT/scanline post-effect is **off by default**; if you enabled it and find
the motion uncomfortable, turn it off again in the Effects settings or set
`crt = false` under `[effects]` in your `config.toml`.

## Multi-line paste seems blocked or shows a confirm prompt

By default, pasting clipboard text that contains a newline shows a confirm
overlay first (`paste_warn_multiline = true`). This is a deliberate security
feature: a multi-line paste can run shell commands the instant it lands. Press
`Enter` to confirm the paste or `Esc` to cancel. To paste multi-line content
without confirmation, set `paste_warn_multiline = false` under your config.

## Windows warns "Windows protected your PC" / unknown publisher on install

The Windows installer (`c0pl4nd-<version>-x86_64-setup.exe`) is **not
Authenticode-signed yet** (a code-signing certificate is pending), so Microsoft
SmartScreen shows a blue *"Windows protected your PC"* warning on first run.
This is expected for a new, independently-distributed app — it reflects the
absence of a paid publisher certificate, not a problem with the download.

- To proceed: click **More info → Run anyway**.
- The lack of Authenticode does **not** weaken update security. Every released
  binary and the update manifest are **minisign-signed** (Ed25519) and carry a
  **SLSA build-provenance attestation**, and the in-app updater verifies the
  signature + checksum against a key embedded in the app *before* installing —
  so automatic updates are cryptographically verified regardless of Authenticode.
- To verify a download yourself, check its `.minisig` against the project's
  public key, or run `gh attestation verify <file> --repo <owner>/<repo>` on the
  published asset.

## My config edit didn't take effect / I think it's invalid

- Config changes are applied live on save; if nothing changed, double-check you
  edited the file at the active path (below) and that the TOML is well-formed.
- C0PL4ND works with **no** config file — an empty or missing file falls back
  to built-in defaults, so a syntax error in your file is the likeliest cause
  of "my setting is ignored." Compare against the full example in
  [CONFIG.md](CONFIG.md).
- For the default keybindings and how to override them, see
  [docs/KEYBINDINGS.md](docs/KEYBINDINGS.md). The command palette
  (`Ctrl+Shift+P` / `Cmd+Shift+P`) always shows the live bindings.

## Where do config and logs live?

**Configuration file:**

- **Linux / macOS:** `~/.config/c0pl4nd/config.toml`
  (or `$XDG_CONFIG_HOME/c0pl4nd/config.toml` when `XDG_CONFIG_HOME` is set)
- **Windows:** `%APPDATA%\c0pl4nd\config.toml`

**Native window geometry** (size/position) is persisted by the app framework
under a per-app folder keyed on the app id `com.itashacorp.c0pl4nd`, alongside
the platform's application-state directory.

### Diagnostic logging

C0PL4ND writes diagnostic/tracing output to **standard error**. To capture it,
launch from a terminal with the log filter set:

```sh
C0PL4ND_LOG=debug c0pl4nd 2> c0pl4nd.log
```

`C0PL4ND_LOG` (and `RUST_LOG` as a fallback) accept the standard `tracing`
env-filter syntax — for example `C0PL4ND_LOG=c0pl4nd_core=trace` to trace just
the core engine. Attach the captured log to any bug report.

> Note: release builds run as a GUI-subsystem application and do not attach a
> console window, so redirect standard error to a file (as above) or run a
> debug build to see the log live.

## See also

- [CONFIG.md](CONFIG.md) — full configuration reference.
- [docs/KEYBINDINGS.md](docs/KEYBINDINGS.md) — default keyboard shortcuts.
- [docs/LAYOUT.md](docs/LAYOUT.md) — tabs, splits, and workspace layout.
- [SECURITY.md](SECURITY.md) — security disclosure process.
