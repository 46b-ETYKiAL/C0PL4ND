//! Theme schema + loader. Themes are simple TOML data files (see
//! `assets/themes/*.toml`); the flagship default is `itasha-void`.

use serde::{Deserialize, Serialize};
use std::path::Path;

mod color_model;
pub mod glyph_coverage;
mod itermcolors;

pub use color_model::{
    contrast_ratio, dim_foreground, enforce_min_contrast, relative_luminance, ColorOptions,
    ContrastScope, IntenseTextStyle, CONTRAST_RATIO_MAX, CONTRAST_RATIO_MIN,
};
pub use glyph_coverage::{curve_for, CoverageCurve};

/// Remap an *indexed* foreground 0-7 to its bright twin 8-15 (the bold-as-bright
/// rule). Every other colour — an already-bright index, an extended 16-255
/// index, a 24-bit RGB, or the theme default — is returned untouched.
///
/// Returning the input unchanged for `Rgb` is the load-bearing half of this
/// function: remapping a 24-bit colour would silently rewrite a colour the
/// program asked for exactly.
fn brighten_indexed(color: crate::grid::Color) -> crate::grid::Color {
    match color {
        crate::grid::Color::Indexed(i) if i < 8 => crate::grid::Color::Indexed(i + 8),
        other => other,
    }
}

/// Whether the minimum-contrast clamp may touch a cell whose *original*
/// foreground was `color`, under `scope`.
///
/// Under [`ContrastScope::IndexedOnly`] both `Indexed` and `Default` qualify:
/// each is palette-derived, so the clamp is adjusting a theme choice rather
/// than an explicit 24-bit colour the program picked.
fn contrast_clamp_applies(color: crate::grid::Color, scope: ContrastScope) -> bool {
    match scope {
        ContrastScope::Never => false,
        ContrastScope::Always => true,
        ContrastScope::IndexedOnly => !matches!(color, crate::grid::Color::Rgb(..)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("could not read theme {0}")]
    Io(String),
    #[error("theme parse error: {0}")]
    Parse(String),
    #[error("invalid hex color {0:?}")]
    BadHex(String),
}

/// The eight ANSI colors for one intensity row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnsiRow {
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
}

/// A complete color scheme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    #[serde(default)]
    pub author: String,
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    #[serde(default)]
    pub cursor_text: String,
    #[serde(default)]
    pub selection_background: String,
    #[serde(default)]
    pub selection_foreground: String,
    pub normal: AnsiRow,
    pub bright: AnsiRow,
}

/// Parse `#RRGGBB` into an `(r, g, b)` triple.
///
/// # Examples
///
/// ```
/// use c0pl4nd_core::theme::parse_hex;
///
/// assert_eq!(parse_hex("#ff0090").unwrap(), (255, 0, 144));
/// // The leading '#' is optional and surrounding whitespace is trimmed.
/// assert_eq!(parse_hex("  00ff90 ").unwrap(), (0, 255, 144));
/// // Wrong length or non-hex digits are rejected.
/// assert!(parse_hex("#fff").is_err());
/// assert!(parse_hex("#gggggg").is_err());
/// ```
pub fn parse_hex(s: &str) -> Result<(u8, u8, u8), ThemeError> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return Err(ThemeError::BadHex(s.to_string()));
    }
    let r = u8::from_str_radix(&h[0..2], 16).map_err(|_| ThemeError::BadHex(s.to_string()))?;
    let g = u8::from_str_radix(&h[2..4], 16).map_err(|_| ThemeError::BadHex(s.to_string()))?;
    let b = u8::from_str_radix(&h[4..6], 16).map_err(|_| ThemeError::BadHex(s.to_string()))?;
    Ok((r, g, b))
}

impl Theme {
    pub fn from_toml(src: &str) -> Result<Theme, ThemeError> {
        let t: Theme = toml::from_str(src).map_err(|e| ThemeError::Parse(e.to_string()))?;
        t.validate()?;
        Ok(t)
    }

    pub fn load_from(path: &Path) -> Result<Theme, ThemeError> {
        let src =
            std::fs::read_to_string(path).map_err(|e| ThemeError::Io(format!("{path:?}: {e}")))?;
        Theme::from_toml(&src)
    }

    /// Confirm every color field is a valid hex triple.
    pub fn validate(&self) -> Result<(), ThemeError> {
        for c in [
            &self.background,
            &self.foreground,
            &self.cursor,
            &self.normal.black,
            &self.normal.red,
            &self.normal.green,
            &self.normal.yellow,
            &self.normal.blue,
            &self.normal.magenta,
            &self.normal.cyan,
            &self.normal.white,
            &self.bright.black,
            &self.bright.red,
            &self.bright.green,
            &self.bright.yellow,
            &self.bright.blue,
            &self.bright.magenta,
            &self.bright.cyan,
            &self.bright.white,
        ] {
            parse_hex(c)?;
        }
        // The optional slots default to an empty string ("theme omits this slot",
        // consumers fall back to a brand colour) — that empty case stays valid.
        // But an explicitly-SET bad hex here should be rejected, not silently
        // ignored, so the user gets feedback that the value was wrong.
        for c in [
            &self.cursor_text,
            &self.selection_background,
            &self.selection_foreground,
        ] {
            if !c.trim().is_empty() {
                parse_hex(c)?;
            }
        }
        Ok(())
    }

    /// Resolve a 256-colour palette index to an `(r,g,b)` triple.
    ///
    /// * `0..=15` — the theme's own `normal` (0-7) and `bright` (8-15) rows, so
    ///   a user theme always wins for the ANSI 16.
    /// * `16..=231` — the standard xterm 6×6×6 colour cube.
    /// * `232..=255` — the standard xterm 24-step greyscale ramp.
    ///
    /// The extended range is resolved through [`crate::term::palette`], the
    /// single source of truth shared with the OSC 4 query/reset baseline —
    /// there is deliberately no second copy of the cube/ramp table. Before this,
    /// `ansi` folded every index through `index % 8`, so `\e[38;5;208m`
    /// (orange) rendered as an unrelated ANSI slot.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::theme::{Theme, parse_hex};
    ///
    /// let t = Theme::builtin_void();
    /// // Index 0..8 read the `normal` row; 8..16 read the `bright` row.
    /// assert_eq!(t.ansi(0), parse_hex(&t.normal.black).unwrap());
    /// assert_eq!(t.ansi(9), parse_hex(&t.bright.red).unwrap());
    /// // 16.. come from the fixed xterm cube: 208 is the canonical orange.
    /// assert_eq!(t.ansi(208), (255, 135, 0));
    /// // …and 232.. from the greyscale ramp.
    /// assert_eq!(t.ansi(232), (8, 8, 8));
    /// ```
    pub fn ansi(&self, index: u8) -> (u8, u8, u8) {
        if let Some(rgb) = crate::term::palette::extended_entry(index) {
            return rgb;
        }
        // 0-15 only: the theme's two ANSI rows.
        let row = if index < 8 {
            &self.normal
        } else {
            &self.bright
        };
        let s = match index % 8 {
            0 => &row.black,
            1 => &row.red,
            2 => &row.green,
            3 => &row.yellow,
            4 => &row.blue,
            5 => &row.magenta,
            6 => &row.cyan,
            _ => &row.white,
        };
        parse_hex(s).unwrap_or((255, 255, 255))
    }

    /// Resolve a [`crate::grid::Color`] against this theme to a concrete
    /// `(r,g,b)` triple. `Color::Default` yields the supplied `default_rgb`
    /// (the caller's effective default fg or bg, which may itself be swapped
    /// under DECSCNM reverse-screen). This is the single color-resolution path
    /// shared by both the winit and egui renderers — neither re-derives it.
    pub fn resolve_color(
        &self,
        color: crate::grid::Color,
        default_rgb: (u8, u8, u8),
    ) -> (u8, u8, u8) {
        match color {
            crate::grid::Color::Default => default_rgb,
            crate::grid::Color::Indexed(i) => self.ansi(i),
            crate::grid::Color::Rgb(r, g, b) => (r, g, b),
        }
    }

    /// Resolve a cell's effective `(foreground, Option<background>)` RGB under
    /// the DEFAULT colour options ([`ColorOptions::default`], which reproduce
    /// Windows Terminal's defaults: bold-as-bright ON, contrast clamp OFF).
    ///
    /// The background is `None` when it should use the window default (so the
    /// renderer can skip painting a quad for the common case). For an inverse
    /// cell, the effective foreground is the cell's background and vice-versa —
    /// matching every mainstream terminal (selections, `\e[7m`, cursor-on-cell
    /// all rely on this). `default_fg` / `default_bg` are the effective defaults
    /// (already swapped under DECSCNM).
    ///
    /// Use [`Theme::cell_colors_with`] to supply non-default options.
    #[allow(clippy::type_complexity)]
    pub fn cell_colors(
        &self,
        cell: &crate::grid::Cell,
        default_fg: (u8, u8, u8),
        default_bg: (u8, u8, u8),
    ) -> ((u8, u8, u8), Option<(u8, u8, u8)>) {
        self.cell_colors_with(cell, default_fg, default_bg, &ColorOptions::default())
    }

    /// [`Theme::cell_colors`] with an explicit colour model.
    ///
    /// The pipeline, in order — the order is load-bearing:
    ///
    /// 1. **Bold-as-bright** — under [`IntenseTextStyle::Bright`] / `All`, a
    ///    bold cell whose foreground is *indexed 0-7* is remapped to its bright
    ///    twin 8-15. A 24-bit [`crate::grid::Color::Rgb`] foreground is NEVER
    ///    remapped, and neither is an already-bright or extended index.
    /// 2. **Resolve** indexed/default colours to RGB through this theme.
    /// 3. **Reverse video** — swap fg and bg for an inverse cell.
    /// 4. **Dim** (SGR 2) — blend the resulting foreground toward its effective
    ///    background by `options.dim_blend`.
    /// 5. **Conceal** (SGR 8) — collapse the foreground onto the background.
    /// 6. **Minimum contrast** — applied LAST, over the post-reverse-video
    ///    colours, so a selected/inverted cell is judged on what is actually
    ///    painted. Governed by `options.contrast_scope`; the default
    ///    [`ContrastScope::IndexedOnly`] leaves 24-bit foregrounds alone so
    ///    gradient TUIs survive, and the default threshold disables it outright.
    #[allow(clippy::type_complexity)]
    pub fn cell_colors_with(
        &self,
        cell: &crate::grid::Cell,
        default_fg: (u8, u8, u8),
        default_bg: (u8, u8, u8),
        options: &ColorOptions,
    ) -> ((u8, u8, u8), Option<(u8, u8, u8)>) {
        // (1) Bold-as-bright, on the INDEX, before any RGB resolution.
        let cell_fg = if cell.flags.bold && options.intense_text_style.remaps_to_bright() {
            brighten_indexed(cell.fg)
        } else {
            cell.fg
        };
        // (2) Resolve.
        let fg = self.resolve_color(cell_fg, default_fg);
        let bg = match cell.bg {
            crate::grid::Color::Default => None,
            other => Some(self.resolve_color(other, default_bg)),
        };
        // (3) Reverse video.
        let (mut eff_fg, eff_bg) = if cell.flags.inverse {
            (bg.unwrap_or(default_bg), Some(fg))
        } else {
            (fg, bg)
        };
        // The concrete colour actually behind the glyph, for the blend/clamp
        // stages: an absent bg means the window default is painted there.
        let painted_bg = eff_bg.unwrap_or(default_bg);
        // (4) Dim.
        if cell.flags.dim {
            eff_fg = dim_foreground(eff_fg, painted_bg, options.dim_blend);
        }
        // (5) Conceal — after dim (dimming an invisible glyph is a no-op) and
        // before the clamp, which must never "rescue" deliberately hidden text.
        if cell.flags.conceal {
            return (painted_bg, eff_bg);
        }
        // (6) Minimum contrast.
        if options.contrast_clamp_active()
            && contrast_clamp_applies(cell_fg, options.contrast_scope)
        {
            eff_fg = enforce_min_contrast(eff_fg, painted_bg, options.min_contrast_ratio);
        }
        (eff_fg, eff_bg)
    }

    /// Imports an iTerm2 `.itermcolors` plist XML document into a [`Theme`].
    ///
    /// Maps `Ansi 0..15 Color` into the [`AnsiRow`] normal (0-7) and bright
    /// (8-15) rows, and `Foreground`/`Background`/`Cursor Color` into the
    /// dynamic colors. Missing slots fall back to [`Theme::builtin_void`].
    /// Returns [`ThemeError::Parse`] if the document carries no recognisable
    /// color entries. `name` becomes the resulting theme's name.
    pub fn from_itermcolors(xml: &str, name: &str) -> Result<Theme, ThemeError> {
        itermcolors::from_itermcolors(xml, name)
    }

    /// A hard-coded fallback used only when no theme file can be loaded —
    /// keeps the terminal usable even if the themes dir is missing.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::theme::Theme;
    ///
    /// // The builtin is always self-consistent (every colour parses, etc.).
    /// let t = Theme::builtin_void();
    /// assert!(t.validate().is_ok());
    /// assert!(!t.name.is_empty());
    /// ```
    pub fn builtin_void() -> Theme {
        let row = |k: &str| k.to_string();
        // Itasha.Corp — the house brand default, shared by every Itasha.Corp
        // app. Brand primaries: electric purple #7700FF + spring green #00FF90.
        // Mirrors assets/themes/itasha-corp.toml so the hard-coded fallback
        // looks identical to the bundled default.
        Theme {
            name: "Itasha.Corp (builtin)".into(),
            author: "Itasha.Corp".into(),
            background: row("#121212"),
            foreground: row("#e8e6f0"),
            cursor: row("#00ff90"),
            cursor_text: row("#121212"),
            selection_background: row("#33106b"),
            selection_foreground: row("#e8e6f0"),
            normal: AnsiRow {
                black: "#1c1c1c".into(),
                red: "#ff3b5c".into(),
                green: "#00ff90".into(),
                yellow: "#ffc44d".into(),
                blue: "#7700ff".into(),
                magenta: "#b44dff".into(),
                cyan: "#00ffc8".into(),
                white: "#e8e6f0".into(),
            },
            bright: AnsiRow {
                // Bright-black is the conventional slot for DIMMED / secondary text —
                // git hashes, code comments, `ls` metadata, prompt segments. The former
                // `#4a4366` scored only 2.04:1 against the `#121212` background, well
                // under the WCAG AA 4.5:1 floor, which is a large part of the reported
                // "some text is hard to see". This value scores 4.73:1 while keeping the
                // theme's violet cast (B > R > G) rather than falling back to a neutral
                // grey. For reference, Windows Terminal's Campbell `#767676` reaches only
                // 4.12:1 against this darker background, so this clears WT too.
                // Pinned by `bright_black_meets_wcag_aa_against_background`.
                black: "#837b9f".into(),
                red: "#ff6f88".into(),
                green: "#5cffb4".into(),
                yellow: "#ffd57a".into(),
                blue: "#9a4dff".into(),
                magenta: "#cf8aff".into(),
                cyan: "#5cffda".into(),
                white: "#ffffff".into(),
            },
        }
    }

    /// Themes COMPILED INTO the binary, keyed by their config name (the file
    /// stem under `assets/themes/`). The terminal-theme loader resolves these so
    /// theme selection ALWAYS works regardless of the process's CWD or whether an
    /// `assets/themes/` directory ships next to the installed binary. The prior
    /// file-only loader silently fell back to `builtin_void` whenever the file
    /// could not be found (i.e. in every installed launch), which the user
    /// experienced as "the theme doesn't change".
    pub const EMBEDDED_THEMES: &'static [(&'static str, &'static str)] = &[
        (
            "itasha-corp",
            include_str!("../../../../assets/themes/itasha-corp.toml"),
        ),
        (
            "itasha-void",
            include_str!("../../../../assets/themes/itasha-void.toml"),
        ),
        (
            "itasha-void-high-contrast",
            include_str!("../../../../assets/themes/itasha-void-high-contrast.toml"),
        ),
        (
            "ghost-paper",
            include_str!("../../../../assets/themes/ghost-paper.toml"),
        ),
        (
            "wired-noir",
            include_str!("../../../../assets/themes/wired-noir.toml"),
        ),
        (
            "wired-colorblind",
            include_str!("../../../../assets/themes/wired-colorblind.toml"),
        ),
        // Ported from the SCR1B3 editor for a cohesive Itasha.Corp product
        // family (calm-canon line).
        (
            "phosphor-amber",
            include_str!("../../../../assets/themes/phosphor-amber.toml"),
        ),
        (
            "lain-mauve",
            include_str!("../../../../assets/themes/lain-mauve.toml"),
        ),
        (
            "a11y-high-contrast",
            include_str!("../../../../assets/themes/a11y-high-contrast.toml"),
        ),
        // itasha-neon family (brand-signature line).
        (
            "itasha-neon",
            include_str!("../../../../assets/themes/itasha-neon.toml"),
        ),
        (
            "itasha-neon-pastel",
            include_str!("../../../../assets/themes/itasha-neon-pastel.toml"),
        ),
        (
            "itasha-neon-soft",
            include_str!("../../../../assets/themes/itasha-neon-soft.toml"),
        ),
        (
            "itasha-neon-night",
            include_str!("../../../../assets/themes/itasha-neon-night.toml"),
        ),
        (
            "itasha-neon-dawn",
            include_str!("../../../../assets/themes/itasha-neon-dawn.toml"),
        ),
        (
            "itasha-neon-aurora",
            include_str!("../../../../assets/themes/itasha-neon-aurora.toml"),
        ),
        // Heritage-alt influence palettes.
        (
            "geocities-bbs",
            include_str!("../../../../assets/themes/geocities-bbs.toml"),
        ),
        (
            "lain-wired",
            include_str!("../../../../assets/themes/lain-wired.toml"),
        ),
        (
            "kusanagi-dive",
            include_str!("../../../../assets/themes/kusanagi-dive.toml"),
        ),
        (
            "akira-redshift",
            include_str!("../../../../assets/themes/akira-redshift.toml"),
        ),
        (
            "atompunk-sodium",
            include_str!("../../../../assets/themes/atompunk-sodium.toml"),
        ),
        (
            "terminal-lock",
            include_str!("../../../../assets/themes/terminal-lock.toml"),
        ),
        (
            "mecha-armour",
            include_str!("../../../../assets/themes/mecha-armour.toml"),
        ),
        (
            "shutoko-night",
            include_str!("../../../../assets/themes/shutoko-night.toml"),
        ),
        // Wave-4 line — ported from the SCR1B3 editor's map-schema themes,
        // translated onto C0PL4ND's ANSI-16 terminal schema for a cohesive
        // Itasha.Corp product family. `kanjo-loop` keeps candy-red ALARM-ONLY
        // (its voice is the lime underglow, diverging from SCR1B3's red-voice).
        (
            "dialup-glow",
            include_str!("../../../../assets/themes/dialup-glow.toml"),
        ),
        (
            "present-day",
            include_str!("../../../../assets/themes/present-day.toml"),
        ),
        (
            "thermoptic",
            include_str!("../../../../assets/themes/thermoptic.toml"),
        ),
        (
            "capsule-mono",
            include_str!("../../../../assets/themes/capsule-mono.toml"),
        ),
        (
            "jet-age",
            include_str!("../../../../assets/themes/jet-age.toml"),
        ),
        (
            "packet-trace",
            include_str!("../../../../assets/themes/packet-trace.toml"),
        ),
        (
            "cockpit-amber",
            include_str!("../../../../assets/themes/cockpit-amber.toml"),
        ),
        (
            "nerv-magi",
            include_str!("../../../../assets/themes/nerv-magi.toml"),
        ),
        (
            "colony-drift",
            include_str!("../../../../assets/themes/colony-drift.toml"),
        ),
        (
            "kanjo-loop",
            include_str!("../../../../assets/themes/kanjo-loop.toml"),
        ),
        (
            "yaksha-ink",
            include_str!("../../../../assets/themes/yaksha-ink.toml"),
        ),
        (
            "datamosh-haze",
            include_str!("../../../../assets/themes/datamosh-haze.toml"),
        ),
    ];

    /// Resolve a compiled-in theme by its config name. Returns `None` for an
    /// unknown name or a theme that fails to parse/validate. This is the
    /// CWD-independent resolution path that makes theme selection work in the
    /// installed app (see [`Theme::EMBEDDED_THEMES`]).
    pub fn builtin_named(name: &str) -> Option<Theme> {
        Self::EMBEDDED_THEMES
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, src)| Theme::from_toml(src).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bright-black (ANSI 8) is the conventional slot for DIMMED / secondary text —
    /// git hashes, code comments, `ls` metadata, prompt segments. It therefore has to
    /// stay legible against the window background, and it is the one palette slot
    /// where a "tasteful" dark value is indistinguishable from a bug.
    ///
    /// It regressed exactly that way: `#4a4366` scored **2.04:1**, less than half the
    /// WCAG AA 4.5:1 floor, and was a large part of the reported "some text is hard to
    /// see". This pins the fix so a future palette edit cannot quietly undo it.
    ///
    /// Deliberately asserted against the REAL WCAG formula rather than a hardcoded
    /// expected hex, so the test still means something if the colour is re-tuned.
    #[test]
    fn bright_black_meets_wcag_aa_against_background() {
        /// WCAG 2.2 AA contrast floor for normal-size body text.
        const WCAG_AA: f32 = 4.5;

        let theme = Theme::builtin_void();
        let bg = parse_hex(&theme.background).expect("background must parse");
        let dim = parse_hex(&theme.bright.black).expect("bright.black must parse");
        let ratio = color_model::contrast_ratio(dim, bg);

        assert!(
            ratio >= WCAG_AA,
            "bright.black {} on background {} is {ratio:.2}:1 — below the WCAG AA \
             floor of {WCAG_AA}:1. This slot carries dimmed/secondary text; a value \
             this dark makes git hashes, comments and `ls` metadata unreadable.",
            theme.bright.black,
            theme.background,
        );

        // Guard the other direction too: bright-black must stay RECESSED relative to
        // primary foreground, or "dim" text stops reading as dim and the tier
        // collapses. Raising contrast must not turn secondary text into body text.
        let fg = parse_hex(&theme.foreground).expect("foreground must parse");
        assert!(
            color_model::relative_luminance(dim) < color_model::relative_luminance(fg),
            "bright.black {} must remain dimmer than the primary foreground {}",
            theme.bright.black,
            theme.foreground,
        );
    }

    #[test]
    fn parse_hex_works() {
        assert_eq!(parse_hex("#08060d").unwrap(), (8, 6, 13));
        assert_eq!(parse_hex("00e5ff").unwrap(), (0, 229, 255));
        assert!(parse_hex("#12345").is_err());
        assert!(parse_hex("#zzzzzz").is_err());
    }

    /// The compiled-in theme set is the CWD-independent resolution path that
    /// fixes "the theme doesn't change" in the installed app: every advertised
    /// name must parse+validate, distinct themes must differ, unknown → None.
    #[test]
    fn embedded_themes_resolve_and_differ() {
        for (name, _) in Theme::EMBEDDED_THEMES {
            assert!(
                Theme::builtin_named(name).is_some(),
                "embedded theme {name:?} must parse + validate"
            );
        }
        let noir = Theme::builtin_named("wired-noir").expect("wired-noir embedded");
        let paper = Theme::builtin_named("ghost-paper").expect("ghost-paper embedded");
        assert_ne!(
            noir.background, paper.background,
            "distinct embedded themes must have distinct backgrounds"
        );
        assert!(Theme::builtin_named("no-such-theme").is_none());
    }

    /// The 12 SCR1B3 Wave-4 themes ported onto C0PL4ND's ANSI-16 schema (M8). The
    /// catalog grows 23 → 35; each new name must parse+validate (so every ANSI
    /// index resolves to a real hex, not the white fallback), and its `normal` and
    /// `bright` rows must differ (a genuine two-intensity terminal palette, not a
    /// degenerate single row).
    #[test]
    fn wave4_ported_themes_resolve_are_full_and_two_intensity() {
        const WAVE4: &[&str] = &[
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
        for name in WAVE4 {
            let t = Theme::builtin_named(name)
                .unwrap_or_else(|| panic!("Wave-4 theme {name:?} must parse + validate"));
            // Every ANSI index 0..16 resolves to the theme's own hex (validate()
            // already proved each slot parses, so none hits the (255,255,255)
            // bad-hex fallback): assert ansi(i) equals the parsed slot value.
            for i in 0u8..16 {
                let row = if i < 8 { &t.normal } else { &t.bright };
                let slot = match i % 8 {
                    0 => &row.black,
                    1 => &row.red,
                    2 => &row.green,
                    3 => &row.yellow,
                    4 => &row.blue,
                    5 => &row.magenta,
                    6 => &row.cyan,
                    _ => &row.white,
                };
                assert_eq!(
                    t.ansi(i),
                    parse_hex(slot).unwrap(),
                    "{name}: ansi({i}) must resolve to its own palette slot"
                );
            }
            // A real two-intensity palette: the bright row is not a copy of normal.
            assert_ne!(
                t.normal, t.bright,
                "{name}: bright row must differ from normal (two-intensity palette)"
            );
        }
        // The catalog grew from 23 to 35 with the Wave-4 line.
        assert_eq!(
            Theme::EMBEDDED_THEMES.len(),
            35,
            "the embedded catalog must be 35 themes after the Wave-4 port"
        );
    }

    #[test]
    fn builtin_theme_is_valid() {
        let t = Theme::builtin_void();
        assert!(t.validate().is_ok());
        assert_eq!(t.ansi(6), (0x00, 0xff, 0xc8)); // cyan = brand mint
        assert_eq!(t.ansi(4), (0x77, 0x00, 0xff)); // blue = brand purple #7700FF
        assert_eq!(t.ansi(2), (0x00, 0xff, 0x90)); // green = brand green #00FF90
    }

    #[test]
    fn validate_checks_optional_slots_when_set_but_allows_empty() {
        // Empty optional slots (the "theme omits this slot" default) stay valid.
        let t = Theme::builtin_void();
        assert!(t.validate().is_ok());

        // An explicitly-set BAD hex in an optional slot is now rejected (was
        // silently ignored — the user got no feedback the value was wrong).
        let mut bad = Theme::builtin_void();
        bad.selection_background = "not-a-color".to_string();
        assert!(matches!(bad.validate(), Err(ThemeError::BadHex(_))));

        // A valid hex in an optional slot passes.
        let mut good = Theme::builtin_void();
        good.cursor_text = "#abcdef".to_string();
        good.selection_foreground = "#012345".to_string();
        assert!(good.validate().is_ok());
    }

    // --- additional edge-coverage -----------------------------------------

    const MINIMAL_TOML: &str = r##"
name = "mini"
background = "#000000"
foreground = "#ffffff"
cursor = "#00ff00"
[normal]
black = "#101010"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
blue = "#0000ff"
magenta = "#ff00ff"
cyan = "#00ffff"
white = "#cccccc"
[bright]
black = "#202020"
red = "#ff4040"
green = "#40ff40"
yellow = "#ffff40"
blue = "#4040ff"
magenta = "#ff40ff"
cyan = "#40ffff"
white = "#ffffff"
"##;

    #[test]
    fn from_toml_parses_and_defaults_optional_fields() {
        let t = Theme::from_toml(MINIMAL_TOML).expect("parse minimal theme");
        assert_eq!(t.name, "mini");
        // Omitted optional fields default to empty strings.
        assert_eq!(t.author, "");
        assert_eq!(t.cursor_text, "");
        assert_eq!(t.selection_background, "");
        assert_eq!(t.selection_foreground, "");
        assert_eq!(t.normal.red, "#ff0000");
        assert_eq!(t.bright.white, "#ffffff");
    }

    #[test]
    fn from_toml_rejects_malformed_toml() {
        let err = Theme::from_toml("this is = = not valid toml [[[").unwrap_err();
        assert!(matches!(err, ThemeError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn from_toml_rejects_bad_hex_via_validate() {
        // Parses as TOML but a color is not a hex triple → BadHex via validate().
        let bad = MINIMAL_TOML.replace("#ff0000", "not-a-hex");
        let err = Theme::from_toml(&bad).unwrap_err();
        assert!(matches!(err, ThemeError::BadHex(_)), "got {err:?}");
    }

    #[test]
    fn load_from_reads_disk_and_round_trips() {
        let tmp = std::env::temp_dir().join(format!("c0pl4nd-theme-{}.toml", std::process::id()));
        std::fs::write(&tmp, MINIMAL_TOML).unwrap();
        let t = Theme::load_from(&tmp).expect("load_from disk");
        assert_eq!(t.name, "mini");
        assert_eq!(t.ansi(1), (0xff, 0, 0)); // normal red
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_from_missing_file_is_io_error() {
        let missing = std::env::temp_dir().join("c0pl4nd-theme-absent-xyzzy.toml");
        let _ = std::fs::remove_file(&missing);
        let err = Theme::load_from(&missing).unwrap_err();
        assert!(matches!(err, ThemeError::Io(_)), "got {err:?}");
    }

    #[test]
    fn theme_error_display_variants() {
        assert!(ThemeError::Io("x".into())
            .to_string()
            .contains("could not read theme"));
        assert!(ThemeError::Parse("y".into())
            .to_string()
            .contains("theme parse error"));
        assert!(ThemeError::BadHex("zz".into())
            .to_string()
            .contains("invalid hex color"));
    }

    #[test]
    fn ansi_resolves_every_index_to_the_right_slot() {
        let t = Theme::builtin_void();
        // normal row (0-7) maps black..white in order.
        assert_eq!(t.ansi(0), parse_hex(&t.normal.black).unwrap());
        assert_eq!(t.ansi(1), parse_hex(&t.normal.red).unwrap());
        assert_eq!(t.ansi(2), parse_hex(&t.normal.green).unwrap());
        assert_eq!(t.ansi(3), parse_hex(&t.normal.yellow).unwrap());
        assert_eq!(t.ansi(4), parse_hex(&t.normal.blue).unwrap());
        assert_eq!(t.ansi(5), parse_hex(&t.normal.magenta).unwrap());
        assert_eq!(t.ansi(6), parse_hex(&t.normal.cyan).unwrap());
        assert_eq!(t.ansi(7), parse_hex(&t.normal.white).unwrap());
        // bright row (8-15) maps the bright slots.
        assert_eq!(t.ansi(8), parse_hex(&t.bright.black).unwrap());
        assert_eq!(t.ansi(9), parse_hex(&t.bright.red).unwrap());
        assert_eq!(t.ansi(10), parse_hex(&t.bright.green).unwrap());
        assert_eq!(t.ansi(11), parse_hex(&t.bright.yellow).unwrap());
        assert_eq!(t.ansi(12), parse_hex(&t.bright.blue).unwrap());
        assert_eq!(t.ansi(13), parse_hex(&t.bright.magenta).unwrap());
        assert_eq!(t.ansi(14), parse_hex(&t.bright.cyan).unwrap());
        assert_eq!(t.ansi(15), parse_hex(&t.bright.white).unwrap());
    }

    /// The regression that motivated the 256-colour `ansi()`: index 16+ used to
    /// fold through `index % 8` into an unrelated ANSI slot, so every 256-colour
    /// program (`ls --color`, bat, delta, fzf, powerlevel10k) rendered wrong.
    /// These are EXACT expected cube/ramp values, not "not black" smoke checks.
    #[test]
    fn ansi_resolves_the_full_256_cube_and_ramp() {
        let t = Theme::builtin_void();

        // --- cube boundaries (16..=231) ---
        // 16 is the cube origin: r=g=b=level[0] → pure black.
        assert_eq!(t.ansi(16), (0, 0, 0));
        // 231 is the cube terminus: r=g=b=level[5] → pure white.
        assert_eq!(t.ansi(231), (255, 255, 255));
        // Blue varies fastest: 16+5 = 21 → (0, 0, 255).
        assert_eq!(t.ansi(21), (0, 0, 255));
        // Then green (stride 6): 16+6 = 22 → (0, 95, 0).
        assert_eq!(t.ansi(22), (0, 95, 0));
        // Then red (stride 36): 16+36 = 52 → (95, 0, 0).
        assert_eq!(t.ansi(52), (95, 0, 0));
        // The headline case from the bug report: 208 is xterm orange.
        // i = 208-16 = 192 → r=level[192/36 % 6 = 5]=255,
        // g=level[192/6 % 6 = 2]=135, b=level[192 % 6 = 0]=0.
        assert_eq!(t.ansi(208), (255, 135, 0));
        // …and it is emphatically NOT the old `% 8` answer.
        assert_ne!(t.ansi(208), parse_hex(&t.bright.black).unwrap());

        // --- greyscale ramp boundaries (232..=255) ---
        assert_eq!(t.ansi(232), (8, 8, 8), "ramp starts at 8, not 0");
        assert_eq!(t.ansi(255), (238, 238, 238), "ramp ends at 238, not 255");
        assert_eq!(t.ansi(233), (18, 18, 18), "ramp step is exactly 10");
        assert_eq!(t.ansi(254), (228, 228, 228));

        // The extended range is theme-INDEPENDENT: it is the fixed xterm table,
        // so two different themes agree on every index >= 16.
        let other = Theme::builtin_named("ghost-paper").expect("ghost-paper embedded");
        for i in 16u8..=255 {
            assert_eq!(
                t.ansi(i),
                other.ansi(i),
                "index {i} must be theme-independent"
            );
        }
        // …while 0-15 still follow the theme (the user's palette wins).
        assert_eq!(t.ansi(1), parse_hex(&t.normal.red).unwrap());
        assert_eq!(t.ansi(9), parse_hex(&t.bright.red).unwrap());
    }

    /// `ansi()` must agree exactly with the OSC-4 default palette on the
    /// extended range — proving the render path and the query path share ONE
    /// table rather than two copies that can drift.
    #[test]
    fn ansi_extended_range_matches_the_osc_default_palette() {
        let t = Theme::builtin_void();
        let osc = crate::term::palette::build_default_palette();
        for i in 16u8..=255 {
            assert_eq!(
                t.ansi(i),
                osc[i as usize],
                "index {i}: render path and OSC-4 baseline disagree"
            );
        }
    }

    #[test]
    fn ansi_falls_back_to_white_on_bad_hex() {
        // A theme with a non-hex ANSI slot resolves that index to the
        // (255,255,255) fallback rather than panicking.
        let mut t = Theme::builtin_void();
        t.normal.red = "garbage".into();
        assert_eq!(t.ansi(1), (255, 255, 255), "bad hex → white fallback");
    }

    #[test]
    fn resolve_color_handles_all_three_variants() {
        use crate::grid::Color;
        let t = Theme::builtin_void();
        let dflt = (1, 2, 3);
        // Default → the supplied default rgb.
        assert_eq!(t.resolve_color(Color::Default, dflt), dflt);
        // Indexed → the ANSI table.
        assert_eq!(t.resolve_color(Color::Indexed(2), dflt), t.ansi(2));
        // Rgb → passed through verbatim.
        assert_eq!(t.resolve_color(Color::Rgb(9, 8, 7), dflt), (9, 8, 7));
    }

    #[test]
    fn cell_colors_default_bg_yields_none() {
        use crate::grid::{Cell, Color};
        let t = Theme::builtin_void();
        let cell = Cell {
            fg: Color::Rgb(10, 20, 30),
            bg: Color::Default,
            ..Cell::default()
        };
        let (fg, bg) = t.cell_colors(&cell, (0, 0, 0), (99, 99, 99));
        assert_eq!(fg, (10, 20, 30));
        assert_eq!(bg, None, "Default bg → None so the renderer skips a quad");
    }

    #[test]
    fn cell_colors_explicit_bg_is_some() {
        use crate::grid::{Cell, Color};
        let t = Theme::builtin_void();
        let cell = Cell {
            fg: Color::Rgb(1, 2, 3),
            bg: Color::Rgb(4, 5, 6),
            ..Cell::default()
        };
        let (fg, bg) = t.cell_colors(&cell, (0, 0, 0), (0, 0, 0));
        assert_eq!(fg, (1, 2, 3));
        assert_eq!(bg, Some((4, 5, 6)));
    }

    #[test]
    fn cell_colors_inverse_swaps_fg_and_bg() {
        use crate::grid::{Cell, CellFlags, Color};
        let t = Theme::builtin_void();
        // Inverse cell with explicit bg: effective fg becomes the bg, effective
        // bg becomes the fg.
        let cell = Cell {
            fg: Color::Rgb(11, 22, 33),
            bg: Color::Rgb(44, 55, 66),
            flags: CellFlags {
                inverse: true,
                ..CellFlags::empty()
            },
            ..Cell::default()
        };
        let (fg, bg) = t.cell_colors(&cell, (0, 0, 0), (7, 7, 7));
        assert_eq!(fg, (44, 55, 66), "inverse fg = the cell's bg");
        assert_eq!(bg, Some((11, 22, 33)), "inverse bg = the cell's fg");
    }

    #[test]
    fn cell_colors_inverse_with_default_bg_uses_default_bg_as_fg() {
        use crate::grid::{Cell, CellFlags, Color};
        let t = Theme::builtin_void();
        // Inverse cell whose bg is Default: the effective foreground falls back to
        // default_bg (the `bg.unwrap_or(default_bg)` branch).
        let cell = Cell {
            fg: Color::Rgb(200, 100, 50),
            bg: Color::Default,
            flags: CellFlags {
                inverse: true,
                ..CellFlags::empty()
            },
            ..Cell::default()
        };
        let (fg, bg) = t.cell_colors(&cell, (1, 1, 1), (9, 8, 7));
        assert_eq!(fg, (9, 8, 7), "inverse fg falls back to default_bg");
        assert_eq!(bg, Some((200, 100, 50)), "inverse bg = the cell's fg");
    }

    // --- colour model: bold-as-bright ------------------------------------

    fn bold_cell(fg: Color) -> Cell {
        Cell {
            fg,
            bg: Color::Default,
            flags: CellFlags {
                bold: true,
                ..CellFlags::empty()
            },
            ..Cell::default()
        }
    }

    use crate::grid::{Cell, CellFlags, Color};

    /// The single largest visible difference vs Windows Terminal: WT defaults
    /// `intenseTextStyle` to `bright`, so bold + indexed 0-7 renders from the
    /// BRIGHT row. Pinned per-index, so a mutant that changes the `+ 8` or the
    /// `< 8` guard is caught.
    #[test]
    fn bold_remaps_indexed_0_to_7_into_the_bright_row() {
        let t = Theme::builtin_void();
        for i in 0u8..8 {
            let (fg, _) = t.cell_colors(&bold_cell(Color::Indexed(i)), (0, 0, 0), (0, 0, 0));
            assert_eq!(
                fg,
                t.ansi(i + 8),
                "bold + indexed {i} must render as bright slot {}",
                i + 8
            );
            assert_ne!(
                fg,
                t.ansi(i),
                "bold + indexed {i} must NOT stay on the normal row"
            );
        }
        // The headline case: bold + indexed 3 (yellow) → slot 11 (bright yellow).
        let (fg, _) = t.cell_colors(&bold_cell(Color::Indexed(3)), (0, 0, 0), (0, 0, 0));
        assert_eq!(fg, parse_hex(&t.bright.yellow).unwrap());
    }

    /// The other half of the rule, and the one that protects gradient output:
    /// bold must NEVER touch a 24-bit colour, an already-bright index, or an
    /// extended (16-255) index.
    #[test]
    fn bold_never_remaps_rgb_bright_or_extended_colors() {
        let t = Theme::builtin_void();
        // 24-bit RGB passes through byte-identical.
        let (fg, _) = t.cell_colors(&bold_cell(Color::Rgb(200, 100, 50)), (0, 0, 0), (0, 0, 0));
        assert_eq!(fg, (200, 100, 50), "bold must not rewrite a 24-bit colour");
        // Already-bright indices 8-15 stay put (no wrap into the cube).
        for i in 8u8..16 {
            let (fg, _) = t.cell_colors(&bold_cell(Color::Indexed(i)), (0, 0, 0), (0, 0, 0));
            assert_eq!(fg, t.ansi(i), "bold + already-bright {i} must be unchanged");
        }
        // Extended indices are untouched (208 must stay orange, not become 216).
        let (fg, _) = t.cell_colors(&bold_cell(Color::Indexed(208)), (0, 0, 0), (0, 0, 0));
        assert_eq!(fg, (255, 135, 0));
        // A Default foreground is untouched.
        let (fg, _) = t.cell_colors(&bold_cell(Color::Default), (11, 22, 33), (0, 0, 0));
        assert_eq!(fg, (11, 22, 33));
    }

    /// The remap is a POLICY, not a hard-coded behaviour: each
    /// `IntenseTextStyle` must produce its documented result.
    #[test]
    fn intense_text_style_governs_the_remap() {
        let t = Theme::builtin_void();
        let cell = bold_cell(Color::Indexed(1));
        let with = |s: IntenseTextStyle| {
            let opts = ColorOptions {
                intense_text_style: s,
                ..ColorOptions::default()
            };
            t.cell_colors_with(&cell, (0, 0, 0), (0, 0, 0), &opts).0
        };
        assert_eq!(with(IntenseTextStyle::Bright), t.ansi(9));
        assert_eq!(with(IntenseTextStyle::All), t.ansi(9));
        assert_eq!(with(IntenseTextStyle::Bold), t.ansi(1), "Bold = face only");
        assert_eq!(with(IntenseTextStyle::None), t.ansi(1), "None = no remap");
    }

    /// A NON-bold cell must never be brightened, whatever the style.
    #[test]
    fn non_bold_cells_are_never_brightened() {
        let t = Theme::builtin_void();
        let cell = Cell {
            fg: Color::Indexed(2),
            ..Cell::default()
        };
        for s in [
            IntenseTextStyle::Bright,
            IntenseTextStyle::Bold,
            IntenseTextStyle::All,
            IntenseTextStyle::None,
        ] {
            let opts = ColorOptions {
                intense_text_style: s,
                ..ColorOptions::default()
            };
            assert_eq!(
                t.cell_colors_with(&cell, (0, 0, 0), (0, 0, 0), &opts).0,
                t.ansi(2)
            );
        }
    }

    // --- colour model: SGR 39 / 49 ----------------------------------------

    /// SGR 39/49 reset to the THEME defaults, not to palette slots 7/0 — the
    /// classic bug where "default foreground" silently becomes ANSI white.
    #[test]
    fn default_fg_and_bg_resolve_to_theme_defaults_not_palette_slots() {
        // MINIMAL_TOML deliberately gives foreground (#ffffff) / background
        // (#000000) values DISTINCT from ANSI slots 7 (#cccccc) / 0 (#101010),
        // so the assertions below can actually fail if the mapping regresses.
        // (`builtin_void` sets foreground == normal.white, which would make the
        // non-vacuity guard at the end of this test trivially true.)
        let t = Theme::from_toml(MINIMAL_TOML).expect("parse minimal theme");
        let theme_fg = parse_hex(&t.foreground).unwrap();
        let theme_bg = parse_hex(&t.background).unwrap();
        let cell = Cell::default(); // what SGR 39 + 49 leaves behind
        let (fg, bg) = t.cell_colors(&cell, theme_fg, theme_bg);
        assert_eq!(fg, theme_fg, "SGR 39 must yield the theme foreground");
        assert_eq!(bg, None, "SGR 49 must yield the window default, not a quad");
        // And the theme default is genuinely distinct from slots 7 / 0 here, so
        // the assertion above could actually fail if the mapping regressed.
        assert_ne!(theme_fg, t.ansi(7));
        assert_ne!(theme_bg, t.ansi(0));
    }

    // --- colour model: dim / conceal ---------------------------------------

    #[test]
    fn dim_blends_the_foreground_toward_the_painted_background() {
        let t = Theme::builtin_void();
        let cell = Cell {
            fg: Color::Rgb(255, 255, 255),
            bg: Color::Rgb(0, 0, 0),
            flags: CellFlags {
                dim: true,
                ..CellFlags::empty()
            },
            ..Cell::default()
        };
        let (fg, bg) = t.cell_colors(&cell, (0, 0, 0), (0, 0, 0));
        assert_eq!(fg, (128, 128, 128), "dim = halfway to the background");
        assert_eq!(bg, Some((0, 0, 0)), "dim must not touch the background");
        // With a Default background, the blend target is the window default.
        let cell = Cell {
            fg: Color::Rgb(255, 255, 255),
            bg: Color::Default,
            flags: CellFlags {
                dim: true,
                ..CellFlags::empty()
            },
            ..Cell::default()
        };
        let (fg, _) = t.cell_colors(&cell, (0, 0, 0), (0, 0, 0));
        assert_eq!(fg, (128, 128, 128));
    }

    #[test]
    fn conceal_paints_the_glyph_in_the_background_color() {
        let t = Theme::builtin_void();
        let cell = Cell {
            fg: Color::Rgb(255, 0, 0),
            bg: Color::Rgb(20, 30, 40),
            flags: CellFlags {
                conceal: true,
                ..CellFlags::empty()
            },
            ..Cell::default()
        };
        let (fg, bg) = t.cell_colors(&cell, (0, 0, 0), (9, 9, 9));
        assert_eq!(fg, (20, 30, 40), "concealed fg == its own background");
        assert_eq!(bg, Some((20, 30, 40)));
        // Even with the clamp switched ON, concealed text stays concealed —
        // the clamp must never "rescue" deliberately hidden text.
        let opts = ColorOptions {
            min_contrast_ratio: 7.0,
            contrast_scope: ContrastScope::Always,
            ..ColorOptions::default()
        };
        let (fg, _) = t.cell_colors_with(&cell, (0, 0, 0), (9, 9, 9), &opts);
        assert_eq!(fg, (20, 30, 40), "the clamp must not un-conceal");
    }

    // --- colour model: minimum contrast ------------------------------------

    #[test]
    fn contrast_clamp_is_off_by_default() {
        let t = Theme::builtin_void();
        // Near-invisible dark grey on black: untouched under the defaults.
        let cell = Cell {
            fg: Color::Indexed(0),
            bg: Color::Rgb(0, 0, 0),
            ..Cell::default()
        };
        let (fg, _) = t.cell_colors(&cell, (0, 0, 0), (0, 0, 0));
        assert_eq!(fg, t.ansi(0), "the default build must not rewrite colours");
    }

    #[test]
    fn contrast_clamp_lifts_indexed_but_spares_rgb_by_default() {
        let t = Theme::builtin_void();
        let opts = ColorOptions {
            min_contrast_ratio: 7.0,
            ..ColorOptions::default() // scope: IndexedOnly
        };
        // Indexed near-black on black IS lifted to meet the target.
        let indexed = Cell {
            fg: Color::Indexed(0),
            bg: Color::Rgb(0, 0, 0),
            ..Cell::default()
        };
        let (fg, _) = t.cell_colors_with(&indexed, (0, 0, 0), (0, 0, 0), &opts);
        assert!(
            contrast_ratio(fg, (0, 0, 0)) >= 7.0,
            "indexed fg must be lifted"
        );
        assert_ne!(fg, t.ansi(0));

        // The SAME illegible colour as 24-bit RGB is left alone — this is what
        // keeps gradient TUIs intact.
        let rgb = Cell {
            fg: Color::Rgb(28, 28, 28),
            bg: Color::Rgb(0, 0, 0),
            ..Cell::default()
        };
        let (fg, _) = t.cell_colors_with(&rgb, (0, 0, 0), (0, 0, 0), &opts);
        assert_eq!(
            fg,
            (28, 28, 28),
            "IndexedOnly must spare 24-bit foregrounds"
        );

        // …until the operator opts into ContrastScope::Always.
        let all = ColorOptions {
            contrast_scope: ContrastScope::Always,
            ..opts
        };
        let (fg, _) = t.cell_colors_with(&rgb, (0, 0, 0), (0, 0, 0), &all);
        assert!(contrast_ratio(fg, (0, 0, 0)) >= 7.0);
    }

    /// The clamp runs AFTER reverse video, so it must judge the colours that are
    /// actually painted — not the pre-swap pair.
    #[test]
    fn contrast_clamp_runs_after_reverse_video() {
        let t = Theme::builtin_void();
        let opts = ColorOptions {
            min_contrast_ratio: 7.0,
            contrast_scope: ContrastScope::Always,
            ..ColorOptions::default()
        };
        // Pre-swap this pair is legible (white on near-black). After the swap
        // the painted pair is near-black text on white — still legible — so the
        // clamp must leave it alone rather than "fixing" the wrong pair.
        let cell = Cell {
            fg: Color::Rgb(255, 255, 255),
            bg: Color::Rgb(10, 10, 10),
            flags: CellFlags {
                inverse: true,
                ..CellFlags::empty()
            },
            ..Cell::default()
        };
        let (fg, bg) = t.cell_colors_with(&cell, (0, 0, 0), (0, 0, 0), &opts);
        assert_eq!(fg, (10, 10, 10), "post-swap pair already passes");
        assert_eq!(bg, Some((255, 255, 255)));

        // Now an inverse cell whose PAINTED pair is illegible: near-black text
        // on a near-black background. The clamp must fire on the swapped pair.
        let bad = Cell {
            fg: Color::Rgb(2, 2, 2),
            bg: Color::Rgb(12, 12, 12),
            flags: CellFlags {
                inverse: true,
                ..CellFlags::empty()
            },
            ..Cell::default()
        };
        let (fg, bg) = t.cell_colors_with(&bad, (0, 0, 0), (0, 0, 0), &opts);
        assert_eq!(bg, Some((2, 2, 2)), "the background is never clamped");
        assert!(
            contrast_ratio(fg, (2, 2, 2)) >= 7.0,
            "clamp judged the painted pair"
        );
    }

    /// Cells store a TAGGED colour, never pre-resolved RGB — so switching the
    /// theme re-resolves the same cell to the new palette.
    #[test]
    fn cells_store_tagged_colors_so_theme_switching_re_resolves() {
        let a = Theme::builtin_void();
        let b = Theme::builtin_named("ghost-paper").expect("ghost-paper embedded");
        let cell = Cell {
            fg: Color::Indexed(1),
            ..Cell::default()
        };
        // The SAME cell resolves differently under two themes.
        assert_ne!(
            a.cell_colors(&cell, (0, 0, 0), (0, 0, 0)).0,
            b.cell_colors(&cell, (0, 0, 0), (0, 0, 0)).0,
            "an indexed cell must re-resolve when the theme changes"
        );
        // An RGB cell is theme-independent by construction.
        let rgb = Cell {
            fg: Color::Rgb(1, 2, 3),
            ..Cell::default()
        };
        assert_eq!(
            a.cell_colors(&rgb, (0, 0, 0), (0, 0, 0)).0,
            b.cell_colors(&rgb, (0, 0, 0), (0, 0, 0)).0
        );
    }

    #[test]
    fn from_itermcolors_delegates_to_importer() {
        // Smoke the public Theme::from_itermcolors delegation path (the importer
        // itself is unit-tested in the submodule).
        let xml = r#"<plist><dict>
            <key>Ansi 1 Color</key>
            <dict>
                <key>Red Component</key><real>1.0</real>
                <key>Green Component</key><real>0.0</real>
                <key>Blue Component</key><real>0.0</real>
            </dict>
        </dict></plist>"#;
        let t = Theme::from_itermcolors(xml, "Imported").expect("import");
        assert_eq!(t.name, "Imported");
        assert_eq!(t.normal.red, "#ff0000");
    }
}
