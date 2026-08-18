//! The glyph-atlas COVERAGE CURVE: which coverage→alpha shape the font atlas is
//! baked with, and the bounded user knob that selects it.
//!
//! # What the curve is, and why this module exists
//!
//! When a glyph is rasterised, each pixel gets a *coverage* value in `0..=1` —
//! how much of that pixel the glyph outline covers. The coverage curve is the
//! function that turns coverage into the ALPHA stored in the font atlas. It is
//! what makes text read heavier or lighter, and the correct shape depends on
//! POLARITY: light-on-dark text needs a fattening curve (thin light stems on a
//! dark ground otherwise disappear), dark-on-light text does not.
//!
//! epaint owns the arithmetic (`epaint::AlphaFromCoverage`, applied once per
//! glyph at atlas-bake time). C0PL4ND did not own the SELECTION: `theme.rs`
//! built its `Visuals` from `Visuals::light()` / `Visuals::dark()` and inherited
//! whichever curve that base happened to carry, without ever naming it. A grep
//! for `text_options` / `alpha_from_coverage` / `AlphaFromCoverage` across
//! `crates/` returned ZERO hits. That is a load-bearing rendering property with
//! no falsifier: an egui bump that changed a default, or a refactor that started
//! from `Visuals::default()` (which carries the DARK curve, so it would silently
//! apply the fattening curve to the light themes too), would change how every
//! glyph in the app is inked and nothing would fail.
//!
//! This module is the fix: the selection becomes an explicit, pure, testable
//! policy that the app applies deliberately.
//!
//! # This is NOT a bug fix
//!
//! The inherited selection is CORRECT for every shipped theme. All 35 files in
//! `assets/themes/` classify to the right polarity, and none sits anywhere near
//! the pivot. So this is not "the curve is wrong"; it is **own it, pin it, make
//! it tunable, make it testable**. The default (`text_contrast == 0.0`) is
//! deliberately byte-identical to the previously-inherited behaviour.
//!
//! # What this knob CANNOT do
//!
//! The curve is consumed once per ATLAS BAKE, and there is one atlas per
//! `egui::Context`. It is therefore a single global shape, not a per-shape
//! decision. So the genuinely-wrong case — a dark glyph on a bright cell
//! (reverse video `ESC[7m`, the selection wash, the cursor cell) rendered
//! through the light-on-dark fattening curve — is **not reachable through this
//! knob**. Correcting that needs a per-shape decision in the shader, which is
//! exactly what epaint's own source comment asks for. This module documents that
//! boundary rather than promising a fix it cannot deliver.
//!
//! # Core, not app
//!
//! This lives in core for the same reason [`super::color_model`] does: it is
//! model behaviour shared by every renderer, and it must not depend on egui. So
//! it returns a plain [`CoverageCurve`] and the app performs the one-line map
//! onto `epaint::AlphaFromCoverage`. [`CoverageCurve::alpha_at`] is a MODEL of
//! epaint's arithmetic kept honest by an app-side test that asserts the two
//! agree across a coverage sweep — see
//! `crates/app/src/egui_app/theme.rs::the_core_curve_model_matches_epaints_arithmetic`.

use super::relative_luminance;

/// Which coverage→alpha curve the glyph atlas is baked with.
///
/// Mirrors `epaint::AlphaFromCoverage` without depending on it. The app maps
/// this onto the epaint enum in one `match`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoverageCurve {
    /// `alpha = coverage`. The honest identity — correct for dark-on-light text,
    /// where the glyph is already the darker thing and needs no help.
    Linear,
    /// `alpha = 2c − c²`. Fattens partial coverage — correct for light-on-dark
    /// text, where thin light stems on a dark ground otherwise wash out. Sharper
    /// than the gamma curve of nominally-equivalent exponent (see
    /// [`ANCHOR_LIGHT_ON_DARK`]).
    TwoCMinusCSq,
    /// `alpha = c^g`. The tuning escape hatch, reached only when the user moves
    /// the contrast knob off its neutral detent. `g` is always within
    /// [`GAMMA_MIN`]..=[`GAMMA_MAX`].
    Gamma(f32),
}

impl CoverageCurve {
    /// The alpha this curve produces for `coverage`.
    ///
    /// A MODEL of `epaint::AlphaFromCoverage::alpha_from_coverage`, not a
    /// replacement for it: nothing renders through this function. It exists so
    /// the curve's arithmetic is checkable in a pure, GPU-free test — and it is
    /// pinned to the real implementation by an app-side equivalence test, so the
    /// two cannot silently drift.
    #[must_use]
    pub fn alpha_at(self, coverage: f32) -> f32 {
        let c = coverage.clamp(0.0, 1.0);
        match self {
            Self::Linear => c,
            Self::TwoCMinusCSq => 2.0 * c - c * c,
            Self::Gamma(g) => c.powf(g),
        }
    }
}

/// Lower bound on the user-tunable exponent (the HEAVIEST ink).
///
/// Below this the curve exceeds the fattening of [`CoverageCurve::TwoCMinusCSq`]
/// at every coverage and glyph counters — the enclosed holes in `#`, `e`, `o`,
/// `8` — start to fill in. Clamped, never rejected: a corrupt config produces
/// the bound, not an error.
pub const GAMMA_MIN: f32 = 0.45;

/// Upper bound on the user-tunable exponent (the LIGHTEST ink).
///
/// Above this, stems drop under one physical pixel of alpha at the default 13pt
/// and sub-pixel features (`.` `,` `'`) erase into the background.
pub const GAMMA_MAX: f32 = 1.60;

/// The contrast value that selects the POLARITY DEFAULT — no gamma at all.
///
/// This is the shipped default, which is what makes the default render path
/// identical to the behaviour that was previously inherited implicitly.
pub const CONTRAST_NEUTRAL: f32 = 0.0;

/// The user knob's bounds. Values outside are clamped, never rejected.
pub const CONTRAST_MIN: f32 = -1.0;
/// See [`CONTRAST_MIN`].
pub const CONTRAST_MAX: f32 = 1.0;

/// The gamma exponent the light-on-dark ramp is anchored on, so sliding off the
/// neutral detent does not JUMP.
///
/// egui's own settings UI equates `TwoCoverageMinusCoverageSq` with gamma 0.5
/// ("approximately the same"), but that is an approximation, and this module
/// does not repeat an approximation without saying so: at `c = 0.5`,
/// `2c − c² = 0.750` while `0.5^0.5 = 0.707` and `0.5^0.55 = 0.683`. 0.55 is
/// therefore a CHOICE, not a derivation — it trades a slightly lighter match at
/// mid-coverage for a closer one in the high-coverage range where stems live.
///
/// It is deliberately NOT asserted to be perceptually neutral. That claim can
/// only be settled by rendering `text_contrast = 0.00` against `+0.01` and
/// LOOKING at them; until that has been done the anchor is provisional, and
/// [`SETTLED`] is what stops any baseline being baked against it in the
/// meantime.
pub const ANCHOR_LIGHT_ON_DARK: f32 = 0.55;

/// The gamma exponent the dark-on-light ramp is anchored on.
///
/// Exactly `1.0`, which IS [`CoverageCurve::Linear`] — so unlike
/// [`ANCHOR_LIGHT_ON_DARK`] this branch is exactly continuous at the neutral
/// detent, with no approximation to justify.
pub const ANCHOR_DARK_ON_LIGHT: f32 = 1.00;

/// Revision of the curve PARAMETERS. Bumped by any change to the anchors or the
/// gamma bounds — see [`CURVE_FINGERPRINT`].
pub const CURVE_REV: u32 = 1;

/// A stable identifier for the exact curve parameters in force.
///
/// Rendered baselines are stored UNDER this string, so a parameter change makes
/// the old baseline directory unreachable rather than silently wrong. It is not
/// a hand-maintained label: [`fingerprint_from_parameters`] recomputes it from
/// the LIVE constants, and a test asserts the two agree — so a parameter edited
/// without a fingerprint bump fails in the ordinary, GPU-free suite.
pub const CURVE_FINGERPRINT: &str = "r1-anchor055-100-g045-160";

/// Whether the curve parameters are SETTLED — i.e. whether they have been
/// eyeballed on a rendered frame and accepted.
///
/// `false` while the curve is under design. It is the structural half of the
/// ordering guarantee: baselines bake ONCE, so a baseline generated against a
/// curve that is still moving silently encodes the old shape forever. While this
/// is `false`, `glyph_curve_gate::unsettled_curve_has_no_committed_baselines`
/// fails if any baseline exists in the tree at all.
///
/// Flipped to `true` by the commit that lands the eyeballed, accepted curve —
/// and by no other commit.
pub const SETTLED: bool = false;

/// Recompute [`CURVE_FINGERPRINT`] from the live parameter constants.
///
/// The whole point is that this reads the CONSTANTS rather than repeating their
/// values: changing `GAMMA_MIN` changes this string, which no longer matches the
/// committed [`CURVE_FINGERPRINT`], which fails a test. That is the mechanism
/// that makes "bump the revision when you change the curve" enforced rather than
/// remembered.
#[must_use]
pub fn fingerprint_from_parameters() -> String {
    format!(
        "r{}-anchor{}-{}-g{}-{}",
        CURVE_REV,
        centi(ANCHOR_LIGHT_ON_DARK),
        centi(ANCHOR_DARK_ON_LIGHT),
        centi(GAMMA_MIN),
        centi(GAMMA_MAX),
    )
}

/// A parameter rendered as zero-padded hundredths (`0.55` → `"055"`,
/// `1.60` → `"160"`), so the fingerprint is filename-safe and fixed-width.
fn centi(v: f32) -> String {
    format!("{:03}", (v * 100.0).round() as i64)
}

/// THE POLICY: which coverage curve to bake the atlas with, for a grid whose
/// text is `fg` on `bg`, at user contrast `contrast`.
///
/// Pure, GPU-free, and total — every input produces a curve.
///
/// # Polarity is decided from `fg` vs `bg`, not from "is the background light"
///
/// The app's existing light/dark chrome pivot asks `luminance(bg) > 0.5` using
/// Rec.601 weights on gamma-encoded sRGB. That is the right question for
/// "should the chrome be a light or dark base", and the wrong one here. The
/// question this function answers is "is the text lighter than what it sits on",
/// which is a direct comparison and has no pivot to fall the wrong side of.
///
/// For all 35 shipped themes the two agree, so this is robustness for IMPORTED
/// themes rather than a visible change: the `.itermcolors` import path can
/// produce a mid-luminance background where a `> 0.5` test is a coin-flip while
/// `fg vs bg` is exact.
///
/// It uses the WCAG [`relative_luminance`] core already owns, rather than a
/// second copy of Rec.601 — "is this lighter than that" is a luminance question
/// and there is one standard implementation of it in this crate.
///
/// # Edge cases, all deliberate
///
/// * `contrast == 0.0` returns the polarity default with NO gamma — the shipped
///   path, byte-identical to what was previously inherited.
/// * A non-finite `contrast` (NaN, ±∞) is a corrupt config value. It returns the
///   polarity DEFAULT rather than a clamped extreme: the safe failure for a
///   corrupt rendering parameter is the shipped default, not maximum ink.
/// * `fg` and `bg` of equal luminance is not light-on-dark, so it takes the
///   `Linear` branch. Such a theme renders invisible text regardless of curve;
///   the curve is not the thing that would save it.
#[must_use]
pub fn curve_for(fg: (u8, u8, u8), bg: (u8, u8, u8), contrast: f32) -> CoverageCurve {
    let light_on_dark = relative_luminance(fg) > relative_luminance(bg);
    let default = if light_on_dark {
        CoverageCurve::TwoCMinusCSq
    } else {
        CoverageCurve::Linear
    };
    if contrast == CONTRAST_NEUTRAL || !contrast.is_finite() {
        return default;
    }
    // Anchor the ramp's neutral point on the polarity default's effective
    // exponent, so moving off 0 slides rather than steps.
    let anchor = if light_on_dark {
        ANCHOR_LIGHT_ON_DARK
    } else {
        ANCHOR_DARK_ON_LIGHT
    };
    let t = contrast.clamp(CONTRAST_MIN, CONTRAST_MAX);
    // Positive contrast means HEAVIER ink, and heavier ink is a SMALLER
    // exponent (c^g rises faster as g falls, for c in 0..1).
    let g = if t > 0.0 {
        anchor - t * (anchor - GAMMA_MIN)
    } else {
        anchor + (-t) * (GAMMA_MAX - anchor)
    };
    CoverageCurve::Gamma(g.clamp(GAMMA_MIN, GAMMA_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two colours of the default `itasha-corp` theme, used as the
    /// light-on-dark case throughout.
    const DARK_BG: (u8, u8, u8) = (0x12, 0x12, 0x12);
    const LIGHT_FG: (u8, u8, u8) = (0xe8, 0xe6, 0xf0);
    /// `ghost-paper`, the shipped dark-on-light case.
    const PAPER_BG: (u8, u8, u8) = (0xf5, 0xf2, 0xea);
    const INK_FG: (u8, u8, u8) = (0x2b, 0x2b, 0x2b);

    // ----- G1: the fingerprint cannot drift from the parameters --------------

    /// ORDERING GATE G1 — the committed fingerprint is recomputed from the LIVE
    /// constants, so a parameter change that forgets to bump the revision fails
    /// here, in the ordinary GPU-free suite, before any baseline can be baked
    /// against it.
    #[test]
    fn fingerprint_matches_parameters() {
        assert_eq!(
            fingerprint_from_parameters(),
            CURVE_FINGERPRINT,
            "the curve PARAMETERS changed but CURVE_FINGERPRINT did not. Bump \
             CURVE_REV and update CURVE_FINGERPRINT in the same commit: rendered \
             baselines are stored under the fingerprint, so leaving it unchanged \
             would let a baseline baked against the OLD curve keep passing \
             against the new one."
        );
    }

    /// The fingerprint is only load-bearing if it actually MOVES when a
    /// parameter does. Recomputed here with one parameter perturbed, proving the
    /// format is sensitive to each of the four rather than merely well-formed.
    #[test]
    fn the_fingerprint_moves_when_any_parameter_moves() {
        let live = fingerprint_from_parameters();
        for (name, perturbed) in [
            (
                "ANCHOR_LIGHT_ON_DARK",
                format!(
                    "r{}-anchor{}-{}-g{}-{}",
                    CURVE_REV,
                    centi(ANCHOR_LIGHT_ON_DARK + 0.01),
                    centi(ANCHOR_DARK_ON_LIGHT),
                    centi(GAMMA_MIN),
                    centi(GAMMA_MAX)
                ),
            ),
            (
                "ANCHOR_DARK_ON_LIGHT",
                format!(
                    "r{}-anchor{}-{}-g{}-{}",
                    CURVE_REV,
                    centi(ANCHOR_LIGHT_ON_DARK),
                    centi(ANCHOR_DARK_ON_LIGHT + 0.01),
                    centi(GAMMA_MIN),
                    centi(GAMMA_MAX)
                ),
            ),
            (
                "GAMMA_MIN",
                format!(
                    "r{}-anchor{}-{}-g{}-{}",
                    CURVE_REV,
                    centi(ANCHOR_LIGHT_ON_DARK),
                    centi(ANCHOR_DARK_ON_LIGHT),
                    centi(GAMMA_MIN + 0.01),
                    centi(GAMMA_MAX)
                ),
            ),
            (
                "GAMMA_MAX",
                format!(
                    "r{}-anchor{}-{}-g{}-{}",
                    CURVE_REV,
                    centi(ANCHOR_LIGHT_ON_DARK),
                    centi(ANCHOR_DARK_ON_LIGHT),
                    centi(GAMMA_MIN),
                    centi(GAMMA_MAX + 0.01)
                ),
            ),
        ] {
            assert_ne!(
                live, perturbed,
                "the fingerprint does not change when {name} changes — it cannot \
                 protect a baseline from a parameter it is blind to"
            );
        }
        // And the revision itself.
        assert!(
            live.starts_with(&format!("r{CURVE_REV}-")),
            "the fingerprint must carry the revision so a deliberate rebake is \
             distinguishable from a parameter drift"
        );
    }

    // ----- the polarity policy ----------------------------------------------

    #[test]
    fn neutral_contrast_selects_the_polarity_default() {
        assert_eq!(
            curve_for(LIGHT_FG, DARK_BG, CONTRAST_NEUTRAL),
            CoverageCurve::TwoCMinusCSq,
            "light-on-dark must get the fattening curve at the shipped default"
        );
        assert_eq!(
            curve_for(INK_FG, PAPER_BG, CONTRAST_NEUTRAL),
            CoverageCurve::Linear,
            "dark-on-light must get the identity curve at the shipped default"
        );
    }

    /// The two branches must not be the same curve, or the polarity test in
    /// [`curve_for`] would be unfalsifiable — inverting it would change nothing.
    #[test]
    fn the_two_polarity_defaults_are_different_curves() {
        assert_ne!(
            curve_for(LIGHT_FG, DARK_BG, CONTRAST_NEUTRAL),
            curve_for(INK_FG, PAPER_BG, CONTRAST_NEUTRAL)
        );
    }

    /// Polarity follows fg-vs-bg, NOT "is the background light". This is the
    /// case the two disagree on: a mid-luminance background (the shape an
    /// `.itermcolors` import can produce) with text DARKER than it. A
    /// `luminance(bg) > 0.5` pivot is a coin-flip here; the direct comparison is
    /// exact.
    #[test]
    fn polarity_uses_fg_versus_bg_not_a_background_pivot() {
        let mid_bg = (0x80, 0x80, 0x80);
        assert_eq!(
            curve_for((0x10, 0x10, 0x10), mid_bg, CONTRAST_NEUTRAL),
            CoverageCurve::Linear,
            "dark text on a mid-grey background is dark-on-light"
        );
        assert_eq!(
            curve_for((0xf0, 0xf0, 0xf0), mid_bg, CONTRAST_NEUTRAL),
            CoverageCurve::TwoCMinusCSq,
            "light text on the SAME mid-grey background is light-on-dark — the \
             background alone cannot decide this"
        );
    }

    #[test]
    fn equal_luminance_is_not_light_on_dark() {
        let c = (0x77, 0x77, 0x77);
        assert_eq!(curve_for(c, c, CONTRAST_NEUTRAL), CoverageCurve::Linear);
    }

    // ----- the contrast ramp -------------------------------------------------

    #[test]
    fn positive_contrast_lowers_the_exponent_and_negative_raises_it() {
        let CoverageCurve::Gamma(heavy) = curve_for(LIGHT_FG, DARK_BG, 0.35) else {
            panic!("a non-neutral contrast must produce a Gamma curve");
        };
        let CoverageCurve::Gamma(light) = curve_for(LIGHT_FG, DARK_BG, -0.35) else {
            panic!("a non-neutral contrast must produce a Gamma curve");
        };
        assert!(
            heavy < ANCHOR_LIGHT_ON_DARK,
            "positive contrast means heavier ink, which is a SMALLER exponent \
             (got {heavy} against anchor {ANCHOR_LIGHT_ON_DARK})"
        );
        assert!(
            light > ANCHOR_LIGHT_ON_DARK,
            "negative contrast means lighter ink, which is a LARGER exponent \
             (got {light})"
        );
    }

    /// The ramp must be monotone across its whole range in BOTH polarities —
    /// a knob that reverses direction somewhere in the middle is unusable, and
    /// the sign asymmetry either side of the anchor is exactly where that could
    /// hide.
    #[test]
    fn the_ramp_is_monotone_across_its_whole_range_in_both_polarities() {
        for (name, fg, bg) in [
            ("light-on-dark", LIGHT_FG, DARK_BG),
            ("dark-on-light", INK_FG, PAPER_BG),
        ] {
            let mut prev = f32::INFINITY;
            for step in -100..=100 {
                let t = step as f32 / 100.0;
                let g = match curve_for(fg, bg, t) {
                    CoverageCurve::Gamma(g) => g,
                    // The neutral detent is the only non-Gamma point; substitute
                    // its anchor so the sequence stays comparable across it.
                    _ => {
                        if fg == LIGHT_FG {
                            ANCHOR_LIGHT_ON_DARK
                        } else {
                            ANCHOR_DARK_ON_LIGHT
                        }
                    }
                };
                assert!(
                    g <= prev + f32::EPSILON,
                    "{name}: the exponent rose from {prev} to {g} at contrast \
                     {t} — the ramp must fall monotonically as contrast rises"
                );
                prev = g;
            }
        }
    }

    /// The neutral detent must not be a visual STEP. The exponent immediately
    /// either side of 0 must be within a hair of the anchor, or a user nudging
    /// the slider off centre sees text jump.
    ///
    /// This checks the ARITHMETIC is continuous. Whether `ANCHOR_LIGHT_ON_DARK`
    /// is *perceptually* neutral against `TwoCMinusCSq` is a different question
    /// that only a rendered frame can answer — see [`SETTLED`].
    #[test]
    fn the_ramp_does_not_step_at_the_neutral_detent() {
        for (fg, bg, anchor) in [
            (LIGHT_FG, DARK_BG, ANCHOR_LIGHT_ON_DARK),
            (INK_FG, PAPER_BG, ANCHOR_DARK_ON_LIGHT),
        ] {
            for t in [0.001_f32, -0.001] {
                let CoverageCurve::Gamma(g) = curve_for(fg, bg, t) else {
                    panic!("contrast {t} is non-neutral and must produce a Gamma");
                };
                assert!(
                    (g - anchor).abs() < 0.01,
                    "at contrast {t} the exponent is {g}, {} away from the anchor \
                     {anchor} — the detent steps instead of sliding",
                    (g - anchor).abs()
                );
            }
        }
    }

    #[test]
    fn the_exponent_is_always_within_bounds_including_out_of_range_input() {
        for step in -300..=300 {
            let t = step as f32 / 100.0; // spans -3.0..=3.0, well outside the knob
            if let CoverageCurve::Gamma(g) = curve_for(LIGHT_FG, DARK_BG, t) {
                assert!(
                    (GAMMA_MIN..=GAMMA_MAX).contains(&g),
                    "contrast {t} produced exponent {g}, outside \
                     {GAMMA_MIN}..={GAMMA_MAX}"
                );
            }
        }
    }

    #[test]
    fn the_extremes_reach_exactly_the_bounds() {
        assert_eq!(
            curve_for(LIGHT_FG, DARK_BG, CONTRAST_MAX),
            CoverageCurve::Gamma(GAMMA_MIN)
        );
        assert_eq!(
            curve_for(LIGHT_FG, DARK_BG, CONTRAST_MIN),
            CoverageCurve::Gamma(GAMMA_MAX)
        );
        assert_eq!(
            curve_for(INK_FG, PAPER_BG, CONTRAST_MAX),
            CoverageCurve::Gamma(GAMMA_MIN)
        );
        assert_eq!(
            curve_for(INK_FG, PAPER_BG, CONTRAST_MIN),
            CoverageCurve::Gamma(GAMMA_MAX)
        );
    }

    /// A corrupt contrast must fall back to the SHIPPED default, not to an
    /// extreme. A NaN in a config file should not silently render every glyph at
    /// maximum ink.
    #[test]
    fn a_non_finite_contrast_falls_back_to_the_polarity_default() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                curve_for(LIGHT_FG, DARK_BG, bad),
                CoverageCurve::TwoCMinusCSq,
                "a non-finite contrast ({bad}) must render as the shipped default"
            );
        }
    }

    // ----- the alpha model ---------------------------------------------------

    #[test]
    fn alpha_at_clamps_its_input_and_spans_the_unit_range() {
        for curve in [
            CoverageCurve::Linear,
            CoverageCurve::TwoCMinusCSq,
            CoverageCurve::Gamma(GAMMA_MIN),
            CoverageCurve::Gamma(GAMMA_MAX),
        ] {
            assert_eq!(curve.alpha_at(0.0), 0.0, "{curve:?} at zero coverage");
            assert_eq!(curve.alpha_at(1.0), 1.0, "{curve:?} at full coverage");
            assert_eq!(curve.alpha_at(-5.0), 0.0, "{curve:?} clamps below zero");
            assert_eq!(curve.alpha_at(5.0), 1.0, "{curve:?} clamps above one");
        }
    }

    /// The whole point of the fattening curve: at partial coverage it must
    /// produce MORE alpha than the identity. If this were false the light-on-dark
    /// branch would be doing nothing.
    #[test]
    fn the_fattening_curve_out_inks_linear_at_every_partial_coverage() {
        for step in 1..100 {
            let c = step as f32 / 100.0;
            assert!(
                CoverageCurve::TwoCMinusCSq.alpha_at(c) > CoverageCurve::Linear.alpha_at(c),
                "at coverage {c} the fattening curve produced no more alpha than \
                 linear"
            );
        }
    }

    /// A smaller exponent must ink more at every partial coverage — the property
    /// the whole `contrast > 0 ⇒ smaller g` mapping rests on.
    #[test]
    fn a_smaller_exponent_inks_more_at_every_partial_coverage() {
        for step in 1..100 {
            let c = step as f32 / 100.0;
            assert!(
                CoverageCurve::Gamma(GAMMA_MIN).alpha_at(c)
                    > CoverageCurve::Gamma(GAMMA_MAX).alpha_at(c),
                "at coverage {c}, gamma {GAMMA_MIN} did not out-ink gamma \
                 {GAMMA_MAX} — the contrast knob's direction would be inverted"
            );
        }
    }

    /// `GAMMA_MIN` is documented as the point below which the curve exceeds the
    /// fattening of `TwoCMinusCSq` "at every coverage". That is a claim about the
    /// bound's meaning, so it is checked rather than asserted in prose.
    #[test]
    fn gamma_min_is_at_least_as_heavy_as_the_fattening_curve() {
        for step in 1..100 {
            let c = step as f32 / 100.0;
            assert!(
                CoverageCurve::Gamma(GAMMA_MIN).alpha_at(c)
                    >= CoverageCurve::TwoCMinusCSq.alpha_at(c) - 0.02,
                "at coverage {c}, GAMMA_MIN ({GAMMA_MIN}) inks materially less \
                 than TwoCMinusCSq — the bound's documented meaning is wrong"
            );
        }
    }
}
