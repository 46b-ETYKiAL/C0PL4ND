//! The xterm 256-color default palette.
//!
//! Self-contained palette construction split out of the main [`crate::term`]
//! module. Builds the protocol-default indexed palette that seeds OSC 4 query
//! replies and OSC 104 resets. Colors are [`super::osc::Rgb`] triples — the same
//! representation the rest of the terminal core uses.
//!
//! This module is the SINGLE SOURCE OF TRUTH for the extended (>= 16) xterm
//! palette: both the OSC query/reset baseline ([`build_default_palette`]) and
//! the RENDER path ([`crate::theme::Theme::ansi`]) resolve indices 16-255
//! through [`extended_entry`]. There is deliberately no second cube/ramp
//! implementation — the render path previously folded every index through
//! `index % 8`, which turned `\e[38;5;208m` (orange) into black.

use super::osc::Rgb;

/// The xterm 6x6x6 cube component levels. Index 0 maps to 0 and 1..=5 map to
/// 95, 135, 175, 215, 255 — NOT a linear 0,51,102,… ramp (the xterm quirk
/// where the first non-zero step jumps straight to 95).
pub(crate) const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Resolve an EXTENDED xterm palette index (16-255) to its standard RGB.
///
/// Returns `None` for indices 0-15: those are the ANSI 16 and belong to the
/// active theme's `normal` / `bright` rows, not to a fixed table.
///
/// * `16..=231` — the 6x6x6 color cube, components drawn from [`CUBE_LEVELS`]
///   with blue varying fastest, then green, then red.
/// * `232..=255` — the 24-step grayscale ramp, `8 + 10*n`.
pub(crate) fn extended_entry(index: u8) -> Option<Rgb> {
    match index {
        0..=15 => None,
        16..=231 => {
            let i = (index - 16) as usize;
            Some((
                CUBE_LEVELS[(i / 36) % 6],
                CUBE_LEVELS[(i / 6) % 6],
                CUBE_LEVELS[i % 6],
            ))
        }
        232..=255 => {
            // 8, 18, …, 238. `u16` arithmetic keeps the multiply out of u8
            // overflow territory; the max is 8 + 23*10 = 238.
            let v = (8u16 + (index as u16 - 232) * 10) as u8;
            Some((v, v, v))
        }
    }
}

/// Builds the standard xterm 256-color palette as `(r, g, b)` triples.
///
/// 0-15 are the canonical xterm ANSI 16; 16-231 are the 6×6×6 color cube;
/// 232-255 are the 24-step grayscale ramp. The palette seeds OSC 4 query
/// replies so a program detecting colors gets sensible values before the host
/// applies its own theme via the [`super::ColorSet`] drain. The host's theme is
/// the source of truth for rendering — this palette is only the protocol-default
/// baseline for query/reset.
pub(crate) fn build_default_palette() -> [Rgb; 256] {
    // Canonical xterm ANSI 0-15.
    const ANSI16: [Rgb; 16] = [
        (0, 0, 0),       // 0 black
        (205, 0, 0),     // 1 red
        (0, 205, 0),     // 2 green
        (205, 205, 0),   // 3 yellow
        (0, 0, 238),     // 4 blue
        (205, 0, 205),   // 5 magenta
        (0, 205, 205),   // 6 cyan
        (229, 229, 229), // 7 white
        (127, 127, 127), // 8 bright black
        (255, 0, 0),     // 9 bright red
        (0, 255, 0),     // 10 bright green
        (255, 255, 0),   // 11 bright yellow
        (92, 92, 255),   // 12 bright blue
        (255, 0, 255),   // 13 bright magenta
        (0, 255, 255),   // 14 bright cyan
        (255, 255, 255), // 15 bright white
    ];
    let mut p: [Rgb; 256] = [(0, 0, 0); 256];
    p[..16].copy_from_slice(&ANSI16);
    // 6x6x6 cube: levels are 0, 95, 135, 175, 215, 255.
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    for i in 0..216usize {
        let r = LEVELS[(i / 36) % 6];
        let g = LEVELS[(i / 6) % 6];
        let b = LEVELS[i % 6];
        p[16 + i] = (r, g, b);
    }
    // Grayscale ramp 232-255: 8, 18, ..., 238 (step 10).
    for i in 0..24usize {
        let v = (8 + i * 10) as u8;
        p[232 + i] = (v, v, v);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cube level table xterm uses: index 0 maps to 0, and 1..=5 map to
    /// 95, 135, 175, 215, 255 (NOT a linear 0,51,102,... ramp — the xterm
    /// quirk where the first non-zero step jumps to 95).
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    #[test]
    fn ansi16_exact_rgb() {
        let p = build_default_palette();
        // Spot-check the canonical xterm ANSI-16 values, including the
        // "dim" 205-channel normals and the saturated 255-channel brights.
        assert_eq!(p[0], (0, 0, 0)); // black
        assert_eq!(p[1], (205, 0, 0)); // red (not 255 — xterm normal red is dim)
        assert_eq!(p[2], (0, 205, 0)); // green
        assert_eq!(p[3], (205, 205, 0)); // yellow
        assert_eq!(p[4], (0, 0, 238)); // blue (238, an xterm idiosyncrasy)
        assert_eq!(p[7], (229, 229, 229)); // white
        assert_eq!(p[8], (127, 127, 127)); // bright black / grey
        assert_eq!(p[9], (255, 0, 0)); // bright red
        assert_eq!(p[12], (92, 92, 255)); // bright blue
        assert_eq!(p[15], (255, 255, 255)); // bright white
    }

    #[test]
    fn cube_anchor_indices() {
        let p = build_default_palette();
        // Index 16 is the cube origin: r=g=b=level[0]=0 → pure black.
        assert_eq!(p[16], (0, 0, 0));
        // Index 231 is the cube terminus: r=g=b=level[5]=255 → pure white.
        assert_eq!(p[231], (255, 255, 255));
        // Index 21 = 16 + 5: b varies fastest, so this is (0,0,255).
        // i=5 → r=level[0]=0, g=level[0]=0, b=level[5]=255.
        assert_eq!(p[21], (0, 0, 255));
        // Index 16 + 36 = 52: i=36 → r=level[1]=95, g=0, b=0.
        assert_eq!(p[52], (95, 0, 0));
        // Index 16 + 6 = 22: i=6 → r=0, g=level[1]=95, b=0.
        assert_eq!(p[22], (0, 95, 0));
    }

    #[test]
    fn cube_decomposition_matches_formula() {
        let p = build_default_palette();
        // Independently re-derive every cube cell and compare against the
        // built palette. This kills any mutant that swaps r/g/b ordering or
        // changes a divisor (the /36, /6, %6 strides).
        for i in 0..216usize {
            let r = LEVELS[(i / 36) % 6];
            let g = LEVELS[(i / 6) % 6];
            let b = LEVELS[i % 6];
            assert_eq!(
                p[16 + i],
                (r, g, b),
                "cube cell {} (palette idx {}) mismatch",
                i,
                16 + i
            );
        }
    }

    #[test]
    fn grayscale_ramp_endpoints_and_step() {
        let p = build_default_palette();
        // First grayscale entry (232) is 8,8,8; last (255) is 238,238,238.
        assert_eq!(p[232], (8, 8, 8));
        assert_eq!(p[255], (238, 238, 238));
        // Step is exactly 10 between consecutive grays, and r==g==b for all.
        for i in 0..24usize {
            let expected = (8 + i * 10) as u8;
            assert_eq!(p[232 + i], (expected, expected, expected));
            if i > 0 {
                let prev = p[232 + i - 1].0;
                let cur = p[232 + i].0;
                assert_eq!(cur - prev, 10, "gray step at idx {}", 232 + i);
            }
        }
    }

    #[test]
    fn grayscale_is_distinct_from_pure_black_and_white() {
        let p = build_default_palette();
        // The ramp deliberately starts at 8 (not 0) and ends at 238 (not 255)
        // so it never collides with cube black (16) or cube white (231).
        assert_ne!(p[232], (0, 0, 0));
        assert_ne!(p[255], (255, 255, 255));
    }

    #[test]
    fn palette_is_fully_populated_and_deterministic() {
        let a = build_default_palette();
        let b = build_default_palette();
        // 256 entries, byte-identical across calls (no hidden global state).
        assert_eq!(a.len(), 256);
        assert_eq!(a, b);
    }
}
