//! End-to-end **rendered-frame** test for the C0PL4ND egui terminal — the
//! both-panes-render regression guard.
//!
//! ## Why this test exists
//!
//! The interaction tests (`egui_terminal.rs`) drive the PTY→grid pipeline but
//! assert on grid STATE, not on rendered pixels. That blind spot is exactly how
//! the "terminal panes render pure black" defect shipped: every state-only test
//! was green while the live render drew nothing. (The defect was a glyphon GPU
//! paint path that composited black inside `egui_tiles` panes on the real
//! swapchain; it was replaced by egui's native coloured-text painter, see
//! `egui_app::paint_grid_native`.)
//!
//! ## …and why it did not close that gap until now
//!
//! It claimed to. Its comment asserted "the default grid is TWO panes
//! side-by-side", so it halved the central band at `w / 2` and named the halves
//! `left_pane_non_bg` and `right_pane_non_bg`. **The default grid is ONE pane** —
//! `egui_app::INITIAL_PANES == 1`, pinned by
//! `egui_app::mod_tests::bootstrap_opens_with_initial_panes` — and this file
//! never split it. So both "per-pane" counts were two halves of the SAME pane,
//! and the right half's pixels were that pane's right-edge border ring rather
//! than a second pane's content. Measured on this branch immediately before the
//! fix, on a grid whose `pane_count()` is 1:
//!
//! ```text
//! real-frame render: 900x600, left_pane_non_bg=925 right_pane_non_bg=908 total=1833
//! ```
//!
//! Two consequences, both of which made the headline guard unfalsifiable:
//!
//! 1. **Split rendering could break entirely and this test would not notice** —
//!    there was no second pane to blank.
//! 2. **A completely EMPTY pane still passed.** The counting region was the raw
//!    band, and a pane's border ring alone paints thousands of pixels against a
//!    floor of 100/200. The sibling's
//!    `the_pane_content_assertion_rejects_a_blank_pane` measures exactly this: on
//!    a frame whose terminal content is erased but whose border ring survives,
//!    the raw rect still clears the floor.
//!
//! ## What it does now
//!
//! It makes the claim TRUE rather than deleting it. The test builds the REAL
//! `C0pl4ndApp` through eframe's creation path with a REAL wgpu render state,
//! then:
//!
//! 1. asserts the grid really is in the split-capable `Grid` view mode;
//! 2. SPLITS with the production Ctrl+Shift+D keybinding and asserts the split
//!    ADDED a pane (so "there are two panes" is checked, never assumed);
//! 3. asserts the two panes are genuinely side by side — disjoint x-ranges — so
//!    "one pane measured twice" can never satisfy it again;
//! 4. types a known token into the focused pane and waits, bounded, for it to
//!    reach THAT pane's grid, failing loudly if it never arrives;
//! 5. waits for EVERY pane's shell to produce output, renders the whole egui
//!    frame, and asserts PER PANE — over each pane's own production
//!    `pane_body_rect`, inset by the window padding and the scrollbar band —
//!    that the pane painted content.
//!
//! Step 5's geometry and measurement are the shared `tests/common` helpers, so
//! the inset that makes them non-vacuous is derived once and proved once (by
//! `the_pane_content_assertion_rejects_a_blank_pane`) instead of being copied
//! here where it could drift out from under that proof.
//!
//! ## Routing — the `#[ignore]` is a marker, not an off switch
//!
//! This is a real-frame test, so it needs a GPU and it belongs with the other
//! real-frame suites in ci.yml's `visual-qa` job, which supplies a software
//! rasteriser (lavapipe) and runs `--run-ignored only`. Before this change the
//! test was un-ignored and ran in the ordinary `--workspace` job on a runner with
//! no adapter, where its GPU probe returned early and libtest reported `ok`: a
//! skip that read as a pass, in the file whose entire job is to stop renders
//! being claimed rather than seen. It now FAILS, with an actionable message, on a
//! host that cannot render.

mod common;

use c0pl4nd::egui_app;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

use common::{
    assert_every_pane_rendered_content, await_every_pane_has_output, chord, isolate_config_dir,
    require_gpu,
};

/// The known text fed into the focused pane's PTY; its glyphs must show up in
/// the rendered frame. A token that cannot pre-exist on a fresh grid.
const TOKEN: &str = "XYZZY";

/// How long the token gets to travel keystrokes → PTY → shell → grid.
///
/// Bounded, and a hard FAILURE on expiry. The previous version `return`ed here,
/// which meant a shell that never echoed produced a green test that had rendered
/// and asserted nothing at all.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);

/// THE ONE HARNESS CONSTRUCTOR for this file, so config isolation cannot be
/// bypassed by adding a scene — asserted structurally by
/// [`no_scene_can_bypass_the_config_isolation`].
///
/// Isolation is not cosmetic here. `C0pl4ndApp::new` loads the developer's real
/// config, and this test asserts on a SPLIT grid: a persisted
/// `view_mode = "tabs"` renders one full-size pane however many exist, and a
/// persisted keybinding change moves the split chord. Either would fail this test
/// for reasons that have nothing to do with rendering. The app also SAVES its
/// config, so an un-isolated run rewrites the machine it is testing.
fn build() -> Harness<'static, egui_app::C0pl4ndApp> {
    // Isolate FIRST: nothing below may resolve the real per-user config dir.
    isolate_config_dir();
    require_gpu();
    let b = Harness::builder().with_size(egui::vec2(900.0, 600.0));
    b.wgpu().build_eframe(|cc| {
        let mut app = egui_app::C0pl4ndApp::new(cc);
        // This test counts painted pixels against each pane's own MODAL colour,
        // so it is already robust to a tint or a theme change — but a low
        // `opacity` fades the pane backing toward the transparent desktop, which
        // changes what "the background" even is mid-pane. Forcing an opaque,
        // untinted window keeps the measurement about glyph rendering. Belt and
        // braces with the config isolation above: this holds even if the shipped
        // DEFAULTS ever move.
        app.config.opacity = 1.0;
        app.config.tint_enabled = false;
        app
    })
}

/// STRUCTURAL GUARD — needs no GPU, so it runs in the ordinary suite and gives
/// this file a cell that is not gated on an adapter.
///
/// The failure it prevents is not a wrong isolation function; it is a SECOND way
/// to construct a harness that never calls one. (The sibling
/// `qa_wide_glyph_snapshot.rs` shipped exactly that: six of sixteen scenes built
/// their own harness and ran against the developer's real config.)
#[test]
fn no_scene_can_bypass_the_config_isolation() {
    const SRC: &str = include_str!("egui_term_render.rs");
    // Split so this test's own needles do not count as call sites.
    let builder_sites = SRC.matches(concat!("Harness::", "builder()")).count();
    let eframe_sites = SRC.matches(concat!("build_", "eframe(")).count();
    let isolate_sites = SRC.matches(concat!("isolate_config", "_dir();")).count();
    assert_eq!(
        builder_sites, 1,
        "this file must construct its harness in exactly ONE place (build), so \
         config isolation cannot be bypassed by adding a scene; found \
         {builder_sites} builder call sites"
    );
    assert_eq!(
        eframe_sites, 1,
        "likewise exactly ONE eframe construction; found {eframe_sites}"
    );
    assert_eq!(
        isolate_sites, 1,
        "the isolation must be invoked from that single constructor, not sprinkled \
         per scene; found {isolate_sites} invocations"
    );
}

/// Type `line` into the focused pane, submit it, and wait — bounded — for
/// `needle` to appear in THAT pane's grid.
///
/// Deliberately a hard failure rather than the best-effort "type and hope" this
/// file used to do: the token round-trip is the evidence that the pane being
/// measured is showing live shell output, so a token that never lands must not be
/// allowed to produce a green frame-assert.
fn type_line_and_await(h: &mut Harness<'_, egui_app::C0pl4ndApp>, line: &str, needle: &str) {
    let pane = h.state().focused_pane();
    assert!(
        h.state().pane_grid_text(pane).is_some(),
        "the focused pane {} has no live terminal, so nothing can be typed into \
         it — this host spawned no shell, and the render assert below would be \
         measuring a pane that was never asked to show anything",
        pane.raw()
    );
    for ch in line.chars() {
        h.event(egui::Event::Text(ch.to_string()));
    }
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();

    let deadline = Instant::now() + TOKEN_TIMEOUT;
    while Instant::now() < deadline {
        h.step();
        if h.state()
            .pane_grid_text(pane)
            .is_some_and(|t| t.contains(needle))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!(
        "{needle:?} never reached pane {}'s grid within {TOKEN_TIMEOUT:?} after \
         typing {line:?}. The PTY→grid half of the pipeline is broken, so the \
         render assert has nothing to prove. The pane's grid holds: {:?}",
        pane.raw(),
        h.state()
            .pane_grid_text(pane)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect::<String>()
    );
}

/// THE deliverable: build the real app, make the grid genuinely TWO panes, type
/// into the focused one, render the real egui frame, and assert per pane that
/// each one drew content inside its OWN production body rect.
///
/// Catches the "black panes" class and the "only one pane renders" class — the
/// latter for the first time, because until now there was only ever one pane.
#[test]
#[ignore = "needs a real GPU; run with --ignored (CI: the visual-qa job)"]
fn terminal_renders_both_panes_through_real_frame() {
    let mut h = build();

    // A split is only meaningful in Grid view; Tabs renders ONE full-size pane
    // however many exist, which would put this test straight back to measuring
    // one pane twice.
    assert_eq!(
        h.state().view_mode(),
        c0pl4nd_core::config::ViewMode::Grid,
        "this test asserts on a side-by-side split, which only the Grid view \
         renders — in Tabs view a second pane exists but is not on screen"
    );

    // Wait for the FIRST pane's shell before splitting: a pane that has not
    // started is a pane whose PTY size is still being negotiated.
    await_every_pane_has_output(&mut h, "term-render (pre-split)");
    let before = h.state().pane_ids().len();

    chord(&mut h, egui::Key::D); // Ctrl+Shift+D — split right
    for _ in 0..6 {
        h.step();
    }
    let ids = h.state().pane_ids();
    assert_eq!(
        ids.len(),
        before + 1,
        "Ctrl+Shift+D must ADD a pane: the grid went from {before} to {} panes. \
         Without a real split this test degenerates into the single-pane blind \
         spot it exists to close — two halves of ONE pane counted as two panes, \
         with the pane's own border ring standing in for the second pane's \
         glyphs.",
        ids.len()
    );
    assert!(
        ids.len() >= 2,
        "the both-panes guard needs at least two panes; found {}",
        ids.len()
    );

    // Type the token into the pane the split just focused, and prove it lands.
    type_line_and_await(&mut h, &format!("echo {TOKEN}"), TOKEN);

    // Render the REAL egui frame. Use `step()` (one frame) NOT `run()`: a live
    // window calls `request_repaint()` every frame, so `run()` would loop to
    // max_steps.
    await_every_pane_has_output(&mut h, "term-render");
    h.step();
    let img = h
        .render()
        .expect("kittest wgpu render of the real frame must succeed");

    // Side by side, not stacked and not overlapping: the panes' x-ranges must be
    // disjoint. This is the property the old `w / 2` halving silently assumed. It
    // is checked BEFORE the per-pane content assert so "two panes reported, one
    // rect" fails by name instead of as a mysterious pixel count.
    let mut rects: Vec<(u64, egui::Rect)> = h
        .state()
        .pane_ids()
        .into_iter()
        .map(|id| {
            (
                id.raw(),
                h.state()
                    .pane_body_rect(id)
                    .expect("every pane is laid out after a rendered frame"),
            )
        })
        .collect();
    rects.sort_by(|a, b| a.1.left().total_cmp(&b.1.left()));
    for w in rects.windows(2) {
        let ((la, a), (lb, b)) = (w[0], w[1]);
        assert!(
            a.right() <= b.left() + 1.0,
            "panes {la} {a:?} and {lb} {b:?} overlap horizontally — they are not \
             the side-by-side split this test measures, so a per-pane count could \
             be reading the same pane twice"
        );
    }

    // THE guard: every pane painted content in its OWN body rect, inset past the
    // padding and the scrollbar band so neither the chrome nor a pane's border
    // ring can satisfy it. A blanked pane fails here, by name.
    assert_every_pane_rendered_content(&h, &img, "term-render");

    eprintln!(
        "real-frame render: {}x{} across {} panes; token {TOKEN:?} confirmed in \
         the focused pane's grid",
        img.width(),
        img.height(),
        rects.len()
    );
}
