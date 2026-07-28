# C0PL4ND Configuration

C0PL4ND is designed to be **great out of the box with zero configuration**. You never *have* to edit a config file. When you want to customize the terminal, everything lives in a single, readable [TOML](https://toml.io) file — never a scripting language, and never something a typo can lock you out of (an invalid config is reported and the previous good settings are kept).

## Config file location

| Platform | Path |
| --- | --- |
| Linux | `~/.config/c0pl4nd/config.toml` (or `$XDG_CONFIG_HOME/c0pl4nd/config.toml`) |
| macOS | `~/.config/c0pl4nd/config.toml` |
| Windows | `%APPDATA%\c0pl4nd\config.toml` |

If the file doesn't exist, C0PL4ND runs entirely on its defaults. Create the file to begin customizing.

## Live reload

Changes are applied **live the moment you save** — no restart required. If your edit contains a syntax error or an invalid value, C0PL4ND reports the problem and continues running with your last valid configuration, so a bad edit never leaves you locked out of your terminal. (A few fields — window opacity, acrylic/transparency, and font fallbacks — take full effect on the next launch.)

---

## Keys at a glance

Most settings are **top-level keys**; a handful are grouped into tables. This reflects the real config schema in `crates/core/src/config/mod.rs`.

| Key / table | Kind | What it controls |
| --- | --- | --- |
| `theme` | top-level string | Active color theme (default: `itasha-corp`) |
| `scrollback_lines` | top-level | Scrollback buffer size (lines per pane) |
| `opacity` | top-level | See-through level, `0.0` (fully transparent) ..= `1.0` (solid) |
| `tint` / `tint_strength` / `tint_enabled` | top-level | Optional colour wash over the window |
| `frost_enabled` / `frost_amount` / `frost_color` / `frost_grain` | top-level | Software frosted-glass wash |
| `ui_scale` | top-level | Whole-UI accessibility zoom (`0.5`..=`3.0`) |
| `startup_panel` | top-level | The neofetch-style launch splash (on by default) |
| `shell` / `term` | top-level | Override the child shell program / its `TERM` value |
| `ligatures` / `copy_on_select` / `paste_warn_multiline` / `history_capture_enabled` | top-level | Editor/selection/clipboard/history behaviour |
| `clipboard_read_allow` | top-level | **Security, default `false`.** Allow a program to READ the system clipboard via OSC 52 |
| `history_sidebar_side` / `view_mode` | top-level | Command-history sidebar side; pane shell layout |
| `[font]` | table | Font family, size, line height, fallback chain |
| `[cursor]` | table | Cursor shape and blink |
| `[window]` | table | Initial size (cols/rows), padding, persisted geometry |
| `[effects]` | table | The optional CRT/scanline + chromatic-aberration effects (off by default) |
| `[keybindings]` | table | Keybinding **reference** (not yet rebindable — see note below) |
| `[update]` | table | The optional update check |
| `[reporting]` | table | Opt-in W1TN3SS crash/issue reporting (off by default) |

---

## Theme

The **default theme is `itasha-corp`** — the Itasha.Corp house brand: a **void-black** background with **Itasha Purple `#7700FF`** structure and **Corp Green `#00FF90`** as the live/cursor voice. `theme` is a **top-level string** that names a theme file stem in the themes directory:

```toml
theme = "itasha-corp"   # the default; other built-in themes can be selected by name
```

There is no `[theme]` table — just the single top-level `theme` key.

## Font

```toml
[font]
family = "Monaspace Neon"   # any installed monospace font family
size = 14.0                  # points
line_height = 20.0           # cell vertical advance, in pixels
fallback = ["Noto Sans JP", "monospace"]   # glyphs the primary font lacks (CJK, etc.)
```

If the requested family isn't installed, C0PL4ND falls back through the `fallback` chain to a bundled monospace default.

## Window / opacity

`opacity` is a **top-level** key (not under `[window]`). The `[window]` table holds the initial grid size, inner padding, and the persisted geometry restored on the next launch.

```toml
opacity = 1.0   # top-level: 0.0 (fully see-through) .. 1.0 (solid)

[window]
cols = 80       # initial terminal width in columns
rows = 24       # initial terminal height in rows
padding = 8     # inner padding between the window edge and the grid, in pixels
# pos_x / pos_y / size_w / size_h / maximized / monitor are written automatically
# to remember your window geometry; you normally don't set these by hand.
```

## Transparency, tint & frost

Three independent controls shape the window's look:

- **Opacity** — the glass clarity. `0.0` = fully see-through (only the terminal
  text remains over the desktop); `1.0` = solid. Painted once, so it is linear —
  clear at low values, solid at 100%. Applies live.
- **Tint** — a colour wash over the window, INDEPENDENT of opacity (it colours the
  see-through glass at any opacity rather than fading with it).
- **Frost** — a software "frosted glass" wash: an adjustable diffuse tint with an
  optional grain texture, independent of opacity. It tints/diffuses the window; it
  does **not** blur the desktop behind the window (a real backdrop blur is not
  possible on this hardware). Off by default.

A fully-clear window is `opacity = 0` with the tint and frost off.

```toml
opacity = 1.0          # 0.0 (fully see-through) .. 1.0 (solid) — the glass clarity

# Tint: a colour wash (works at any opacity)
tint = "#08060d"       # #RRGGBB colour (brand-canon VOID BLACK)
tint_strength = 0.0    # 0.0 (no tint) .. 1.0 (strong)
tint_enabled = true    # master ON/OFF for the tint wash

# Frost: software frosted glass (independent of opacity)
frost_enabled = false  # master ON/OFF
frost_amount = 0.25    # 0.0 (clear) .. 1.0 (max; capped so text stays legible)
frost_color = ""       # #RRGGBB, or empty to follow the theme background
frost_grain = true     # subtle procedural grain so the frost reads as diffused glass
```

> **Hybrid-GPU laptops (NVIDIA + Intel):** a see-through window needs a GPU whose
> swapchain surface exposes a transparent `CompositeAlphaMode`. The discrete GPU
> often reports `Opaque`-only (→ solid black window), so C0PL4ND auto-selects the
> **integrated** GPU (matching the sibling app SCR1B3). If the window is still
> opaque, check `<config_dir>/gpu-diag.log` — it lists every adapter, its surface
> `alpha_modes`, and the one chosen.

> **Upgrading from an older config?** The retired multi-mode keys
> (`transparency_enabled`, `window_mode`, `acrylic`, and the OS blur backdrops
> `glass`/`mica`/`vibrancy`/`dim`) are simply **ignored** on load — your file still
> parses, and the retained `opacity` value carries the see-through level. On the
> hybrid-GPU target those OS blur backdrops never composited, so they were dropped
> in favour of the one portable effect that works.

## Cursor

```toml
[cursor]
style = "block"   # "block" | "bar" | "underline"
blink = true
```

## Scrollback

`scrollback_lines` is a **top-level** key (there is no `[scrollback]` table):

```toml
scrollback_lines = 10000   # number of lines retained per pane
```

## UI scale (accessibility zoom)

```toml
ui_scale = 1.0   # whole-interface zoom multiplier, clamped to 0.5 .. 3.0 (1.0 = 100%)
```

This is the persisted zoom for the entire UI (chrome + grid), distinct from the transient `Ctrl/Cmd +`/`-` keyboard zoom.

## Shell & TERM

```toml
# shell = "/bin/zsh"     # override the child shell; omit to use the platform default
term = "xterm-256color"  # the TERM advertised to the child shell (default: xterm-256color)
```

`COLORTERM` is always `truecolor` and is not configurable.

## Editor, selection & history behaviour

```toml
ligatures = false            # enable programming ligatures / complex text shaping
copy_on_select = false       # X11-style: copy a mouse selection the moment the drag ends
paste_warn_multiline = true  # confirm before pasting clipboard text containing a newline (a safety feature)
clipboard_read_allow = false # SECURITY, default deny: may a program in a pane READ your system
                             # clipboard via `OSC 52 ; c ; ?`. Writing TO the clipboard from a
                             # program always works and is unaffected. While denied, a read query
                             # is answered with an empty payload, so well-behaved programs
                             # continue instead of hanging. Turn on only if you need it.
history_capture_enabled = true   # record echoed commands for the palette + history sidebar
history_sidebar_side = "right"   # which side the command-history sidebar docks to: "left" | "right"
view_mode = "grid"               # pane shell layout: "grid" (tiling) | "tabs" (single full-size pane)
```

## CRT / scanline & chromatic-aberration effects

Retro post-effects are available for the full *into the wired* aesthetic. They are **off by default** for clarity and performance.

```toml
[effects]
crt_scanlines = false              # master toggle for the CRT/scanline overlay
scanline_darkness = 0.4            # 0.0 (none) .. 1.0 (strong) — scanline trough darkness
chromatic_aberration_enabled = false   # explicit ON/OFF for chromatic aberration
chromatic_aberration = 0.0         # intensity; only applied when chromatic_aberration_enabled = true
```

`scanline_darkness` defaults to `0.4` (so enabling scanlines reads as distinct lines, not a flat grey film). Chromatic aberration is a checkbox plus an enabled-gated intensity — the intensity does nothing until `chromatic_aberration_enabled = true`.

## Startup panel

On launch, C0PL4ND shows a neofetch-style splash: a brand ASCII logo beside your local system stats (OS, kernel, host, uptime, shell, terminal, CPU, memory, GPU). It reads only local facts and never touches the network. On by default.

```toml
startup_panel = true   # set false to launch straight to a clean prompt
```

## Updates

C0PL4ND can check whether a newer release exists. This is the **only** outbound
network feature; the check is **read-only and sends zero identifiers** (see
[PRIVACY.md](PRIVACY.md)). The default is `notify`.

```toml
[update]
mode = "notify"            # off | notify | manual | auto
check_interval_hours = 24  # hours between on-launch checks (notify/auto); 1..=168
check_on_launch = false    # legacy on-launch toggle, retained so older configs keep loading
channel = "stable"         # release channel to track
```

- **`notify`** *(default)* — once per launch (at most once per
  `check_interval_hours`) check GitHub Releases and show a passive toast if a
  newer version exists. Never downloads or installs on its own.
- **`manual`** — **no on-launch network**; check only when you press
  "Check for updates" in Settings (or run `c0pl4nd update`).
- **`off`** — never check, never touch the network for updates.
- **`auto`** — like `notify`, but also downloads and applies a
  cryptographically verified (SHA-256 + minisign, against a signed `latest.json`
  manifest) update when one is found.

`check_on_launch` is a legacy compatibility flag; `mode` is the canonical
control. A network-on-launch `mode` (`notify`/`auto`) **or** `check_on_launch =
true` performs an on-launch check.

## Reporting (opt-in W1TN3SS)

Crash/error/issue reporting is **opt-in** and **both streams default OFF** — nothing is captured-for-send or transmitted until you explicitly opt in from Settings → Privacy. An older config with no `[reporting]` table loads with reporting fully off.

```toml
[reporting.streams]
crash_reports = "off"   # off | ask_each_time | … (default: off)
manual_issues = "off"   # off | ask_each_time | … (default: off)

[reporting.issue_intake]
repo = "46b-ETYKiAL/Itasha.Corp_C0PL4ND"      # the GitHub owner/repo the "Report an issue" deep link targets
mailto_alias = "46b.AbandonSomething@proton.me"   # the mailto: fallback address
```

## Keybindings

C0PL4ND ships with sensible default keybindings (open the **command palette** with `Ctrl/Cmd+Shift+P` to discover available actions and their current shortcuts). For a scannable, code-verified table of every default binding, see **[docs/KEYBINDINGS.md](docs/KEYBINDINGS.md)** — that page is canonical.

> **Not yet rebindable.** In the current shell the keybindings are **fixed**. The `[keybindings]` table below (and its read-only mirror in the Settings window) is reserved for a future rebinding dispatcher; **editing it does not yet change the live shortcuts**. It is shown here so you can see the action names that will become rebindable.

The schema is `action = chord` (an action name on the left, a chord string on the right) — not `chord = action`. The `mod` modifier maps to **Ctrl** on Windows/Linux and **Cmd** (⌘) on macOS:

```toml
[keybindings]
copy             = "mod+shift+c"
paste            = "mod+shift+v"
new_tab          = "mod+shift+t"
close_tab        = "mod+shift+w"
next_tab         = "mod+shift+]"
split_right      = "mod+shift+d"
split_down       = "mod+shift+e"
search           = "mod+shift+f"
command_palette  = "mod+shift+p"
history_sidebar  = "mod+shift+h"
increase_font    = "mod+plus"
decrease_font    = "mod+minus"
```

---

## Full example `config.toml`

A configuration with the common keys shown at their default values. Copy this to your config path and edit freely — every key is optional, and C0PL4ND works fine with an empty file.

```toml
# ~/.config/c0pl4nd/config.toml
# (Windows: %APPDATA%\c0pl4nd\config.toml)
#
# Every setting here is shown at its default value.
# Delete any line to fall back to the default.

theme = "itasha-corp"         # void-black + Itasha Purple #7700FF + Corp Green #00FF90
scrollback_lines = 10000
opacity = 1.0                 # 0.0 (fully see-through) .. 1.0 (solid) — the single transparency control
ui_scale = 1.0                # 0.5 .. 3.0
startup_panel = true          # neofetch-style splash on launch

# Optional colour wash over the window (works at any opacity)
tint = "#08060d"
tint_strength = 0.0           # 0.0 (no tint) .. 1.0 (strong)
tint_enabled = true           # master ON/OFF for the tint wash

# Software frosted glass (independent of opacity; not a real desktop blur)
frost_enabled = false
frost_amount = 0.25           # 0.0 (clear) .. 1.0 (max; capped for legibility)
frost_color = ""              # #RRGGBB, or empty to follow the theme background
frost_grain = true            # subtle procedural grain

# Shell / behaviour
term = "xterm-256color"
ligatures = false
copy_on_select = false
paste_warn_multiline = true
clipboard_read_allow = false    # deny OSC 52 clipboard READ (security)
history_capture_enabled = true
history_sidebar_side = "right"  # "left" | "right"
view_mode = "grid"              # "grid" | "tabs"

[font]
family = "Monaspace Neon"
size = 14.0
line_height = 20.0
fallback = ["Noto Sans JP", "monospace"]

[cursor]
style = "block"               # "block" | "bar" | "underline"
blink = true

[window]
cols = 80
rows = 24
padding = 8

[effects]
crt_scanlines = false             # CRT/scanline overlay — OFF by default
scanline_darkness = 0.4           # 0.0 .. 1.0
chromatic_aberration_enabled = false
chromatic_aberration = 0.0

[update]
mode = "notify"               # off | notify | manual | auto (read-only check)
check_interval_hours = 24     # 1 .. 168
check_on_launch = false       # legacy compatibility flag
channel = "stable"

[reporting.streams]
crash_reports = "off"         # opt-in; OFF by default
manual_issues = "off"         # opt-in; OFF by default

# [keybindings] is a read-only reference until rebinding is wired (see note above).
[keybindings]
copy            = "mod+shift+c"
paste           = "mod+shift+v"
new_tab         = "mod+shift+t"
close_tab       = "mod+shift+w"
next_tab        = "mod+shift+]"
split_right     = "mod+shift+d"
split_down      = "mod+shift+e"
search          = "mod+shift+f"
command_palette = "mod+shift+p"
history_sidebar = "mod+shift+h"
increase_font   = "mod+plus"
decrease_font   = "mod+minus"
```

---

## Notes

- **Defaults are the product.** The single most-praised property of a great terminal is being usable instantly. Treat config as optional polish, not a requirement.
- **No programming language.** Configuration is declarative TOML by design. There is no scripting runtime to learn, and no way for a config file to make the terminal unusable.
- **Privacy.** Nothing in your config — and nothing about your shell session — is transmitted anywhere unless you explicitly opt into update checks or reporting. See [SECURITY.md](SECURITY.md) and [PRIVACY.md](PRIVACY.md).
