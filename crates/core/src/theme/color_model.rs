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
/// cannot reach `target` (a mid-grey background caps out around 10.4:1) the
/// pole itself is returned — the most legible colour available — rather than
/// failing or leaving the unreadable original.
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
    // toward black if it is light. The 0.1791 pivot is the luminance at which
    // black and white contrast equally against a colour ((1.05/0.05).sqrt()
    // solved for L), so this always picks the higher-headroom pole.
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

    #[test]
    fn clamp_is_a_no_op_when_the_pair_already_passes() {
        let fg = (255, 255, 255);
        assert_eq!(enforce_min_contrast(fg, (0, 0, 0), 21.0), fg);
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
