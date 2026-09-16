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

/// The opaque sRGB triple of an egui colour, in the `(u8, u8, u8)` vocabulary
/// the core WCAG helpers take. `Color32::to_array` yields `[r, g, b, a]`; the
/// alpha is dropped because a contrast ratio is only defined between opaque
/// colours (every colour these helpers are called with — surfaces, text, the
/// close-red, the focus ring — is opaque).
fn rgb_triple(c: Color32) -> (u8, u8, u8) {
    let [r, g, b, _] = c.to_array();
    (r, g, b)
}

/// WCAG contrast ratio between two opaque colours — `1.0` (identical) up to
/// `21.0` (black on white). WCAG 2.4.11 / 2.4.13 require **>= 3:1** for a focus
/// indicator against BOTH the focused control and its surroundings, which is the
/// floor [`focus_ring_color`] is built to guarantee.
///
/// A thin `Color32` ADAPTER over [`c0pl4nd_core::theme::contrast_ratio`], not a
/// second implementation. This module used to carry its own `relative_luminance`
/// (the sRGB linearisation + the 0.2126/0.7152/0.0722 weights) plus its own ratio
/// arithmetic, duplicating `c0pl4nd_core::theme::color_model` — which the
/// terminal renderer's minimum-contrast clamp uses. Two private copies of one
/// standard, and the local test only ever checked this copy against hard-coded
/// literals, so the two could have drifted apart with nothing failing. Both now
/// resolve to the single core implementation; the app-side
/// `relative_luminance` wrapper went with them, having existed only to feed this
/// function (nothing else called it). Callers wanting the raw WCAG luminance use
/// [`c0pl4nd_core::theme::relative_luminance`]; [`luminance`] remains the cheap
/// Rec.601 approximation used for the light/dark polarity pivot, which is a
/// deliberately different thing — polarity only needs a rough split, whereas a
/// ratio that claims WCAG conformance must use the WCAG formula.
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    c0pl4nd_core::theme::contrast_ratio(rgb_triple(a), rgb_triple(b))
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

/// The WCAG 2.2 **1.4.3 AA** contrast floor for normal-size body TEXT.
///
/// Distinct from [`FOCUS_RING_MIN_CONTRAST`] on purpose: 3:1 is the *non-text*
/// floor for indicators and graphical objects (1.4.11), and applying it to text
/// would sign off on a hint nobody can read.
pub const TEXT_MIN_CONTRAST: f32 = 4.5;

/// [`ChromeColors::accent`] made legible AS TEXT on `surface`.
///
/// `accent` is derived from the theme's `selection_background` — a colour chosen
/// to sit BEHIND text as a wash, where being close to the background is the
/// point. Painting it as a foreground has no contrast guarantee whatsoever, and
/// the shipped `void` theme proves it: its selection colour `#33106b` on the
/// status bar's `#202020` panel is **1.11:1** against a 4.5:1 floor, which
/// rendered the pane counter and the welcome toast as near-invisible dark purple
/// on dark grey.
///
/// This is the TEXT analogue of [`focus_ring_color`]: the theme's own hue is
/// preferred and returned untouched when it already clears the floor, and only a
/// failing accent is lifted — via the core clamp, which blends toward whichever
/// monochrome pole has the headroom, so the accent keeps as much chroma as
/// legibility allows instead of snapping to white.
pub fn accent_text_color(accent: Color32, surface: Color32) -> Color32 {
    let (r, g, b) = c0pl4nd_core::theme::enforce_min_contrast(
        rgb_triple(accent),
        rgb_triple(surface),
        TEXT_MIN_CONTRAST,
    );
    Color32::from_rgb(r, g, b)
}

/// Parse a `c0pl4nd_core::Theme` `#rrggbb` field into an egui `Color32`, falling
/// back to `fallback` when the field is empty or unparseable (e.g. the optional
/// `selection_background` slot a minimal theme omits).
pub(crate) fn theme_color(hex: &str, fallback: Color32) -> Color32 {
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

/// The epaint coverage curve for grid text of colour `fg` on `bg`, at the user's
/// `text_contrast`.
///
/// The ONE place `c0pl4nd_core`'s engine-free [`CoverageCurve`] is mapped onto
/// epaint's enum. Core deliberately does not depend on egui (see
/// [`c0pl4nd_core::theme::glyph_coverage`]'s module docs), so the policy is
/// decided there and translated here — this function holds no policy of its own,
/// which is what keeps the decision unit-testable without a GPU or an egui
/// context.
///
/// Takes the already-resolved colours rather than the `Theme`, so it cannot
/// disagree with the `bg`/`fg` its caller derived from the same theme (including
/// their fallbacks when a minimal theme omits a slot).
pub(crate) fn alpha_from_coverage_for(
    fg: Color32,
    bg: Color32,
    text_contrast: f32,
) -> egui::epaint::AlphaFromCoverage {
    use c0pl4nd_core::theme::CoverageCurve;
    use egui::epaint::AlphaFromCoverage;
    match c0pl4nd_core::theme::curve_for(rgb_triple(fg), rgb_triple(bg), text_contrast) {
        CoverageCurve::Linear => AlphaFromCoverage::Linear,
        CoverageCurve::TwoCMinusCSq => AlphaFromCoverage::TwoCoverageMinusCoverageSq,
        CoverageCurve::Gamma(g) => AlphaFromCoverage::Gamma(g),
    }
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
///
/// # `text_contrast` and the glyph coverage curve
///
/// `text_contrast` is `config.font.text_contrast`, and it selects the GLYPH
/// COVERAGE CURVE the font atlas is baked with (see
/// [`c0pl4nd_core::theme::glyph_coverage`]). It is a REQUIRED parameter rather
/// than an optional one on purpose: the curve was previously inherited as a
/// side effect of the `Visuals::light()` / `Visuals::dark()` choice above and
/// never named anywhere in this repo, which is precisely the silent-inheritance
/// defect this signature exists to remove. An overload that defaulted it would
/// recreate that defect for any call site that forgot to pass it.
///
/// `0.0` — the shipped default — reproduces the previously-inherited curve, so
/// this is a no-op at the defaults.
pub fn visuals_from_theme(theme: &c0pl4nd_core::Theme, text_contrast: f32) -> Visuals {
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
    // OWN THE GLYPH COVERAGE CURVE. Both `Visuals::light()` and `Visuals::dark()`
    // carry a `text_options.alpha_from_coverage`, and until this line C0PL4ND
    // took whichever one that base happened to have without ever naming it —
    // making how every glyph in the app is inked a silent side effect of an
    // unrelated chrome-polarity decision.
    //
    // The selection now comes from the GRID's own polarity (`fg` vs `bg`) rather
    // than the chrome base's, and from the user's contrast knob. For every
    // shipped theme this resolves to the same curve as before, which is the
    // point: the behaviour is pinned, not changed.
    v.text_options.alpha_from_coverage = alpha_from_coverage_for(fg, bg, text_contrast);
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
    ///
    /// This is the raw theme colour and carries NO contrast guarantee — it is
    /// for FILLS and strokes. Anything that paints it as TEXT on
    /// [`Self::panel`] must use [`Self::accent_text`] instead.
    pub accent: Color32,
    /// [`Self::accent`] made legible as TEXT on [`Self::panel`] (WCAG AA
    /// [`TEXT_MIN_CONTRAST`]). Identical to `accent` for any theme whose
    /// selection colour already clears the floor.
    pub accent_text: Color32,
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
            accent_text: accent_text_color(accent, panel),
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
    use c0pl4nd_core::theme::glyph_coverage::CONTRAST_NEUTRAL;

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

    // ----- the glyph coverage curve -----------------------------------------

    /// THE anti-drift pin between core's engine-free curve MODEL and epaint's
    /// real arithmetic.
    ///
    /// `CoverageCurve::alpha_at` exists so the curve can be reasoned about and
    /// tested in `c0pl4nd-core`, which must not depend on egui. Nothing renders
    /// through it — epaint's `alpha_from_coverage` does the real work. That makes
    /// it a second implementation of one function, and a second implementation
    /// with no equivalence check is a copy waiting to drift: core's tests would
    /// stay green while describing arithmetic the renderer no longer performs.
    ///
    /// This is the check. It runs in the app crate because this is the only crate
    /// that can see BOTH.
    #[test]
    fn the_core_curve_model_matches_epaints_arithmetic() {
        use c0pl4nd_core::theme::glyph_coverage::{CoverageCurve, GAMMA_MAX, GAMMA_MIN};
        use egui::epaint::AlphaFromCoverage;
        let pairs = [
            (CoverageCurve::Linear, AlphaFromCoverage::Linear),
            (
                CoverageCurve::TwoCMinusCSq,
                AlphaFromCoverage::TwoCoverageMinusCoverageSq,
            ),
            (
                CoverageCurve::Gamma(GAMMA_MIN),
                AlphaFromCoverage::Gamma(GAMMA_MIN),
            ),
            (
                CoverageCurve::Gamma(GAMMA_MAX),
                AlphaFromCoverage::Gamma(GAMMA_MAX),
            ),
            (CoverageCurve::Gamma(0.55), AlphaFromCoverage::Gamma(0.55)),
        ];
        for (model, real) in pairs {
            // Sweep past the ends too, so the clamping agrees as well as the
            // curve — a model that clamped differently at the extremes would
            // mis-describe the first and last pixel of every stem.
            for step in -10..=110 {
                let c = step as f32 / 100.0;
                let (m, r) = (model.alpha_at(c), real.alpha_from_coverage(c));
                assert!(
                    (m - r).abs() < 1e-6,
                    "core's model of {model:?} says alpha({c}) = {m}, epaint's \
                     {real:?} says {r} — the model has drifted from the \
                     arithmetic the renderer actually performs"
                );
            }
        }
    }

    /// The whole selection policy, checked at the seam that applies it: the
    /// `Visuals` handed to egui must carry the curve `curve_for` chose, for BOTH
    /// polarities and for a non-neutral contrast.
    #[test]
    fn visuals_carry_the_curve_the_policy_selected() {
        use egui::epaint::AlphaFromCoverage;
        let dark = c0pl4nd_core::Theme::builtin_void();
        let light = c0pl4nd_core::Theme::builtin_named("ghost-paper").expect("ghost-paper embedded");

        assert_eq!(
            visuals_from_theme(&dark, CONTRAST_NEUTRAL)
                .text_options
                .alpha_from_coverage,
            AlphaFromCoverage::TwoCoverageMinusCoverageSq,
            "a dark theme's light-on-dark grid text needs the fattening curve"
        );
        assert_eq!(
            visuals_from_theme(&light, CONTRAST_NEUTRAL)
                .text_options
                .alpha_from_coverage,
            AlphaFromCoverage::Linear,
            "a light theme's dark-on-light grid text needs the identity curve"
        );
        assert!(
            matches!(
                visuals_from_theme(&dark, 0.35)
                    .text_options
                    .alpha_from_coverage,
                AlphaFromCoverage::Gamma(_)
            ),
            "a non-neutral contrast must reach the Visuals as a Gamma curve — \
             otherwise the knob is wired to nothing"
        );
    }

    /// THE "no visible change at the default" contract.
    ///
    /// Before this work the curve was whatever `Visuals::light()` /
    /// `Visuals::dark()` happened to carry. Making the selection EXPLICIT is only
    /// safe if the explicit answer is the same answer — otherwise shipping the
    /// pin would silently re-ink every existing user's terminal.
    ///
    /// This compares against the egui bases directly rather than against a
    /// hard-coded enum variant, so it is a genuine equivalence rather than two
    /// copies of one guess. It is also the guard that would fire on an egui
    /// upgrade that changed a base default: at that point the app's rendering no
    /// longer matches its inherited past, and that is a decision to take
    /// deliberately, not a diff to absorb silently.
    #[test]
    fn the_default_contrast_reproduces_the_previously_inherited_curve() {
        for (name, theme, base) in [
            (
                "itasha-void (dark base)",
                c0pl4nd_core::Theme::builtin_void(),
                Visuals::dark(),
            ),
            (
                "ghost-paper (light base)",
                c0pl4nd_core::Theme::builtin_named("ghost-paper").expect("ghost-paper embedded"),
                Visuals::light(),
            ),
        ] {
            assert_eq!(
                visuals_from_theme(&theme, CONTRAST_NEUTRAL)
                    .text_options
                    .alpha_from_coverage,
                base.text_options.alpha_from_coverage,
                "{name}: the EXPLICIT curve selection at the neutral detent must \
                 equal the curve this theme's egui base previously supplied \
                 implicitly. A mismatch means pinning the curve CHANGED how every \
                 glyph is inked."
            );
        }
    }

    /// Every SHIPPED theme must classify to the same curve its egui base would
    /// have supplied. The two tests above check the two representative themes;
    /// this checks the whole set, which is what makes "no shipped theme changes"
    /// a measurement rather than a sample.
    #[test]
    fn no_shipped_theme_changes_curve_under_the_explicit_selection() {
        let mut checked = 0usize;
        for (name, src) in c0pl4nd_core::Theme::EMBEDDED_THEMES {
            let theme = c0pl4nd_core::Theme::from_toml(src)
                .unwrap_or_else(|e| panic!("shipped theme {name} must parse: {e}"));
            let base = if is_light(theme_color(&theme.background, brand::BG)) {
                Visuals::light()
            } else {
                Visuals::dark()
            };
            assert_eq!(
                visuals_from_theme(&theme, CONTRAST_NEUTRAL)
                    .text_options
                    .alpha_from_coverage,
                base.text_options.alpha_from_coverage,
                "{name}: the fg-vs-bg polarity policy disagrees with the \
                 background-luminance pivot this theme's chrome base uses, so \
                 pinning the curve would change how this theme renders"
            );
            checked += 1;
        }
        assert!(
            checked >= 30,
            "only {checked} themes were checked — the shipped set is ~35, so this \
             assertion is not covering what it claims to"
        );
    }

    #[test]
    fn visuals_from_dark_theme_are_dark() {
        let t = c0pl4nd_core::Theme::builtin_void();
        let v = visuals_from_theme(&t, CONTRAST_NEUTRAL);
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
        let v = visuals_from_theme(&t, CONTRAST_NEUTRAL);
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

    /// [`contrast_ratio`] is an ADAPTER over the core implementation
    /// (`c0pl4nd_core::theme`), not a second copy of the formula — the state this
    /// replaced, where `contrast_ratio_matches_the_wcag_reference_points` only
    /// ever compared the app's private copy against literals, so the two copies
    /// could drift apart with nothing failing.
    ///
    /// This pins the adapter to core across a spread of colours, including
    /// channel-ASYMMETRIC ones (the WCAG weights are 0.2126/0.7152/0.0722, so a
    /// swapped or dropped channel moves the answer) and probes either side of the
    /// 0.04045 linear-segment knee. Re-inlining a local copy of the maths fails
    /// here the moment it disagrees by more than float noise, and the final block
    /// pins [`rgb_triple`] — the one piece of the conversion this module still
    /// owns — against the literal triples the probes were built from.
    #[test]
    fn wcag_helpers_are_adapters_over_the_core_implementation() {
        const PROBES: [(u8, u8, u8); 10] = [
            (0, 0, 0),
            (255, 255, 255),
            (255, 0, 0),  // asymmetric: R is weighted 0.2126…
            (0, 255, 0),  // …G 0.7152…
            (0, 0, 255),  // …B 0.0722 — a channel swap moves all three
            (10, 10, 10), // below the 0.04045 knee (the `/ 12.92` segment)
            (11, 11, 11), // just above it (the `powf(2.4)` segment)
            (0x12, 0x34, 0x56),
            (0xab, 0xcd, 0xef),
            (0xE8, 0x11, 0x23), // CLOSE_RED, a real call-site colour
        ];
        let as_color = |(r, g, b): (u8, u8, u8)| Color32::from_rgb(r, g, b);

        for (i, a) in PROBES.iter().enumerate() {
            for b in PROBES.iter().skip(i) {
                let app = contrast_ratio(as_color(*a), as_color(*b));
                let core = c0pl4nd_core::theme::contrast_ratio(*a, *b);
                assert!(
                    (app - core).abs() < 1e-6,
                    "contrast_ratio drifted for {a:?} vs {b:?}: app {app} vs core {core}"
                );
            }
        }

        // The equality above would also hold if BOTH sides were constant, so
        // prove the probe set actually exercises the scale: the luminances must
        // span it, and the ratios must reach the 21:1 maximum.
        let mut lums: Vec<f32> = PROBES
            .iter()
            .map(|p| c0pl4nd_core::theme::relative_luminance(*p))
            .collect();
        lums.sort_by(f32::total_cmp);
        let span = lums.last().unwrap() - lums.first().unwrap();
        assert!(
            span > 0.9,
            "probe set does not span the luminance scale: {span}"
        );
        assert!(
            (contrast_ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.05,
            "the adapter must still reach the 21:1 WCAG maximum"
        );

        // The `Color32 -> (u8, u8, u8)` step is the only thing this module still
        // owns, so pin it directly: a swapped or dropped channel in `rgb_triple`
        // would leave every equality above intact if both sides were fed the same
        // wrong triple, but it CANNOT survive being compared against the literal
        // triple the colour was built from.
        for probe in PROBES {
            assert_eq!(
                rgb_triple(as_color(probe)),
                probe,
                "rgb_triple must preserve channel order and drop only alpha"
            );
        }
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

    /// THE STATUS-BAR LEGIBILITY GATE. Every colour the status bar paints TEXT
    /// with must clear the WCAG 2.2 AA 1.4.3 floor against the surface it is
    /// painted on, for every shipped theme. A failing pair breaks the build.
    ///
    /// It regressed exactly the way `bright.black` did, one tier up: the status
    /// bar draws its pane counter and its toast in `colors.accent`, which is the
    /// theme's `selection_background` — a colour designed to sit BEHIND text.
    /// On the default `void` theme that is `#33106b` on the `#202020` panel:
    /// **1.11:1**, a quarter of the floor. Rendered, the welcome toast and the
    /// "1/6 panes" counter were dark purple on dark grey — present in the frame,
    /// invisible to a reader.
    ///
    /// Asserted against the real WCAG formula rather than a hardcoded hex, so it
    /// still means something when a palette is re-tuned, and asserted over the
    /// WHOLE builtin fleet so a newly-embedded theme cannot slip a 1.11:1 pair
    /// back in.
    #[test]
    fn every_status_bar_text_colour_meets_wcag_aa_on_its_surface() {
        for (name, colors) in every_builtin_palette() {
            // (label, text colour, surface) — the status bar paints on `panel`.
            for (label, text) in [
                ("accent_text (pane counter, toast)", colors.accent_text),
                ("fg (the hint labels)", colors.fg),
            ] {
                let ratio = contrast_ratio(text, colors.panel);
                assert!(
                    ratio >= TEXT_MIN_CONTRAST,
                    "{name}: status-bar {label} {text:?} on panel {:?} is only \
                     {ratio:.2}:1 — below the WCAG AA text floor of \
                     {TEXT_MIN_CONTRAST}:1. Text this close to its background is \
                     painted but unreadable.",
                    colors.panel,
                );
            }
        }
    }

    /// [`accent_text_color`] must be a REAL clamp, not a pass-through: it leaves
    /// an already-legible accent untouched (so a vivid theme keeps its hue) and
    /// lifts a failing one until it clears the floor.
    ///
    /// Without the first half, "returns white always" would satisfy the gate
    /// above while destroying every theme's accent; without the second, a
    /// pass-through would satisfy the first half while fixing nothing.
    #[test]
    fn accent_text_color_lifts_only_what_fails_the_floor() {
        let panel = Color32::from_rgb(0x20, 0x20, 0x20);

        // The real defect: void's selection colour is unreadable as text.
        let void = Color32::from_rgb(0x33, 0x10, 0x6b);
        let before = contrast_ratio(void, panel);
        assert!(
            before < TEXT_MIN_CONTRAST,
            "the regression fixture must actually be a failing pair, got \
             {before:.2}:1"
        );
        let lifted = accent_text_color(void, panel);
        assert_ne!(lifted, void, "a failing accent must be lifted");
        assert!(
            contrast_ratio(lifted, panel) >= TEXT_MIN_CONTRAST,
            "the lift must clear the floor, got {:.2}:1",
            contrast_ratio(lifted, panel)
        );

        // An accent that already clears the floor keeps its exact hue.
        let bright = brand::GREEN;
        assert!(contrast_ratio(bright, panel) >= TEXT_MIN_CONTRAST);
        assert_eq!(
            accent_text_color(bright, panel),
            bright,
            "a legible accent must be returned untouched, not washed to a pole"
        );
    }

    /// The clamp must be wired into `ChromeColors`, not merely defined. Cutting
    /// `accent_text: accent_text_color(accent, panel)` back to `accent` leaves
    /// `every_status_bar_text_colour_meets_wcag_aa_on_its_surface` failing, but
    /// this names the wire directly.
    #[test]
    fn chrome_colors_derives_accent_text_from_the_clamp() {
        for (name, colors) in every_builtin_palette() {
            assert_eq!(
                colors.accent_text,
                accent_text_color(colors.accent, colors.panel),
                "{name}: accent_text must be the clamped accent"
            );
        }
        // And on the default theme it must genuinely DIFFER from the raw accent —
        // otherwise the field could be a rename of `accent` and every assertion
        // above would still hold.
        let void = ChromeColors::from_theme(&c0pl4nd_core::Theme::builtin_void());
        assert_ne!(
            void.accent_text, void.accent,
            "void's accent #33106b fails the text floor, so its accent_text must \
             differ from it"
        );
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
                visuals_from_theme(&theme, CONTRAST_NEUTRAL).interact_cursor,
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
