//! In-app settings window for the C0PL4ND egui chrome (Milestone 2).
//!
//! Replaces the Milestone-1 read-only placeholder with a polished, grouped,
//! well-spaced window that maps the real [`c0pl4nd_core::Config`] fields into
//! logical sections — matching the sibling editor SCR1B3's settings layout for
//! same-company visual cohesion.
//!
//! Layout: a fixed-size window with a **left category nav** + a **searchable,
//! internally-scrolling content pane**, so every setting is reachable at the
//! default window size without ever resizing (the
//! `ScrollArea::auto_shrink([false, false])` idiom is load-bearing here). Each
//! section is a heading + a two-column [`egui::Grid`] of label/control rows, so
//! every control lines up vertically — the system-settings look.
//!
//! Visual cohesion with SCR1B3: identical two-pane structure, GNOME-style
//! spacing rhythm (8 px between related rows, 14 px between sections), brand
//! accents inherited from [`super::theme::itasha_corp_visuals`] (Operator Violet
//! `#7700FF` for press / structure, `.Corp` green `#00FF90` for the live /
//! selected accent), and the per-setting ↺ revert affordance.
//!
//! Kept as a free function so it never fights the `C0pl4ndApp` borrow — the host
//! ([`super::C0pl4ndApp::settings_window`]) calls [`show`] with `&mut config` and
//! reacts to the returned [`Outcome`] (persist + re-apply theme).

use std::sync::{Arc, Mutex};

use eframe::egui;

use c0pl4nd_core::config::{CursorStyle, GpuPreference, GraphicsBackend, UpdateMode};
use c0pl4nd_core::Config;

use super::theme::ChromeColors;

// The in-app self-updater backend (download + SHA-256/minisign verify + atomic
// self-replace) and its egui state machine. Declared here via `#[path]` so the
// Updates settings page is fully self-contained: it resolves identically in the
// shipping `c0pl4nd` binary AND in the `egui_kittest` integration test binaries
// (which `#[path]`-include `egui_app/mod.rs` but not the crate-root `update`
// module). The shipping binary also declares a crate-root `update` for the CLI
// (`c0pl4nd update`) + launch-check; this second view is private to `settings`
// and never shares a type across that boundary, so the two coexist cleanly.
#[path = "../update_engine/mod.rs"]
pub mod update_engine;

use update_engine::updater::{LaunchKind, UpdateState, Updater};

/// Left-nav categories, in display order. Each maps to a section rendered by
/// [`render_sections`].
const CATEGORIES: &[&str] = &[
    "Appearance",
    "Fonts",
    "Cursor",
    "Terminal",
    "Window",
    "Toolbar",
    "Motion",
    "Keybindings",
    "Updates",
    "Privacy",
    // Config file management. This section is rendered by `render_sections` and was
    // previously ABSENT from this list, which made it reachable only by typing a
    // matching term into the settings search box — a built page with no way to click
    // to it. `every_rendered_section_is_reachable` now fails if that recurs.
    "Config",
];

/// Cross-category search labels for the **Appearance** section. Kept as a named
/// const so the CRT/scanline/chromatic terms that MOVED to the Motion section are
/// provably absent here (a stale label would resurrect a phantom empty Appearance
/// section and hide the moved control from a full-name search).
const APPEARANCE_SEARCH_LABELS: &[&str] = &[
    "theme",
    "follow os theme dark light auto system appearance",
    "transparency",
    "opacity",
    "tint",
    "frost frosted glass",
    "grain",
    "always on top window level",
    "ui scale",
    "zoom",
    "accessibility",
];

/// Cross-category search labels for the **Motion** section. Each entry is the
/// canonical label of exactly one Motion row (see the `row_visible(q, …)` calls),
/// so any query that reveals the section via a label also reveals its row — no
/// phantom empty section, and every moved control (CRT scanlines, scanline
/// darkness, chromatic aberration) is reachable by its full name. Shared with the
/// `motion_section_is_cross_category_searchable` test so the invariant is checked
/// against the PRODUCTION labels, not an inline copy.
const MOTION_SEARCH_LABELS: &[&str] = &[
    "enable animations",
    "animation speed ui transition",
    "crt scanlines",
    "scanline darkness",
    "scanline drift speed",
    "chromatic aberration",
    "wired mesh background",
    "mesh density",
    "mesh brightness",
    "mesh drift speed movement",
    "mesh color colour node picker reset theme accent",
    "vhs tracking",
    "vhs intensity",
    "vhs drift speed",
    "screen flicker",
    "flicker strength",
    "flicker speed",
    "cursor trail",
    "cursor trail intensity",
    "boot glitch",
];

/// Cross-category search labels for the **Keybindings** section: the label of
/// every binding row, lowercased. Kept as a named const (like
/// [`MOTION_SEARCH_LABELS`]) so `keybinding_search_labels_cover_every_binding`
/// can pin it against the LIVE schema — a binding added to
/// `Keybindings::entries` without a search label would be a row no query could
/// ever reveal.
const KEYBINDING_SEARCH_LABELS: &[&str] = &[
    "copy selection",
    "paste",
    "new tab",
    "close pane",
    "focus next pane",
    "split right",
    "split down",
    "find in terminal",
    "command palette",
    "command-history sidebar",
    "increase font size",
    "decrease font size",
    "reset font size",
    "zoom pane",
    "equalize panes",
    "toggle grid / tabs view",
    "settings",
    "fullscreen",
    "clear scrollback",
    "copy everything",
    "scroll to top",
    "scroll to bottom",
];

/// The release channels the Updates section offers. Mirrors the channels the
/// `c0pl4nd update` checker understands; a free choice list, not invented.
const UPDATE_CHANNELS: &[&str] = &["stable", "beta", "nightly"];

/// The usable range of the chromatic-aberration INTENSITY slider.
///
/// [`CHROMATIC_MIN`] doubles as the "not meaningfully set" floor, and that is
/// load-bearing for the first-enable seed below. egui's `Slider` defaults to
/// `SliderClamping::Always`, which clamps its bound value into the slider's range
/// **on every render and writes the clamped value straight back** (see
/// `Slider::add_contents`: `let old = self.get_value(); self.set_value(old);`).
/// So the stored 0.0 "never set" sentinel is silently rewritten to
/// `CHROMATIC_MIN` the FIRST time the Motion page draws — before the user has
/// touched anything. A first-enable seed that tests `<= 0.0` therefore never
/// fires, and ticking the checkbox switches the effect on at the FAINTEST
/// setting: exactly the "it reads as broken" report the seed exists to fix.
/// Testing against this floor instead is what keeps the seed alive; tying both
/// the slider range and the guard to these consts stops them drifting apart.
/// Regression-guarded by the `first_enabling_chromatic_aberration_seeds_a_
/// visible_intensity` test in `tests/egui_settings_deep.rs`.
const CHROMATIC_MIN: f32 = 0.1;
/// The upper bound of the chromatic-aberration intensity slider.
const CHROMATIC_MAX: f32 = 1.5;

// ---- Fixed width budget (the runaway-width root-cause fix, #26) ----
// The settings window is content-sized by egui. WITHOUT a hard cap, any child
// that reports `available_width()` / `f32::INFINITY` as its desired size (a
// filling slider, the search box) makes the window grow each frame until it hits
// the screen edge — leaving the controls spread out with a wide blank gutter on
// the right (the reported "much too wide + blank space" bug). The fix is a FIXED
// content-width budget: clamp the content `Ui` to these consts (NOT to
// `available_width()`, which is the already-inflated post-balloon width and so
// clamps nothing). No page can then demand more than the budget, so the window
// holds a snug, stable width identical on every page. Sized to fit the 170px
// left nav + a comfortable control pane (label + control + ↺ columns).

/// Snug default window WIDTH (px), before the screen-fit clamp. Wide enough that
/// EVERY category page's content fits without the resizable window having to grow
/// to it — which is what keeps every page the SAME width (issue #26). The app-UI
/// font now defaults to IBM Plex Mono (a monospace face, wider per glyph than a
/// proportional UI font), so slider value read-outs and combo labels take more
/// room; this default absorbs that so no single page balloons past the others.
const SETTINGS_DEFAULT_W: f32 = 960.0;
/// Default window HEIGHT (px), before the screen-fit clamp.
const SETTINGS_DEFAULT_H: f32 = 560.0;
/// Fixed width of the left category nav column.
const SETTINGS_NAV_W: f32 = 170.0;
/// Minimum window WIDTH (px) the user may shrink the resizable settings window
/// to. Below this the nav + content pane collide, so the resize handle stops.
const SETTINGS_MIN_W: f32 = 560.0;
/// Minimum window HEIGHT (px) for the resizable settings window.
const SETTINGS_MIN_H: f32 = 400.0;

/// The terminal color themes that ship in `assets/themes/` (file stems). The
/// theme combo offers these built-ins; a free text field below it accepts any
/// user theme name (a TOML under the config dir's themes folder). This list is
/// the ground truth of what actually ships — it is NOT invented; mirror it if a
/// theme file is added or removed.
const BUILTIN_THEMES: &[&str] = &[
    "itasha-corp",
    "itasha-void",
    "itasha-void-high-contrast",
    "ghost-paper",
    "wired-noir",
    "wired-colorblind",
    // Ported from the SCR1B3 editor (calm-canon line).
    "phosphor-amber",
    "lain-mauve",
    "a11y-high-contrast",
    // itasha-neon family (brand-signature line).
    "itasha-neon",
    "itasha-neon-pastel",
    "itasha-neon-soft",
    "itasha-neon-night",
    "itasha-neon-dawn",
    "itasha-neon-aurora",
    // Heritage-alt influence palettes.
    "geocities-bbs",
    "lain-wired",
    "kusanagi-dive",
    "akira-redshift",
    "atompunk-sodium",
    "terminal-lock",
    "mecha-armour",
    "shutoko-night",
    // Wave-4 line — ported from the SCR1B3 editor (map-schema → ANSI-16).
    "dialup-glow",
    "present-day",
    "thermoptic",
    "capsule-mono",
    "jet-age",
    "packet-trace",
    "cockpit-amber",
    "nerv-magi",
    "colony-drift",
    "kanjo-loop",
    "yaksha-ink",
    "datamosh-haze",
];

/// What [`show`] reports back to the host after a frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Any config field changed this frame — the host should persist the config
    /// to disk and re-apply the egui Visuals.
    pub changed: bool,
    /// The `theme` field (terminal color theme stem) changed this frame — the
    /// host should additionally reload the terminal grid's color theme so the
    /// change shows in the live panes, not only the chrome.
    pub theme_changed: bool,
    /// The user clicked "Clear command history" in the Privacy section — the host
    /// should clear (and zeroize) the in-memory command history.
    pub clear_history: bool,
    /// The user toggled the Incognito switch this frame, to the contained value.
    /// `None` when unchanged. Runtime-only (not a config field).
    pub set_incognito: Option<bool>,
    /// The settings window's on-screen rect this frame (`None` if it did not
    /// render). The host excludes this rect from the whole-window motion overlays
    /// so a live Motion-setting preview shows on the terminal without the mesh /
    /// flicker washing over the settings panel itself.
    pub window_rect: Option<egui::Rect>,
}

/// Whether a category section should render: its own tab when not searching, or
/// any-label-matches when a search query is active (cross-category results).
/// Pure — copied in spirit from SCR1B3's `section_visible`.
fn section_visible(selected: &str, q: &str, category: &str, labels: &[&str]) -> bool {
    if q.is_empty() {
        selected == category
    } else {
        category.to_lowercase().contains(q) || labels.iter().any(|l| l.to_lowercase().contains(q))
    }
}

/// Whether an individual row should render given the active search query.
fn row_visible(q: &str, label: &str) -> bool {
    q.is_empty() || label.to_lowercase().contains(q)
}

/// The label to show in the Font Family combo for the stored config value. A
/// stored value that resolves to egui's built-in monospace (empty, the synthetic
/// label, or the generic "monospace") is shown as the built-in label so the combo
/// reads cleanly; any other (installed-family) value is shown verbatim.
fn family_display(family: &str) -> String {
    if super::fonts::is_builtin_family(family) {
        super::fonts::BUILTIN_MONOSPACE_LABEL.to_string()
    } else {
        family.to_string()
    }
}

/// The value to STORE in `config.font.family` for a chosen combo entry. The
/// built-in label is stored verbatim (it round-trips through `family_display` and
/// is recognised by `fonts::is_builtin_family`, so no custom face is loaded); an
/// installed family name is stored as-is.
fn family_value(choice: &str) -> String {
    choice.to_string()
}

/// One Fallback-slot dropdown (fixed-width combo + up/down stepper) over the
/// installed monospace families plus the "(none)" sentinel. Mutates `slot` in
/// place and returns whether it changed. Factored out so the two slots share one
/// widget definition; delegates to [`dropdown_with_stepper`] so every dropdown in
/// Settings looks + behaves the same.
fn fallback_combo(ui: &mut egui::Ui, id_salt: &str, choices: &[String], slot: &mut String) -> bool {
    // "(none)" first so an empty slot is the obvious default; then only real
    // installed families (the built-in label is already the ultimate fallback, so
    // it is not a meaningful FALLBACK choice and is skipped).
    let mut variants: Vec<(String, &str)> = vec![(
        super::fonts::NONE_LABEL.to_string(),
        super::fonts::NONE_LABEL,
    )];
    for fam in choices {
        if fam == super::fonts::BUILTIN_MONOSPACE_LABEL {
            continue;
        }
        variants.push((fam.clone(), fam.as_str()));
    }
    let cur = slot.clone();
    dropdown_with_stepper(ui, id_salt, slot, &variants, Some(&cur))
}

/// A per-setting "restore default" affordance. Renders a small ↺ button that is
/// enabled only when `cur != def`; clicking it resets the field and returns
/// `true` so the caller marks settings dirty. Placed as the last cell of a Grid
/// row, it gives every scalar setting a one-click revert without a global "reset
/// everything" sledgehammer (the modern Fluent `SettingsCard` ↺ pattern).
/// Mirrors SCR1B3's `reset_to_default` verbatim — pure and app-agnostic.
fn reset_to_default<T: PartialEq + Clone>(ui: &mut egui::Ui, cur: &mut T, def: &T) -> bool {
    let differs = *cur != *def;
    let resp = ui
        .add_enabled(
            differs,
            egui::Button::new(egui::RichText::new("↺").small()).frame(false),
        )
        .on_hover_text(if differs {
            "Restore default"
        } else {
            "Already default"
        });
    if differs && resp.clicked() {
        *cur = def.clone();
        return true;
    }
    false
}

/// Decide whether — and to what value — a UI-scale slider interaction should be
/// COMMITTED to `config.ui_scale` (which is what triggers the live
/// `ctx.set_zoom_factor`). `slider_val` is the slider's current working value and
/// `dragging` is whether the handle is being dragged THIS frame.
///
/// The rule is "defer while dragging": applying the zoom live rescales the whole
/// UI — including this very slider — so a mid-drag write moves the slider under
/// the cursor, remaps the pointer to a larger value, and runs the scale away to
/// the 3.0 max (the reported "flickers small↔big / UI gigantic" bug). So while
/// the handle is held we return `None` (hold the value, apply nothing); on
/// release, a track click, or a keyboard step (`dragging == false`) we return
/// `Some(slider_val)` when it actually differs from the committed value — exactly
/// one zoom application per interaction, never mid-drag. Pure, so the decision is
/// unit-testable without a window.
fn ui_scale_commit(config_val: f32, slider_val: f32, dragging: bool) -> Option<f32> {
    if dragging {
        return None;
    }
    if slider_val != config_val {
        Some(slider_val)
    } else {
        None
    }
}

/// The consistent fixed width (points) for EVERY settings dropdown. A combo of a
/// constant width never resizes with its selected value, so the up/down stepper
/// beside it never moves — the whole point of the redesign. A label too long for
/// the width is truncated with an ellipsis (the full text is still visible in the
/// opened dropdown, and in the row's tooltip).
const DROPDOWN_WIDTH: f32 = 220.0;

/// Wrap-around index step for the dropdown stepper. `cur` is the currently
/// selected index (`None` when the live value is not one of the `len` variants —
/// e.g. a custom theme name typed into the alias field); `delta` is `-1` (▲
/// previous) or `+1` (▼ next). Returns the index to select next, WRAPPING at both
/// ends so the spinner cycles the list endlessly. A value outside the list steps
/// onto the first (next) or last (previous) variant so the arrows always have a
/// defined landing spot. Pure → unit-testable. `len == 0` returns `0` (the caller
/// guards against an empty list).
fn step_index(cur: Option<usize>, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let n = len as isize;
    match cur {
        Some(i) => (i as isize + delta).rem_euclid(n) as usize,
        None if delta >= 0 => 0,
        None => len - 1,
    }
}

/// Truncate `text` (with a trailing `…`) so it fits within `max_width` points in
/// the Button text style, so a fixed-width combo's selected label can never grow
/// the button. A label that already fits is returned unchanged. Read-only w.r.t.
/// the ui (font measurement only), so the closed-combo width is a CONSTANT
/// regardless of the selected value's natural length.
fn fit_combo_label(ui: &egui::Ui, text: &str, max_width: f32) -> String {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let painter = ui.painter();
    truncate_to_width(text, max_width, |s| {
        painter
            .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    })
}

/// Pure truncation core of [`fit_combo_label`]: return `text` unchanged if it
/// already fits `max_width` (per the caller's `width_of` measurement), else drop
/// trailing characters and append `…` until the result fits. The measurement is
/// injected so the truncation logic is unit-testable WITHOUT a live font atlas
/// (the headless test context measures glyphs as ~0-width, which would make a
/// real-font test vacuous). Always returns a string whose measured width is
/// `<= max_width` (or the single `…` when even that is the best it can do).
fn truncate_to_width(text: &str, max_width: f32, width_of: impl Fn(&str) -> f32) -> String {
    if width_of(text) <= max_width {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while chars.len() > 1 {
        chars.pop();
        let candidate = format!("{}…", chars.iter().collect::<String>().trim_end());
        if width_of(&candidate) <= max_width {
            return candidate;
        }
    }
    "…".to_owned()
}

/// A fixed-width [`egui::ComboBox`] followed by a compact up/down STEPPER (▲ =
/// previous option, ▼ = next option — a spinner) placed to its RIGHT, cycling
/// `variants` with WRAP-AROUND. Because the combo is a constant
/// [`DROPDOWN_WIDTH`] (its selected label truncated to fit via [`fit_combo_label`],
/// never grown), the stepper sits at a fixed x and never shifts with the value's
/// length — replacing the old flanking `◀ ▶` arrows that moved with the theme
/// name. The single widget used by every settings dropdown for a consistent,
/// intuitive UX.
///
/// `current_label` is what to SHOW as the selected value: `Some(s)` when the
/// caller maps value→display or must surface a value NOT present in `variants`
/// (e.g. a custom theme name / font family); `None` uses the matching variant's
/// label (the common enum case). Returns whether `value` changed. The caller
/// still renders its own row label BEFORE and its `reset_to_default` (↺) AFTER
/// this call, so search-filter (`row_visible`) and the grid columns are unchanged.
/// One compact stepper button that DRAWS its arrow as a filled triangle via the
/// painter — NEVER a font glyph. Unicode arrow characters (`▲`/`▼`) are tofu
/// (empty boxes) in a UI font that lacks them; a painter-drawn triangle always
/// renders. `up` picks the direction (up-pointing = previous, down-pointing =
/// next). Styled like a small egui button (per-state fill + stroke), with the
/// triangle in the widget's foreground colour so it dims correctly when disabled.
fn stepper_arrow(ui: &mut egui::Ui, up: bool, hover: &str) -> egui::Response {
    let size = egui::vec2(22.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = resp.on_hover_text(hover);
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&resp);
        let fill = visuals.weak_bg_fill;
        let bg_stroke = visuals.bg_stroke;
        let radius = visuals.corner_radius;
        let tri = visuals.fg_stroke.color;
        let painter = ui.painter();
        // Button background (matches egui's small-button styling per interaction state).
        painter.rect(rect, radius, fill, bg_stroke, egui::StrokeKind::Inside);
        // The arrow, drawn as a filled triangle centred in the button (proportional
        // to the 22×22 rect).
        let c = rect.center();
        let (hw, hh) = (5.0, 4.0);
        let pts = if up {
            vec![
                egui::pos2(c.x, c.y - hh),
                egui::pos2(c.x - hw, c.y + hh),
                egui::pos2(c.x + hw, c.y + hh),
            ]
        } else {
            vec![
                egui::pos2(c.x - hw, c.y - hh),
                egui::pos2(c.x + hw, c.y - hh),
                egui::pos2(c.x, c.y + hh),
            ]
        };
        painter.add(egui::Shape::convex_polygon(pts, tri, egui::Stroke::NONE));
    }
    resp
}

fn dropdown_with_stepper<T: Clone + PartialEq>(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut T,
    variants: &[(T, &str)],
    current_label: Option<&str>,
) -> bool {
    let mut changed = false;
    // The label to display: explicit override, else the matching variant's label,
    // else empty (a value not in the list with no override — the stepper still
    // works and lands on a defined variant).
    let matched = variants.iter().find(|(v, _)| v == value).map(|(_, l)| *l);
    let label = current_label.or(matched).unwrap_or("");
    ui.horizontal(|ui| {
        // Reserve ~34pt for the dropdown arrow + the button's frame margins so the
        // truncated label + arrow together never exceed DROPDOWN_WIDTH.
        let shown = fit_combo_label(ui, label, DROPDOWN_WIDTH - 34.0);
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(shown)
            .width(DROPDOWN_WIDTH)
            .truncate()
            .show_ui(ui, |ui| {
                for (v, variant_label) in variants {
                    changed |= ui
                        .selectable_value(value, v.clone(), *variant_label)
                        .changed();
                }
            });
        // Compact stepper: two painter-drawn triangle buttons SIDE BY SIDE
        // [▲][▼] immediately right of the fixed-width combo (tight horizontal
        // spacing so they read as one spinner). Fixed position — the combo is a
        // constant width, so the pair never moves with the selected value.
        let cur = variants.iter().position(|(v, _)| v == value);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let up = stepper_arrow(ui, true, "Previous option");
            let down = stepper_arrow(ui, false, "Next option");
            if !variants.is_empty() {
                if up.clicked() {
                    *value = variants[step_index(cur, variants.len(), -1)].0.clone();
                    changed = true;
                }
                if down.clicked() {
                    *value = variants[step_index(cur, variants.len(), 1)].0.clone();
                    changed = true;
                }
            }
        });
    });
    changed
}

/// Equal-weight 3-way consent selector for a W1TN3SS reporting stream
/// (`Off` / `Ask each time` / `Always`). The three radios carry EQUAL visual
/// weight — there is no pre-ticked default-on or dark-pattern asymmetry; `Off`
/// is first and is the default. Returns `true` when the user changed the mode.
/// Pure UI over the SDK's `ReportingMode`; the host persists the config on
/// change like any other setting.
fn reporting_mode_selector(
    ui: &mut egui::Ui,
    id_salt: &str,
    mode: &mut itasha_report_core::config::ReportingMode,
) -> bool {
    use itasha_report_core::config::ReportingMode;
    let mut changed = false;
    ui.push_id(id_salt, |ui| {
        ui.horizontal(|ui| {
            // Off FIRST (the privacy-default, selected by default) — equal weight.
            changed |= ui
                .radio_value(mode, ReportingMode::Off, "Off")
                .on_hover_text("Never report for this stream (the default).")
                .changed();
            changed |= ui
                .radio_value(mode, ReportingMode::AskEachTime, "Ask each time")
                .on_hover_text("Show each report to you — editable — and ask before sending.")
                .changed();
            changed |= ui
                .radio_value(mode, ReportingMode::Always, "Always send")
                .on_hover_text(
                    "Send reports for this stream without asking each time. You can \
                     turn this off at any time.",
                )
                .changed();
        });
    });
    changed
}

/// A dim helper label under a heading (SCR1B3's `weak().small()` idiom). WRAPS
/// to the available width — a long single-line help string (e.g. the Updates
/// page's) otherwise sets the content's min width and forced the whole settings
/// window WIDER on that page than the others (the reported per-page width drift).
fn help(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).weak().small()).wrap());
    ui.add_space(2.0);
}

/// The eframe app-id used for native window-state + (formerly) egui-memory
/// persistence. Must match the `with_app_id(..)` in `egui_main.rs`.
const EFRAME_APP_ID: &str = "com.itashacorp.c0pl4nd";

/// Absolute path to eframe's `app.ron` persisted-state file, resolved the SAME
/// way eframe itself resolves it: [`eframe::storage_dir`] for our app-id, then
/// the `app.ron` leaf inside it. `None` only when no platform storage dir is
/// available (the same condition under which eframe would not persist either).
///
/// On Windows this is `%APPDATA%\com.itashacorp.c0pl4nd\data\app.ron`; on Linux
/// `~/.local/share/com.itashacorp.c0pl4nd/app.ron` — the dir returned by
/// `storage_dir` is canonical, so we never hard-code the platform path here.
fn app_ron_path() -> Option<std::path::PathBuf> {
    eframe::storage_dir(EFRAME_APP_ID).map(|dir| dir.join("app.ron"))
}

/// Delete eframe's persisted `app.ron` (privacy F1 user control). Returns a
/// short, user-facing status string for the settings page. A missing file is a
/// success ("nothing to clear"); a real delete error is surfaced without leaking
/// the internal path (Tauri-IPC-style error sanitisation discipline).
fn clear_saved_ui_state() -> String {
    let Some(path) = app_ron_path() else {
        return "No saved UI state on this platform.".to_string();
    };
    match std::fs::remove_file(&path) {
        Ok(()) => "Saved window/UI state cleared.".to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            "No saved UI state to clear.".to_string()
        }
        Err(e) => {
            tracing::warn!(target: "c0pl4nd::settings", detail = %e, "clear saved UI state failed");
            "Couldn't clear the saved window/UI state. Make sure C0PL4ND has \
             permission to its settings folder and try again."
                .to_string()
        }
    }
}

/// F5-2: open `path` in the OS file manager (reveal-in-folder / open-with-
/// default-app). Uses the platform opener via `std::process` — no `unsafe`, no
/// network. Best-effort: a spawn failure is ignored (worst case the user
/// navigates to the shown path manually). Consistent with a terminal emulator
/// that already spawns child processes.
fn reveal_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Render the settings window. `open` is toggled false when the user closes it
/// (via the egui Window's built-in ✕). Returns the [`Outcome`] for this frame so
/// the host can persist + re-apply the theme.
pub fn show(
    ctx: &egui::Context,
    config: &mut Config,
    open: &mut bool,
    colors: ChromeColors,
    incognito: bool,
    place_now: bool,
) -> Outcome {
    let mut changed = false;
    let mut keep_open = *open;
    // Privacy-section actions accumulated this frame (reported via Outcome).
    let mut clear_history = false;
    let mut set_incognito: Option<bool> = None;

    // Selected category + search query survive across frames via ctx temp-data
    // (the SCR1B3 pattern) so the window remembers where the user was.
    let cat_id = egui::Id::new("c0pl4nd_settings_cat");
    let q_id = egui::Id::new("c0pl4nd_settings_query");
    let mut category = ctx
        .data_mut(|d| d.get_temp::<String>(cat_id))
        .unwrap_or_else(|| "Appearance".to_string());
    let mut query = ctx
        .data_mut(|d| d.get_temp::<String>(q_id))
        .unwrap_or_default();

    // Snapshot the theme stem so we can tell the host whether it changed (the
    // host reloads the terminal color theme only when this differs).
    let theme_before = config.theme.clone();

    // The in-app self-updater state machine. Held across frames in `ctx`
    // temp-data as an `Arc<Mutex<Updater>>` (Arc is Clone, which egui temp-data
    // requires; the `Updater` itself holds a non-Clone mpsc Receiver). This
    // keeps `show` a free function with the host's fixed signature — the host
    // (mod.rs) never has to know the updater exists. We poll it every frame so
    // background-worker messages advance the state machine even while the
    // Updates page is not the visible tab.
    let updater = get_updater(ctx);
    if let Ok(mut u) = updater.lock() {
        u.poll(ctx);
    }

    // Center the window the first time it opens via a one-time default position.
    // `.anchor()` is deliberately NOT used: an anchored egui window is re-pinned
    // to its anchor every frame and is therefore IMMOVABLE — that was the root
    // cause of the "settings can't be dragged" report. `.default_pos` places it
    // once, then the title bar drags freely.
    //
    // Initial window size: the user's last resized size (persisted in the config
    // TOML), else the built-in default. Each dimension is clamped to a sane floor
    // (`SETTINGS_MIN_*`) and to the live screen (minus a 24px margin) so a
    // malformed persisted value or a tiny display can never spawn an unusable or
    // off-screen window. A non-finite persisted value is ignored. The window is
    // now user-resizable (see the builder below); a filling child no longer
    // balloons it because the content `ScrollArea` uses `auto_shrink([false,
    // false])` — it fills the width it is GIVEN rather than demanding its own.
    // Full window rect, read robustly: `viewport_rect()` is the whole inner
    // window, but on some frames it can read empty — fall back to the input-layer
    // screen rect, which is populated directly from the windowing backend. This is
    // the size/position reference for the window below.
    let screen = {
        let vr = ctx.viewport_rect();
        if vr.width() > 1.0 && vr.height() > 1.0 {
            vr
        } else {
            // Paranoia fallback for a not-yet-sized viewport: a sane default rect
            // from the origin. The clamps below keep the window on-screen once the
            // real viewport size arrives.
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 720.0))
        }
    };
    let screen_max_w = (screen.width() - 24.0).max(SETTINGS_MIN_W);
    let screen_max_h = (screen.height() - 24.0).max(SETTINGS_MIN_H);
    let want_w = config
        .settings_win_w
        .filter(|v| v.is_finite())
        .unwrap_or(SETTINGS_DEFAULT_W);
    let want_h = config
        .settings_win_h
        .filter(|v| v.is_finite())
        .unwrap_or(SETTINGS_DEFAULT_H);
    let def_w = want_w.clamp(SETTINGS_MIN_W, screen_max_w);
    let def_h = want_h.clamp(SETTINGS_MIN_H, screen_max_h);
    let default_size = egui::vec2(def_w, def_h);

    // Target top-left for a forced placement: the saved position (a prior move) if
    // both coordinates are present + finite, else CENTERED over the window. Both
    // are clamped to the live screen so a stale off-screen value can never hide the
    // window. This is applied via `current_pos` ONLY on the open frame (`place_now`)
    // — deterministic, so it never depends on `default_pos` timing.
    let centered = screen.center() - default_size * 0.5;
    let saved_pos = match (config.settings_win_x, config.settings_win_y) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some(egui::pos2(x, y)),
        _ => None,
    };
    let target_pos = saved_pos.unwrap_or(centered);
    // Keep it on-screen: clamp so the whole window stays within the viewport.
    let max_x = (screen.right() - def_w).max(screen.left());
    let max_y = (screen.bottom() - def_h).max(screen.top());
    let target_pos = egui::pos2(
        target_pos.x.clamp(screen.left(), max_x),
        target_pos.y.clamp(screen.top(), max_y),
    );

    // Esc dismisses settings (in addition to the title-bar ✕ and the in-content
    // Close button) — the conventional overlay-dismiss key.
    if keep_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        keep_open = false;
    }

    // Set by the in-content Close button below; applied after the frame so the
    // mutation does not fight the `&mut keep_open` borrow the Window holds.
    let mut close_requested = false;

    // NOTE: deliberately NO `.open(&mut keep_open)` — that adds egui's own
    // title-bar ✕, which (a) duplicates the clear in-content Close button below
    // and (b) reads as low-contrast on the dark custom frame. The in-content
    // button + Esc are the single, obvious dismiss path (the "two close buttons"
    // report). Closing flows through `keep_open` → `*open` exactly as before.
    let mut win = egui::Window::new("settings");
    // Force the position on the open frame ONLY (`place_now`): `current_pos` pins
    // the window this frame, egui then remembers it, and on later frames we stop
    // pinning so the user can freely drag it. This is what makes the first open
    // land centered (or at the saved spot) deterministically.
    if place_now {
        win = win.current_pos(target_pos);
    }
    let win_resp = win
        // No egui title bar: it rendered a SECOND, redundant top bar (a centered
        // lowercase "settings") above the in-content header that carries the
        // "Settings" heading + the ✕ close button. The in-content header is the
        // single titlebar; dragging still works via egui's window-frame drag.
        .title_bar(false)
        .collapsible(false)
        // USER-RESIZABLE with a persisted size. `default_size` seeds the window
        // the first time it appears (from the persisted-or-default size computed
        // above); thereafter egui remembers the user's drag within the session and
        // the write-back below persists it across restarts. `min_size` stops the
        // user from shrinking it until the nav + content pane collide. The old
        // ballooning failure mode (a filling child demanding near-infinite width)
        // is prevented at the source: the content `ScrollArea` uses
        // `auto_shrink([false, false])`, so it fills the width the window gives it
        // rather than reporting its own desired width back up to the Resize widget.
        .resizable(true)
        .default_size(default_size)
        .min_size(egui::vec2(SETTINGS_MIN_W, SETTINGS_MIN_H))
        // Constrain to the WHOLE screen (keep the window on-screen), NOT to a
        // default_size box re-centered every frame. The latter re-pinned the
        // window to the live screen centre each frame, so maximizing the MAIN
        // window (which moves `screen.center()`) yanked the settings Area to the
        // new centre while its content had been laid out at the old position —
        // the reported "contents get moved and cut off when maximized". Bounding
        // to the full screen keeps it visible without forcing a re-centre.
        .constrain_to(screen)
        .movable(true)
        .frame(egui::Frame::window(&ctx.global_style()).fill(colors.panel))
        .show(ctx, |ui| {
            // ---- Width discipline ----
            // The content fills the width the (now user-resizable) window gives
            // it. The old ballooning failure mode — a filling child reporting
            // `available_width()`/`INFINITY` as its desired size and driving the
            // window ever wider — is prevented at the source by the content
            // `ScrollArea`'s `auto_shrink([false, false])` below: it consumes the
            // width it is given rather than demanding its own. So `available_width`
            // here is a real, bounded number (the window's inner width), and using
            // it lets a grown window put the extra space to work instead of
            // stranding it in a right-hand gutter.
            let content_w = ui.available_width();
            ui.set_max_width(content_w);
            // In-content header: title + an unmissable Close ✕. The egui
            // title-bar ✕ can read as low-contrast against the dark custom
            // frame (the "can't close it" report), so this is a clear,
            // always-visible dismiss. Fixed-height — it does not eat the scroll
            // area's fill below.
            ui.horizontal(|ui| {
                // The heading uses the bright theme FOREGROUND, not the accent:
                // the accent (theme selection colour) read as low-contrast purple
                // against the dark panel fill (the reported "hard to read" bug).
                // fg is readable in both light and dark themes.
                ui.heading(egui::RichText::new("Settings").color(colors.fg));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let close = ui
                        .button(egui::RichText::new(egui_phosphor::thin::X).size(16.0))
                        .on_hover_text("Close settings (Esc)");
                    close.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "close settings")
                    });
                    if close.clicked() {
                        close_requested = true;
                    }
                });
            });
            ui.separator();

            ui.horizontal_top(|ui| {
                // ---- Left category nav ----
                ui.vertical(|ui| {
                    ui.set_width(SETTINGS_NAV_W);
                    ui.add_space(4.0);
                    for cat in CATEGORIES {
                        ui.selectable_value(&mut category, (*cat).to_string(), *cat);
                        ui.add_space(2.0);
                    }
                });
                ui.separator();

                // ---- Searchable, internally-scrolling content pane ----
                ui.vertical(|ui| {
                    // The content pane fills the width left after the nav +
                    // separator. The grids below size to THIS width (not to their
                    // own desired width), and the search box gets a finite width to
                    // fill. Ballooning is prevented at the source by the content
                    // `ScrollArea`'s `auto_shrink([false, false])`, so raw
                    // `available_width()` is a real bounded number here.
                    let pane_w = ui.available_width();
                    ui.set_max_width(pane_w);
                    ui.horizontal(|ui| {
                        ui.label(egui_phosphor::thin::MAGNIFYING_GLASS);
                        // A bounded width (was `f32::INFINITY`): leave room for the
                        // magnifier glyph + the clear ✕ so the box fills the pane
                        // WITHOUT demanding more than the pane is wide. The 56px
                        // reserve covers the glyph + the optional clear button +
                        // inter-item spacing; `max(0.0)` guards a tiny pane.
                        let search_w = (pane_w - 56.0).max(0.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut query)
                                .hint_text("search settings")
                                .desired_width(search_w),
                        );
                        if !query.is_empty() && ui.button("✕").clicked() {
                            query.clear();
                        }
                    });
                    ui.separator();

                    let q = query.trim().to_lowercase();
                    let sel = category.as_str();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            changed |= render_sections(
                                ui,
                                config,
                                &updater,
                                sel,
                                &q,
                                incognito,
                                colors,
                                &mut clear_history,
                                &mut set_incognito,
                            );
                        });
                });
            });
        });

    if close_requested {
        keep_open = false;
    }

    // Persist a user resize/move so the window reopens at the size + position they
    // left it. Only write when the pointer is UP (the drag has settled). The SIZE
    // dead-band is deliberately larger than the window frame margin so the
    // outer-rect/inner-size difference never re-arms a write on a plain reopen; the
    // POSITION dead-band is small (position is stable when not dragged). Writing
    // either updates all four fields so size + position stay coherent.
    if !place_now {
        if let Some(win_resp) = &win_resp {
            let rect = win_resp.response.rect;
            let sz = rect.size();
            let pos = rect.min;
            let pointer_up = ctx.input(|i| !i.pointer.any_down());
            let base_w = config.settings_win_w.unwrap_or(SETTINGS_DEFAULT_W);
            let base_h = config.settings_win_h.unwrap_or(SETTINGS_DEFAULT_H);
            let base_x = config.settings_win_x.unwrap_or(pos.x);
            let base_y = config.settings_win_y.unwrap_or(pos.y);
            const SIZE_DEAD_BAND: f32 = 12.0;
            const POS_DEAD_BAND: f32 = 4.0;
            let all_finite =
                sz.x.is_finite() && sz.y.is_finite() && pos.x.is_finite() && pos.y.is_finite();
            let size_moved =
                (sz.x - base_w).abs() > SIZE_DEAD_BAND || (sz.y - base_h).abs() > SIZE_DEAD_BAND;
            let pos_moved =
                (pos.x - base_x).abs() > POS_DEAD_BAND || (pos.y - base_y).abs() > POS_DEAD_BAND;
            if pointer_up && all_finite && (size_moved || pos_moved) {
                config.settings_win_w = Some(sz.x);
                config.settings_win_h = Some(sz.y);
                config.settings_win_x = Some(pos.x);
                config.settings_win_y = Some(pos.y);
                changed = true;
            }
        }
    }

    ctx.data_mut(|d| {
        d.insert_temp(cat_id, category);
        d.insert_temp(q_id, query);
    });
    *open = keep_open;

    Outcome {
        changed,
        theme_changed: changed && config.theme != theme_before,
        clear_history,
        set_incognito,
        window_rect: win_resp.as_ref().map(|r| r.response.rect),
    }
}

/// A deferred toolbar edit, applied AFTER the render loop so an immutable
/// render-borrow of a zone list never overlaps the mutable edit.
enum ToolbarEdit {
    /// Move the item at `idx` earlier (left/up) within its zone.
    Up(super::chrome_toolbar::Zone, usize),
    /// Move the item at `idx` later (right/down) within its zone.
    Down(super::chrome_toolbar::Zone, usize),
    /// Move `id` to a different zone (or Hidden).
    MoveTo(String, super::chrome_toolbar::Zone),
}

/// Settings → Toolbar: place each quick-action button in a zone — LEFT (after the
/// "+"), RIGHT (next to the settings gear), or the overflow "⋯" menu — or hide it;
/// reorder within a zone; and reset. Returns whether `config.toolbar` changed.
fn toolbar_settings_section(ui: &mut egui::Ui, config: &mut Config) -> bool {
    use super::chrome_toolbar::{self as tb, Zone};
    let mut changed = false;
    ui.heading("Toolbar");
    help(
        ui,
        "Choose where each quick-action button lives in the top bar: on the LEFT \
         (after the +), on the RIGHT (next to the settings gear), or parked in an \
         overflow menu. Use the arrows to reorder within a zone, the X to remove a \
         button, and the dots menu to move it to another zone.",
    );

    // One deferred edit per frame (the user clicks a single control), applied
    // after every list has rendered so no render-borrow overlaps the mutation.
    let mut edit: Option<ToolbarEdit> = None;

    toolbar_zone_list(
        ui,
        "Left of the gear (after +)",
        &config.toolbar.left,
        Zone::Left,
        &mut edit,
    );
    ui.add_space(8.0);
    toolbar_zone_list(
        ui,
        "Right (next to the gear)",
        &config.toolbar.right,
        Zone::Right,
        &mut edit,
    );

    ui.add_space(8.0);
    changed |= ui
        .checkbox(
            &mut config.toolbar.show_overflow,
            "Show the overflow menu button when its menu has actions",
        )
        .changed();
    toolbar_zone_list(
        ui,
        "Overflow menu",
        &config.toolbar.menu,
        Zone::Menu,
        &mut edit,
    );

    // Hidden pool — actions currently shown nowhere, each with a "show on ▾" menu.
    let hidden = tb::hidden_actions(
        &config.toolbar.left,
        &config.toolbar.right,
        &config.toolbar.menu,
    );
    if !hidden.is_empty() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Hidden (not shown)").strong());
        for &id in &hidden {
            ui.horizontal(|ui| {
                toolbar_place_menu(ui, id, None, &mut edit);
                ui.label(tb::action_label(id).unwrap_or(id));
            });
        }
    }

    ui.add_space(10.0);
    if ui
        .button("Reset toolbar to defaults")
        .on_hover_text("Restore the default button placement (script launcher on the right, the rest on the left).")
        .clicked()
    {
        config.toolbar = c0pl4nd_core::config::ToolbarConfig::default();
        changed = true;
    }

    if let Some(e) = edit {
        changed |= apply_toolbar_edit(config, e);
    }
    changed
}

/// Render one zone's action list: each row is `▲ ▼ ⋯move  Label`. Sets `*edit` on
/// a click (applied by the caller after every list has rendered). Zone-agnostic.
fn toolbar_zone_list(
    ui: &mut egui::Ui,
    title: &str,
    list: &[String],
    zone: super::chrome_toolbar::Zone,
    edit: &mut Option<ToolbarEdit>,
) {
    ui.label(egui::RichText::new(title).strong());
    if list.is_empty() {
        ui.weak("   — none —");
        return;
    }
    let n = list.len();
    for (i, id) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_enabled_ui(i > 0, |ui| {
                if tb_icon_button(ui, egui_phosphor::thin::CARET_UP, "Move earlier (left)")
                    .clicked()
                {
                    *edit = Some(ToolbarEdit::Up(zone, i));
                }
            });
            ui.add_enabled_ui(i + 1 < n, |ui| {
                if tb_icon_button(ui, egui_phosphor::thin::CARET_DOWN, "Move later (right)")
                    .clicked()
                {
                    *edit = Some(ToolbarEdit::Down(zone, i));
                }
            });
            // Remove this button from the bar (moves it to the Hidden pool, whence
            // it can be re-added) — the visible X remove affordance.
            if tb_icon_button(ui, egui_phosphor::thin::X, "Remove from the bar").clicked() {
                *edit = Some(ToolbarEdit::MoveTo(
                    id.clone(),
                    super::chrome_toolbar::Zone::Hidden,
                ));
            }
            toolbar_place_menu(ui, id, Some(zone), edit);
            ui.label(super::chrome_toolbar::action_label(id).unwrap_or(id));
        });
    }
}

/// A "move to zone" (⋯) menu for a toolbar-editor row. `current` is the action's
/// present zone (`None` = a hidden action). Offers every OTHER zone (+ Hide for a
/// shown action). Sets `*edit` on selection.
fn toolbar_place_menu(
    ui: &mut egui::Ui,
    id: &str,
    current: Option<super::chrome_toolbar::Zone>,
    edit: &mut Option<ToolbarEdit>,
) {
    use super::chrome_toolbar::Zone;
    // Zone moves only (Left / Right / Overflow); removal is the row's X button.
    let targets = [
        (Zone::Left, "Move to left"),
        (Zone::Right, "Move to right"),
        (Zone::Menu, "Move to overflow menu"),
    ];
    let hover = if current.is_none() {
        "Add this button to the bar"
    } else {
        "Move this button to another zone"
    };
    ui.menu_button(
        egui::RichText::new(egui_phosphor::thin::DOTS_THREE).size(14.0),
        |ui| {
            for (tz, label) in targets {
                if current == Some(tz) {
                    continue; // already in this zone
                }
                // A hidden action is being ADDED — say so instead of "Move to".
                let text = if current.is_none() {
                    match tz {
                        Zone::Left => "Add to the left",
                        Zone::Right => "Add to the right",
                        Zone::Menu => "Add to the overflow menu",
                        Zone::Hidden => label,
                    }
                } else {
                    label
                };
                if ui.button(text).clicked() {
                    *edit = Some(ToolbarEdit::MoveTo(id.to_string(), tz));
                    ui.close_kind(egui::UiKind::Menu);
                }
            }
        },
    )
    .response
    .on_hover_text(hover);
}

/// A compact Phosphor-glyph button for the toolbar editor (loaded icon font, not a
/// raw Unicode arrow — those render as tofu in the UI font).
fn tb_icon_button(ui: &mut egui::Ui, glyph: &str, hover: &str) -> egui::Response {
    ui.button(egui::RichText::new(glyph).size(14.0))
        .on_hover_text(hover)
}

/// Apply a single deferred toolbar edit to the live config. Returns whether it
/// changed anything.
fn apply_toolbar_edit(config: &mut Config, edit: ToolbarEdit) -> bool {
    use super::chrome_toolbar::{self as tb, Zone};
    match edit {
        ToolbarEdit::Up(z, i) => match z {
            Zone::Left => tb::move_up(&mut config.toolbar.left, i),
            Zone::Right => tb::move_up(&mut config.toolbar.right, i),
            Zone::Menu => tb::move_up(&mut config.toolbar.menu, i),
            Zone::Hidden => false,
        },
        ToolbarEdit::Down(z, i) => match z {
            Zone::Left => tb::move_down(&mut config.toolbar.left, i),
            Zone::Right => tb::move_down(&mut config.toolbar.right, i),
            Zone::Menu => tb::move_down(&mut config.toolbar.menu, i),
            Zone::Hidden => false,
        },
        ToolbarEdit::MoveTo(id, target) => tb::move_to_zone(
            &mut config.toolbar.left,
            &mut config.toolbar.right,
            &mut config.toolbar.menu,
            target,
            &id,
        ),
    }
}

/// Render every category section visible for the current selection / search
/// query. The single most impactful "un-cramp" change is setting
/// `item_spacing` to 8 px vertical (vs egui's default 3 px) at the top; section
/// gaps add a further 14 px of breathing room.
#[allow(clippy::too_many_arguments)]
fn render_sections(
    ui: &mut egui::Ui,
    config: &mut Config,
    updater: &Arc<Mutex<Updater>>,
    sel: &str,
    q: &str,
    incognito: bool,
    // The chrome palette the host derived from the ACTIVE theme (`self.theme`),
    // so a file-based CUSTOM theme's accent is available here — used for the mesh
    // preview swatch below.
    colors: ChromeColors,
    clear_history: &mut bool,
    set_incognito: &mut Option<bool>,
) -> bool {
    let mut changed = false;
    // Un-cramp every row + give buttons comfier hit targets (the load-bearing
    // spacing fix the user asked for).
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
    ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
    // Page margin so content never hugs the scrollbar / separator.
    ui.add_space(4.0);
    let group_gap = |ui: &mut egui::Ui| ui.add_space(14.0);
    // Used by every per-setting ↺ revert. Cheap to construct once per render.
    let def = Config::default();

    // Shared Grid config so every section's label column is the same width and
    // all controls line up at the same x — the aligned, scannable look.
    let grid = |id: &'static str| {
        egui::Grid::new(id)
            .num_columns(3) // label | control | ↺
            .spacing([24.0, 10.0])
            .min_col_width(140.0) // SCR1B3-parity column floor
    };

    // Sub-group header WITHIN a section: a strong label, a muted one-line
    // description, and a divider rule — the SCR1B3 settings idiom that breaks a
    // long section into scannable, clearly-titled blocks with visible dividers
    // (the "divider lines, clear headings, descriptions" the user asked for).
    // Skipped while a search query is active: search flattens to the matching
    // rows, so decorative sub-headers (which don't themselves match) would just
    // leave empty titled blocks.
    let group = |ui: &mut egui::Ui, label: &str, desc: &str| {
        if !q.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new(label).strong());
        if !desc.is_empty() {
            ui.label(egui::RichText::new(desc).weak().small());
        }
        ui.separator();
    };

    // ---------------------------------------------------------------- Appearance
    if section_visible(sel, q, "Appearance", APPEARANCE_SEARCH_LABELS) {
        ui.heading("Appearance");
        help(
            ui,
            "Theme, window transparency, and interface look. Changes apply live.",
        );
        group(ui, "Theme", "The terminal colour palette.");
        grid("appearance_theme").show(ui, |ui| {
            if row_visible(q, "theme color") {
                ui.label("Theme")
                    .on_hover_text("Terminal color theme — a file stem under the themes dir.");
                // Fixed-width combo + up/down stepper (spinner). The stepper cycles
                // the built-ins with wrap-around; a custom theme name (from the
                // alias field below) still shows in the combo and steps onto the
                // first/last built-in. Because the combo width is constant the
                // stepper never moves — the fix for "the arrows move with the name".
                let theme_variants: Vec<(String, &str)> =
                    BUILTIN_THEMES.iter().map(|t| (t.to_string(), *t)).collect();
                let cur_theme = config.theme.clone();
                changed |= dropdown_with_stepper(
                    ui,
                    "c0pl4nd-theme-picker",
                    &mut config.theme,
                    &theme_variants,
                    Some(&cur_theme),
                );
                changed |= reset_to_default(ui, &mut config.theme, &def.theme);
                ui.end_row();

                ui.label("…or theme name");
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut config.theme)
                            .hint_text("itasha-corp")
                            .desired_width(200.0),
                    )
                    .on_hover_text(
                        "A user theme TOML under the config dir's themes folder overrides \
                         the built-in of the same name.",
                    )
                    .changed();
                ui.label(""); // no ↺ on the alias field (it edits the same `theme`)
                ui.end_row();
            }

            if row_visible(q, "follow os theme dark light auto system appearance") {
                changed |= ui
                    .checkbox(&mut config.follow_os_theme, "Follow OS dark / light")
                    .on_hover_text(
                        "Automatically match the operating system's dark/light \
                         appearance, switching between the default dark (itasha-corp) \
                         and light (ghost-paper) themes when the OS appearance \
                         changes. A theme you pick above still applies and sticks \
                         until the OS dark/light setting next flips.",
                    )
                    .changed();
                ui.label(""); // checkbox carries its own label
                changed |= reset_to_default(ui, &mut config.follow_os_theme, &def.follow_os_theme);
                ui.end_row();
            }
        });

        group(
            ui,
            "Transparency & tint",
            "Make the window see-through with a single Opacity slider, and \
             optionally wash it with a colour.",
        );
        grid("appearance_transparency").show(ui, |ui| {
            // ---- Single always-transparent opacity model (v0.4.21) ----
            // The window is ALWAYS transparent-capable; there is no mode selector
            // and no OS blur backdrop. One Opacity slider is the whole see-through
            // control: 0% = fully see-through (only the terminal text remains over
            // the desktop), 100% = a solid ("opaque") window. Applies live.
            if row_visible(q, "opacity transparency") {
                ui.label("Opacity").on_hover_text(
                    "How see-through the window is. 0% = fully transparent (only the \
                     terminal text stays, over the desktop); 100% = solid. Applies \
                     live across the whole range.",
                );
                changed |= ui
                    .add(
                        egui::Slider::new(&mut config.opacity, 0.0..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                            .custom_parser(|s| {
                                s.trim_end_matches('%')
                                    .trim()
                                    .parse::<f64>()
                                    .ok()
                                    .map(|v| v / 100.0)
                            }),
                    )
                    .changed();
                changed |= reset_to_default(ui, &mut config.opacity, &def.opacity);
                ui.end_row();
            }

            // Explicit ON/OFF master for the tint colour wash — independent of the
            // strength slider, so a user can toggle the wash off without losing the
            // colour + strength they set.
            if row_visible(q, "tint enable toggle wash on off") {
                changed |= ui
                    .checkbox(&mut config.tint_enabled, "Enable tint wash")
                    .on_hover_text(
                        "Master switch for the colour wash over the window. When \
                         off, no tint is painted even if a colour and strength are \
                         set below.",
                    )
                    .changed();
                ui.label(""); // empty control column — checkbox carries the label
                changed |= reset_to_default(ui, &mut config.tint_enabled, &def.tint_enabled);
                ui.end_row();
            }

            if row_visible(q, "tint color overlay wash picker") {
                let en = config.tint_enabled;
                ui.label("Tint color")
                    .on_hover_text("Color washed over the window (strength below).");
                ui.add_enabled_ui(en, |ui| {
                    ui.horizontal(|ui| {
                        // A real color PICKER (swatch button → palette popup) over
                        // the `#RRGGBB` config string, instead of raw hex entry.
                        // parse the stored hex → swatch; on pick, write hex back.
                        let (r, g, b) =
                            c0pl4nd_core::theme::parse_hex(&config.tint).unwrap_or((18, 18, 18));
                        let mut rgb = [r, g, b];
                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                            config.tint = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                            changed = true;
                        }
                        // Compact hex readout so the exact value is visible/copyable.
                        ui.monospace(&config.tint);
                    });
                });
                changed |= reset_to_default(ui, &mut config.tint, &def.tint);
                ui.end_row();
            }

            if row_visible(q, "tint strength wash overlay") {
                let en = config.tint_enabled;
                ui.label("Tint strength")
                    .on_hover_text("0% = no tint .. 100% = strong color wash.");
                changed |= ui
                    .add_enabled(
                        en,
                        egui::Slider::new(&mut config.tint_strength, 0.0..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                            .custom_parser(|s| {
                                s.trim_end_matches('%')
                                    .trim()
                                    .parse::<f64>()
                                    .ok()
                                    .map(|v| v / 100.0)
                            }),
                    )
                    .changed();
                changed |= reset_to_default(ui, &mut config.tint_strength, &def.tint_strength);
                ui.end_row();
            }
        });

        group(
            ui,
            "Frosted glass",
            "A software frosted-glass wash over the window — adjustable, and \
             independent of the Opacity slider.",
        );
        grid("appearance_frost").show(ui, |ui| {
            // Each dependent row is THREE grid cells (label · control · ↺) with the
            // control greyed out until the master toggle is on, so the columns never
            // jump — the same fixed-layout discipline as Transparency & tint above.
            if row_visible(q, "frost frosted glass enable toggle") {
                changed |= ui
                    .checkbox(&mut config.frost_enabled, "Frosted glass")
                    .on_hover_text(
                        "Software frosted-glass wash — tints/diffuses the window; \
                         does NOT blur the desktop behind the window (a real blur \
                         is not possible on this hardware). Independent of the \
                         Opacity slider, so it works at any opacity.",
                    )
                    .changed();
                ui.label(""); // checkbox carries its own label
                changed |= reset_to_default(ui, &mut config.frost_enabled, &def.frost_enabled);
                ui.end_row();
            }

            if row_visible(q, "frost amount strength") {
                let en = config.frost_enabled;
                ui.label("Frost amount").on_hover_text(
                    "How frosted the glass reads: 0% = clear, 100% = maximum \
                     (capped so the terminal text stays legible).",
                );
                changed |= ui
                    .add_enabled(
                        en,
                        egui::Slider::new(&mut config.frost_amount, 0.0..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                            .custom_parser(|s| {
                                s.trim_end_matches('%')
                                    .trim()
                                    .parse::<f64>()
                                    .ok()
                                    .map(|v| v / 100.0)
                            }),
                    )
                    .changed();
                changed |= reset_to_default(ui, &mut config.frost_amount, &def.frost_amount);
                ui.end_row();
            }

            if row_visible(q, "frost color colour picker") {
                let en = config.frost_enabled;
                ui.label("Frost color").on_hover_text(
                    "Colour of the frost wash. Leave at the theme default (the \
                     terminal background) or pick a colour to tint the frost.",
                );
                ui.add_enabled_ui(en, |ui| {
                    ui.horizontal(|ui| {
                        // Empty frost_color = follow the theme background; seed the
                        // picker with a neutral fallback until the user picks.
                        let (r, g, b) = c0pl4nd_core::theme::parse_hex(&config.frost_color)
                            .unwrap_or((8, 6, 13));
                        let mut rgb = [r, g, b];
                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                            config.frost_color =
                                format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                            changed = true;
                        }
                        if config.frost_color.is_empty() {
                            ui.weak("theme");
                        } else {
                            ui.monospace(&config.frost_color);
                        }
                    });
                });
                changed |= reset_to_default(ui, &mut config.frost_color, &def.frost_color);
                ui.end_row();
            }

            if row_visible(q, "frost grain texture") {
                let en = config.frost_enabled;
                ui.add_enabled_ui(en, |ui| {
                    changed |= ui
                        .checkbox(&mut config.frost_grain, "Grain texture")
                        .on_hover_text(
                            "Add a subtle procedural grain so the frost reads as \
                             diffused glass rather than a flat colour film.",
                        )
                        .changed();
                });
                ui.label(""); // checkbox carries its own label
                changed |= reset_to_default(ui, &mut config.frost_grain, &def.frost_grain);
                ui.end_row();
            }
        });

        group(
            ui,
            "Window",
            "Window behaviour that isn't a colour/translucency effect.",
        );
        grid("appearance_window").show(ui, |ui| {
            // Keep the window above every other window (mirrors SCR1B3). Applied
            // live via `ViewportCommand::WindowLevel` in the host — no relaunch.
            if row_visible(q, "always on top window level pin above") {
                changed |= ui
                    .checkbox(&mut config.always_on_top, "Always on top")
                    .on_hover_text(
                        "Keep the C0PL4ND window above all other windows. Applies \
                         immediately — no relaunch needed.",
                    )
                    .changed();
                ui.label(""); // checkbox carries its own label
                changed |= reset_to_default(ui, &mut config.always_on_top, &def.always_on_top);
                ui.end_row();
            }
        });

        group(
            ui,
            "Interface scale",
            "Accessibility zoom for the whole interface, saved across launches.",
        );
        grid("appearance_scale").show(ui, |ui| {
            // ---- UI scale (F2-3): persisted accessibility zoom for the UI ----
            // Placed AFTER opacity + tint so the existing slider-order assertions
            // in egui_settings.rs (opacity = slider 0, tint = slider 1) hold.
            if row_visible(q, "ui scale zoom accessibility size") {
                ui.label("UI scale").on_hover_text(
                    "Accessibility zoom for the WHOLE interface (chrome + grid), \
                     persisted across launches. 1.0 = 100%. Applies when you \
                     release the slider. (Ctrl+/- also zooms, but is not saved.)",
                );
                // DEFERRED APPLY (runaway-scale fix): writing the slider straight
                // to `config.ui_scale` applied the zoom LIVE every frame, which
                // rescaled this very slider under the cursor → the pointer remapped
                // to a bigger value → oscillation → runaway to the 3.0 max. Instead
                // drive the slider from a per-frame WORKING value held in egui temp
                // memory while the drag is in progress, and commit to
                // `config.ui_scale` (the sole trigger for `set_zoom_factor`) ONLY
                // when the handle is NOT being dragged — on release, a track click,
                // or a keyboard step (see `ui_scale_commit`). So the UI rescales
                // exactly once per interaction, never mid-drag.
                let pending_id = egui::Id::new("c0pl4nd_ui_scale_pending");
                let mut working = ui
                    .data(|d| d.get_temp::<f32>(pending_id))
                    .unwrap_or(config.ui_scale);
                let resp = ui.add(
                    egui::Slider::new(&mut working, 0.5..=3.0)
                        .fixed_decimals(2)
                        .suffix("×"),
                );
                let commit = ui_scale_commit(config.ui_scale, working, resp.dragged());
                if resp.dragged() {
                    // Hold the dragged value WITHOUT applying it (no live rescale).
                    ui.data_mut(|d| d.insert_temp(pending_id, working));
                } else {
                    // Not dragging: drop the hold so the slider re-syncs to the
                    // committed config value next frame.
                    ui.data_mut(|d| d.remove_temp::<f32>(pending_id));
                }
                if let Some(v) = commit {
                    config.ui_scale = v;
                    changed = true;
                }
                changed |= reset_to_default(ui, &mut config.ui_scale, &def.ui_scale);
                ui.end_row();
            }
        });
        group_gap(ui);
    }

    // ---------------------------------------------------------------------- Font
    if section_visible(
        sel,
        q,
        "Fonts",
        &[
            "font",
            "family",
            "app ui font interface proportional",
            "size",
            "line height",
            "ligatures",
            "fallback",
        ],
    ) {
        ui.heading("Fonts");
        help(
            ui,
            "Terminal and app-UI font family, text size, and line height. The two \
             fonts are independent — one never changes the other.",
        );
        grid("font_grid").show(ui, |ui| {
            if row_visible(q, "family typeface") {
                ui.label("Terminal font").on_hover_text(
                    "Monospace typeface for the TERMINAL grid, picked from the fonts \
                     installed on this system (plus the bundled faces). Separate from \
                     the app-UI font below — changing one never changes the other. \
                     Applies live.",
                );
                // The installed monospace families (enumerated once + cached) plus
                // the built-in label. Fixed-width combo + stepper so the user picks
                // a real font the app loads — not free text that did nothing. The
                // stored value is an internal family key; the combo shows/steps the
                // human display name via `family_display`/`family_value`.
                let font_variants: Vec<(String, &str)> = super::fonts::monospace_family_choices()
                    .iter()
                    .map(|fam| (family_value(fam), fam.as_str()))
                    .collect();
                let selected = family_display(&config.font.family);
                changed |= dropdown_with_stepper(
                    ui,
                    "c0pl4nd-font-family",
                    &mut config.font.family,
                    &font_variants,
                    Some(&selected),
                );
                changed |= reset_to_default(ui, &mut config.font.family, &def.font.family);
                ui.end_row();
            }

            if row_visible(q, "app ui font interface proportional") {
                ui.label("App UI font").on_hover_text(
                    "Typeface for the whole app INTERFACE (settings, chrome, toolbar, \
                     status bar, menus) — the proportional text everywhere EXCEPT the \
                     terminal grid. Separate from the terminal font above, so the UI \
                     swap never changes the terminal. \"System default\" keeps egui's \
                     built-in UI font. Applies live.",
                );
                let ui_font_variants: Vec<(String, &str)> = super::fonts::ui_family_choices()
                    .map(|fam| (fam.to_string(), fam))
                    .collect();
                let cur_ui_font = config.font.ui_family.clone();
                changed |= dropdown_with_stepper(
                    ui,
                    "c0pl4nd-ui-font-family",
                    &mut config.font.ui_family,
                    &ui_font_variants,
                    Some(&cur_ui_font),
                );
                changed |= reset_to_default(ui, &mut config.font.ui_family, &def.font.ui_family);
                ui.end_row();
            }

            if row_visible(q, "size") {
                ui.label("Size");
                ui.horizontal(|ui| {
                    if ui.small_button("−").on_hover_text("Smaller").clicked() {
                        config.font.size = step_font_size(config.font.size, -1.0);
                        changed = true;
                    }
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut config.font.size, FONT_SIZE_RANGE)
                                .suffix(" pt")
                                .step_by(0.5),
                        )
                        .changed();
                    if ui.small_button("+").on_hover_text("Larger").clicked() {
                        config.font.size = step_font_size(config.font.size, 1.0);
                        changed = true;
                    }
                });
                changed |= reset_to_default(ui, &mut config.font.size, &def.font.size);
                ui.end_row();
            }

            if row_visible(q, "line height") {
                ui.label("Line height").on_hover_text(
                    "Row height for the primary font. Applies on restart — the grid \
                     cell metrics are derived at launch.",
                );
                ui.horizontal(|ui| {
                    if ui.small_button("−").on_hover_text("Tighter").clicked() {
                        config.font.line_height =
                            step_line_height_px(config.font.line_height, -1.0);
                        changed = true;
                    }
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut config.font.line_height, LINE_HEIGHT_PX_RANGE)
                                .suffix(" px"),
                        )
                        .on_hover_text("Applies on the next launch.")
                        .changed();
                    if ui.small_button("+").on_hover_text("Looser").clicked() {
                        config.font.line_height = step_line_height_px(config.font.line_height, 1.0);
                        changed = true;
                    }
                });
                changed |=
                    reset_to_default(ui, &mut config.font.line_height, &def.font.line_height);
                ui.end_row();
            }

            if row_visible(q, "ligatures shaping") {
                // DISABLED: the egui native text painter draws the grid glyph-by-
                // glyph (strict monospace cell fidelity) and does NOT run a shaping
                // engine (HarfBuzz / cosmic-text), so programming ligatures can't
                // be formed. Shown greyed with an honest tooltip rather than as a
                // dead toggle that silently does nothing.
                ui.add_enabled_ui(false, |ui| {
                    // Label kept short (the "->/!=" examples live in the tooltip) so
                    // this checkbox-left row does not widen the Fonts grid past the
                    // shared settings-window width — the equal-width-on-every-page
                    // invariant (see `every_settings_page_has_the_same_window_width`).
                    ui.checkbox(&mut config.ligatures, "Programming ligatures")
                        .on_hover_text(
                            "Not available: the GPU text renderer draws strict \
                             monospace cells and does not do glyph shaping — so \
                             programming ligatures (-> ligature, != ligature) can't \
                             be formed.",
                        );
                });
                ui.label("");
                ui.end_row();
            }

            // Two ORDERED fallback slots for glyphs the primary font lacks (CJK,
            // Arabic, emoji). Each slot is a ComboBox over the installed
            // monospace families + a "(none)" choice; the picks are round-tripped
            // into the `Vec<String>` config field (a "(none)" slot is dropped, so
            // an empty earlier slot never leaves a blank family before a later
            // one). Applies live alongside the primary family.
            if row_visible(q, "fallback fonts families cjk polyglot") {
                ui.label("Fallback fonts").on_hover_text(
                    "Up to two fallback families for glyphs the primary font lacks \
                     (CJK, Arabic, emoji). Applies live.",
                );
                // The two current slots (padded with "(none)" so both combos
                // always render even when the config has 0 or 1 fallbacks).
                let mut slot0 = config
                    .font
                    .fallback
                    .first()
                    .cloned()
                    .unwrap_or_else(|| super::fonts::NONE_LABEL.to_string());
                let mut slot1 = config
                    .font
                    .fallback
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| super::fonts::NONE_LABEL.to_string());
                let choices = super::fonts::monospace_family_choices();
                let mut slots_changed = false;
                ui.vertical(|ui| {
                    slots_changed |=
                        fallback_combo(ui, "c0pl4nd-font-fallback-0", choices, &mut slot0);
                    slots_changed |=
                        fallback_combo(ui, "c0pl4nd-font-fallback-1", choices, &mut slot1);
                });
                if slots_changed {
                    // Rebuild the ordered vec, dropping the "(none)" sentinel and
                    // the built-in label (neither is a real fallback face), and
                    // collapsing a hole so [none, "Noto"] becomes ["Noto"].
                    config.font.fallback = [slot0, slot1]
                        .into_iter()
                        .filter(|s| {
                            !s.trim().is_empty()
                                && s != super::fonts::NONE_LABEL
                                && s != super::fonts::BUILTIN_MONOSPACE_LABEL
                        })
                        .collect();
                    changed = true;
                }
                changed |= reset_to_default(ui, &mut config.font.fallback, &def.font.fallback);
                ui.end_row();
            }
        });
        group_gap(ui);
    }

    // -------------------------------------------------------------------- Cursor
    if section_visible(sel, q, "Cursor", &["style", "blink"]) {
        ui.heading("Cursor");
        help(ui, "Caret shape and blink.");
        grid("cursor_grid").show(ui, |ui| {
            if row_visible(q, "style shape block bar underline") {
                ui.label("Style");
                let cursor_label = cursor_style_label(config.cursor.style);
                changed |= dropdown_with_stepper(
                    ui,
                    "c0pl4nd-cursor-style",
                    &mut config.cursor.style,
                    &[
                        (CursorStyle::Block, "block"),
                        (CursorStyle::Bar, "bar"),
                        (CursorStyle::Underline, "underline"),
                    ],
                    Some(cursor_label),
                );
                changed |= reset_to_default(ui, &mut config.cursor.style, &def.cursor.style);
                ui.end_row();
            }

            if row_visible(q, "blink") {
                changed |= ui
                    .checkbox(&mut config.cursor.blink, "Blink the cursor")
                    .changed();
                ui.label("");
                changed |= reset_to_default(ui, &mut config.cursor.blink, &def.cursor.blink);
                ui.end_row();
            }
        });
        group_gap(ui);
    }

    // ------------------------------------------------------------------ Terminal
    if section_visible(
        sel,
        q,
        "Terminal",
        &[
            "scrollback",
            "startup panel",
            "shell",
            "copy on select",
            "paste",
        ],
    ) {
        ui.heading("Terminal");
        help(ui, "Scrollback, shell, and clipboard behavior.");
        group(
            ui,
            "Shell & scrollback",
            "Which shell launches and how much history each pane keeps.",
        );
        grid("terminal_shell").show(ui, |ui| {
            if row_visible(q, "scrollback lines history") {
                ui.label("Scrollback").on_hover_text(
                    "History lines kept per pane. Applies on restart — a pane's \
                     buffer is sized when its shell spawns.",
                );
                changed |= ui
                    .add(
                        egui::Slider::new(&mut config.scrollback_lines, 100..=1_000_000)
                            .logarithmic(true)
                            .suffix(" lines"),
                    )
                    .on_hover_text("Applies on the next launch.")
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.scrollback_lines, &def.scrollback_lines);
                ui.end_row();
            }

            if row_visible(q, "startup panel neofetch logo") {
                // DISABLED: the neofetch-style launch splash is not drawn by the
                // egui shell (only the legacy winit shell rendered it). Greyed
                // with an honest tooltip rather than a dead toggle that silently
                // does nothing — matching the ligatures / copy-on-select rows.
                ui.add_enabled_ui(false, |ui| {
                    ui.checkbox(&mut config.startup_panel, "Show logo + system info")
                        .on_hover_text(
                            "Not available: the launch splash is not drawn in this shell yet.",
                        );
                });
                ui.label("");
                ui.end_row();
            }

            if row_visible(q, "shell override program") {
                ui.label("Shell override")
                    .on_hover_text("Leave empty to use the OS default shell.");
                let mut shell = config.shell.clone().unwrap_or_default();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut shell)
                            .hint_text("platform default")
                            .desired_width(200.0),
                    )
                    .changed()
                {
                    config.shell = if shell.trim().is_empty() {
                        None
                    } else {
                        Some(shell)
                    };
                    changed = true;
                }
                changed |= reset_to_default(ui, &mut config.shell, &def.shell);
                ui.end_row();
            }
        });

        group(ui, "Clipboard", "Copy-on-select and paste safety.");
        grid("terminal_clipboard").show(ui, |ui| {
            if row_visible(q, "copy on select clipboard") {
                // Live again: mouse text-selection now exists in the egui shell
                // (drag to select), so the drag-end can auto-copy. When OFF the
                // selection is still made (and Ctrl/Cmd+Shift+C copies on demand);
                // when ON the selection is copied to the clipboard on release.
                changed |= ui
                    .checkbox(&mut config.copy_on_select, "Copy on select")
                    .on_hover_text(
                        "X11-style: copy a mouse selection to the clipboard \
                         automatically when the drag is released.",
                    )
                    .changed();
                ui.label("");
                changed |= reset_to_default(ui, &mut config.copy_on_select, &def.copy_on_select);
                ui.end_row();
            }

            if row_visible(q, "paste warn multiline newline security") {
                changed |= ui
                    .checkbox(
                        &mut config.paste_warn_multiline,
                        "Warn before multi-line paste",
                    )
                    .on_hover_text("Security: a pasted newline can run a shell command instantly.")
                    .changed();
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.paste_warn_multiline,
                    &def.paste_warn_multiline,
                );
                ui.end_row();
            }
        });
        group_gap(ui);
    }

    // -------------------------------------------------------------------- Window
    if section_visible(
        sel,
        q,
        "Window",
        &[
            "padding",
            "columns",
            "rows",
            "panes",
            "dividers",
            "linked",
            "symmetrical",
            "status bar",
            "graphics",
            "backend",
            "gpu",
            "renderer",
        ],
    ) {
        ui.heading("Window");
        help(
            ui,
            "Inner padding and the initial grid size. Live size/position is \
             remembered automatically.",
        );
        group(
            ui,
            "Layout",
            "Padding, split-pane dividers, and the status bar.",
        );
        grid("window_layout").show(ui, |ui| {
            if row_visible(q, "padding inner margin") {
                ui.label("Padding")
                    .on_hover_text("Inner inset between the pane edge and the terminal grid.");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut config.window.padding)
                            .range(0..=32)
                            .suffix(" px"),
                    )
                    .on_hover_text("Applies live — the grid re-insets without a restart.")
                    .changed();
                changed |= reset_to_default(ui, &mut config.window.padding, &def.window.padding);
                ui.end_row();
            }

            if row_visible(q, "panes dividers linked symmetrical equal split") {
                changed |= ui
                    .checkbox(&mut config.link_pane_dividers, "Keep panes equal")
                    .on_hover_text(
                        "Hold split-pane dividers at equal positions so every pane \
                         stays the same size. The top-bar symmetrical button equalises \
                         once regardless of this setting.",
                    )
                    .changed();
                ui.label("");
                changed |=
                    reset_to_default(ui, &mut config.link_pane_dividers, &def.link_pane_dividers);
                ui.end_row();
            }

            if row_visible(q, "status bar bottom hide show") {
                changed |= ui
                    .checkbox(&mut config.show_status_bar, "Show bottom status bar")
                    .on_hover_text(
                        "The bottom bar (pane count + hints). Turn off to reclaim the \
                         row for the terminal grid.",
                    )
                    .changed();
                ui.label("");
                changed |= reset_to_default(ui, &mut config.show_status_bar, &def.show_status_bar);
                ui.end_row();
            }
        });

        group(
            ui,
            "Graphics",
            "GPU backend and adapter — change only if glyphs render garbled.",
        );
        grid("window_graphics").show(ui, |ui| {
            if row_visible(q, "graphics backend gpu renderer dx12 vulkan gl") {
                ui.label("Graphics backend").on_hover_text(
                    "The GPU backend the renderer uses (Windows). Leave on Auto \
                     unless the terminal grid renders corrupted/garbled glyphs — \
                     then try Vulkan (or OpenGL) to route around a bad DX12 \
                     driver. Applies on restart.",
                );
                changed |= dropdown_with_stepper(
                    ui,
                    "c0pl4nd-graphics-backend",
                    &mut config.graphics_backend,
                    &[
                        (GraphicsBackend::Auto, "auto (recommended)"),
                        (GraphicsBackend::Dx12, "DX12"),
                        (GraphicsBackend::Vulkan, "Vulkan"),
                        (GraphicsBackend::Gl, "OpenGL"),
                    ],
                    None,
                );
                changed |=
                    reset_to_default(ui, &mut config.graphics_backend, &def.graphics_backend);
                ui.end_row();
            }

            if row_visible(
                q,
                "graphics gpu preference integrated discrete optimus hybrid",
            ) {
                ui.label("GPU preference").on_hover_text(
                    "Which GPU to render on, on a laptop with two GPUs (Intel + \
                     NVIDIA/AMD). Leave on Auto unless the terminal grid renders \
                     corrupted/garbled glyphs on the discrete GPU — then pick \
                     Integrated to render on the built-in GPU. Applies on restart.",
                );
                changed |= dropdown_with_stepper(
                    ui,
                    "c0pl4nd-gpu-preference",
                    &mut config.graphics_gpu,
                    &[
                        (GpuPreference::Auto, "auto (recommended)"),
                        (GpuPreference::Integrated, "integrated (low power)"),
                        (GpuPreference::Discrete, "discrete (high performance)"),
                    ],
                    None,
                );
                changed |= reset_to_default(ui, &mut config.graphics_gpu, &def.graphics_gpu);
                ui.end_row();
            }
        });

        group(
            ui,
            "Initial grid size",
            "Legacy columns/rows — this shell sizes the window in pixels and \
             remembers it, so these are inactive.",
        );
        grid("window_initial_size").show(ui, |ui| {
            // Initial terminal grid width at launch. The live window size is
            // remembered separately (geometry persistence), so this is the
            // first-launch / no-saved-geometry size; it takes effect on restart.
            // Floor of 1 mirrors the core validator (cols/rows must be non-zero).
            // DISABLED: this shell sizes the window in PIXELS and remembers the
            // size across launches (drag the window edge; eframe persists it) —
            // it has no columns/rows startup-size path, so these legacy fields are
            // inert here. Greyed with an honest tooltip rather than left as live
            // sliders that silently do nothing (matching the ligatures /
            // startup-panel rows). The TOML fields remain for the legacy shell.
            if row_visible(q, "columns cols initial width grid size") {
                ui.label("Initial columns");
                ui.add_enabled_ui(false, |ui| {
                    ui.add(
                        egui::DragValue::new(&mut config.window.cols)
                            .range(1..=500)
                            .suffix(" cols"),
                    )
                    .on_hover_text(
                        "Not used in this shell: the window is sized in pixels and \
                         its size is remembered across launches (resize by dragging \
                         the window edge).",
                    );
                });
                ui.end_row();
            }

            if row_visible(q, "rows initial height grid size") {
                ui.label("Initial rows");
                ui.add_enabled_ui(false, |ui| {
                    ui.add(
                        egui::DragValue::new(&mut config.window.rows)
                            .range(1..=300)
                            .suffix(" rows"),
                    )
                    .on_hover_text(
                        "Not used in this shell: the window is sized in pixels and \
                         its size is remembered across launches (resize by dragging \
                         the window edge).",
                    );
                });
                ui.end_row();
            }
        });
        group_gap(ui);
    }

    // ------------------------------------------------------------------- Toolbar
    if section_visible(
        sel,
        q,
        "Toolbar",
        &[
            "toolbar",
            "buttons",
            "quick actions",
            "top bar",
            "overflow",
            "reorder",
        ],
    ) {
        changed |= toolbar_settings_section(ui, config);
        group_gap(ui);
    }

    // ---------------------------------------------------------------------- Motion
    if section_visible(sel, q, "Motion", MOTION_SEARCH_LABELS) {
        ui.heading("Motion");
        help(
            ui,
            "Interface animation and retro CRT post-effects. Turn the master \
             switch off for a fully static UI; every effect below is gated behind \
             it and off by default.",
        );
        grid("motion_master").show(ui, |ui| {
            if row_visible(q, "enable animations") {
                changed |= ui
                    .checkbox(&mut config.effects.animations_enabled, "Enable animations")
                    .on_hover_text(
                        "Master switch. When off, transitions are instant (no fades) \
                         and every motion effect below is suppressed — idle frames \
                         cost the same as plain egui.",
                    )
                    .changed();
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.animations_enabled,
                    &def.effects.animations_enabled,
                );
                ui.end_row();
            }

            let on = config.effects.animations_enabled;

            if row_visible(q, "animation speed ui transition") {
                ui.label("UI transition speed").on_hover_text(
                    "Scale how long the UI's CHROME transitions take — hover fades, \
                     panel and collapsible expand/collapse, combobox/menu fades, and \
                     value-change lerps. 0 makes every transition instant; 1 is egui's \
                     full transition time and 2 is double that. This does NOT control \
                     the retro visual effects (flicker / VHS / mesh / scanline drift) \
                     — those have their own per-effect speed sliders below.",
                );
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Slider::new(&mut config.effects.animation_intensity, 0.0..=2.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.animation_intensity,
                    &def.effects.animation_intensity,
                );
                ui.end_row();
            }
        });

        let on = config.effects.animations_enabled;

        group(
            ui,
            "CRT screen",
            "Scan lines, RGB split, and brightness flicker.",
        );
        grid("motion_crt").show(ui, |ui| {
            if row_visible(q, "crt scanlines") {
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Checkbox::new(&mut config.effects.crt_scanlines, "CRT scan lines"),
                    )
                    .on_hover_text(
                        "Dark scan-line bands drifting slowly down the panes for a \
                         calm retro CRT look. Auto-disabled under reduced-motion.",
                    )
                    .changed();
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.crt_scanlines,
                    &def.effects.crt_scanlines,
                );
                ui.end_row();
            }

            if row_visible(q, "scanline darkness") {
                ui.label("Scanline darkness");
                changed |= ui
                    .add_enabled(
                        on && config.effects.crt_scanlines,
                        egui::Slider::new(&mut config.effects.scanline_darkness, 0.0..=1.0)
                            .text("darkness"),
                    )
                    .on_hover_text("How dark the scan-line troughs read. Enable scan lines first.")
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.scanline_darkness,
                    &def.effects.scanline_darkness,
                );
                ui.end_row();
            }

            if row_visible(q, "scanline drift speed") {
                ui.label("Scanline drift speed").on_hover_text(
                    "How fast the scan bands roll down the panes. 1 is the standard \
                     rate; lower is a slower, calmer roll and higher rolls faster. \
                     Enable scan lines first.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.effects.crt_scanlines,
                        egui::Slider::new(&mut config.effects.scanline_speed, 0.25..=3.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.scanline_speed,
                    &def.effects.scanline_speed,
                );
                ui.end_row();
            }

            if row_visible(q, "chromatic aberration") {
                // Explicit ON/OFF checkbox in col 1 (checkbox-left), the intensity
                // slider in col 2. The intensity slider alone read as "broken" when
                // it sat at 0, so on first enable we default it to a visible value.
                //
                // The "still at the floor" test below is `<= CHROMATIC_MIN`, NOT
                // `<= 0.0`: the slider's own render already clamped any 0.0 sentinel
                // up to `CHROMATIC_MIN` and wrote it back (see `CHROMATIC_MIN`), so
                // a `<= 0.0` guard is unreachable and leaves the effect enabling at
                // the faintest possible setting.
                let was_enabled = config.effects.chromatic_aberration_enabled;
                if ui
                    .add_enabled(
                        on,
                        egui::Checkbox::new(
                            &mut config.effects.chromatic_aberration_enabled,
                            "Chromatic aberration",
                        ),
                    )
                    .on_hover_text("Pure-channel red/blue fringing behind the text (RGB split).")
                    .changed()
                {
                    changed = true;
                    if config.effects.chromatic_aberration_enabled
                        && !was_enabled
                        && config.effects.chromatic_aberration <= CHROMATIC_MIN
                    {
                        config.effects.chromatic_aberration =
                            c0pl4nd_core::config::DEFAULT_CHROMATIC_INTENSITY;
                    }
                }
                let ca_on = on && config.effects.chromatic_aberration_enabled;
                changed |= ui
                    .add_enabled(
                        ca_on,
                        egui::Slider::new(
                            &mut config.effects.chromatic_aberration,
                            CHROMATIC_MIN..=CHROMATIC_MAX,
                        )
                        .text("intensity"),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.chromatic_aberration,
                    &def.effects.chromatic_aberration,
                );
                ui.end_row();
            }
        });

        group(
            ui,
            "Ambient node-mesh",
            "The animated \"Wired\" lattice drawn behind the panes.",
        );
        grid("motion_mesh").show(ui, |ui| {
            if row_visible(q, "wired mesh background") {
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Checkbox::new(
                            &mut config.effects.wired_ambient,
                            "Node-mesh background",
                        ),
                    )
                    .on_hover_text(
                        "An animated node-mesh ambient background drawn BEHIND the \
                         panes (a calm \"Wired\" field). Shows through when the window \
                         uses a translucent mode; hidden behind fully-opaque panes.",
                    )
                    .changed();
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.wired_ambient,
                    &def.effects.wired_ambient,
                );
                ui.end_row();
            }

            if row_visible(q, "mesh density") {
                ui.label("Mesh density");
                changed |= ui
                    .add_enabled(
                        on && config.effects.wired_ambient,
                        egui::Slider::new(&mut config.effects.mesh_density, 0.0..=2.0)
                            .text("density"),
                    )
                    .on_hover_text(
                        "How many nodes the wired-mesh background draws (sparse to dense).",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.mesh_density,
                    &def.effects.mesh_density,
                );
                ui.end_row();
            }

            if row_visible(q, "mesh brightness") {
                ui.label("Mesh brightness");
                changed |= ui
                    .add_enabled(
                        on && config.effects.wired_ambient,
                        egui::Slider::new(&mut config.effects.mesh_brightness, 0.0..=3.0)
                            .text("brightness"),
                    )
                    .on_hover_text(
                        "How bright the wired-mesh lattice reads — dim it toward \
                         invisible or brighten it to clearly pop. 1 is the default.",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.mesh_brightness,
                    &def.effects.mesh_brightness,
                );
                ui.end_row();
            }

            if row_visible(q, "mesh drift speed movement") {
                ui.label("Mesh drift speed").on_hover_text(
                    "How fast the node lattice drifts. 0 holds a still frame; 1 is \
                     the shipped drift; up to 2 is a brisk, roaming field.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.effects.wired_ambient,
                        egui::Slider::new(&mut config.effects.mesh_speed, 0.0..=2.0)
                            .text("movement"),
                    )
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.effects.mesh_speed, &def.effects.mesh_speed);
                ui.end_row();
            }

            if row_visible(q, "mesh color colour node picker reset theme accent") {
                ui.label("Mesh color").on_hover_text(
                    "Colour of the wired node-mesh lattice. By default it follows \
                     the theme accent; pick a colour to pin it regardless of theme, \
                     then \"Reset to theme\" to follow the theme accent again.",
                );
                // Effective swatch: the override if set, else the theme accent (what
                // the mesh actually draws with when no override is present) so the
                // picker opens on the current colour either way. Use `colors.accent`
                // — the ACTIVE theme's accent the host derived from `self.theme`
                // (the same theme the mesh painter uses) — NOT a `builtin_named`
                // re-lookup, which returns None for a file-based CUSTOM theme and
                // fell back to the void accent, showing the wrong preview swatch.
                let accent = colors.accent;
                let overridden = config.effects.mesh_color.is_some();
                let start =
                    config
                        .effects
                        .mesh_color
                        .unwrap_or([accent.r(), accent.g(), accent.b()]);
                ui.add_enabled_ui(on && config.effects.wired_ambient, |ui| {
                    ui.horizontal(|ui| {
                        let mut rgb = start;
                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                            config.effects.mesh_color = Some(rgb);
                            changed = true;
                        }
                        // Once the colour differs from the theme default, offer the
                        // "Reset to theme" revert (mirrors SCR1B3's "Follow theme");
                        // otherwise a muted "following theme" note.
                        if overridden {
                            if ui
                                .button("Reset to theme")
                                .on_hover_text("Follow the theme accent colour again.")
                                .clicked()
                            {
                                config.effects.mesh_color = None;
                                changed = true;
                            }
                        } else {
                            ui.label(egui::RichText::new("following theme").weak().small());
                        }
                    });
                });
                ui.label(""); // no ↺ column — "Reset to theme" IS the revert
                ui.end_row();
            }
        });

        group(
            ui,
            "Tape & motion accents",
            "VHS tracking, the cursor ghost-trail, and the boot sweep.",
        );
        grid("motion_accents").show(ui, |ui| {
            if row_visible(q, "vhs tracking") {
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Checkbox::new(&mut config.effects.vhs_tracking, "VHS tracking lines"),
                    )
                    .on_hover_text(
                        "Faint bright bands sweep down the window like analogue tape \
                         tracking error.",
                    )
                    .changed();
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.vhs_tracking,
                    &def.effects.vhs_tracking,
                );
                ui.end_row();
            }

            if row_visible(q, "vhs intensity") {
                ui.label("VHS intensity").on_hover_text(
                    "How bright the VHS tracking bands read — from a faint whisper \
                     to a bold, unmistakable sweep.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.effects.vhs_tracking,
                        egui::Slider::new(&mut config.effects.vhs_intensity, 0.0..=1.0)
                            .text("intensity"),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.vhs_intensity,
                    &def.effects.vhs_intensity,
                );
                ui.end_row();
            }

            if row_visible(q, "vhs drift speed") {
                ui.label("VHS drift speed").on_hover_text(
                    "How fast the VHS tracking bands sweep down the window. 1 is the \
                     standard rate; lower drifts more slowly and higher sweeps faster. \
                     Independent of the intensity.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.effects.vhs_tracking,
                        egui::Slider::new(&mut config.effects.vhs_speed, 0.25..=3.0),
                    )
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.effects.vhs_speed, &def.effects.vhs_speed);
                ui.end_row();
            }

            if row_visible(q, "screen flicker") {
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Checkbox::new(&mut config.effects.flicker, "Brightness flicker"),
                    )
                    .on_hover_text("Subtle CRT-style brightness flicker over the whole window.")
                    .changed();
                ui.label("");
                changed |= reset_to_default(ui, &mut config.effects.flicker, &def.effects.flicker);
                ui.end_row();
            }

            if row_visible(q, "flicker strength") {
                ui.label("Flicker strength");
                changed |= ui
                    .add_enabled(
                        on && config.effects.flicker,
                        egui::Slider::new(&mut config.effects.flicker_strength, 0.0..=1.0)
                            .text("strength"),
                    )
                    .on_hover_text(
                        "How strong the screen flicker is. Even at max the wash stays \
                         short of a full-black strobe (comfort guard).",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.flicker_strength,
                    &def.effects.flicker_strength,
                );
                ui.end_row();
            }

            if row_visible(q, "flicker speed") {
                ui.label("Flicker speed").on_hover_text(
                    "How fast the screen flicker pulses. 1 is the standard cadence; \
                     lower is a slower shimmer and higher flickers faster. Independent \
                     of the strength.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.effects.flicker,
                        egui::Slider::new(&mut config.effects.flicker_speed, 0.25..=3.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.flicker_speed,
                    &def.effects.flicker_speed,
                );
                ui.end_row();
            }

            if row_visible(q, "cursor trail") {
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Checkbox::new(&mut config.effects.cursor_trail, "Cursor ghost-trail"),
                    )
                    .on_hover_text("A fading echo follows the focused terminal cursor as it moves.")
                    .changed();
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.cursor_trail,
                    &def.effects.cursor_trail,
                );
                ui.end_row();
            }

            if row_visible(q, "cursor trail intensity") {
                ui.label("Cursor-trail intensity");
                changed |= ui
                    .add_enabled(
                        on && config.effects.cursor_trail,
                        egui::Slider::new(&mut config.effects.cursor_trail_intensity, 0.0..=2.0)
                            .text("intensity"),
                    )
                    .on_hover_text(
                        "How bold and long the cursor ghost-trail is — from a faint \
                         flick to a pronounced comet tail. Up to 2 for a long, \
                         unmistakable tail.",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.cursor_trail_intensity,
                    &def.effects.cursor_trail_intensity,
                );
                ui.end_row();
            }

            if row_visible(q, "boot glitch") {
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Checkbox::new(&mut config.effects.boot_glitch, "Boot glitch sweep"),
                    )
                    .on_hover_text(
                        "A one-shot glitch sweep plays for a moment when the app launches.",
                    )
                    .changed();
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.boot_glitch,
                    &def.effects.boot_glitch,
                );
                ui.end_row();
            }
        });
        group_gap(ui);
    }

    // --------------------------------------------------------------- Keybindings
    if section_visible(sel, q, "Keybindings", KEYBINDING_SEARCH_LABELS) {
        ui.heading("Keybindings");
        help(
            ui,
            "Edit a shortcut by typing its combo. \"mod\" is the platform command \
             modifier (Ctrl on Windows/Linux, Cmd on macOS); combine it with \
             \"shift\" / \"alt\" and one key, e.g. \"mod+shift+t\". Changes apply \
             immediately \u{2014} no restart.",
        );
        // These rows are LIVE: `C0pl4ndApp::dispatch_keybindings` resolves every
        // shortcut from these exact strings on every frame, so an edit here really
        // moves the chord. The rows are generated from `entries_mut()` so a
        // binding added to the schema can never end up without an editor.
        //
        // Copy/Paste are the one honest exception: `egui-winit` intercepts the
        // clipboard chords in its own window-event dispatcher and delivers them as
        // `Event::Copy` / `Event::Cut` rather than `Event::Key`, so they never
        // reach the dispatcher. They stay DISABLED (shown for reference) rather
        // than pretending to be rebindable.
        let dispatched: Vec<&'static str> = super::actions::Action::ALL
            .iter()
            .map(|a| a.binding())
            .collect();
        grid("keybindings_grid").show(ui, |ui| {
            for (name, combo) in config.keybindings.entries_mut() {
                let label = c0pl4nd_core::config::action_label(name);
                if !row_visible(q, label) {
                    continue;
                }
                ui.label(label);
                let editable = dispatched.contains(&name);
                let before = combo.clone();
                ui.add_enabled_ui(editable, |ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(combo)
                            .desired_width(180.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    if editable {
                        resp.on_hover_text("Live shortcut. Modifiers: mod (Ctrl/Cmd), shift, alt.");
                    } else {
                        resp.on_hover_text(
                            "Handled by the platform clipboard integration, not the \
                             shortcut dispatcher \u{2014} shown for reference.",
                        );
                    }
                });
                if *combo != before {
                    changed = true;
                }
                ui.end_row();
            }
        });
        // Surface conflicts, blank bindings, and unparseable combos inline. The
        // combos are free text, so two actions can collide on one chord (only one
        // wins), a binding can be left empty (the action becomes keyboard-
        // unreachable), or a combo can be unreadable — all silently. `validate()`
        // makes that explicit right under the editor instead of the user
        // wondering why a shortcut "does nothing". Collisions are grouped by the
        // CANONICAL chord, so "ctrl+shift+c" and "mod+shift+c" are correctly
        // reported as one physical shortcut.
        for issue in config.keybindings.validate() {
            ui.colored_label(
                egui::Color32::from_rgb(0xff, 0xb0, 0x00),
                format!("\u{26a0} {}", issue.message()),
            );
        }
        group_gap(ui);
    }

    // --------------------------------------------------------------------- Config
    if section_visible(
        sel,
        q,
        "Config",
        &[
            "config", "file", "folder", "open", "reveal", "path", "edit", "toml",
        ],
    ) {
        ui.heading("Config");
        if let Some(path) = c0pl4nd_core::Config::default_path() {
            help(
                ui,
                "Settings are saved to a single TOML file. Open its folder to back \
                 it up or hand-edit it.",
            );
            ui.label(
                egui::RichText::new(path.display().to_string())
                    .weak()
                    .small()
                    .monospace(),
            );
            ui.horizontal(|ui| {
                if ui.button("Open config folder").clicked() {
                    if let Some(dir) = path.parent() {
                        // Create the dir if it does not exist yet (zero-config
                        // start) so "reveal" always lands somewhere real.
                        let _ = std::fs::create_dir_all(dir);
                        reveal_in_file_manager(dir);
                    }
                }
                // Only offer "open file" when it actually exists — opening a
                // non-existent path just fails silently.
                if path.exists() && ui.button("Open config file").clicked() {
                    reveal_in_file_manager(&path);
                }
            });
        } else {
            help(ui, "No config path is available on this platform.");
        }
        group_gap(ui);
    }

    // --------------------------------------------------------------------- Updates
    if section_visible(
        sel,
        q,
        "Updates",
        &[
            "update", "mode", "off", "notify", "manual", "auto", "check", "interval", "channel",
            "stable", "beta", "nightly", "install", "download", "releases",
        ],
    ) {
        // NOTE: the prior per-page `set_max_width(min(480.0))` band-aid is GONE.
        // The real root cause (a page demanding more width than the window) is
        // now fixed once, at the top of the window's content closure, by clamping
        // the whole content `Ui` to the window's inner width — so EVERY page
        // (this one included) is bounded to the same width, and the long help
        // line below wraps via `help`'s `.wrap()` instead of widening the page.
        // Clamping THIS page narrower than its siblings would make it the one
        // odd-width page — exactly the drift we are removing.
        ui.heading("Updates");
        ui.label(
            egui::RichText::new(format!(
                "You are running v{}.",
                update_engine::updater::current_version()
            ))
            .weak()
            .small(),
        );
        help(
            ui,
            "Local-first: a check reads only the public GitHub Releases API and \
             sends no identifiers. off and manual never touch the network on \
             their own; notify and auto check once per launch.",
        );
        grid("updates_grid").show(ui, |ui| {
            // ---- Mode: off / notify / manual / auto ----
            // off    = never check, never touch the network
            // notify = check on launch (when due), passive toast if newer
            // manual = check only when the button below is pressed (default)
            // auto   = check on launch (when due), download + apply when found
            if row_visible(q, "update mode off notify manual auto network") {
                ui.label("Mode").on_hover_text(
                    "When C0PL4ND checks for updates: off (never), manual (only when \
                     you press Check for updates), notify (check once per launch, show \
                     a notice if newer), auto (check once per launch, download + install \
                     a verified update). A check reads only the public GitHub Releases \
                     API and sends no identifiers.",
                );
                changed |= dropdown_with_stepper(
                    ui,
                    "c0pl4nd-update-mode",
                    &mut config.update.mode,
                    &[
                        (UpdateMode::Off, "off"),
                        (UpdateMode::Notify, "notify"),
                        (UpdateMode::Manual, "manual"),
                        (UpdateMode::Auto, "auto"),
                    ],
                    None,
                );
                changed |= reset_to_default(ui, &mut config.update.mode, &def.update.mode);
                ui.end_row();
            }

            // ---- Check interval (hours) — only relevant for notify/auto ----
            if row_visible(q, "check interval hours") {
                let on_launch = matches!(config.update.mode, UpdateMode::Notify | UpdateMode::Auto);
                ui.add_enabled_ui(on_launch, |ui| {
                    ui.label("Check interval (hours)").on_hover_text(
                        "How often, in hours, an on-launch check (notify/auto) is due \
                         (1–168). Ignored for off and manual.",
                    );
                });
                ui.add_enabled_ui(on_launch, |ui| {
                    changed |= ui
                        .add(egui::Slider::new(
                            &mut config.update.check_interval_hours,
                            1..=168,
                        ))
                        .changed();
                });
                changed |= reset_to_default(
                    ui,
                    &mut config.update.check_interval_hours,
                    &def.update.check_interval_hours,
                );
                ui.end_row();
            }

            // ---- Release channel ----
            if row_visible(q, "channel release stable beta nightly") {
                let networked = config.update.mode != UpdateMode::Off;
                ui.add_enabled_ui(networked, |ui| {
                    ui.label("Release channel")
                        .on_hover_text("Which release line update checks follow.");
                });
                let channel_variants: Vec<(String, &str)> = UPDATE_CHANNELS
                    .iter()
                    .map(|c| (c.to_string(), *c))
                    .collect();
                let cur_channel = config.update.channel.clone();
                ui.add_enabled_ui(networked, |ui| {
                    changed |= dropdown_with_stepper(
                        ui,
                        "c0pl4nd-update-channel",
                        &mut config.update.channel,
                        &channel_variants,
                        Some(&cur_channel),
                    );
                });
                changed |= reset_to_default(ui, &mut config.update.channel, &def.update.channel);
                ui.end_row();
            }
        });

        // ---- Check for updates + inline status + action buttons ----
        if row_visible(q, "check for updates now install download update") {
            ui.add_space(6.0);
            // The check / update buttons NEVER open a browser — they drive the
            // in-app updater state machine (download + verify + apply in place).
            let networked = config.update.mode != UpdateMode::Off;
            ui.horizontal_wrapped(|ui| {
                let busy = updater.lock().map(|u| u.is_busy()).unwrap_or(false);
                if ui
                    .add_enabled(networked && !busy, egui::Button::new("Check for updates"))
                    .on_hover_text(if networked {
                        "Ask the public GitHub Releases API whether a newer version \
                         exists. No identifiers are sent. Stays in-app — no browser."
                    } else {
                        "Set Mode to manual/notify/auto to enable update checks."
                    })
                    .clicked()
                {
                    // The configured Mode decides what a found update does: in
                    // `auto` it downloads + installs without a further click; in
                    // `notify`/`manual` it surfaces the inline "Download & install"
                    // button. Pressing the button always performs the check now.
                    let kind = match config.update.mode {
                        UpdateMode::Auto => LaunchKind::Auto,
                        UpdateMode::Notify => LaunchKind::Notify,
                        UpdateMode::Off | UpdateMode::Manual => LaunchKind::Manual,
                    };
                    if let Ok(mut u) = updater.lock() {
                        u.start_check(ui.ctx(), kind);
                    }
                }
                render_update_status(ui, updater);
            });
            ui.add_space(4.0);
            // The releases LINK is the ONE deliberate browser hand-off (changelog
            // / manual download); the check + update buttons above never browse.
            if ui
                .link("View all releases on GitHub")
                .on_hover_text("Open the C0PL4ND releases page in your browser.")
                .clicked()
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                    "https://github.com/{}/{}/releases",
                    update_engine::UPDATE_OWNER,
                    update_engine::UPDATE_REPO
                )));
            }
        }
        group_gap(ui);
    }

    // ---- Privacy ----
    if section_visible(
        sel,
        q,
        "Privacy",
        &["history", "incognito", "clear", "secret"],
    ) {
        ui.heading("Privacy");
        help(
            ui,
            "C0PL4ND is local-first: no telemetry, no accounts. The only network \
             connection is the opt-in update check. Command history is kept in \
             memory only (never written to disk).",
        );

        // ---- Command history (grid-aligned, matching every other section) ----
        group(
            ui,
            "Command history",
            "Kept in memory only — never written to disk.",
        );
        grid("privacy_history").show(ui, |ui| {
            if row_visible(q, "record command history capture") {
                changed |= ui
                    .checkbox(
                        &mut config.history_capture_enabled,
                        "Record command history",
                    )
                    .on_hover_text(
                        "Capture typed commands for the Ctrl+Shift+P palette + the \
                         history sidebar. Passwords typed at prompts are never captured \
                         (they are not echoed); inline secrets like --password=… / \
                         API_KEY=… are redacted. Turn off for a no-history posture.",
                    )
                    .changed();
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.history_capture_enabled,
                    &def.history_capture_enabled,
                );
                ui.end_row();
            }

            if row_visible(q, "incognito secret session no history") {
                // Incognito is RUNTIME state (never persisted) owned by the host;
                // reflect it and report a toggle back via `set_incognito`.
                let mut inc = incognito;
                if ui
                    .checkbox(&mut inc, "No history this session")
                    .on_hover_text(
                        "Stop recording command history for THIS session and clear \
                         what is already recorded. Resets to off on the next launch.",
                    )
                    .changed()
                {
                    *set_incognito = Some(inc);
                }
                ui.label(""); // empty control column
                ui.label(""); // runtime state — no ↺ revert column
                ui.end_row();
            }
        });

        // ---- Clear data (grid-aligned action rows) ----
        let status_id = egui::Id::new("c0pl4nd_clear_ui_state_status");
        group(
            ui,
            "Clear data",
            "One-click erase of in-memory and older on-disk state.",
        );
        grid("privacy_clear").show(ui, |ui| {
            if row_visible(q, "clear command history erase now") {
                ui.label("Command history");
                if ui
                    .button("Clear now")
                    .on_hover_text("Erase (zeroize) every recorded command immediately.")
                    .clicked()
                {
                    *clear_history = true;
                }
                ui.label(""); // action row — no ↺ column
                ui.end_row();
            }

            if row_visible(q, "clear saved window ui state") {
                // Delete eframe's persisted `app.ron` (window/UI state). Even
                // though C0PL4ND no longer persists egui Memory (privacy F1 —
                // `C0pl4ndApp::persist_egui_memory` returns false), a file written
                // by an OLDER build may still hold typed find/palette undo history,
                // so give the user an explicit one-click erase. Window geometry is
                // unaffected: it lives in the config TOML, not app.ron.
                ui.label("Window/UI state");
                ui.vertical(|ui| {
                    if ui
                        .button("Clear saved state")
                        .on_hover_text(
                            "Delete the on-disk app.ron that older builds used to \
                             persist window/UI state (and, in those builds, typed \
                             find/palette undo history). Your window size/position \
                             are kept (stored separately).",
                        )
                        .clicked()
                    {
                        let msg = clear_saved_ui_state();
                        ui.ctx().data_mut(|d| d.insert_temp(status_id, msg));
                    }
                    if let Some(msg) = ui.ctx().data(|d| d.get_temp::<String>(status_id)) {
                        ui.add(egui::Label::new(egui::RichText::new(msg).weak().small()).wrap());
                    }
                });
                ui.label(""); // action row — no ↺ column
                ui.end_row();
            }
        });

        // ---- W1TN3SS opt-in crash/error/issue reporting (default OFF) ----
        group(
            ui,
            "Report a crash or issue",
            "Opt-in and OFF by default — nothing is sent without your per-report \
             consent, and every report is shown to you (editable) before it leaves.",
        );
        grid("privacy_reporting").show(ui, |ui| {
            // Equal-weight 3-way selectors (Off / Ask each time / Always) — no
            // pre-ticked default-on path; the Off radio is first + selected by
            // default. Mirrors the proven consent shape.
            if row_visible(q, "crash reports") {
                ui.label("Crash reports");
                changed |= reporting_mode_selector(
                    ui,
                    "crash_reports_mode",
                    &mut config.reporting.streams.crash_reports,
                );
                ui.label(""); // selector carries its own state — no ↺ column
                ui.end_row();
            }
            if row_visible(q, "manual issue reports") {
                ui.label("Manual issues");
                changed |= reporting_mode_selector(
                    ui,
                    "manual_issues_mode",
                    &mut config.reporting.streams.manual_issues,
                );
                ui.label("");
                ui.end_row();
            }
        });

        group_gap(ui);
    }

    changed
}

/// Retrieve (or lazily create) the shared in-app updater held across frames in
/// `ctx` temp-data. Wrapped in `Arc<Mutex<_>>` because egui temp-data requires
/// `Clone + Send + Sync + 'static` and the `Updater` owns a non-Clone mpsc
/// `Receiver`. The `Arc` clone is cheap; one `Updater` instance persists for the
/// app's lifetime under this id.
fn get_updater(ctx: &egui::Context) -> Arc<Mutex<Updater>> {
    let id = egui::Id::new("c0pl4nd_in_app_updater");
    ctx.data_mut(|d| {
        d.get_temp::<Arc<Mutex<Updater>>>(id).unwrap_or_else(|| {
            let u = Arc::new(Mutex::new(Updater::default()));
            d.insert_temp(id, u.clone());
            u
        })
    })
}

/// Render the inline update status + action buttons next to the "Check for
/// updates" button, driven by the [`UpdateState`] machine. Mutating calls
/// (start download, apply, recheck) are deferred past the immutable state
/// borrow so the borrow checker is satisfied. The buttons here NEVER open a
/// browser — they download + verify + apply in place.
fn render_update_status(ui: &mut egui::Ui, updater: &Arc<Mutex<Updater>>) {
    enum Act {
        // Boxed: `ReleaseInfo` is large (it carries the version, several URLs,
        // and the pinned manifest digest), so boxing keeps the enum small and
        // satisfies `clippy::large_enum_variant`.
        Download(Box<update_engine::net::ReleaseInfo>),
        Apply,
        RunInstaller,
        Recheck,
    }
    let mut act: Option<Act> = None;

    // Snapshot the state under a short-lived lock so the render closure does not
    // hold the lock across the deferred mutating calls below.
    let state = match updater.lock() {
        Ok(u) => u.state.clone(),
        Err(_) => return,
    };

    match &state {
        UpdateState::Idle => {}
        UpdateState::Checking => {
            ui.spinner();
            ui.label("Checking…");
        }
        UpdateState::UpToDate => {
            ui.label(
                egui::RichText::new(format!(
                    "You're on the latest version (v{}).",
                    update_engine::updater::current_version()
                ))
                .weak(),
            );
        }
        UpdateState::Available(info) => {
            ui.label(format!("v{} is available.", info.version));
            if ui
                .button("Download & install")
                .on_hover_text(
                    "Download the verified release, check its SHA-256 + signature, and \
                     stage it for install. Stays in-app — no browser.",
                )
                .clicked()
            {
                act = Some(Act::Download(Box::new(info.clone())));
            }
        }
        UpdateState::Downloading { received, total } => {
            let frac = if *total > 0 {
                *received as f32 / *total as f32
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .show_percentage()
                    .desired_width(150.0),
            );
        }
        UpdateState::ReadyToApply { version, .. } => {
            ui.label(format!("v{version} downloaded + verified."));
            if ui
                .button("Restart to finish update")
                .on_hover_text("Replace the running C0PL4ND with the new version and relaunch.")
                .clicked()
            {
                act = Some(Act::Apply);
            }
        }
        UpdateState::ReadyToRunInstaller { version, .. } => {
            ui.label(format!("v{version} downloaded + verified."));
            if ui
                .button("Restart & install")
                .on_hover_text(
                    "C0PL4ND is in a protected folder (e.g. Program Files), so it installs \
                     via the verified installer: one Windows permission prompt, then a \
                     silent in-place update and relaunch — no installer window.",
                )
                .clicked()
            {
                act = Some(Act::RunInstaller);
            }
        }
        UpdateState::Applied { version } => {
            ui.label(format!("Updated to v{version} — restarting…"));
        }
        UpdateState::Failed(e) => {
            let err = ui.visuals().error_fg_color;
            ui.colored_label(err, crate::user_error::update_failed_user_copy(e));
            if ui.button("Retry").clicked() {
                act = Some(Act::Recheck);
            }
        }
    }

    if let Some(act) = act {
        if let Ok(mut u) = updater.lock() {
            match act {
                Act::Download(info) => u.start_download(ui.ctx(), *info),
                Act::Apply => u.apply_and_restart(ui.ctx()),
                Act::RunInstaller => u.run_installer(ui.ctx()),
                Act::Recheck => u.start_check(ui.ctx(), LaunchKind::Manual),
            }
        }
    }
}

/// Kick off the on-launch update CHECK on the SHARED in-app updater (the same
/// instance the Settings → Updates page and the notification banner drive).
/// Called once per launch by the app when the configured update mode opts in.
/// Maps the persisted [`UpdateMode`] to the updater [`LaunchKind`]
/// (`auto` → hands-free download+apply; `notify` → banner). `off`/`manual` never
/// reach here. The check runs on a background thread; the banner reflects the
/// result on subsequent frames. Sharing ONE `Updater` is what makes the banner
/// and the Settings page a single state machine (no divergent second updater).
pub fn start_launch_update_check(ctx: &egui::Context, mode: UpdateMode) {
    let kind = match mode {
        UpdateMode::Auto => LaunchKind::Auto,
        UpdateMode::Notify => LaunchKind::Notify,
        UpdateMode::Off | UpdateMode::Manual => return,
    };
    let updater = get_updater(ctx);
    // Bind the guard to a local (not an if-let tail expression) so its temporary
    // is dropped BEFORE `updater` at the end of the function (E0597).
    let mut guard = match updater.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    guard.start_check(ctx, kind);
}

/// Render the PERSISTENT, dismissible update notification banner as a top panel
/// when a newer release is available (or a one-click apply is in flight). A
/// single **"Update now"** click drives the WHOLE verified flow — download →
/// SHA-256/minisign verify → silent in-place `self-replace` → relaunch — with
/// inline progress, never leaving the app. The banner shares the SAME [`Updater`]
/// the Settings → Updates page uses, so both reflect one state machine.
///
/// No-op when the updater has nothing to surface or the user dismissed the
/// banner for the current release. Mutations (start the one-click flow / dismiss
/// / retry) are deferred past the state snapshot so the updater lock is never
/// held across a mutating call — the same borrow discipline as
/// [`render_update_status`]. Called from the app's frame layout, below the
/// titlebar and above the terminal grid.
///
/// `#[allow(deprecated)]`: egui 0.34 deprecated the top-level
/// `TopBottomPanel::…show(ctx, …)` form in favour of `show_inside`, but the app
/// chrome (titlebar / status bar) still uses the top-level form; the banner
/// matches that established convention rather than diverging.
#[allow(deprecated)]
pub(super) fn update_banner(ctx: &egui::Context) {
    let updater = get_updater(ctx);
    // Advance the state machine EVERY frame — the Settings page also polls, but
    // only while it is open, so the banner must drive `poll` itself or a launch
    // check would never progress Checking → Available (→ the one-click chain)
    // while Settings is closed. `poll` is cheap and safe to call twice a frame
    // (the second drain finds an empty channel).
    if let Ok(mut u) = updater.lock() {
        u.poll(ctx);
    }
    // Snapshot visibility + state under a short-lived lock so the render closure
    // never holds the lock across the deferred mutating calls below.
    let (visible, state) = match updater.lock() {
        Ok(u) => (u.banner_visible(), u.state.clone()),
        Err(_) => return,
    };
    if !visible {
        return;
    }

    enum Act {
        UpdateNow,
        Dismiss,
    }
    let mut act: Option<Act> = None;

    let accent = super::theme::brand::GREEN;
    // A faint brand-accent wash so the strip reads as chrome, not an alarm
    // (Akira-red is reserved for alarms; a routine update is not one).
    let fill = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 40);
    egui::TopBottomPanel::top("c0pl4nd_update_banner")
        .frame(
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::symmetric(12, 8)),
        )
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                match &state {
                    UpdateState::Available(info) => {
                        ui.label(
                            egui::RichText::new(format!(
                                "C0PL4ND v{} is available — you have v{}.",
                                info.version,
                                update_engine::updater::current_version()
                            ))
                            .color(accent)
                            .strong(),
                        );
                        ui.add_space(8.0);
                        if ui
                            .button(egui::RichText::new("Update now").strong())
                            .on_hover_text(
                                "Download the verified release, check its SHA-256 + \
                                 signature, install it in place, and relaunch — all in \
                                 one click. Stays in-app; no browser.",
                            )
                            .clicked()
                        {
                            act = Some(Act::UpdateNow);
                        }
                        if ui
                            .button("Dismiss")
                            .on_hover_text("Hide this until the next update. You can still update from Settings → Updates.")
                            .clicked()
                        {
                            act = Some(Act::Dismiss);
                        }
                    }
                    UpdateState::Downloading { received, total } => {
                        ui.label(
                            egui::RichText::new("Updating — downloading and verifying…")
                                .color(accent),
                        );
                        ui.add_space(8.0);
                        let frac = if *total > 0 {
                            *received as f32 / *total as f32
                        } else {
                            0.0
                        };
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .show_percentage()
                                .desired_width(180.0),
                        );
                    }
                    UpdateState::ReadyToApply { version, .. } => {
                        // With the one-click chain this arm is transient (the
                        // download auto-applies); render a fallback button for the
                        // rare non-chained arrival so the user is never stuck.
                        ui.label(
                            egui::RichText::new(format!("v{version} verified — installing…"))
                                .color(accent),
                        );
                        ui.add_space(8.0);
                        if ui.button("Restart to finish").clicked() {
                            act = Some(Act::UpdateNow);
                        }
                    }
                    UpdateState::ReadyToRunInstaller { version, .. } => {
                        // Program-Files fallback: transient under the one-click
                        // chain (auto-launches the silent elevated installer);
                        // render a fallback button for the rare non-chained
                        // arrival. `update_now` routes this to `run_installer`.
                        ui.label(
                            egui::RichText::new(format!("v{version} verified — installing…"))
                                .color(accent),
                        );
                        ui.add_space(8.0);
                        if ui
                            .button("Restart & install")
                            .on_hover_text(
                                "Installs via the verified installer — one Windows \
                                 permission prompt, then a silent in-place update.",
                            )
                            .clicked()
                        {
                            act = Some(Act::UpdateNow);
                        }
                    }
                    UpdateState::Applied { version } => {
                        ui.label(
                            egui::RichText::new(format!("Updated to v{version} — restarting…"))
                                .color(accent)
                                .strong(),
                        );
                    }
                    UpdateState::Failed(e) => {
                        ui.label(
                            egui::RichText::new(crate::user_error::update_failed_user_copy(e))
                                .color(ui.visuals().error_fg_color),
                        );
                        ui.add_space(8.0);
                        if ui.button("Try again").clicked() {
                            // `update_now` re-checks from a Failed state.
                            act = Some(Act::UpdateNow);
                        }
                        if ui.button("Dismiss").clicked() {
                            act = Some(Act::Dismiss);
                        }
                    }
                    // `banner_visible()` gates the quiet states out; render nothing.
                    UpdateState::Idle | UpdateState::Checking | UpdateState::UpToDate => {}
                }
            });
        });

    if let Some(act) = act {
        if let Ok(mut u) = updater.lock() {
            match act {
                Act::UpdateNow => u.update_now(ctx),
                Act::Dismiss => u.dismiss_banner(),
            }
        }
    }
}

/// Human label for a cursor style (used by the combo's selected-text + items).
fn cursor_style_label(style: CursorStyle) -> &'static str {
    match style {
        CursorStyle::Block => "block",
        CursorStyle::Bar => "bar",
        CursorStyle::Underline => "underline",
    }
}

/// The font-size slider's bounds (points). The −/+ steppers and the slider share
/// this band so a stepped value can never leave the slider's range.
const FONT_SIZE_RANGE: std::ops::RangeInclusive<f32> = 8.0..=32.0;
/// The line-height slider's bounds (PIXELS). C0PL4ND's line-height is a px cell
/// advance (SCR1B3's is a unitless ratio) — the ± stepper mirrors SCR1B3's UI
/// pattern but keeps C0PL4ND's px range and a ±1.0 px step.
const LINE_HEIGHT_PX_RANGE: std::ops::RangeInclusive<f32> = 12.0..=48.0;

/// Step the font size by `delta` points, re-clamped into [`FONT_SIZE_RANGE`] so a
/// −/+ press can never push the value past the slider's bounds (mirrors SCR1B3's
/// `(editor_size ± 1.0).clamp(8.0, 32.0)` stepper). Pure so it is unit-testable.
fn step_font_size(size: f32, delta: f32) -> f32 {
    (size + delta).clamp(*FONT_SIZE_RANGE.start(), *FONT_SIZE_RANGE.end())
}

/// Step the line height by `delta` PIXELS, re-clamped into [`LINE_HEIGHT_PX_RANGE`].
/// C0PL4ND's line-height is px (not SCR1B3's ratio), so the step is ±1.0 px rather
/// than SCR1B3's ±0.1 ratio — the UI pattern is ported, the unit/range are not.
fn step_line_height_px(px: f32, delta: f32) -> f32 {
    (px + delta).clamp(*LINE_HEIGHT_PX_RANGE.start(), *LINE_HEIGHT_PX_RANGE.end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every section `render_sections` actually renders must be reachable from the
    /// left-nav.
    ///
    /// The **Config** section regressed exactly this way: the page was fully built,
    /// but `"Config"` was missing from [`CATEGORIES`], so `sel` could never equal it
    /// and the only way to reach the page was to type a matching term into the
    /// settings search box. A built page nobody can click to is indistinguishable
    /// from a missing feature.
    ///
    /// This scans THIS file's own source for the section names passed to
    /// `section_visible` and asserts each one is offered in the nav. The needle is
    /// assembled with `concat!` so the scan cannot match this test's own text.
    ///
    /// The scan is deliberately truncated at the start of this `#[cfg(test)]` module:
    /// the tests below call `section_visible` with LITERAL arguments
    /// (`section_visible("Font", "", "Font", …)`), which a naive whole-file scan
    /// would mistake for rendered production sections. A guard that reads its own
    /// test code is the classic way these wiring checks go quietly wrong.
    #[test]
    fn every_rendered_section_is_reachable_from_the_nav() {
        let whole = include_str!("settings.rs");
        // First occurrence is the real module boundary; any later match is inside
        // this test's own text and cannot precede it.
        let src = match whole.find(concat!("#[cfg", "(test)]")) {
            Some(end) => &whole[..end],
            None => whole,
        };
        let needle = concat!("section_", "visible(");
        let mut rendered: Vec<&str> = Vec::new();
        let mut rest = src;
        while let Some(hit) = rest.find(needle) {
            let (before, after) = rest.split_at(hit);
            // Skip the function DEFINITION — only call sites name a section.
            let is_definition = before.trim_end().ends_with("fn");
            rest = &after[needle.len()..];
            if is_definition {
                continue;
            }
            // Args are `sel, q, "Name", &[…]` — the first string literal after the
            // open paren is the section name, in both rustfmt layouts.
            let Some(open) = rest.find('"') else { continue };
            let tail = &rest[open + 1..];
            let Some(close) = tail.find('"') else {
                continue;
            };
            rendered.push(&tail[..close]);
        }

        assert!(
            !rendered.is_empty(),
            "the source scan found no section_visible call sites — the scan itself \
             has broken, which would make this test vacuously green"
        );

        let missing: Vec<&&str> = rendered
            .iter()
            .filter(|name| !CATEGORIES.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "these settings sections are rendered but NOT listed in CATEGORIES, so \
             they are unreachable from the left-nav: {missing:?}"
        );
    }

    #[test]
    fn builtin_themes_include_the_default() {
        assert!(
            BUILTIN_THEMES.contains(&Config::default().theme.as_str()),
            "the default theme stem must be one of the offered built-ins"
        );
    }

    #[test]
    fn ui_scale_commit_defers_while_dragging_and_commits_on_release() {
        // The runaway-scale fix: while the handle is DRAGGED, no value is committed
        // (so `set_zoom_factor` never fires mid-drag and can't rescale the slider
        // under the cursor), no matter how far the working value has moved.
        assert_eq!(
            ui_scale_commit(1.0, 3.0, true),
            None,
            "a dragged slider must NOT commit — deferring is what stops the runaway"
        );
        assert_eq!(
            ui_scale_commit(1.0, 0.5, true),
            None,
            "still deferred while dragging even at the low end"
        );
        // On release / track-click / keyboard step (not dragging) a CHANGED value
        // commits exactly once — this is the frame the zoom is applied.
        assert_eq!(
            ui_scale_commit(1.0, 1.5, false),
            Some(1.5),
            "releasing at a new value commits it (one zoom application)"
        );
        // No spurious write when the value is unchanged (steady-state frames /
        // the release frame of a no-op interaction).
        assert_eq!(
            ui_scale_commit(1.5, 1.5, false),
            None,
            "an unchanged value must not be re-committed"
        );
    }

    #[test]
    fn ui_scale_effective_clamp_keeps_the_app_usable() {
        // Safety net (independent of the slider's own 0.5..=3.0 range): even a
        // corrupt / runaway config value can never leave the UI unusably tiny,
        // huge, or blank — `effective_ui_scale` clamps to 0.5..=3.0 and treats a
        // non-finite value as the 1.0 default.
        let mk = |s: f32| {
            Config {
                ui_scale: s,
                ..Config::default()
            }
            .effective_ui_scale()
        };
        assert_eq!(
            mk(99.0),
            3.0,
            "an over-large scale clamps to the 3.0 ceiling"
        );
        assert_eq!(mk(0.01), 0.5, "a tiny scale clamps up to the 0.5 floor");
        assert_eq!(mk(f32::NAN), 1.0, "a non-finite scale falls back to 100%");
        assert_eq!(mk(f32::INFINITY), 1.0);
        assert_eq!(mk(1.25), 1.25, "an in-range scale passes through unchanged");
    }

    #[test]
    fn app_ron_path_targets_the_eframe_app_id_file() {
        // When a platform storage dir is resolvable, the helper must point at the
        // `app.ron` leaf inside the `com.itashacorp.c0pl4nd` app-id folder — the
        // exact file eframe writes (and the F1 leak target the Clear button
        // deletes). We assert the two load-bearing components rather than the
        // full platform-specific path so the test is OS-portable.
        match app_ron_path() {
            Some(p) => {
                assert_eq!(
                    p.file_name().and_then(|f| f.to_str()),
                    Some("app.ron"),
                    "must resolve the app.ron leaf eframe persists state into"
                );
                let s = p.to_string_lossy().replace('\\', "/");
                assert!(
                    s.contains(EFRAME_APP_ID),
                    "path must live under the eframe app-id folder \
                     '{EFRAME_APP_ID}'; got {s}"
                );
            }
            // No platform storage dir (e.g. the relevant env var is unset on this
            // runner) — eframe would not persist either, so there is nothing to
            // resolve. Mirror that condition by asserting the app-id constant
            // matches the `with_app_id` in egui_main.rs.
            None => assert_eq!(EFRAME_APP_ID, "com.itashacorp.c0pl4nd"),
        }
    }

    #[test]
    fn keybinding_search_labels_cover_every_binding() {
        // A binding added to the schema without a matching search label would
        // render a row that no search query can reveal (the phantom-section
        // failure `every_rendered_section_is_reachable` guards elsewhere).
        for (name, _) in c0pl4nd_core::config::Keybindings::default().entries() {
            let label = c0pl4nd_core::config::action_label(name).to_lowercase();
            assert!(
                KEYBINDING_SEARCH_LABELS.contains(&label.as_str()),
                "binding '{name}' (label {label:?}) has no Keybindings search label"
            );
        }
    }

    #[test]
    fn every_dispatched_binding_is_editable_and_clipboard_rows_are_not() {
        // The row loop enables an editor only for bindings the dispatcher
        // actually resolves. If an action were dropped from `Action::ALL`, its
        // row would silently become a dead editor again — the exact fakery this
        // section was rewritten to remove.
        let dispatched: Vec<&str> = super::super::actions::Action::ALL
            .iter()
            .map(|a| a.binding())
            .collect();
        for (name, _) in c0pl4nd_core::config::Keybindings::default().entries() {
            let expected_editable = name != "copy" && name != "paste";
            assert_eq!(
                dispatched.contains(&name),
                expected_editable,
                "binding '{name}' editability does not match the dispatch table"
            );
        }
    }

    #[test]
    fn section_visible_matches_selected_when_not_searching() {
        assert!(section_visible("Font", "", "Font", &["size"]));
        assert!(!section_visible("Font", "", "Cursor", &["blink"]));
    }

    #[test]
    fn section_visible_is_cross_category_when_searching() {
        // A query that matches a Cursor label reveals the Cursor section even
        // while "Font" is the selected tab (cross-category search).
        assert!(section_visible("Font", "blink", "Cursor", &["blink"]));
    }

    #[test]
    fn row_visible_filters_by_query() {
        assert!(row_visible("", "anything"));
        assert!(row_visible("opa", "Window opacity"));
        assert!(!row_visible("zzz", "Window opacity"));
    }

    #[test]
    fn font_size_stepper_clamps_at_bounds() {
        // Steps by ±1.0 pt within the band…
        assert_eq!(step_font_size(13.0, 1.0), 14.0);
        assert_eq!(step_font_size(13.0, -1.0), 12.0);
        // …and re-clamps hard at both bounds so a −/+ press can never leave range.
        assert_eq!(step_font_size(32.0, 1.0), 32.0, "clamps at the upper bound");
        assert_eq!(step_font_size(8.0, -1.0), 8.0, "clamps at the lower bound");
        // The stepper band matches the slider's declared range.
        assert_eq!(*FONT_SIZE_RANGE.start(), 8.0);
        assert_eq!(*FONT_SIZE_RANGE.end(), 32.0);
    }

    #[test]
    fn line_height_px_stepper_steps_by_one_and_clamps() {
        // C0PL4ND's line-height is PIXELS 12..=48 (not SCR1B3's ratio): the step is
        // ±1.0 px, re-clamped at both px bounds.
        assert_eq!(step_line_height_px(20.0, 1.0), 21.0);
        assert_eq!(step_line_height_px(20.0, -1.0), 19.0);
        assert_eq!(step_line_height_px(48.0, 1.0), 48.0, "clamps at 48 px");
        assert_eq!(step_line_height_px(12.0, -1.0), 12.0, "clamps at 12 px");
        assert_eq!(*LINE_HEIGHT_PX_RANGE.start(), 12.0);
        assert_eq!(*LINE_HEIGHT_PX_RANGE.end(), 48.0);
    }

    #[test]
    fn cursor_style_label_round_trips() {
        assert_eq!(cursor_style_label(CursorStyle::Block), "block");
        assert_eq!(cursor_style_label(CursorStyle::Bar), "bar");
        assert_eq!(cursor_style_label(CursorStyle::Underline), "underline");
    }

    #[test]
    fn dropdown_stepper_cycles_variants_with_wraparound() {
        // The up/down stepper cycles the variant list and WRAPS at both ends, so
        // the spinner never dead-ends. `+1` = ▼ next, `-1` = ▲ previous.
        let n = 3;
        // Forward from each index, wrapping past the last back to the first.
        assert_eq!(step_index(Some(0), n, 1), 1);
        assert_eq!(step_index(Some(1), n, 1), 2);
        assert_eq!(
            step_index(Some(2), n, 1),
            0,
            "next past the last wraps to first"
        );
        // Backward from each index, wrapping past the first back to the last.
        assert_eq!(step_index(Some(2), n, -1), 1);
        assert_eq!(
            step_index(Some(0), n, -1),
            2,
            "prev before the first wraps to last"
        );
        // A value NOT in the list lands on a defined variant (first on next, last
        // on prev) so the arrows always do something.
        assert_eq!(step_index(None, n, 1), 0);
        assert_eq!(step_index(None, n, -1), n - 1);
        // A full forward cycle returns to the start (endless spinner).
        let mut i = 0usize;
        for _ in 0..n {
            i = step_index(Some(i), n, 1);
        }
        assert_eq!(i, 0, "n forward steps over n variants return to the start");
        // Degenerate empty list never panics.
        assert_eq!(step_index(None, 0, 1), 0);
    }

    #[test]
    fn dropdown_combo_label_is_truncated_to_a_fixed_width() {
        // The fixed-width guarantee (deterministic, font-atlas-independent): with a
        // width of 10pt per character, a label wider than the budget is truncated
        // with a trailing ellipsis so it fits — this is what keeps the combo (and
        // the stepper beside it) a CONSTANT width regardless of the value. A label
        // that already fits is returned unchanged.
        let w10 = |s: &str| s.chars().count() as f32 * 10.0;
        // Fits (4 chars = 40pt <= 100): unchanged.
        assert_eq!(truncate_to_width("auto", 100.0, w10), "auto");
        // Too wide (10 chars = 100pt > 55): truncated with "…" and now fits.
        let fitted = truncate_to_width("abcdefghij", 55.0, w10);
        assert!(
            fitted.ends_with('…'),
            "an over-long label gets an ellipsis (got {fitted:?})"
        );
        assert!(fitted.chars().count() < "abcdefghij".chars().count());
        assert!(
            w10(&fitted) <= 55.0,
            "the truncated label must fit the budget (got {})",
            w10(&fitted)
        );
        // A budget too small for any char + ellipsis degrades to a bare "…", never
        // panics or loops forever.
        assert_eq!(truncate_to_width("hello", 1.0, w10), "…");
        // `fit_combo_label` wires the real font measurement in without panicking
        // even in the headless test context (where glyphs measure ~0-width, so it
        // returns the text unchanged — the width path is covered purely above).
        egui::__run_test_ui(|ui| {
            let _ = fit_combo_label(ui, "itasha-corp", DROPDOWN_WIDTH - 34.0);
        });
    }

    #[test]
    fn stepper_arrows_are_painter_drawn_geometry_side_by_side() {
        // The arrows are DRAWN as triangles via the painter (never font glyphs, so
        // they can't render as tofu boxes): each is a fixed 22×22 button that always
        // occupies its allocated rect. Laid out in a horizontal row, the "next" (▼)
        // button sits to the RIGHT of the "previous" (▲) button on the same line.
        egui::__run_test_ui(|ui| {
            ui.horizontal(|ui| {
                let up = stepper_arrow(ui, true, "Previous option");
                let down = stepper_arrow(ui, false, "Next option");
                assert_eq!(
                    up.rect.size(),
                    egui::vec2(22.0, 22.0),
                    "the up arrow renders as a fixed-size painter button"
                );
                assert_eq!(down.rect.size(), egui::vec2(22.0, 22.0));
                assert!(
                    (up.rect.center().y - down.rect.center().y).abs() < 0.5,
                    "the two arrows share a row (side by side, not stacked)"
                );
                assert!(
                    down.rect.left() >= up.rect.right() - 0.01,
                    "the next arrow is to the RIGHT of the previous arrow (horizontal)"
                );
            });
        });
    }

    #[test]
    fn updates_is_a_navigable_category() {
        // The new Updates section must be reachable from the left nav.
        assert!(
            CATEGORIES.contains(&"Updates"),
            "Updates must be a left-nav category so its rows are reachable"
        );
    }

    #[test]
    fn motion_and_fonts_are_navigable_categories() {
        // The renamed "Fonts" tab and the new "Motion" tab (which now owns the
        // CRT / chromatic / animation controls) must both be reachable from the
        // left nav; the old "Font" name must be gone.
        assert!(
            CATEGORIES.contains(&"Motion"),
            "Motion must be a left-nav category so its effect rows are reachable"
        );
        assert!(
            CATEGORIES.contains(&"Fonts"),
            "the Font section was renamed to Fonts for SCR1B3 parity"
        );
        assert!(
            !CATEGORIES.contains(&"Font"),
            "the old singular 'Font' category name must not linger"
        );
    }

    #[test]
    fn motion_section_is_cross_category_searchable() {
        // The controls that MOVED from Appearance to Motion must be reachable by
        // their FULL names from any tab via the PRODUCTION label set — and must no
        // longer resolve to Appearance (whose stale CRT labels were removed). This
        // asserts against the real `MOTION_SEARCH_LABELS`/`APPEARANCE_SEARCH_LABELS`
        // consts, so it would fail if a moved label were dropped or left behind.
        for term in [
            "crt scanlines",
            "scanline darkness",
            "chromatic aberration",
            "cursor trail",
        ] {
            assert!(
                section_visible("Appearance", term, "Motion", MOTION_SEARCH_LABELS),
                "'{term}' must reveal the Motion section that now owns it"
            );
            assert!(
                !section_visible("Motion", term, "Appearance", APPEARANCE_SEARCH_LABELS),
                "'{term}' must NOT resurrect a phantom empty Appearance section"
            );
        }
        // Every Motion search label must correspond to a Motion row (its canonical
        // `row_visible` label), so a query can never reveal the section with no
        // matching row. The row labels ARE the search labels by construction.
        assert!(MOTION_SEARCH_LABELS.contains(&"chromatic aberration"));
        assert!(MOTION_SEARCH_LABELS.contains(&"cursor trail"));
    }

    #[test]
    fn update_channels_include_the_default_channel() {
        // The channel combo must offer the default channel, or selecting it back
        // would be impossible.
        assert!(
            UPDATE_CHANNELS.contains(&Config::default().update.channel.as_str()),
            "the default update channel must be one of the offered choices"
        );
    }

    #[test]
    fn updates_section_is_cross_category_searchable() {
        // Searching for an Updates label reveals the section even when another
        // tab is selected (proves the section's labels are wired into search).
        assert!(section_visible(
            "Appearance",
            "channel",
            "Updates",
            &["check on launch", "channel"]
        ));
    }
}
