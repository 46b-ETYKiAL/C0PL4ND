//! KNOWN-DEFECT REPRODUCTION — the dotted styled underline (`ESC[4:4m`) paints
//! nothing at all.
//!
//! # Status: this test FAILS on purpose. It is the executable spec for a fix.
//!
//! It is `#[ignore]`d so it never reddens the ordinary suite, and it lives in
//! its OWN file so the `visual-qa` CI job — which runs
//! `--test qa_wide_glyph_snapshot --run-ignored only`, i.e. every ignored test
//! in THAT file — does not pick it up either. Run it deliberately:
//!
//! ```text
//! cargo test -p c0pl4nd --test underline_dotted_4_4_defect -- --ignored --nocapture
//! ```
//!
//! # What is wrong
//!
//! Every other styled-underline variant renders. Measured on the real frame
//! (1100x720, ppp 1.0, 12-cell run, SGR 58 colour `#FF8000`, the underline
//! band being scanline y=63 of the row y=48..64):
//!
//! | SGR     | style  | pixels of the underline colour in the row |
//! |---------|--------|-------------------------------------------|
//! | `4`     | single | 94  (a continuous 94px rule)               |
//! | `4:1`   | single | 94                                         |
//! | `4:2`   | double | 188 (two 94px rules at y=62 and y=64)      |
//! | `4:3`   | curly  | ~110 within tolerance, spanning y=62..64   |
//! | `4:5`   | dashed | 60  (13 dashes with gaps)                  |
//! | **`4:4`** | **dotted** | **0 — and 0 even at a per-channel tolerance of 90/255** |
//!
//! The row's pixels under `4:4` are byte-identical to the no-underline control
//! (`18,18,18` throughout). To a user, `ESC[4:4m` is indistinguishable from
//! `ESC[24m`.
//!
//! # Where it is NOT
//!
//! * Not the VT parser. `crates/core/src/term.rs` maps `Some(4) =>
//!   UnderlineStyle::Dotted` and `crates/core/src/term/tests.rs` covers it.
//! * Not the app-side style bridge, and not a missing dispatch: walking the
//!   frame's `Shape` list shows the `U::Dotted` arm DOES emit its dashes —
//!   **46 filled rects**, the first six being
//!   `[8.0 63.0]-[9.0 64.0]`, `[10.0 63.0]-[11.1 64.0]`,
//!   `[12.1 63.0]-[13.1 64.0]`, `[14.1 63.0]-[15.1 64.0]`,
//!   `[16.2 63.0]-[17.2 64.0]`, `[18.2 63.0]-[19.2 64.0]`.
//!
//! So the shapes are emitted and then rasterise to nothing. The dashes are
//! ~1.02pt wide (`thickness = (ch * 0.06).max(1.0 / ppp)` = 1.02, and the arm
//! draws `bar(x, x + thickness, y)` every `thickness * 2.0` = 2.04), which is
//! at/below epaint's 1px feathering width, so antialiasing consumes the quad
//! entirely. The `4:5` dashed arm survives only because its dash is
//! `thickness * 4.0` = 4.08pt wide.
//!
//! # The shape of a fix (for whoever owns `egui_app/mod.rs`)
//!
//! Make each dot at least one PHYSICAL pixel of actual coverage — e.g. give the
//! dotted arm a dash width of `(thickness * 2.0).max(2.0 / ppp)` on a
//! `(thickness * 4.0).max(4.0 / ppp)` period (still visibly finer than dashed),
//! and/or snap each dot's x edges with `snap_to_physical` the way the background
//! quads already are, so a dot always covers whole pixels. The assertion below
//! is deliberately weak — it only demands that `4:4` be *visible at all* and
//! *gapped* — so any reasonable fix satisfies it.

use c0pl4nd::egui_app;
use egui_kittest::Harness;
use std::time::{Duration, Instant};

const W: u32 = 1100;
const H: u32 = 720;
/// SGR-58 underline colour. Absent from the chrome and the default theme, so an
/// exact match is unambiguous (measured: 0 hits with no underline).
const DECO: [u8; 3] = [255, 128, 0];

/// Point the config loader at a throwaway dir for this process.
///
/// `C0pl4ndApp::new` both LOADS and SAVES the user's config, so without this a
/// diagnostic run would read (and rewrite) the developer's real
/// `%APPDATA%\c0pl4nd\config.toml` — and a persisted tint/opacity would change
/// the pixels this file measures.
fn isolate_config() {
    use std::sync::OnceLock;
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("c0pl4nd-dotted-defect-")
            .tempdir()
            .expect("create the throwaway config dir")
    });
    std::env::set_var("APPDATA", dir.path());
    std::env::set_var("XDG_CONFIG_HOME", dir.path());
    std::env::set_var("HOME", dir.path());
}

/// Fail with an ACTIONABLE message when the host cannot render, rather than an
/// opaque panic from inside the harness. Never skips: this test is `#[ignore]`d,
/// so it only runs when something explicitly asked for it, and reporting green
/// without rendering would assert nothing.
fn require_gpu() {
    let backends =
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
    let adapters = pollster::block_on(wgpu::Instance::default().enumerate_adapters(backends));
    assert!(
        !adapters.is_empty(),
        "no wgpu adapter for backends {backends:?} — this reproduction cannot \
         render. On a headless Linux host: `apt-get install -y \
         mesa-vulkan-drivers` (lavapipe). Override with WGPU_BACKEND=<vulkan|gl>."
    );
}

/// The real app with every effect that would composite over the grid turned off,
/// driven until its shell has emitted output (so a late banner cannot land on
/// top of the fed row).
fn harness() -> Harness<'static, egui_app::C0pl4ndApp> {
    isolate_config();
    require_gpu();
    let mut h = Harness::builder()
        .with_size(egui::vec2(W as f32, H as f32))
        .wgpu()
        .build_eframe(|cc| {
            let mut app = egui_app::C0pl4ndApp::new(cc);
            app.config.opacity = 1.0;
            app.config.tint_enabled = false;
            app.config.frost_enabled = false;
            app.config.effects.wired_ambient = false;
            app.config.effects.crt_scanlines = false;
            app.config.effects.flicker = false;
            app.config.effects.vhs_tracking = false;
            app.config.effects.chromatic_aberration_enabled = false;
            app
        });
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        h.step();
        if h.state()
            .test_focused_buffer_text()
            .is_some_and(|t| !t.trim().is_empty())
        {
            for _ in 0..10 {
                h.step();
            }
            return h;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the focused pane never produced startup output");
}

/// Clear, feed `payload`, render, and count pixels within `tol` of [`DECO`].
/// Returns `(count, distinct_scanlines)`.
fn underline_pixels(
    h: &mut Harness<'_, egui_app::C0pl4ndApp>,
    payload: &str,
    tol: i32,
) -> (u64, Vec<u32>) {
    let mut buf = String::from("\x1b[2J\x1b[H");
    buf.push_str(payload);
    h.state_mut().test_feed_focused(buf.as_bytes());
    for _ in 0..5 {
        h.step();
    }
    let img = h.render().expect("kittest wgpu render must succeed");
    let (mut n, mut ys) = (0u64, Vec::new());
    for y in 0..img.height() {
        let mut hit = false;
        for x in 0..img.width() {
            let p = img.get_pixel(x, y).0;
            let d = (0..3)
                .map(|i| (i32::from(p[i]) - i32::from(DECO[i])).abs())
                .max()
                .unwrap_or(i32::MAX);
            if d <= tol {
                n += 1;
                hit = true;
            }
        }
        if hit {
            ys.push(y);
        }
    }
    (n, ys)
}

/// 12 cells of SPACES carrying `sgr` plus the SGR-58 underline colour. Blank
/// cells emit no glyph, so every [`DECO`]-coloured pixel in the frame is the
/// underline itself.
fn row(sgr: &str) -> String {
    format!(
        "{sgr}\x1b[58;2;{};{};{}m            \x1b[0m\r\n",
        DECO[0], DECO[1], DECO[2]
    )
}

/// FAILS TODAY. `ESC[4:4m` must paint a visible, gapped dotted underline.
///
/// The assertion is intentionally minimal — "some pixels, and not solid" — so it
/// constrains a fix as little as possible while still being impossible to
/// satisfy with the current zero-pixel behaviour.
#[test]
#[ignore = "KNOWN DEFECT reproduction: FAILS by design — ESC[4:4m dotted \
            underline rasterises to zero pixels. See the module docs."]
fn dotted_underline_4_4_must_be_visible() {
    let mut h = harness();

    // Baselines that PROVE the probe works, so a failure below cannot be blamed
    // on the measurement.
    let (solid, solid_ys) = underline_pixels(&mut h, &row("\x1b[4m"), 0);
    assert!(
        solid > 0 && solid_ys.len() == 1,
        "PROBE BROKEN: the plain SGR 4 underline must measure as one continuous \
         scanline, got {solid} pixels on {solid_ys:?}. Fix the probe before \
         trusting anything below."
    );
    let (off, _) = underline_pixels(&mut h, &row(""), 90);
    assert_eq!(
        off, 0,
        "PROBE BROKEN: with no underline SGR the frame must contain zero \
         underline-coloured pixels, got {off}"
    );

    // The defect. `4:5` (dashed) is measured alongside purely as evidence that a
    // GAPPED variant can and does render — the dotted arm is the only one that
    // vanishes.
    let (dashed, _) = underline_pixels(&mut h, &row("\x1b[4:5m"), 0);
    let (dotted_exact, _) = underline_pixels(&mut h, &row("\x1b[4:4m"), 0);
    let (dotted_tol, dotted_ys) = underline_pixels(&mut h, &row("\x1b[4:4m"), 90);

    eprintln!(
        "solid(4)={solid}  dashed(4:5)={dashed}  dotted(4:4) exact={dotted_exact} \
         tol90={dotted_tol} on scanlines {dotted_ys:?}"
    );

    assert!(
        dotted_tol > 0,
        "ESC[4:4m (dotted underline) painted NOTHING: {dotted_exact} exact-colour \
         pixels and {dotted_tol} pixels even within a per-channel tolerance of \
         90/255. For comparison the same run renders {solid} pixels as a solid \
         underline and {dashed} as a dashed one. The `U::Dotted` arm DOES emit \
         its rects (46 of them, ~1.02pt wide) — they rasterise away under \
         epaint's 1px feathering. See the module docs for the shape of a fix."
    );
    assert!(
        dotted_exact * 10 < solid * 9,
        "a DOTTED underline must be gapped, not solid: it covered \
         {dotted_exact} of the {solid} pixels a continuous underline covers"
    );
}
