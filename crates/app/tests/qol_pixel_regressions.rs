//! PIXEL regression guards for two defects that were invisible to every
//! existing test because both live entirely in the RASTERISED frame.
//!
//! Run them (they need a real renderer — see [`require_gpu`]; they FAIL, never
//! skip, without one):
//!
//! ```text
//! cargo test -p c0pl4nd --test qol_pixel_regressions -- --ignored --nocapture
//! ```
//!
//! CI runs them in the `visual-qa` job alongside `qa_wide_glyph_snapshot`, on
//! lavapipe, so they are gated rather than "green on a developer's desk".
//!
//! ## Why these two needed PIXELS
//!
//! * **`ESC[4:4m` (dotted underline) painted nothing.** The parser mapped it
//!   (`crates/core` covers `4:4 -> Dotted`), the app-side style bridge carried
//!   it, and the `U::Dotted` arm ran and emitted 46 filled rects — every
//!   *state* assertion anyone could write was already green. The rects were
//!   ~1.02pt wide, at or under epaint's 1px antialiasing feather, so they
//!   rasterised to **zero pixels**: byte-identical to no underline even at a
//!   per-channel tolerance of 90/255, while the same run gave 94px solid and
//!   60px dashed. Only reading the frame catches that.
//!
//!   The measured survey that isolated it, on this harness (1100x720, ppp 1.0,
//!   a 12-cell run, SGR-58 colour `#FF8000`, underline band at scanline y=63):
//!
//!   | SGR   | style  | pixels of the underline colour, before the fix |
//!   |-------|--------|------------------------------------------------|
//!   | `4`   | single | 94 (one continuous rule)                       |
//!   | `4:1` | single | 94                                             |
//!   | `4:2` | double | 188 (two rules, y=62 and y=64)                 |
//!   | `4:3` | curly  | ~110 within tolerance, spanning y=62..64       |
//!   | `4:5` | dashed | 60 (gapped)                                    |
//!   | `4:4` | dotted | **0**, and still 0 at a tolerance of 90/255    |
//!
//!   Every row except `4:4` is asserted by
//!   `qa_wide_glyph_snapshot::styled_underline_variants_are_visually_distinct`;
//!   `4:4` is asserted here. After the fix it measures 48 pixels in 16 dots on
//!   the single scanline y=63 — visible, gapped, and finer-grained than dashed,
//!   which is what the test below checks against the solid and dashed runs the
//!   same harness renders rather than against those numbers.
//!
//! * **Nothing measures the find highlight's painted WIDTH.** The search tests
//!   assert span bookkeeping — `search_match_count`, `search_selected`,
//!   `byte_to_col` — and `byte_to_col` is correct, so a span→rect off-by-one in
//!   `paint_search_highlight` (treating the half-open `col_end` as inclusive)
//!   would leave the last matched character untinted with the whole suite
//!   green. The guard below is a REGRESSION guard, not a fix: the width is
//!   correct in this tree, measured at 6.000 cells for a 6-character query at
//!   ppp 1.0 and 2.0, at line start, at line end, and after a wide (2-cell)
//!   glyph. Injecting the off-by-one makes it render 39px against the 46px a
//!   6-cell background quad renders, and fails this test.
//!
//! ## The discipline (same as `qa_wide_glyph_snapshot.rs`)
//!
//! Expected positions are never guessed. Each test paints a CALIBRATION
//! background run through the production path (`paint_grid_native` PASS 1) over
//! the *same cell columns* the thing under test should cover, measures it in the
//! rendered frame, and asserts the thing under test occupies exactly that. Two
//! independent painters, one cell range, byte-comparable answers. Every
//! assertion is paired with a CONTROL render that must contain ZERO pixels of
//! the colour being counted, so "some pixel happened to be orange" cannot pass.

use c0pl4nd::egui_app;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

const HARNESS_W: u32 = 1100;
const HARNESS_H: u32 = 720;

/// SGR-58 underline colour / calibration colours. All are absent from the
/// chrome and from every default-theme surface, so an exact-match count is
/// unambiguous — each measures 0 in the control renders below.
const DECO: [u8; 3] = [255, 128, 0];
const CAL_BG: [u8; 3] = [173, 41, 209];

mod common;

use common::isolate_config_dir;

/// Assert this host can actually render, with an ACTIONABLE message if it
/// cannot. It must NEVER skip: every test here is `#[ignore]`d, so it runs only
/// when something explicitly asked for it, and reporting green without a frame
/// would assert nothing at all.
fn require_gpu() {
    let backends =
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
    let adapters = pollster::block_on(wgpu::Instance::default().enumerate_adapters(backends));
    assert!(
        !adapters.is_empty(),
        "no wgpu adapter for backends {backends:?} — these pixel tests cannot \
         render. On a headless Linux runner: `apt-get install -y \
         mesa-vulkan-drivers` (lavapipe). Override with WGPU_BACKEND=<vulkan|gl>."
    );
}

/// THE harness for this file: the real app at 1 physical pixel per point with
/// every effect that would tint, fade or overlay the grid turned OFF.
///
/// Those overrides are what make an EXACT colour comparison legitimate — a
/// persisted opacity, tint, frost, scanlines, flicker, VHS banding, the ambient
/// mesh or chromatic aberration each composite over the grid and would shift the
/// bytes for a reason unrelated to the paint contract under test.
fn px_harness() -> Harness<'static, egui_app::C0pl4ndApp> {
    isolate_config_dir();
    require_gpu();
    let mut h = Harness::builder()
        .with_size(egui::vec2(HARNESS_W as f32, HARNESS_H as f32))
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
            // PIN THE CARET'S BLINK PHASE, for the same reason the effects above
            // are disabled: an exact colour count must not depend on when the
            // frame happened to be captured. The caret's phase is a function of
            // the frame clock, so an unpinned caret is present in some runs and
            // absent in others — and it paints in the theme's CURSOR colour,
            // which is close enough to this file's measured colours to matter if
            // it ever moved over a measured cell.
            app.set_cursor_blink_phase(Some(egui_app::CursorBlinkPhase::On));
            app
        });
    // Wait out the deferred first-frame PTY spawn and the shell banner, so a
    // late line of startup output cannot land on top of the fed content
    // mid-assert (that race is measurable: it silently blanks the fed row).
    let deadline = Instant::now() + Duration::from_secs(20);
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
    panic!(
        "the focused pane never produced startup output — a still-booting shell \
         can overwrite the fed grid mid-render, so these pixel asserts would be \
         measuring a race rather than the paint path"
    );
}

/// The set of pixels exactly matching one colour.
#[derive(Debug, Clone)]
struct PxMask {
    n: u64,
    /// Leftmost / rightmost matching x, inclusive. Meaningless when `n == 0`.
    x0: u32,
    x1: u32,
    /// Every DISTINCT scanline carrying a match, ascending. This is the
    /// discriminator between underline variants: a single underline occupies
    /// exactly ONE scanline, a double TWO, a curl at least three.
    ys: Vec<u32>,
}

impl PxMask {
    fn is_empty(&self) -> bool {
        self.n == 0
    }
}

/// Every pixel within `tol` (per channel, L-inf) of `want`. `tol == 0` is exact.
fn px_mask(img: &image::RgbaImage, want: [u8; 3], tol: i32) -> PxMask {
    let (mut n, mut x0, mut x1) = (0u64, u32::MAX, 0u32);
    let mut ys: Vec<u32> = Vec::new();
    for y in 0..img.height() {
        let mut hit_row = false;
        for x in 0..img.width() {
            let p = img.get_pixel(x, y).0;
            let d = (0..3)
                .map(|i| (i32::from(p[i]) - i32::from(want[i])).abs())
                .max()
                .unwrap_or(i32::MAX);
            if d <= tol {
                n += 1;
                x0 = x0.min(x);
                x1 = x1.max(x);
                hit_row = true;
            }
        }
        if hit_row {
            ys.push(y);
        }
    }
    PxMask { n, x0, x1, ys }
}

/// How many CONTIGUOUS horizontal runs of `want` sit on scanline `y`. This is
/// what separates "dotted" from "dashed" and from "solid": a solid rule is one
/// run, dashed is a handful of long ones, dotted is many short ones.
fn px_runs_on_scanline(img: &image::RgbaImage, want: [u8; 3], y: u32) -> u32 {
    let (mut runs, mut prev) = (0u32, false);
    for x in 0..img.width() {
        let p = img.get_pixel(x, y).0;
        let hit = (0..3)
            .map(|i| (i32::from(p[i]) - i32::from(want[i])).abs())
            .max()
            .unwrap_or(i32::MAX)
            == 0;
        if hit && !prev {
            runs += 1;
        }
        prev = hit;
    }
    runs
}

/// Clear the screen, feed `payload` into the focused pane's emulator, and render
/// one real frame. The `ESC[2J ESC[H` prefix makes each call independent.
fn px_feed(h: &mut Harness<'_, egui_app::C0pl4ndApp>, payload: &str) -> image::RgbaImage {
    let mut buf = String::from("\x1b[2J\x1b[H");
    buf.push_str(payload);
    h.state_mut().test_feed_focused(buf.as_bytes());
    for _ in 0..5 {
        h.step();
    }
    h.render().expect("kittest wgpu render must succeed")
}

/// [`px_feed`], retried until `probe` finds the fed content in the frame.
///
/// A shell that emits a late line after the feed re-clears the row, so a single
/// feed-then-render can measure an EMPTY grid and report a false zero. Retrying
/// on the probe (rather than sleeping and hoping) makes the wait observable; a
/// genuinely absent paint still exhausts the budget and fails on the caller's
/// own assertion, so this can never launder a real zero into a pass.
fn px_feed_until(
    h: &mut Harness<'_, egui_app::C0pl4ndApp>,
    payload: &str,
    probe: impl Fn(&image::RgbaImage) -> bool,
) -> image::RgbaImage {
    let mut img = px_feed(h, payload);
    for _ in 0..15 {
        if probe(&img) {
            return img;
        }
        img = px_feed(h, payload);
    }
    img
}

/// 12 cells of SPACES carrying `sgr` plus the SGR-58 underline colour. Blank
/// cells emit no glyph, so every [`DECO`]-coloured pixel in the frame is the
/// underline itself and nothing else.
fn underline_row(sgr: &str) -> String {
    format!(
        "{sgr}\x1b[58;2;{};{};{}m            \x1b[0m\r\n",
        DECO[0], DECO[1], DECO[2]
    )
}

// ---------------------------------------------------------------------------
// Defect 1 — `ESC[4:4m` dotted underline rasterised to nothing
// ---------------------------------------------------------------------------

/// The dotted underline must be VISIBLE, land on ONE scanline, and be GAPPED
/// into many short dots — measured against the solid and dashed variants
/// rendered by the same run, so the comparison is to real paint rather than to
/// a remembered number.
///
/// The `solid` and `none` measurements are PROBE CHECKS: if the plain `SGR 4`
/// underline did not measure as one continuous scanline, or a row with no
/// underline SGR measured any [`DECO`] pixel at all, the measurement is broken
/// and nothing below it can be trusted. They fail with that message rather than
/// letting a broken probe report a green dotted underline.
#[test]
#[ignore = "needs a real GPU; run with --ignored (CI: the visual-qa job)"]
fn dotted_underline_paints_visible_gapped_dots_on_one_scanline() {
    let mut h = px_harness();

    // --- probe checks ------------------------------------------------------
    let solid_img = px_feed_until(&mut h, &underline_row("\x1b[4m"), |i| {
        !px_mask(i, DECO, 0).is_empty()
    });
    let solid = px_mask(&solid_img, DECO, 0);
    assert!(
        !solid.is_empty() && solid.ys.len() == 1,
        "PROBE BROKEN: the plain SGR 4 underline must measure as ONE continuous \
         scanline, got {} pixels on scanlines {:?}. Fix the probe before \
         trusting anything below.",
        solid.n,
        solid.ys
    );
    assert_eq!(
        px_runs_on_scanline(&solid_img, DECO, solid.ys[0]),
        1,
        "PROBE BROKEN: a SOLID underline must be a single unbroken run"
    );

    // THE CONTROL. Same 12 cells, same SGR-58 colour, no underline style: the
    // frame must contain ZERO pixels of the underline colour. Without this,
    // every count below could be some other orange thing in the frame.
    let control = px_feed(&mut h, &underline_row(""));
    let off = px_mask(&control, DECO, 90);
    assert!(
        off.is_empty(),
        "CONTROL FAILED: with no underline SGR the frame must contain zero \
         underline-coloured pixels, got {} (even at tolerance 90/255)",
        off.n
    );

    // --- the regression ----------------------------------------------------
    let dashed_img = px_feed_until(&mut h, &underline_row("\x1b[4:5m"), |i| {
        !px_mask(i, DECO, 0).is_empty()
    });
    let dashed = px_mask(&dashed_img, DECO, 0);
    assert!(
        !dashed.is_empty() && dashed.ys.len() == 1,
        "PROBE BROKEN: `4:5` (dashed) is the yardstick the dotted style is \
         measured against below, so it must itself paint on exactly one \
         scanline; got {} pixels on {:?}",
        dashed.n,
        dashed.ys
    );
    let dotted_img = px_feed_until(&mut h, &underline_row("\x1b[4:4m"), |i| {
        !px_mask(i, DECO, 0).is_empty()
    });
    let dotted = px_mask(&dotted_img, DECO, 0);

    assert!(
        !dotted.is_empty(),
        "ESC[4:4m (dotted underline) painted NOTHING — zero pixels of the \
         underline colour, while the same run renders {} pixels solid and {} \
         dashed. This is the defect: the U::Dotted arm emits its rects, but at \
         ~1.02pt wide they fall at or under epaint's 1px antialiasing feather \
         and rasterise away entirely, making ESC[4:4m indistinguishable from \
         ESC[24m. Each dot must cover whole PHYSICAL pixels.",
        solid.n,
        dashed.n
    );
    assert_eq!(
        dotted.ys.len(),
        1,
        "a dotted underline stays on ONE scanline, got {:?}",
        dotted.ys
    );

    // GAPPED, not solid.
    assert!(
        dotted.n * 10 < solid.n * 9,
        "a DOTTED underline must be gapped: it covered {} of the {} pixels a \
         continuous underline covers",
        dotted.n,
        solid.n
    );
    // …and substantially drawn, so a single surviving dot cannot pass.
    assert!(
        dotted.n * 4 > solid.n,
        "a DOTTED underline must still be substantially drawn ({} of {} pixels) \
         — a couple of surviving dots is the same defect one step smaller",
        dotted.n,
        solid.n
    );
    // It must span the RUN, not just its left edge: the dots reach across the
    // full 12 cells the solid rule covers, bar the trailing gap.
    assert!(
        dotted.x1 * 10 >= solid.x1 * 9 && dotted.x0 <= solid.x0 + 4,
        "the dots must span the whole run: dotted covers x {}..{} where solid \
         covers x {}..{}",
        dotted.x0,
        dotted.x1,
        solid.x0,
        solid.x1
    );

    // FINER than dashed — the property that keeps the two styles telling apart.
    let dotted_runs = px_runs_on_scanline(&dotted_img, DECO, dotted.ys[0]);
    assert!(
        dotted_runs > 6,
        "a DOTTED underline is many small dots, got {dotted_runs} run(s) across \
         the 12-cell span"
    );
    // …and finer than DASHED specifically, measured against the dashed run this
    // same harness just rendered rather than against a written-down number.
    // `4:4` and `4:5` are the only two GAPPED variants, so "dotted is not just
    // gapped but finer-grained than dashed" is the whole difference between
    // them: a dotted arm that regressed into the dashed geometry satisfies every
    // other assertion here (visible, one scanline, gapped, substantially drawn,
    // spans the run, more than six runs) and would ship `4:4` and `4:5` as the
    // same style.
    let dashed_runs = px_runs_on_scanline(&dashed_img, DECO, dashed.ys[0]);
    eprintln!(
        "dotted={} px in {dotted_runs} runs | dashed={} px in {dashed_runs} runs \
         | solid={} px",
        dotted.n, dashed.n, solid.n
    );
    assert!(
        dotted_runs > dashed_runs,
        "a DOTTED underline must be finer-grained than a DASHED one: dotted \
         painted {dotted_runs} run(s) ({} px) against dashed's {dashed_runs} \
         run(s) ({} px) on the same 12-cell span. At or below dashed's run count \
         the two styles are indistinguishable to a user.",
        dotted.n,
        dashed.n
    );
}

// ---------------------------------------------------------------------------
// Defect 2 — the find highlight vs the physical pixel grid
// ---------------------------------------------------------------------------

/// The exact-colour run of `want` on scanline `y`, as `(x0, x1)` inclusive.
///
/// EXACT equality is the point. `paint_grid_native`'s background quads and the
/// find highlight's tint are both opaque-composited fills, so every FULLY
/// covered pixel carries the identical byte triple; a pixel the quad covers only
/// PARTIALLY is a blend and does not match. Counting exact pixels therefore
/// measures the quad's whole-pixel extent — which is precisely the property that
/// distinguishes a rect snapped to the physical pixel grid from one whose edges
/// land mid-pixel.
fn px_exact_run_on_scanline(img: &image::RgbaImage, want: [u8; 3], y: u32) -> Option<(u32, u32)> {
    let (mut x0, mut x1) = (u32::MAX, 0u32);
    for x in 0..img.width() {
        let p = img.get_pixel(x, y).0;
        if p[0] == want[0] && p[1] == want[1] && p[2] == want[2] {
            x0 = x0.min(x);
            x1 = x1.max(x);
        }
    }
    (x0 != u32::MAX).then_some((x0, x1))
}

/// The find-overlay highlight must cover EXACTLY the matched cells — the same
/// whole pixel columns a background quad over those same cells covers.
///
/// The payload puts a 6-cell calibration background on row 0 at columns 13..19,
/// and a row whose SECOND occurrence of `findme` sits at columns 13..19. The
/// calibration is painted by `paint_grid_native`'s PASS 1; the highlight is
/// painted by `paint_search_highlight`. Two independent painters over ONE cell
/// range, so the assertion is `same x0, same x1` — no expected pixel count is
/// written down anywhere, and re-tuning the font or the padding cannot make it
/// stale.
///
/// It measures the SECOND (inactive) match deliberately. The active match is
/// drawn with a 1.5px opaque inside stroke and a 1px corner radius, both of which
/// paint over the fill's own edge columns; the inactive match is a bare
/// 30%-alpha tint, so it measures the fill rect and nothing else.
///
/// # What this guards
///
/// A span→rect off-by-one. `cell_spans_for_search` produces half-open
/// `[col_start, col_end)` cell ranges, and `paint_search_highlight` must paint
/// `col_end - col_start` cells. Treating `col_end` as INCLUSIVE leaves the last
/// matched character untinted, which is invisible to every existing search test
/// — they assert span bookkeeping (`search_match_count`, `search_selected`,
/// `byte_to_col`), never painted width. Verified by injecting exactly that
/// mutation: the highlight then covers 39px where the 6-cell background quad
/// covers 46, and this test fails with both numbers in the message.
///
/// It does NOT guard pixel snapping, and deliberately makes no claim about it:
/// `epaint::Tessellator::tessellate_rect` already rounds every filled rect to
/// the physical pixel grid (`round_rects_to_pixels`, default on, applied for
/// `RectShape::filled`'s `StrokeKind::Outside`), so adding a `snap_to_physical`
/// call in the painter is a measured no-op — a mutant that removes such a call
/// survives, because epaint puts the edges back.
#[test]
#[ignore = "needs a real GPU; run with --ignored (CI: the visual-qa job)"]
fn search_highlight_covers_exactly_the_matched_cells() {
    let mut h = px_harness();

    // Row 0: 13 blank cells, then 6 cells of calibration background (columns
    // 13..19). Row 1: `findme` twice — at columns 3..9 and 13..19.
    let payload = format!(
        "{}\x1b[48;2;{};{};{}m      \x1b[0m\r\nzz findme zz findme zz\r\n",
        " ".repeat(13),
        CAL_BG[0],
        CAL_BG[1],
        CAL_BG[2]
    );
    let before = px_feed_until(&mut h, &payload, |i| !px_mask(i, CAL_BG, 0).is_empty());
    let cal = px_mask(&before, CAL_BG, 0);
    assert!(
        !cal.is_empty(),
        "the calibration background never reached the frame — either PASS 1 does \
         not paint backgrounds, or the payload never reached the grid"
    );
    let (cal_y0, cal_y1) = (cal.ys[0], cal.ys[cal.ys.len() - 1]);
    assert_eq!(
        cal.ys.len() as u32,
        cal_y1 - cal_y0 + 1,
        "the calibration band must be contiguous scanlines, got {:?}",
        cal.ys
    );
    // Measure the calibration on ONE scanline through its middle, so the
    // comparison below is run-to-run rather than bounding-box-to-bounding-box.
    let cal_mid_y = (cal_y0 + cal_y1) / 2;
    let (cal_x0, cal_x1) = px_exact_run_on_scanline(&before, CAL_BG, cal_mid_y)
        .expect("the calibration quad must cover its own middle scanline");
    let row_h = cal_y1 - cal_y0 + 1;
    let cell_w = (cal_x1 - cal_x0 + 1) as f32 / 6.0;
    assert!(
        cell_w > 2.0,
        "the calibration quad is implausibly narrow ({} px for 6 cells)",
        cal_x1 - cal_x0 + 1
    );
    // Row 1 sits immediately below row 0. Pick a scanline inside it that is BARE
    // BACKGROUND across the calibration columns in the pre-overlay frame — no
    // glyph ink, and inset from the band's edges so the highlight's 1px corner
    // radius cannot clip it. Sampling the row's geometric middle instead lands
    // on the x-height of `findme`, and the "tint" sampled there is glyph-over-
    // tint, which then measures the GLYPHS rather than the highlight.
    let (band0, band1) = (cal_y1 + 3, cal_y1 + row_h - 2);
    assert!(band1 < before.height(), "row 1 is off the frame");
    let row1_mid_y = (band0..band1)
        .find(|&y| {
            let first = before.get_pixel(cal_x0, y).0;
            (cal_x0..=cal_x1).all(|x| before.get_pixel(x, y).0 == first)
        })
        .expect(
            "row 1 has no glyph-free scanline across the matched columns — the \
             tint sample below would measure glyph ink instead of the highlight",
        );

    // Open the find overlay and type the query through the REAL keyboard path.
    h.event(egui::Event::Key {
        key: egui::Key::F,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        },
    });
    h.step();
    for ch in "findme".chars() {
        h.event(egui::Event::Text(ch.to_string()));
    }
    for _ in 0..6 {
        h.step();
    }
    assert_eq!(
        h.state().search_match_count(),
        2,
        "the query must have matched the fed row exactly twice — the second \
         (INACTIVE) match is the one measured below"
    );
    assert_eq!(
        h.state().search_selected(),
        0,
        "the FIRST match must be the selected one, so the match over the \
         calibration columns is the inactive tint this test measures"
    );
    let after = h.render().expect("kittest wgpu render must succeed");

    // The tint colour, sampled from the middle of the span — a pixel the rect
    // covers fully under any implementation, so this cannot beg the question.
    let mid_x = (cal_x0 + cal_x1) / 2;
    let tint = after.get_pixel(mid_x, row1_mid_y).0;
    let tint = [tint[0], tint[1], tint[2]];
    let bare = before.get_pixel(mid_x, row1_mid_y).0;
    assert_ne!(
        tint,
        [bare[0], bare[1], bare[2]],
        "no highlight was painted over the matched cells at all — the pixel at \
         the centre of columns 13..19 is unchanged from the pre-overlay frame"
    );

    let (hx0, hx1) = px_exact_run_on_scanline(&after, tint, row1_mid_y)
        .expect("the sampled tint must occur on the scanline it was sampled from");
    assert_eq!(
        (hx0, hx1),
        (cal_x0, cal_x1),
        "the find highlight must cover EXACTLY the matched cells. It covers x \
         {hx0}..{hx1} ({} px) where a background quad over the same 6 cells \
         covers x {cal_x0}..{cal_x1} ({} px, {cell_w:.2} px/cell). A span short \
         by one cell means the last matched character renders untinted: check \
         that `paint_search_highlight` treats `CellSpan`'s `col_end` as \
         EXCLUSIVE, the way `cell_spans_for_search` builds it.",
        hx1 - hx0 + 1,
        cal_x1 - cal_x0 + 1,
    );

    // THE CONTROL. With the overlay closed again, the tint colour must be GONE
    // from the frame entirely — proving the run measured above is the highlight
    // and not something already in the grid.
    h.event(egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    for _ in 0..6 {
        h.step();
    }
    let closed = h.render().expect("kittest wgpu render must succeed");
    assert!(
        px_exact_run_on_scanline(&closed, tint, row1_mid_y).is_none(),
        "CONTROL FAILED: with the find overlay closed, the highlight tint \
         {tint:?} must not appear on the matched row at all"
    );
}
