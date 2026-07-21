//! Theme-derived egui chrome styling. [`visuals_from_theme`] and
//! [`ChromeColors`] derive the egui `Visuals` + the chrome surface palette
//! (titlebar / tab strip / status bar / settings window / panel fills) FROM the
//! active terminal [`c0pl4nd_core::Theme`], so the WHOLE app UI follows the
//! selected theme: a LIGHT theme (e.g. `ghost-paper`) produces a light egui
//! base, a DARK theme a dark base (chosen from the theme background's luminance
//! via [`is_light`]). The terminal grid's glyph colours still come from the same
//! `Theme`'s ANSI map (Milestone 2).
//!
//! The two-tone C0PL4ND wordmark tints BOTH tones from the theme (bright,
//! readability-guaranteed via [`ensure_readable_tone`], hue-distant so they
//! contrast); everything else (surfaces, text, hover/press/selection accents)
//! is likewise derived from the theme. The [`brand`] module exposes the Itasha
//! purple/`.Corp` green pair as the wordmark FALLBACKS plus `BG`/`FG` fallbacks
//! used when a minimal theme omits the optional slots.

use egui::{Color32, CornerRadius, Stroke, Visuals};

/// Perceptual luminance (sRGB Rec.601 weights, 0.0..=1.0) of an egui colour.
/// Used to pick a LIGHT vs DARK egui base for a theme and to derive sensible
/// shaded panel/widget fills regardless of the theme's polarity.
pub fn luminance(c: Color32) -> f32 {
    let [r, g, b, _] = c.to_array();
    (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0
}

/// True when `c` reads as a LIGHT colour (luminance > 0.5). The whole-app
/// theming pivot: a light theme background produces a light egui base, a dark
/// one a dark base.
pub fn is_light(c: Color32) -> bool {
    luminance(c) > 0.5
}

/// WCAG 2.x **relative luminance** (0.0..=1.0) of an opaque colour — the
/// gamma-linearised, differently-weighted sibling of [`luminance`] (which is the
/// cheap Rec.601 approximation used for the light/dark polarity pivot). The two
/// are deliberately separate: polarity only needs a rough split, whereas a
/// contrast RATIO that claims WCAG conformance must use the WCAG formula.
pub fn relative_luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let s = f32::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b, _] = c.to_array();
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG contrast ratio between two opaque colours — `1.0` (identical) up to
/// `21.0` (black on white). WCAG 2.4.11 / 2.4.13 require **>= 3:1** for a focus
/// indicator against BOTH the focused control and its surroundings, which is the
/// floor [`focus_ring_color`] is built to guarantee.
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// The Windows-standard destructive close-red (`#E81123`) — the hover fill of
/// the caption ✕ and the tab ×. Lives here (next to [`focus_ring_color`], which
/// must contrast against it) so the ring's guarantee is computed against the real
/// value rather than a copy that could drift.
pub const CLOSE_RED: Color32 = Color32::from_rgb(0xE8, 0x11, 0x23);

/// The pressed shade of [`CLOSE_RED`] — visibly darker so a held ✕ is never the
/// same pixel as a merely-hovered one.
pub const CLOSE_RED_PRESSED: Color32 = Color32::from_rgb(0xA3, 0x0C, 0x18);

/// The keyboard-focus ring colour for the flat chrome buttons.
///
/// egui 0.34 has **no** dedicated focus-ring style (there is no `focus_stroke`
/// field); [`egui::Style::interact`] merely returns the *active* visuals for a
/// focused widget, so a keyboard-focused chrome button otherwise looks
/// permanently PRESSED and is indistinguishable from hover. The chrome therefore
/// paints its own ring, and this picks its colour.
///
/// The ring must clear the WCAG 2.4.11/2.4.13 **3:1** floor against every surface
/// it can land on: the titlebar `panel`, the window `bg`, and the ✕'s
/// [`CLOSE_RED`] hover fill. The theme `accent` is preferred (brand-consistent)
/// and only falls back to the higher-contrast monochrome pole when it does not
/// clear the floor against all three — and one of white/black always does, so the
/// guarantee holds for ANY theme.
pub fn focus_ring_color(colors: ChromeColors) -> Color32 {
    let against = [colors.panel, colors.bg, CLOSE_RED];
    let worst = |c: Color32| {
        against
            .iter()
            .map(|s| contrast_ratio(c, *s))
            .fold(f32::INFINITY, f32::min)
    };
    let accent_score = worst(colors.accent);
    if accent_score >= FOCUS_RING_MIN_CONTRAST {
        return colors.accent;
    }
    let (white, black) = (worst(Color32::WHITE), worst(Color32::BLACK));
    if white >= black {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

/// The WCAG 2.4.11 non-text contrast floor a focus indicator must clear.
pub const FOCUS_RING_MIN_CONTRAST: f32 = 3.0;

/// Parse a `c0pl4nd_core::Theme` `#rrggbb` field into an egui `Color32`, falling
/// back to `fallback` when the field is empty or unparseable (e.g. the optional
/// `selection_background` slot a minimal theme omits).
fn theme_color(hex: &str, fallback: Color32) -> Color32 {
    match c0pl4nd_core::theme::parse_hex(hex) {
        Ok((r, g, b)) => Color32::from_rgb(r, g, b),
        Err(_) => fallback,
    }
}

/// Shade `base` toward white (dark themes) or toward black (light themes) by
/// `amount` (0.0..=1.0), so panels/widgets read as a subtly-raised surface above
/// the window background in EITHER polarity. On a dark base this lightens; on a
/// light base it darkens — the conventional "elevated surface" cue.
fn shade(base: Color32, amount: f32) -> Color32 {
    let toward = if is_light(base) {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    base.lerp_to_gamma(toward, amount)
}

/// Nudge `tone` until it reads clearly and BRIGHTLY against `bg`: if the
/// luminance gap to `bg` is below a legibility floor, lerp the tone toward the
/// readable pole (white on a dark surface, black on a light one) just far enough
/// to clear the floor. A tone that already contrasts is returned untouched, so a
/// vivid theme colour keeps its chroma; only a muddy/too-dark (or, on a light
/// theme, too-pale) tone is lifted off the background. This is what keeps the
/// wordmark from rendering as a near-background, hard-to-read blob — the failure
/// mode of a fixed dark purple sitting on the dark void surface.
fn ensure_readable_tone(tone: Color32, bg: Color32) -> Color32 {
    const MIN_GAP: f32 = 0.34;
    let bg_l = luminance(bg);
    let toward = if bg_l < 0.5 {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    let mut out = tone;
    let mut t = 0.0_f32;
    // Cap at 0.8 so a lifted tone keeps some of its hue rather than washing fully
    // to white/black; ~10 steps max, evaluated once per frame.
    while (luminance(out) - bg_l).abs() < MIN_GAP && t < 0.8 {
        t += 0.08;
        out = tone.lerp_to_gamma(toward, t);
    }
    out
}

/// Build an `egui::Visuals` DERIVED FROM the active terminal colour `theme`, so
/// the whole chrome (titlebar / tab strip / status bar / settings window /
/// panel fills) follows the selected theme — light themes (e.g. `ghost-paper`)
/// produce a LIGHT egui base, dark themes a dark base.
///
/// The polarity is chosen from the theme background's luminance
/// ([`is_light`]); the chosen [`Visuals::light`]/[`Visuals::dark`] base is then
/// overridden so window/panel/widget backgrounds derive from `theme.background`
/// (panels/widgets subtly [`shade`]d so they read as raised surfaces), text from
/// `theme.foreground`, and selection/hyperlink/accent from
/// `theme.selection_background` (falling back to a bright accent when the theme
/// omits it). The two-tone C0PL4ND wordmark keeps its fixed brand accent (drawn
/// directly in `chrome.rs`); only the surfaces follow the theme.
pub fn visuals_from_theme(theme: &c0pl4nd_core::Theme) -> Visuals {
    let bg = theme_color(&theme.background, Color32::from_rgb(0x12, 0x12, 0x12));
    let fg = theme_color(&theme.foreground, Color32::from_rgb(0xe8, 0xe6, 0xf0));
    let light = is_light(bg);

    // Panel + bezel as raised surfaces above the window bg, deeper shade for the
    // bezel (widget fills) than the panel so the elevation reads at a glance.
    let panel = shade(bg, 0.06);
    let bezel = shade(bg, 0.12);

    // Accent: the theme's selection colour drives the live/hover accent and
    // selection wash. The cursor colour is the press/active accent. Both fall
    // back to the brand pair when the theme omits the optional slots.
    let accent = theme_color(&theme.selection_background, brand::GREEN);
    let press = theme_color(&theme.cursor, brand::PURPLE);
    let sel = {
        let [r, g, b, _] = accent.to_array();
        Color32::from_rgba_unmultiplied(r, g, b, 0x60)
    };

    // Weak/secondary text: blend fg toward bg so it reads as muted in either
    // polarity (the analogue of the fixed `MUTED` tone the dark theme used).
    let muted = fg.lerp_to_gamma(bg, 0.55);

    let mut v = if light {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    v.extreme_bg_color = bg;
    v.panel_fill = panel;
    v.window_fill = panel;
    v.faint_bg_color = panel;
    v.override_text_color = Some(fg);
    v.hyperlink_color = accent;
    v.selection.bg_fill = sel;
    v.selection.stroke = Stroke::new(1.0f32, accent);
    v.error_fg_color = Color32::from_rgb(0xff, 0x3b, 0x5c); // alarm red (polarity-agnostic)
    v.warn_fg_color = Color32::from_rgb(0xff, 0xc4, 0x4d); // warn amber

    let radius = CornerRadius::same(4);
    for ws in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        ws.corner_radius = radius;
    }
    v.widgets.noninteractive.bg_fill = panel;
    v.widgets.inactive.bg_fill = bezel;
    v.widgets.inactive.weak_bg_fill = panel;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0f32, fg);
    v.widgets.hovered.bg_fill = bezel;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0f32, accent); // accent outline on hover
    v.widgets.hovered.fg_stroke = Stroke::new(1.0f32, accent);
    v.widgets.active.bg_fill = bezel;
    v.widgets.active.bg_stroke = Stroke::new(1.0f32, press); // press accent
    v.widgets.active.fg_stroke = Stroke::new(1.0f32, fg);

    // Pointing-hand cursor over interactive controls. egui's default is
    // `interact_cursor: None` (style.rs:1538) and `Button` only calls
    // `set_cursor_icon` when it is `Some` (widgets/button.rs:374-378) — so with
    // the default NO button, tab, or toolbar control anywhere in the app changed
    // the mouse cursor, which reads as "not clickable". This is set on the
    // Visuals (not per-widget) so every button in the app inherits it from ONE
    // place.
    //
    // It does not fight the two explicit `set_cursor_icon` call-sites: the
    // frameless resize edges run BEFORE the panels and sit on the window border
    // where no button lives, and the terminal grid is not a `Button` (it is a
    // bare `interact` rect), so its I-beam / link hand are unaffected — a widget
    // only inherits this cursor by being a `Button`.
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);

    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0f32, bezel); // separators
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0f32, fg);
    v.weak_text_color = Some(muted);
    v.window_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0f32, bezel);

    v
}

/// Theme-derived chrome colours, computed once per frame from the active
/// terminal [`c0pl4nd_core::Theme`] and handed to the chrome painters so the
/// titlebar / tab strip / status bar / settings panels follow the theme without
/// each call-site re-deriving them. The two-tone C0PL4ND wordmark's tones
/// ([`ChromeColors::logo_a`]/[`ChromeColors::logo_b`]) are themselves
/// theme-derived (bright, readability-guaranteed); everything else is theme-led.
#[derive(Debug, Clone, Copy)]
pub struct ChromeColors {
    /// Window background (the central pane fill behind the grid).
    pub bg: Color32,
    /// Raised-surface fill for the titlebar / status bar / settings window.
    pub panel: Color32,
    /// Deeper-shaded fill for widget surfaces / hairlines / inactive borders.
    pub bezel: Color32,
    /// Primary text colour (from the theme foreground).
    pub fg: Color32,
    /// Muted/secondary text + glyph-button base (fg blended toward bg).
    pub muted: Color32,
    /// Live/selected accent (from the theme selection colour; brand green when
    /// the theme omits it). Used for the focused tab, status accent, headings.
    pub accent: Color32,
    /// First tone of the two-tone C0PL4ND wordmark ("C0PL"). Derived from the
    /// theme's BRIGHT magenta (echoing the brand purple) and guaranteed both
    /// readable and bright against the titlebar surface, so it tints with the
    /// theme yet is never a muddy, too-dark, low-contrast wordmark.
    pub logo_a: Color32,
    /// Second tone of the wordmark ("4ND"). Derived from the theme's BRIGHT green
    /// (echoing the brand green), hue-distant from [`Self::logo_a`] so the two
    /// tones always contrast, and likewise guaranteed readable + bright against
    /// the titlebar surface.
    pub logo_b: Color32,
}

impl ChromeColors {
    /// Derive the chrome colours from the active terminal theme — the single
    /// place the chrome's surface palette is computed (mirrors the shading the
    /// egui Visuals use in [`visuals_from_theme`] so chrome painted directly
    /// with these colours matches the Visuals-styled widgets exactly).
    pub fn from_theme(theme: &c0pl4nd_core::Theme) -> Self {
        let bg = theme_color(&theme.background, brand::BG);
        let fg = theme_color(&theme.foreground, brand::FG);
        let accent = theme_color(&theme.selection_background, brand::GREEN);
        let panel = shade(bg, 0.06);
        // The two-tone wordmark draws on the titlebar `panel` surface, so both
        // tones are made readable against `panel` (not the window bg). They pull
        // from the theme's BRIGHT magenta/green — hue-distant, so they always
        // contrast — and fall back to the brand purple/green pair.
        let logo_a = ensure_readable_tone(theme_color(&theme.bright.magenta, brand::PURPLE), panel);
        let logo_b = ensure_readable_tone(theme_color(&theme.bright.green, brand::GREEN), panel);
        Self {
            bg,
            panel,
            bezel: shade(bg, 0.12),
            fg,
            muted: fg.lerp_to_gamma(bg, 0.55),
            accent,
            logo_a,
            logo_b,
        }
    }
}

/// Brand accent colors exposed to the chrome module so the wordmark and
/// placeholder panes can paint with the same palette without re-deriving it.
pub mod brand {
    use egui::Color32;

    /// `#7700FF` — Itasha purple (structural accent).
    pub const PURPLE: Color32 = Color32::from_rgb(0x77, 0x00, 0xff);
    /// `#00FF90` — .Corp green (live accent).
    pub const GREEN: Color32 = Color32::from_rgb(0x00, 0xff, 0x90);
    /// `#e8e6f0` — foreground text (fallback when a theme omits foreground).
    pub const FG: Color32 = Color32::from_rgb(0xe8, 0xe6, 0xf0);
    /// `#121212` — void background (fallback when a theme omits background).
    pub const BG: Color32 = Color32::from_rgb(0x12, 0x12, 0x12);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_separates_light_and_dark() {
        assert!(is_light(Color32::WHITE));
        assert!(!is_light(Color32::BLACK));
        // A near-white paper is light; the void #121212 is dark.
        assert!(is_light(Color32::from_rgb(0xf5, 0xf2, 0xea)));
        assert!(!is_light(Color32::from_rgb(0x12, 0x12, 0x12)));
    }

    #[test]
    fn shade_lightens_dark_and_darkens_light() {
        // Dark base shades TOWARD white (raised surface reads brighter).
        let dark = Color32::from_rgb(0x12, 0x12, 0x12);
        assert!(luminance(shade(dark, 0.12)) > luminance(dark));
        // Light base shades TOWARD black (raised surface reads darker).
        let light = Color32::from_rgb(0xf0, 0xee, 0xf5);
        assert!(luminance(shade(light, 0.12)) < luminance(light));
    }

    #[test]
    fn visuals_from_dark_theme_are_dark() {
        let t = c0pl4nd_core::Theme::builtin_void();
        let v = visuals_from_theme(&t);
        assert!(
            !is_light(v.window_fill),
            "a dark theme must produce a dark egui base (window_fill={:?})",
            v.window_fill
        );
        // Text + extreme bg derive from the theme, not the fixed brand palette.
        assert_eq!(
            v.override_text_color,
            Some(theme_color(&t.foreground, brand::FG))
        );
        assert_eq!(v.extreme_bg_color, theme_color(&t.background, brand::BG));
    }

    #[test]
    fn visuals_from_light_theme_are_light() {
        let t = c0pl4nd_core::Theme::builtin_named("ghost-paper").expect("ghost-paper embedded");
        let v = visuals_from_theme(&t);
        assert!(
            is_light(v.window_fill),
            "a light theme (ghost-paper) must produce a LIGHT egui base \
             (window_fill={:?})",
            v.window_fill
        );
        // The extreme bg is the theme's light background, not the dark void.
        assert!(is_light(v.extreme_bg_color));
    }

    #[test]
    fn chrome_colors_follow_theme_polarity() {
        let dark = ChromeColors::from_theme(&c0pl4nd_core::Theme::builtin_void());
        assert!(!is_light(dark.bg) && !is_light(dark.panel));
        let light = ChromeColors::from_theme(
            &c0pl4nd_core::Theme::builtin_named("ghost-paper").expect("ghost-paper embedded"),
        );
        assert!(is_light(light.bg) && is_light(light.panel));
    }

    /// Every builtin theme this app ships, so the theme-wide guarantees below
    /// (focus-ring contrast) are asserted against the real fleet, not one theme.
    const ALL_BUILTIN_THEMES: [&str; 13] = [
        "ghost-paper",
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

    fn every_builtin_palette() -> Vec<(&'static str, ChromeColors)> {
        std::iter::once((
            "void",
            ChromeColors::from_theme(&c0pl4nd_core::Theme::builtin_void()),
        ))
        .chain(ALL_BUILTIN_THEMES.iter().map(|name| {
            let t = c0pl4nd_core::Theme::builtin_named(name)
                .unwrap_or_else(|| panic!("{name} embedded"));
            (*name, ChromeColors::from_theme(&t))
        }))
        .collect()
    }

    #[test]
    fn contrast_ratio_matches_the_wcag_reference_points() {
        // The two anchors of the WCAG scale: identical colours are 1:1, and
        // black-on-white is the 21:1 maximum.
        assert!((contrast_ratio(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.01);
        assert!((contrast_ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.05);
        // Symmetric in its arguments.
        let (a, b) = (
            Color32::from_rgb(0x12, 0x34, 0x56),
            Color32::from_rgb(0xab, 0xcd, 0xef),
        );
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 1e-4);
    }

    #[test]
    fn focus_ring_clears_the_wcag_floor_against_every_surface_on_every_theme() {
        // WCAG 2.4.11/2.4.13: a focus indicator needs >= 3:1 against BOTH the
        // control and its surroundings. The ring can land on the titlebar panel,
        // the window background, or the ✕'s close-red hover fill — so all three
        // must clear the floor, for every shipped theme.
        for (name, colors) in every_builtin_palette() {
            let ring = focus_ring_color(colors);
            for (label, surface) in [
                ("panel", colors.panel),
                ("bg", colors.bg),
                ("close-red", CLOSE_RED),
            ] {
                let ratio = contrast_ratio(ring, surface);
                assert!(
                    ratio >= FOCUS_RING_MIN_CONTRAST,
                    "{name}: focus ring {ring:?} only reaches {ratio:.2}:1 against {label} \
                     (WCAG floor is {FOCUS_RING_MIN_CONTRAST}:1)"
                );
            }
        }
    }

    #[test]
    fn focus_ring_prefers_the_brand_accent_but_falls_back_when_it_cannot_contrast() {
        // A palette whose accent is nearly the panel colour cannot be the ring —
        // the fallback pole is chosen instead (proving the guarantee is not
        // satisfied by luck of the theme).
        let mut colors = ChromeColors::from_theme(&c0pl4nd_core::Theme::builtin_void());
        colors.accent = colors.panel;
        let ring = focus_ring_color(colors);
        assert_ne!(
            ring, colors.accent,
            "an accent that cannot contrast must not be used"
        );
        assert!(contrast_ratio(ring, colors.panel) >= FOCUS_RING_MIN_CONTRAST);
    }

    #[test]
    fn visuals_set_a_pointing_hand_interact_cursor() {
        // D2: egui's default is None, so nothing in the app changed the cursor.
        // Asserted on both polarities — it is set unconditionally.
        for theme in [
            c0pl4nd_core::Theme::builtin_void(),
            c0pl4nd_core::Theme::builtin_named("ghost-paper").expect("ghost-paper embedded"),
        ] {
            assert_eq!(
                visuals_from_theme(&theme).interact_cursor,
                Some(egui::CursorIcon::PointingHand),
                "buttons must show a pointing hand on hover"
            );
        }
    }

    #[test]
    fn wordmark_tones_are_readable_bright_and_contrasting() {
        // Normalised per-channel distance (0.0..=1.0) — proves the two tones are
        // genuinely different colours, not just different luminances.
        let chan_dist = |a: Color32, b: Color32| {
            let [ar, ag, ab, _] = a.to_array();
            let [br, bg, bb, _] = b.to_array();
            ((ar as f32 - br as f32).abs()
                + (ag as f32 - bg as f32).abs()
                + (ab as f32 - bb as f32).abs())
                / (255.0 * 3.0)
        };
        // The two flagship polarities PLUS the 12 SCR1B3 Wave-4 ports (M8): the
        // wordmark contrast guarantee (`ensure_readable_tone` on the theme's bright
        // magenta/green) must hold for every newly-embedded theme too.
        let wave4 = [
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
        for name in ["void", "ghost-paper"].iter().chain(wave4.iter()) {
            let name = *name;
            let theme = if name == "void" {
                c0pl4nd_core::Theme::builtin_void()
            } else {
                c0pl4nd_core::Theme::builtin_named(name)
                    .unwrap_or_else(|| panic!("{name} embedded"))
            };
            let c = ChromeColors::from_theme(&theme);
            // Each tone clears the legibility floor against the titlebar surface…
            let gap_a = (luminance(c.logo_a) - luminance(c.panel)).abs();
            let gap_b = (luminance(c.logo_b) - luminance(c.panel)).abs();
            assert!(gap_a >= 0.30, "{name}: logo_a not readable (gap {gap_a})");
            assert!(gap_b >= 0.30, "{name}: logo_b not readable (gap {gap_b})");
            // …and the two tones are visibly DIFFERENT colours (contrasting).
            let d = chan_dist(c.logo_a, c.logo_b);
            assert!(
                d > 0.12,
                "{name}: wordmark tones too similar (chan_dist {d})"
            );
        }
    }
}
