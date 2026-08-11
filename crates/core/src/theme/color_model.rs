//! The terminal colour model: intense-text (bold-as-bright) policy, the WCAG
//! contrast primitives, and the minimum-contrast clamp.
//!
//! These are the knobs that decide *how* a resolved cell colour is adjusted
//! before it reaches the renderer. They live in `core` (not the app crate)
//! because they are protocol/model behaviour shared by every renderer — the app
//! crate's egui-flavoured `contrast_ratio(Color32, Color32)` is a chrome/focus-
//! ring helper over a different colour type and is deliberately left alone.
//!
//! # Why bold-as-bright exists
//!
//! Windows Terminal (and xterm, and most Linux consoles before it) default
//! `intenseTextStyle` to `bright`: SGR 1 (bold) combined with an *indexed*
//! colour 0-7 renders from the bright palette rows 8-15. PowerShell/PSReadLine,
//! git, cargo and npm all emit bold+colour constantly, so a terminal that does
//! not do this reads as visibly duller than Windows Terminal side-by-side. The
//! remap applies ONLY to indexed 0-7 — a 24-bit `Color::Rgb` is never touched.

/// How SGR 1 (bold / increased intensity) is rendered.
///
/// Mirrors Windows Terminal's `intenseTextStyle` setting. The default is
/// [`IntenseTextStyle::Bright`], matching Windows Terminal's own default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntenseTextStyle {
    /// Bold remaps an indexed 0-7 foreground to its bright twin 8-15, and does
    /// NOT ask the renderer for a bold font face. (Windows Terminal default.)
    #[default]
    Bright,
    /// Bold selects a bold font face only; the palette index is untouched.
    Bold,
    /// Bold does both: the bright-palette remap AND a bold font face.
    All,
    /// Bold is ignored entirely — neither a remap nor a bold face.
    None,
}

impl IntenseTextStyle {
    /// Whether bold should remap an indexed 0-7 foreground into the bright row.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::theme::IntenseTextStyle;
    ///
    /// assert!(IntenseTextStyle::Bright.remaps_to_bright());
    /// assert!(IntenseTextStyle::All.remaps_to_bright());
    /// assert!(!IntenseTextStyle::Bold.remaps_to_bright());
    /// assert!(!IntenseTextStyle::None.remaps_to_bright());
    /// ```
    pub const fn remaps_to_bright(self) -> bool {
        matches!(self, IntenseTextStyle::Bright | IntenseTextStyle::All)
    }

    /// Whether bold should additionally request a bold font face from the
    /// renderer.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::theme::IntenseTextStyle;
    ///
    /// assert!(IntenseTextStyle::Bold.uses_bold_face());
    /// assert!(IntenseTextStyle::All.uses_bold_face());
    /// assert!(!IntenseTextStyle::Bright.uses_bold_face());
    /// assert!(!IntenseTextStyle::None.uses_bold_face());
    /// ```
    pub const fn uses_bold_face(self) -> bool {
        matches!(self, IntenseTextStyle::Bold | IntenseTextStyle::All)
    }
}

/// Which cells the minimum-contrast clamp is allowed to touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContrastScope {
    /// Never clamp.
    Never,
    /// Clamp only cells whose foreground came from the ANSI/256 index space
    /// ([`crate::grid::Color::Indexed`]) or the theme default. This is the
    /// DEFAULT: a 24-bit `Color::Rgb` foreground is an explicit artistic choice
    /// by the program (gradient TUIs, image-to-ANSI renderers, `lolcat`) and
    /// clamping it visibly destroys the gradient.
    #[default]
    IndexedOnly,
    /// Clamp every cell, including 24-bit RGB foregrounds.
    Always,
}

/// The lowest ratio the WCAG formula can produce (identical colours). Using it
/// as the default threshold makes the clamp a structural no-op.
pub const CONTRAST_RATIO_MIN: f32 = 1.0;

/// The highest ratio the WCAG formula can produce (pure black on pure white).
pub const CONTRAST_RATIO_MAX: f32 = 21.0;

/// Tunable knobs for the cell colour model.
///
/// [`Default`] reproduces Windows Terminal's out-of-the-box behaviour:
/// bold-as-bright ON, minimum-contrast clamp OFF.
///
/// # Examples
///
/// ```
/// use c0pl4nd_core::theme::{ColorOptions, ContrastScope, IntenseTextStyle};
///
/// let d = ColorOptions::default();
/// assert_eq!(d.intense_text_style, IntenseTextStyle::Bright);
/// // A threshold of 1.0 can never fail, so the clamp is off by default.
/// assert_eq!(d.min_contrast_ratio, 1.0);
/// assert_eq!(d.contrast_scope, ContrastScope::IndexedOnly);
/// assert!(!d.contrast_clamp_active());
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorOptions {
    /// How SGR 1 is rendered. Defaults to [`IntenseTextStyle::Bright`].
    pub intense_text_style: IntenseTextStyle,
    /// The WCAG contrast ratio a cell's foreground must reach against its
    /// effective background. Clamped into `1.0..=21.0` on use; `1.0` (the
    /// default) disables the clamp because every colour pair already meets it.
    ///
    /// The default is OFF deliberately. Windows Terminal ships
    /// `minimumContrastRatio: 0`, and a clamp that is on by default silently
    /// rewrites colours the program explicitly asked for — a worse failure than
    /// the low-contrast cell it fixes. Enabling it is a user decision.
    pub min_contrast_ratio: f32,
    /// Which foregrounds the clamp may touch. Defaults to
    /// [`ContrastScope::IndexedOnly`] so 24-bit gradient output is preserved
    /// even when the clamp is switched on.
    pub contrast_scope: ContrastScope,
    /// How far a dim (SGR 2) foreground is blended toward its background, in
    /// `0.0..=1.0`. `0.0` leaves the colour untouched; `1.0` makes it invisible.
    /// The default `0.5` matches the conventional "half intensity" reading of
    /// ECMA-48 SGR 2 and works in both light and dark themes (unlike a plain
    /// multiply, which *raises* contrast on a light background).
    pub dim_blend: f32,
}

impl Default for ColorOptions {
    fn default() -> Self {
        ColorOptions {
            intense_text_style: IntenseTextStyle::Bright,
            min_contrast_ratio: CONTRAST_RATIO_MIN,
            contrast_scope: ContrastScope::IndexedOnly,
            dim_blend: 0.5,
        }
    }
}

impl ColorOptions {
    /// Whether the minimum-contrast clamp can change any colour under these
    /// options. False when the scope is [`ContrastScope::Never`] or the
    /// threshold is at (or below) the structural minimum.
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::theme::{ColorOptions, ContrastScope};
    ///
    /// let mut o = ColorOptions::default();
    /// assert!(!o.contrast_clamp_active());
    /// o.min_contrast_ratio = 4.5;
    /// assert!(o.contrast_clamp_active());
    /// o.contrast_scope = ContrastScope::Never;
    /// assert!(!o.contrast_clamp_active());
    /// ```
    pub fn contrast_clamp_active(&self) -> bool {
        self.contrast_scope != ContrastScope::Never && self.min_contrast_ratio > CONTRAST_RATIO_MIN
    }
}

/// One sRGB channel linearised per the WCAG 2.x relative-luminance definition.
fn linearize(channel: u8) -> f32 {
    let c = channel as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.x relative luminance of an opaque sRGB colour, in `0.0..=1.0`.
///
/// # Examples
///
/// ```
/// use c0pl4nd_core::theme::relative_luminance;
///
/// assert!(relative_luminance((0, 0, 0)).abs() < 1e-6);
/// assert!((relative_luminance((255, 255, 255)) - 1.0).abs() < 1e-6);
/// // Green dominates the luminance weighting.
/// assert!(relative_luminance((0, 255, 0)) > relative_luminance((255, 0, 0)));
/// ```
pub fn relative_luminance(rgb: (u8, u8, u8)) -> f32 {
    0.2126 * linearize(rgb.0) + 0.7152 * linearize(rgb.1) + 0.0722 * linearize(rgb.2)
}

/// WCAG 2.x contrast ratio between two opaque sRGB colours: `1.0` for identical
/// colours up to `21.0` for pure black against pure white.
///
/// # Examples
///
/// ```
/// use c0pl4nd_core::theme::contrast_ratio;
///
/// assert!((contrast_ratio((0, 0, 0), (255, 255, 255)) - 21.0).abs() < 0.01);
/// assert!((contrast_ratio((9, 9, 9), (9, 9, 9)) - 1.0).abs() < 1e-5);
/// // Symmetric in its arguments.
/// let (a, b) = ((12, 200, 90), (40, 10, 77));
/// assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 1e-5);
/// ```
pub fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Linear blend from `from` toward `to`; `t == 0.0` yields `from`, `t == 1.0`
/// yields `to`. `t` is clamped into `0.0..=1.0`.
fn blend(from: (u8, u8, u8), to: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| -> u8 {
        // `round` then clamp keeps the result inside u8 for every finite `t`.
        (a as f32 + (b as f32 - a as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// Apply the "dim" (SGR 2) rendition: blend the foreground `t` of the way
/// toward its effective background.
///
/// # Examples
///
/// ```
/// use c0pl4nd_core::theme::dim_foreground;
///
/// // Halfway between white text and black background.
/// assert_eq!(dim_foreground((255, 255, 255), (0, 0, 0), 0.5), (128, 128, 128));
/// // A blend of 0 is a no-op; a blend of 1 collapses onto the background.
/// assert_eq!(dim_foreground((10, 20, 30), (0, 0, 0), 0.0), (10, 20, 30));
/// assert_eq!(dim_foreground((10, 20, 30), (1, 2, 3), 1.0), (1, 2, 3));
/// ```
pub fn dim_foreground(fg: (u8, u8, u8), bg: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    blend(fg, bg, t)
}

/// Number of blend steps the clamp walks between the original foreground and
/// the contrast pole. 32 steps is ~3 sRGB levels of granularity per channel at
/// the extremes — finer than the eye resolves, and bounded so the scan can
/// never spin.
const CLAMP_STEPS: u32 = 32;

/// Raise `fg` against `bg` until it meets `target` WCAG contrast, by blending
/// toward whichever monochrome pole (black or white) is further from `bg`.
///
/// Returns `fg` unchanged when it already meets `target`. When even the pole
/// cannot reach `target` the pole itself is returned — the most legible colour
/// available — rather than failing or leaving the unreadable original.
///
/// That fallthrough is far more reachable than it looks, and this doc used to
/// say so wrongly: it claimed a mid-grey background "caps out around 10.4:1",
/// which overstates the real ceiling by ~2.3x. Measured over the whole sRGB
/// cube on this toolchain, the best ratio ANY foreground can reach is
/// **4.6075:1** against grey 117, and the floor across all 16 777 216
/// backgrounds is **4.5826:1** at `(3, 137, 1)`. So a 7.0 target is unreachable
/// for the 59 greys `90..=148`, and a 21.0 target is unreachable for EVERY
/// background — including pure black, because `contrast_ratio(white, black)` is
/// 20.999998f32, not 21.0f32.
///
/// The practical consequence is that a caller must NOT assume the returned
/// colour meets `target`; on this branch, by design, it does not. The same
/// mistake shows up in test design: a greyscale sweep run only at 4.5 never
/// enters this branch at all (measured over the full 256x256 grid: 20 042
/// no-op, 45 494 lift, **0** unreachable).
///
/// # Examples
///
/// ```
/// use c0pl4nd_core::theme::{contrast_ratio, enforce_min_contrast};
///
/// // Already legible: returned untouched.
/// let fg = (255, 255, 255);
/// assert_eq!(enforce_min_contrast(fg, (0, 0, 0), 4.5), fg);
///
/// // Dark grey on black is illegible; the clamp lifts it toward white.
/// let lifted = enforce_min_contrast((40, 40, 40), (0, 0, 0), 4.5);
/// assert!(contrast_ratio(lifted, (0, 0, 0)) >= 4.5);
/// ```
pub fn enforce_min_contrast(fg: (u8, u8, u8), bg: (u8, u8, u8), target: f32) -> (u8, u8, u8) {
    let target = target.clamp(CONTRAST_RATIO_MIN, CONTRAST_RATIO_MAX);
    if contrast_ratio(fg, bg) >= target {
        return fg;
    }
    // Move away from the background: toward white if the background is dark,
    // toward black if it is light. 0.1791 is the luminance at which black and
    // white contrast equally against a colour ((1.05/0.05).sqrt() solved for L),
    // TRUNCATED to four decimals: the exact root is 0.17912878474779198, so the
    // literal sits 2.878e-5 below it. "Always picks the higher-headroom pole" is
    // therefore not quite true, and this comment used to claim it was — for the
    // 1004 sRGB colours whose luminance lands in [0.1791, 0.17912878) the `else`
    // branch is taken and BLACK is chosen while white is fractionally better.
    // The difference is under 1.2e-3 of contrast: invisible, not worth widening
    // the literal for, but a property NOT to assert in a test. Pin the
    // documented pivot; a sweep asserting "the better pole" fails on 1004 of
    // 16 777 216 inputs and reads as a bug.
    let pole = if relative_luminance(bg) < 0.1791 {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    };
    for step in 1..=CLAMP_STEPS {
        let candidate = blend(fg, pole, step as f32 / CLAMP_STEPS as f32);
        if contrast_ratio(candidate, bg) >= target {
            return candidate;
        }
    }
    pole
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_hits_the_wcag_reference_points() {
        assert!(relative_luminance((0, 0, 0)).abs() < 1e-6);
        assert!((relative_luminance((255, 255, 255)) - 1.0).abs() < 1e-6);
        // The three primaries carry exactly the WCAG coefficient weights.
        assert!((relative_luminance((255, 0, 0)) - 0.2126).abs() < 1e-4);
        assert!((relative_luminance((0, 255, 0)) - 0.7152).abs() < 1e-4);
        assert!((relative_luminance((0, 0, 255)) - 0.0722).abs() < 1e-4);
    }

    /// The LINEAR branch of `linearize`, which only channels `0..=10` reach.
    ///
    /// The reference-point test above samples 0 and 255 only: 255 takes the
    /// `powf` branch, and 0 is a FIXED POINT of this branch (`0 / 12.92 == 0`),
    /// so neither can observe the `/ 12.92` divide at all. Three OTHER tests do
    /// execute the line with a discriminating channel and still cannot fail —
    /// the `(9,9,9)` doctest compares a colour with itself (ratio 1.0 for any
    /// luminance), the symmetry doctest perturbs both orderings identically, and
    /// `clamp_is_a_no_op_when_the_pair_already_passes` uses a 1.0 target that
    /// both the true and the inflated ratio clear. Executing a line is not
    /// asserting on it.
    ///
    /// WCAG defines the low branch as `c / 12.92`. Replacing `/` with `%`
    /// returns `c` itself (`c <= 0.04045 < 12.92`, so the remainder IS `c`),
    /// inflating near-black luminance ~13x — which silently changes whether the
    /// contrast clamp fires on the near-black backgrounds terminal themes use.
    #[test]
    fn the_linear_branch_divides_by_the_wcag_slope() {
        // Channel 10 is the LARGEST value on the linear branch
        // (10/255 = 0.03922 <= 0.04045 < 11/255). Grey, so the three luminance
        // weights sum to 1 and L is just the channel's linearised value:
        // (10/255) / 12.92 = 0.00303528.
        let l = relative_luminance((10, 10, 10));
        assert!(
            (l - 0.0030353).abs() < 1e-6,
            "grey-10 luminance was {l}, expected the WCAG linear-branch value"
        );
        // Stated again as an inequality, so the assertion cannot be satisfied by
        // an identity: the branch must SHRINK the channel by the 12.92 slope.
        assert!(
            l < 10.0 / 255.0 / 10.0,
            "the linear branch must divide by 12.92, not pass the channel \
             through unchanged (got {l})"
        );
        // And the branch boundary is where it claims to be: channel 11 crosses
        // onto the (unmutated) powf branch and must still be brighter.
        assert!(
            relative_luminance((11, 11, 11)) > l,
            "channel 11 sits above channel 10 on the luminance curve"
        );
    }

    #[test]
    fn contrast_ratio_is_bounded_symmetric_and_exact_at_the_poles() {
        assert!((contrast_ratio((0, 0, 0), (255, 255, 255)) - 21.0).abs() < 0.01);
        assert!((contrast_ratio((255, 255, 255), (0, 0, 0)) - 21.0).abs() < 0.01);
        assert!((contrast_ratio((77, 77, 77), (77, 77, 77)) - 1.0).abs() < 1e-5);
        // Every pair stays inside the mathematical bounds.
        for r in [0u8, 63, 127, 191, 255] {
            for g in [0u8, 127, 255] {
                let a = (r, g, 30);
                let b = (g, 30, r);
                let ratio = contrast_ratio(a, b);
                assert!(
                    (CONTRAST_RATIO_MIN..=CONTRAST_RATIO_MAX).contains(&ratio),
                    "ratio {ratio} out of bounds for {a:?}/{b:?}"
                );
            }
        }
    }

    #[test]
    fn dim_blend_endpoints_and_midpoint_are_exact() {
        assert_eq!(
            dim_foreground((200, 100, 50), (0, 0, 0), 0.0),
            (200, 100, 50)
        );
        assert_eq!(dim_foreground((200, 100, 50), (4, 6, 8), 1.0), (4, 6, 8));
        // Exact midpoint, with .5 rounding away from zero.
        assert_eq!(
            dim_foreground((10, 20, 30), (20, 40, 60), 0.5),
            (15, 30, 45)
        );
        // Out-of-range blends clamp rather than overshoot into a wrapped u8.
        assert_eq!(dim_foreground((10, 20, 30), (0, 0, 0), -5.0), (10, 20, 30));
        assert_eq!(dim_foreground((10, 20, 30), (0, 0, 0), 5.0), (0, 0, 0));
    }

    #[test]
    fn dim_lowers_contrast_on_both_dark_and_light_backgrounds() {
        // The blend-toward-background definition is direction-correct in both
        // polarities — a plain multiply would RAISE contrast on a light theme.
        let dark_bg = (0, 0, 0);
        let light_bg = (255, 255, 255);
        let on_dark = dim_foreground((255, 255, 255), dark_bg, 0.5);
        let on_light = dim_foreground((0, 0, 0), light_bg, 0.5);
        assert!(contrast_ratio(on_dark, dark_bg) < contrast_ratio((255, 255, 255), dark_bg));
        assert!(contrast_ratio(on_light, light_bg) < contrast_ratio((0, 0, 0), light_bg));
    }

    /// The `>= target` EARLY RETURN, exercised on the path it is named for.
    ///
    /// RIGHT OUTCOME, WRONG REASON. This test used to open with
    /// `enforce(white, black, 21.0) == white`. `contrast_ratio(white, black)` is
    /// 20.999998f32, strictly BELOW the 21.0 ceiling, so the early return never
    /// fired: the call walked all 32 blend steps, failed every one, and fell
    /// through to `pole` — which for a black background happens to BE white. The
    /// expected colour arrived down the UNREACHABLE path, so the assertion could
    /// not observe the no-op gate at all. That case is a genuine unreachable
    /// fixture and now lives with the other unreachable ones.
    ///
    /// Two things make the replacement able to fail:
    ///
    /// 1. `fg` is deliberately NOT a pole. Against a pole foreground every blend
    ///    step is a no-change, so a pole `fg` cannot distinguish the early return
    ///    from a scan that succeeds on step 1 — the same blind spot in a
    ///    different costume.
    /// 2. `fg == bg` at `CONTRAST_RATIO_MIN`. `contrast_ratio(x, x)` is EXACTLY
    ///    1.0f32, which is the only way to reach `>=` AT equality and therefore
    ///    the only way to observe it widened to `>`. The `(10,10,10)`-on-black
    ///    fixture below is 1.0607:1 — strictly greater, so it never touches the
    ///    boundary. Measured: with `>=` mutated to `>`, this test used to pass.
    #[test]
    fn clamp_is_a_no_op_when_the_pair_already_passes() {
        // (1) A non-pole foreground that comfortably clears the target (12.55:1)
        // must come back BYTE-IDENTICAL, not merely still-passing: a lift toward
        // white would also "still pass" while silently rewriting the colour.
        let bright = (200, 200, 200);
        assert!(
            contrast_ratio(bright, (0, 0, 0)) >= 4.5,
            "premise: {bright:?} on black must already pass 4.5 for this to be \
             a no-op case at all"
        );
        assert_eq!(
            enforce_min_contrast(bright, (0, 0, 0), 4.5),
            bright,
            "an already-passing pair must be returned unchanged"
        );

        // (2) The equality boundary, the sole killer of a `>=` widened to `>`.
        for same in [(0, 0, 0), (128, 128, 128), (200, 30, 90), (255, 255, 255)] {
            assert_eq!(
                contrast_ratio(same, same),
                CONTRAST_RATIO_MIN,
                "premise: a colour against itself is exactly 1.0:1"
            );
            assert_eq!(
                enforce_min_contrast(same, same, CONTRAST_RATIO_MIN),
                same,
                "the floor is met AT equality, so {same:?} must survive untouched"
            );
        }

        // A target at or below the structural minimum can never trigger.
        let dull = (10, 10, 10);
        assert_eq!(enforce_min_contrast(dull, (0, 0, 0), 1.0), dull);
        assert_eq!(enforce_min_contrast(dull, (0, 0, 0), 0.0), dull);
    }

    #[test]
    fn clamp_reaches_the_target_and_picks_the_right_pole() {
        // Dark-on-dark lifts toward WHITE (every channel goes up).
        let lifted = enforce_min_contrast((30, 30, 40), (0, 0, 0), 7.0);
        assert!(contrast_ratio(lifted, (0, 0, 0)) >= 7.0);
        assert!(lifted.0 > 30 && lifted.1 > 30 && lifted.2 > 40);
        // Light-on-light darkens toward BLACK (every channel goes down).
        let darkened = enforce_min_contrast((230, 230, 220), (255, 255, 255), 7.0);
        assert!(contrast_ratio(darkened, (255, 255, 255)) >= 7.0);
        assert!(darkened.0 < 230 && darkened.1 < 230 && darkened.2 < 220);
    }

    /// THE LIFT MUST BE MINIMAL, not merely sufficient.
    ///
    /// Every other assertion about this function is satisfied by returning the
    /// POLE — pure white on a dark background, pure black on a light one. Pure
    /// white passes "contrast >= target", passes "every channel moved away from
    /// the background", and is obviously not the untouched original. So a clamp
    /// that slammed every low-contrast colour to a pole would have shipped
    /// green, taking the theme's hue with it: `#405060` and `#602030` would both
    /// come back `#ffffff`.
    ///
    /// That is not hypothetical. `step as f32 / CLAMP_STEPS as f32` mutated to
    /// `*` (or `%`) makes step 1 yield `t >= 1`, `blend` clamps to the pole, and
    /// the function returns it on the FIRST iteration — passing every test that
    /// existed before this one.
    ///
    /// Minimality is asserted WITHOUT reimplementing the search: re-blend the
    /// PREVIOUS step and require that it FAILS the target. If the returned
    /// colour is the first blend that passes, the one before it must not — that
    /// is the definition, and it pins the step size, the loop bounds and the
    /// direction all at once.
    #[test]
    fn the_clamp_lifts_the_minimum_distance_not_all_the_way_to_the_pole() {
        // Cases chosen so a genuine minimal lift lands well short of the pole.
        for &(fg, bg, target) in &[
            ((40, 40, 40), (0, 0, 0), 4.5_f32),
            ((30, 30, 40), (0, 0, 0), 7.0),
            ((60, 20, 20), (0, 0, 0), 4.5),
            ((230, 230, 220), (255, 255, 255), 7.0),
            ((200, 210, 200), (255, 255, 255), 4.5),
        ] {
            let out = enforce_min_contrast(fg, bg, target);
            let pole = if relative_luminance(bg) < 0.1791 {
                (255, 255, 255)
            } else {
                (0, 0, 0)
            };

            // Premise: the pole itself must clear the target, or "stopped short
            // of the pole" would be vacuous (an unreachable target legitimately
            // returns the pole — that path is covered by the sibling test).
            assert!(
                contrast_ratio(pole, bg) >= target,
                "premise: {target} must be reachable for fg={fg:?} bg={bg:?}"
            );
            assert!(
                contrast_ratio(out, bg) >= target,
                "the lift must actually reach the target"
            );
            assert_ne!(
                out, pole,
                "the clamp went ALL THE WAY to the pole {pole:?} for fg={fg:?} \
                 bg={bg:?} target={target} — that destroys the colour's hue and \
                 is what a broken step size looks like"
            );

            // The load-bearing half: find which step produced `out`, and prove
            // the step BEFORE it does not clear the target.
            let step = (1..=CLAMP_STEPS)
                .find(|&s| blend(fg, pole, s as f32 / CLAMP_STEPS as f32) == out)
                .expect("the result must be one of the blend steps");
            assert!(
                step >= 1,
                "a returned colour must come from a real lift step"
            );
            let previous = blend(fg, pole, (step - 1) as f32 / CLAMP_STEPS as f32);
            assert!(
                contrast_ratio(previous, bg) < target,
                "NOT MINIMAL: step {step} was returned for fg={fg:?} bg={bg:?} \
                 target={target}, but step {} ({previous:?}) already reaches \
                 {:.3} — the clamp overshot",
                step - 1,
                contrast_ratio(previous, bg)
            );
        }
    }

    #[test]
    fn clamp_returns_the_best_available_pole_when_the_target_is_unreachable() {
        // A mid-grey background caps out near 10.4:1 against either pole, so a
        // 21:1 demand is unreachable — the clamp must still return the most
        // legible colour, never the unreadable original and never a panic.
        let bg = (119, 119, 119);
        let out = enforce_min_contrast((120, 120, 120), bg, 21.0);
        assert!(out == (0, 0, 0) || out == (255, 255, 255));
        assert!(contrast_ratio(out, bg) > contrast_ratio((120, 120, 120), bg));
    }

    #[test]
    fn clamp_target_is_clamped_into_the_legal_ratio_range() {
        // A caller passing a nonsense target must not produce a nonsense colour:
        // 1000.0 behaves exactly like the 21.0 ceiling.
        let bg = (0, 0, 0);
        assert_eq!(
            enforce_min_contrast((30, 30, 30), bg, 1000.0),
            enforce_min_contrast((30, 30, 30), bg, CONTRAST_RATIO_MAX)
        );
    }

    #[test]
    fn intense_text_style_matrix_is_exhaustive_and_wt_compatible() {
        // The four-way matrix, pinned: a mutant that swaps a branch is caught.
        assert_eq!(IntenseTextStyle::default(), IntenseTextStyle::Bright);
        let cases = [
            (IntenseTextStyle::Bright, true, false),
            (IntenseTextStyle::Bold, false, true),
            (IntenseTextStyle::All, true, true),
            (IntenseTextStyle::None, false, false),
        ];
        for (style, remap, face) in cases {
            assert_eq!(style.remaps_to_bright(), remap, "{style:?} remap");
            assert_eq!(style.uses_bold_face(), face, "{style:?} face");
        }
    }

    #[test]
    fn default_options_match_windows_terminal_and_disable_the_clamp() {
        let d = ColorOptions::default();
        assert_eq!(d.intense_text_style, IntenseTextStyle::Bright);
        assert_eq!(d.min_contrast_ratio, CONTRAST_RATIO_MIN);
        assert_eq!(d.contrast_scope, ContrastScope::IndexedOnly);
        assert_eq!(d.dim_blend, 0.5);
        assert!(!d.contrast_clamp_active(), "the clamp ships OFF");
    }

    /// The pole-selection pivot is a STRICT `<`.
    ///
    /// CORRECTS THE RECORD. Commit 80663c5 declared this mutant EQUIVALENT and
    /// deliberately left it unchased, reasoning that it "changes behaviour only
    /// when a background's relative luminance is exactly 0.1791, where black and
    /// white contrast equally by construction". The second clause is false, and
    /// the conclusion drawn from it is false. Measured on the witness below:
    /// black contrasts 4.5820:1 and white 4.5831513:1 — close, but NOT equal
    /// (delta 1.1516e-3), so the tie-break is decided, not arbitrary. And the
    /// two programs do not return near-identical colours: at a 4.5 target the
    /// strict `<` returns `(0, 0, 0)` and the `<=` mutant returns
    /// `(255, 255, 255)`. That is the entire representable span, pure black
    /// versus pure white — the most visible difference this function can
    /// produce. The mutant is observable and killable, so it is killed here
    /// rather than pardoned.
    ///
    /// `relative_luminance(bg) < 0.1791` picks white BELOW the pivot and black
    /// AT OR ABOVE it, so widening it to `<=` changes the chosen pole for
    /// exactly one class of background: one whose luminance lands EXACTLY on the
    /// pivot in f32. Every other test of this function uses `(0,0,0)` or
    /// `(255,255,255)` backgrounds, nowhere near it.
    ///
    /// Over all 16 777 216 sRGB triples exactly ONE lands on the pivot —
    /// `(64, 113, 217)`, confirmed by an exhaustive f32 sweep under two
    /// independent `powf` implementations (native f32, and f64 rounded to f32),
    /// with every other triple at least 1 ULP away. But that constant is a
    /// product of `f32::powf`, which binds the PLATFORM libm, so it is used only
    /// as a first guess: if it misses, the witness is SEARCHED FOR at run time.
    /// A hard-coded constant that silently stopped being pivotal on some libm
    /// would turn this test vacuous while leaving it green, which is the exact
    /// failure the search exists to prevent.
    #[test]
    fn the_pole_pivot_is_strict_so_an_exactly_pivotal_background_darkens() {
        const PIVOT: f32 = 0.1791;

        // `linearize` is non-decreasing in its channel and f32 addition is
        // monotonic, so for any (r, b) the green channel is binary-searchable
        // for the first value whose luminance reaches the pivot. That bounds the
        // fallback search at 256*256*8 evaluations instead of 2^24.
        fn find_pivotal_background() -> Option<(u8, u8, u8)> {
            let guess = (64u8, 113u8, 217u8);
            if relative_luminance(guess) == PIVOT {
                return Some(guess);
            }
            for r in 0..=255u8 {
                for b in 0..=255u8 {
                    let (mut lo, mut hi) = (0u16, 255u16);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        if relative_luminance((r, mid as u8, b)) < PIVOT {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    let candidate = (r, lo as u8, b);
                    if relative_luminance(candidate) == PIVOT {
                        return Some(candidate);
                    }
                }
            }
            None
        }

        // Premise, asserted loudly: without a witness the comparison has no
        // observable strictness at all and the assertions below would be
        // vacuous. A silently vacuous test is worse than no test.
        let bg = find_pivotal_background().expect(
            "premise: no sRGB colour lands exactly on the 0.1791 pole pivot \
             under this platform's powf, so pole selection has no observable \
             boundary to pin",
        );

        // At the pivot the two candidate poles contrast almost identically
        // against `bg` (black 4.582:1, white 4.583:1) — that is what makes it
        // the pivot — so BOTH clear a 4.5 target and the tie is broken by the
        // comparison alone: `<` is false at equality, so the pole is BLACK.
        let out = enforce_min_contrast(bg, bg, 4.5);
        assert!(
            contrast_ratio(out, bg) >= 4.5,
            "the clamp must still reach the target for bg={bg:?}, got {out:?}"
        );
        assert!(
            relative_luminance(out) < relative_luminance(bg),
            "a background exactly ON the pivot must clamp toward BLACK; \
             bg={bg:?} produced {out:?}"
        );
    }

    #[test]
    fn contrast_clamp_active_needs_both_a_scope_and_a_real_threshold() {
        let mut o = ColorOptions::default();
        assert!(!o.contrast_clamp_active());
        o.min_contrast_ratio = 4.5;
        assert!(o.contrast_clamp_active());
        o.contrast_scope = ContrastScope::Always;
        assert!(o.contrast_clamp_active());
        o.contrast_scope = ContrastScope::Never;
        assert!(!o.contrast_clamp_active(), "Never wins over any threshold");
        o.contrast_scope = ContrastScope::IndexedOnly;
        o.min_contrast_ratio = CONTRAST_RATIO_MIN;
        assert!(!o.contrast_clamp_active(), "a 1.0 threshold is a no-op");
    }
}
