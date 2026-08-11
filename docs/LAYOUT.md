# C0PL4ND — Layout & Multiplexing

C0PL4ND uses a **split-tree** to arrange multiple terminals in a single window, with **drag-to-rearrange** and **pane zoom** — all keyboard-first, none of it required if you only want a single pane.

---

## The grid model

```
WINDOW
└── TAB(s)
    └── LAYOUT (split-tree, up to MAX_PANES = 6 leaves)
        └── CELL (each leaf)
            └── TERMINAL (PTY)
```

- Each **window** holds one or more **window-level tabs**.
- Each window-level tab holds a **split-tree layout** of up to **six cells (panes)**.
- Each **cell** owns a terminal (PTY) with its own shell + scrollback.
- Splits run horizontally (side-by-side) or vertically (top/bottom).
- A single-pane tab draws **no** pane chrome — visually identical to a non-multiplexed terminal.
- More than one pane gets a 1-px chrome: the **focused** pane carries a subtle signal-teal border; the others a muted grey.

`MAX_PANES = 6` (`crates/app/src/egui_app/grid.rs:23`) is a readability guardrail — past six panes the per-cell text becomes too small to scan. Trying to split past it is blocked with a transient notice.

---

## Default keybindings

These are the keybindings the app ships with. In the current shell they are **fixed** — not yet user-rebindable (the `[keybindings]` config section is a read-only reference until the rebinding dispatcher lands). See **[docs/KEYBINDINGS.md](KEYBINDINGS.md)** for the canonical, code-verified list.

The `mod` modifier is **Ctrl** on Windows/Linux and **Cmd** (⌘) on macOS.

### Window-level tabs
| Action | Key |
|---|---|
| New tab | `Ctrl+Shift+T` |
| Close focused tab or pane | `Ctrl+Shift+W` |
| Next tab | `Ctrl+Shift+]` |

### Splits & panes
| Action | Key |
|---|---|
| Split right (vertical) | `Ctrl+Shift+D` |
| Split down (horizontal) | `Ctrl+Shift+E` |
| Focus pane by direction | `Ctrl/Cmd+Shift + Arrow` |
| Pane zoom (toggle) | `Ctrl+Shift+Z` |

`Equalize Cells` is run from the **command palette** (`Ctrl+Shift+P`), not from a dedicated chord.

### Search & palette
| Action | Key |
|---|---|
| Search scrollback | `Ctrl+Shift+F` |
| Command palette | `Ctrl+Shift+P` |

---

## Drag-to-rearrange

Panes are rearranged by dragging them within the split-tree. The grid is built on
[`egui_tiles`](https://docs.rs/egui_tiles) (`crates/app/src/egui_app/grid.rs`), and drag-to-rearrange
is that crate's own affordance: drag a pane and drop it against an edge of another
pane to re-split the tree around it.

To move keyboard focus between panes without the mouse, use `Ctrl/Cmd+Shift + Arrow`
(focus the adjacent pane in that direction).

---

## Not in this shell

The repository also contains a second, **unshipped** layout engine
(`crates/core/src/layout/`, `crates/core/src/layout_persist.rs`) used only by the
`c0pl4nd-legacy` binary, which is gated behind the default-off `legacy-winit`
feature and is not part of any release build.

The following belong to that engine and are **not** available in the shell you are
running. They are listed here only because earlier revisions of this document
described them as current:

- Quick-layout presets (`Layout: 1x2`, `2x2`, `1+3`, `2x3`, …)
- Named workspace save/restore (`Save Layout As…` / `Restore Layout`)
- `Reset Layout`
- Nested tabs *within* a cell (each cell owns exactly one terminal)
- The custom five-zone (top/bottom/left/right/centre-merge) drop classifier and
  its centre-merge behaviour
- Automatic restore of a saved default layout on launch, and the corrupt-layout-file
  fallback that went with it

C0PL4ND starts with a single pane — the zero-config baseline.

---

## Architecture notes

- The shipped grid is an `egui_tiles::Tree<Pane>` in `crates/app/src/egui_app/grid.rs`;
  pane interaction lives alongside it in the same module directory.
- `MAX_PANES` is defined at `crates/app/src/egui_app/grid.rs:23`.
- `crates/app/src/drag.rs` (five-zone classifier) and `crates/app/src/pane_render.rs`
  are declared from `crates/app/src/main.rs` and used only by `crates/app/src/window.rs` —
  i.e. they are part of the `legacy-winit` binary, not the shipped shell.
- The split-tree engine at `crates/core/src/layout/` and its JSON persistence at
  `crates/core/src/layout_persist.rs` are likewise legacy-only.
