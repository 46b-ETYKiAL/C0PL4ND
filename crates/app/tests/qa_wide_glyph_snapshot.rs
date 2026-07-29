//! On-demand VISUAL-QA snapshots: render the REAL C0pl4ndApp egui frame in
//! several states to PNGs so a human — or an agent with image-reading — can
//! EYEBALL the rendering that cannot be asserted from accessibility/grid state
//! alone.
//!
//! Run on demand (needs a renderer — see [`require_gpu`]; FAILS, never skips,
//! without one):
//!   cargo test -p c0pl4nd --test qa_wide_glyph_snapshot -- --ignored --nocapture
//! Each test prints the absolute PNG path it wrote. The PNGs themselves are not
//! gated — pixel output is non-deterministic across GPU drivers, so nothing here
//! diffs against a committed baseline. The driver-independent assertions below
//! ARE gated, by the `visual-qa` job in ci.yml, which supplies a software
//! rasteriser (lavapipe) so this file has a real CI cell instead of only ever
//! running on a developer's desk.
//!
//! ## What IS asserted (and what is not)
//!
//! Not-gated does not mean not-asserted. [`snapshot`] asserts the two properties
//! that hold on ANY driver: the frame renders at the harness size, and it is not
//! a single uniform colour (i.e. something was actually painted). A blank frame
//! is a real failure mode — a broken paint path still clears the target — and it
//! used to produce a PNG and pass, because this file only rendered and saved.
//!
//! What is NOT asserted, and needs a human (or an agent with image-reading) to
//! eyeball the PNG: glyph shaping, wide/CJK advance widths, colour fidelity,
//! cursor placement, layout. That is the eyeball this file exists for; the
//! assertions only stop it lying when there is nothing to eyeball at all.

use c0pl4nd::egui_app;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

/// The harness surface size. `snapshot` asserts the rendered frame matches, so
/// the size lives here rather than being repeated as a magic number.
const HARNESS_W: u32 = 1100;
const HARNESS_H: u32 = 720;

/// Assert this host can actually render, with an ACTIONABLE message if it cannot.
///
/// This is a diagnostic, NOT a gate: `egui_kittest` is already fail-closed (its
/// `create_render_state` ends in `.expect("Failed to create render state")`), so a
/// GPU-less host fails the test either way. All this adds is a message that names
/// the cause and the fix instead of an opaque panic from inside the harness.
///
/// It must NEVER skip. Every test here is `#[ignore]`d, so it runs only when
/// something explicitly asked for it — a host that cannot honour that request has
/// failed, and reporting green would assert nothing at all. This function used to
/// return `bool`, and each test did `let Some(h) = build() else { return }`: on a
/// GPU-less runner all 15 "passed" without rendering a single frame.
///
/// The adapter enumeration deliberately mirrors the backend set `egui_kittest`
/// itself resolves (`Backends::from_env()`, defaulting to `PRIMARY | GL`). A probe
/// on a DIFFERENT backend set can disagree with the harness it is speaking for.
fn require_gpu() {
    let backends =
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
    let adapters = pollster::block_on(wgpu::Instance::default().enumerate_adapters(backends));
    assert!(
        !adapters.is_empty(),
        "no wgpu adapter for backends {backends:?} — these visual-QA tests cannot render. \
         On a headless Linux runner install a software rasteriser: \
         `apt-get install -y mesa-vulkan-drivers` (lavapipe, auto-registered as an ICD). \
         Override the backend set with WGPU_BACKEND=<vulkan|gl|...> if needed."
    );
}

/// THE ONE HARNESS CONSTRUCTOR. Every scene in this file builds its harness
/// here — this is the file's only kittest builder call site, and
/// [`no_scene_can_bypass_the_config_isolation`] asserts that structurally.
///
/// That single funnel is what makes [`isolate_config_dir`] load-bearing rather
/// than advisory. It used to be called only from [`build`], and SIX of the
/// sixteen scenes constructed their harness by other routes (an inline builder
/// for the HiDPI scene, `build_tinted`, `render_with`) — so they ran against the
/// developer's real `%APPDATA%\c0pl4nd\config.toml`, both READING it (the same
/// scene rendered different pixels on two machines, or after any settings
/// change) and WRITING to it. Four of those six are asserting regression guards,
/// and an ambient config with `crt_scanlines` / `wired_ambient` / `flicker` on
/// made two of them FAIL outright. CI runs this file under nextest
/// (process-per-test), so the `set_var` redirect performed by some OTHER test's
/// `build()` never reached them: the exposure was deterministic, not bad luck.
///
/// `pixels_per_point` is `None` for the default 1.0 rendering; `mutate` runs on
/// the freshly-constructed app inside the eframe creation closure, which is the
/// only point a scene can override live config before the first frame.
///
/// Panics (via [`require_gpu`] or the harness itself) when the host cannot render —
/// never silently degrades. See [`require_gpu`].
fn build_harness(
    pixels_per_point: Option<f32>,
    mutate: impl FnOnce(&mut egui_app::C0pl4ndApp) + 'static,
) -> Harness<'static, egui_app::C0pl4ndApp> {
    // Isolate FIRST: nothing this function goes on to touch may resolve the real
    // per-user config dir, not even incidentally (the app's `gpu-diag.log` is
    // written next to `config.toml`, so an un-isolated render litters there too).
    isolate_config_dir();
    require_gpu();
    let mut builder = Harness::builder().with_size(egui::vec2(HARNESS_W as f32, HARNESS_H as f32));
    if let Some(ppp) = pixels_per_point {
        builder = builder.with_pixels_per_point(ppp);
    }
    builder.wgpu().build_eframe(move |cc| {
        let mut app = egui_app::C0pl4ndApp::new(cc);
        mutate(&mut app);
        app
    })
}

/// Build a real-wgpu harness over the production `C0pl4ndApp`, on the persisted
/// (now isolated → default) config.
fn build() -> Harness<'static, egui_app::C0pl4ndApp> {
    build_harness(None, |_| {})
}

/// Point `Config::default_path()` at a throwaway dir for this test process.
///
/// These scenes build the REAL `C0pl4ndApp`, which loads AND SAVES the user's
/// config. Opening the Settings window persists its width, so simply RUNNING
/// visual QA rewrote the developer's own `%APPDATA%/c0pl4nd/config.toml` —
/// `settings_win_w` changed from 1001.0 to 1015.0 on a scene that only looked at
/// a page. A diagnostic aid must not mutate the machine it is diagnosing, and a
/// test that reads real user config is also not reproducible: two runs render
/// different frames depending on what the developer last set.
///
/// `default_path()` resolves from `APPDATA` (Windows) / `XDG_CONFIG_HOME` /
/// `HOME`, so overriding those redirects both the load and the save. Called from
/// [`build_harness`] — the file's ONLY harness constructor — so every scene is
/// covered by construction, and idempotent so repeated calls in one process keep
/// using the same dir.
///
/// The dir is a `tempfile` dir, NOT a name derived from the process id: pids
/// recycle, and a recycled name is a dir a PREVIOUS run already saved a config
/// into — which is the same read-contamination this function exists to remove,
/// just sourced from an old test run instead of the developer's profile. The
/// `TempDir` is parked in a `static`, so it lives for the whole process (the app
/// keeps writing to it) and is never dropped mid-run.
fn isolate_config_dir() {
    use std::sync::OnceLock;
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("c0pl4nd-qa-cfg-")
            .tempdir()
            .expect("create the QA config dir")
    });
    // Edition 2021: `set_var` is safe here. All these scenes run single-threaded
    // (`--test-threads=1`, and the wgpu renders serialise anyway), and the value
    // is constant per process, so there is no racing writer.
    std::env::set_var("APPDATA", dir.path());
    std::env::set_var("XDG_CONFIG_HOME", dir.path());
    std::env::set_var("HOME", dir.path());
}

/// STRUCTURAL GUARD — the reason [`isolate_config_dir`] cannot silently stop
/// covering a scene again. Needs no GPU, so it runs in the ordinary suite.
///
/// The previous regression was not a wrong isolation function; it was a SECOND
/// (and third, and fourth) way to construct a harness that never called it.
/// Asserting the file has exactly ONE harness-builder call site is what makes
/// "every scene is isolated" checkable instead of a claim: a new scene that
/// hand-rolls its own builder fails here, immediately, by name.
#[test]
fn no_scene_can_bypass_the_config_isolation() {
    const SRC: &str = include_str!("qa_wide_glyph_snapshot.rs");
    // Split so this test's own needles do not count as call sites.
    let builder_sites = SRC.matches(concat!("Harness::", "builder()")).count();
    let eframe_sites = SRC.matches(concat!("build_", "eframe(")).count();
    let isolate_sites = SRC.matches(concat!("isolate_config", "_dir();")).count();
    assert_eq!(
        builder_sites, 1,
        "this file must construct its harness in exactly ONE place (build_harness), \
         so config isolation cannot be bypassed by adding a scene; found \
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

/// Render the current frame, ASSERT it is a real image, and save it to
/// `%TEMP%/c0pl4nd-qa-<name>.png`.
///
/// The assertions are deliberately GPU-INDEPENDENT. Exact pixels vary across
/// drivers, so this cannot diff against a committed baseline (see the module
/// doc) — but "the frame rendered at the requested size and something was
/// actually painted" is deterministic everywhere, and it is the property that
/// actually regresses. Previously this helper only rendered, saved and printed:
/// a fully blank frame — the exact symptom of a broken paint path — produced a
/// PNG and passed, so every test in this file was an artifact generator rather
/// than a check. An unasserted render is not a test.
fn snapshot(h: &mut Harness<'_, egui_app::C0pl4ndApp>, name: &str) {
    h.step();
    let img = h.render().expect("kittest wgpu render must succeed");

    // The rendered buffer is PHYSICAL pixels, so the expected size scales with
    // pixels_per_point. Deriving it from the live ctx (rather than hardcoding
    // 1100x720) is what makes this meaningful for `qa_launch_frame_hidpi`, which
    // renders at ppp 1.5 to reproduce the reported HiDPI garble: it pins that the
    // frame is actually produced at the display's physical resolution instead of
    // being rendered at 1x and stretched — the very class of bug that test exists
    // for.
    let ppp = h.ctx.pixels_per_point();
    let expect_w = (HARNESS_W as f32 * ppp).round() as u32;
    let expect_h = (HARNESS_H as f32 * ppp).round() as u32;
    assert_eq!(
        (img.width(), img.height()),
        (expect_w, expect_h),
        "QA-SNAPSHOT[{name}]: the frame must render at the harness size scaled by \
         pixels_per_point ({ppp})"
    );

    // Not uniformly one colour: a blank/cleared frame is a real, seen failure
    // mode (a broken paint callback still clears the target), and it is exactly
    // what an eyeball-only snapshot silently accepts.
    let px = img.as_raw();
    let first: &[u8] = &px[0..4];
    let painted = px.chunks_exact(4).any(|c| c != first);
    assert!(
        painted,
        "QA-SNAPSHOT[{name}]: the frame is a single uniform colour ({first:?}) — nothing was painted"
    );

    let out = std::env::temp_dir().join(format!("c0pl4nd-qa-{name}.png"));
    img.save(&out).expect("save QA snapshot PNG");
    eprintln!(
        "QA-SNAPSHOT[{name}]: {}x{} -> {}",
        img.width(),
        img.height(),
        out.display()
    );
}

/// Send a Ctrl+Shift+<key> chord (the default keybinding modifier on this host).
fn chord(h: &mut Harness<'_, egui_app::C0pl4ndApp>, key: egui::Key) {
    h.event(egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        },
    });
    for _ in 0..3 {
        h.step();
    }
}

/// Type a line into the focused pane and submit it, then poll for it to land.
fn type_line(h: &mut Harness<'_, egui_app::C0pl4ndApp>, line: &str, needle: &str) {
    if h.state().focused_grid_text().is_none() {
        return;
    }
    for ch in line.chars() {
        h.event(egui::Event::Text(ch.to_string()));
    }
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        h.step();
        if h.state()
            .focused_grid_text()
            .is_some_and(|t| t.contains(needle))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_launch_frame() {
    let mut h = build();
    // A few frames to let the shell banner + prompt land.
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    snapshot(&mut h, "launch");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_launch_frame_hidpi() {
    // Reproduce a 1.5x HiDPI display (the reported garble machine) — the default
    // qa harness renders at ppp 1.0, which never reproduced it.
    let mut h = build_harness(Some(1.5), |_| {});
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    snapshot(&mut h, "launch-hidpi");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_wide_glyph_frame() {
    let mut h = build();
    type_line(&mut h, "echo ASCII | 日本語 | ＡＢＣ | 😀 | end", "end");
    snapshot(&mut h, "wide-glyph");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_settings_page() {
    let mut h = build();
    for _ in 0..10 {
        h.step();
    }
    // The gear caption button is labelled "settings" in the AccessKit tree.
    h.get_by_label("settings").click();
    for _ in 0..4 {
        h.step();
    }
    snapshot(&mut h, "settings");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_toolbar_settings_page() {
    let mut h = build();
    for _ in 0..10 {
        h.step();
    }
    h.get_by_label("settings").click();
    for _ in 0..4 {
        h.step();
    }
    // Select the Toolbar category to render the toolbar editor (icons must NOT be
    // tofu; every row must show the reorder arrows + X remove + move menu).
    h.get_by_label("Toolbar").click();
    for _ in 0..4 {
        h.step();
    }
    snapshot(&mut h, "toolbar-settings");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_terminal_settings_page() {
    let mut h = build();
    for _ in 0..10 {
        h.step();
    }
    h.get_by_label("settings").click();
    for _ in 0..4 {
        h.step();
    }
    // Select Terminal to render the Clipboard group — including the OSC 52
    // "Allow programs to read the clipboard" row. That row ships DEFAULT-OFF and
    // guards a real exfiltration path, so it must be visibly unchecked and
    // legible: a security toggle nobody has ever looked at is a security toggle
    // whose rendered state nobody has confirmed.
    h.get_by_label("Terminal").click();
    for _ in 0..4 {
        h.step();
    }
    snapshot(&mut h, "terminal-settings");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_motion_settings_page() {
    let mut h = build();
    for _ in 0..10 {
        h.step();
    }
    h.get_by_label("settings").click();
    for _ in 0..4 {
        h.step();
    }
    // Select the Motion category to render the regrouped Motion page — the four
    // grouped sections (master / CRT screen / Ambient node-mesh / Tape & motion
    // accents) with checkbox-left rows and the new movement/intensity sliders.
    h.get_by_label("Motion").click();
    for _ in 0..4 {
        h.step();
    }
    snapshot(&mut h, "motion-settings");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_command_palette() {
    let mut h = build();
    for _ in 0..10 {
        h.step();
    }
    chord(&mut h, egui::Key::P); // Ctrl+Shift+P
    snapshot(&mut h, "palette");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_find_overlay() {
    let mut h = build();
    type_line(&mut h, "echo findme_token_123", "findme");
    chord(&mut h, egui::Key::F); // Ctrl+Shift+F opens the find overlay
    for ch in "findme".chars() {
        h.event(egui::Event::Text(ch.to_string()));
    }
    snapshot(&mut h, "find");
}

#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_split_panes() {
    let mut h = build();
    for _ in 0..10 {
        h.step();
    }
    chord(&mut h, egui::Key::D); // Ctrl+Shift+D — split right
    for _ in 0..6 {
        h.step();
    }
    snapshot(&mut h, "split");
}

/// Build a real-wgpu harness whose live config has window-transparency ON with a
/// strong background TINT at a LOW opacity — the exact state the tint/transparency
/// fixes must be eyeballed in. `new(cc)` loads the persisted config, then we
/// override just the transparency fields for the QA render (mirrors the user
/// dialling Settings → Appearance). Panics without a GPU; see [`require_gpu`].
fn build_tinted(opacity: f32, tint_strength: f32) -> Harness<'static, egui_app::C0pl4ndApp> {
    build_harness(None, move |app| {
        // Single always-transparent model: the opacity slider is the whole
        // see-through control; a low opacity + a strong tint is the state to
        // eyeball the tint/transparency fixes in.
        app.config.opacity = opacity;
        app.config.tint = "#ff0040".to_string();
        app.config.tint_enabled = true;
        app.config.tint_strength = tint_strength;
    })
}

/// VISUAL-QA: window transparency ON + strong red tint at LOW opacity, split into
/// two panes. Eyeball checklist against the tint/transparency fixes:
///   1. the TINT wash reaches the panes, the gap/divider between them, AND the
///      top bar + status bar UNIFORMLY (not just the pane backgrounds);
///   2. the terminal TEXT is NOT reddened (tint is behind the glyphs);
///   3. at this low opacity the background reads as clearly see-through.
#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_tint_transparent_low_opacity() {
    let mut h = build_tinted(0.10, 0.8);
    for _ in 0..10 {
        h.step();
    }
    chord(&mut h, egui::Key::D); // split right → two panes + a divider to inspect
    for _ in 0..8 {
        h.step();
    }
    snapshot(&mut h, "tint-transparent-low-opacity");
}

/// VISUAL-QA: with the SAME tint/transparency config, open Settings. The Settings
/// window MUST stay solid + readable (opaque) and MUST NOT be washed red — the
/// reported "settings window is tinted/transparent" bug.
#[test]
#[ignore = "visual-QA aid: needs a real GPU; run explicitly with --ignored"]
fn qa_tint_settings_stays_opaque() {
    let mut h = build_tinted(0.10, 0.8);
    for _ in 0..10 {
        h.step();
    }
    h.get_by_label("settings").click();
    for _ in 0..4 {
        h.step();
    }
    snapshot(&mut h, "tint-settings-opaque");
}

/// Render the real app with a config mutation applied. Panics without a GPU; see
/// [`require_gpu`].
///
/// The mutation is applied on top of the ISOLATED (default) config, not on top
/// of whatever the developer last saved — these are asserting regression guards
/// whose thresholds are stated against the default surface, and an ambient
/// `crt_scanlines` / `wired_ambient` / `flicker` used to make them fail.
fn render_with(mutate: impl FnOnce(&mut c0pl4nd_core::Config) + 'static) -> image::RgbaImage {
    let mut h = build_harness(None, move |app| mutate(&mut app.config));
    for _ in 0..5 {
        h.step();
    }
    h.render().expect("render")
}

/// The modal non-zero alpha (and its pixel count) over the pane band — the alpha
/// the terminal BACKING composited to, ignoring the fully-transparent pixels.
fn modal_pane_alpha(img: &image::RgbaImage) -> (u8, u64) {
    let (w, hgt) = (img.width(), img.height());
    let mut hist = [0u64; 256];
    for y in (hgt / 6)..(hgt - hgt / 8) {
        for x in 0..w {
            hist[img.get_pixel(x, y).0[3] as usize] += 1;
        }
    }
    hist.iter()
        .enumerate()
        .skip(1) // ignore fully-transparent
        .max_by_key(|(_, n)| **n)
        .map(|(a, n)| (a as u8, *n))
        .unwrap_or((0, 0))
}

/// Fraction (%) of the WHOLE window that is non-transparent.
fn nonzero_pct(img: &image::RgbaImage) -> f64 {
    let (w, hgt) = (img.width(), img.height());
    let mut nz = 0u64;
    for y in 0..hgt {
        for x in 0..w {
            if img.get_pixel(x, y).0[3] != 0 {
                nz += 1;
            }
        }
    }
    nz as f64 / (u64::from(w) * u64::from(hgt)) as f64 * 100.0
}

/// REGRESSION GUARD (needs a GPU): with the tint AND frost OFF and no ambient
/// effects, opacity 0 is a genuinely CLEAR window — the whole surface reaches
/// alpha 0 except the sparse, opaque glyph text. Proves the clean-glass path
/// (opacity purely controls see-through; nothing hazes it when tint/frost are off).
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn opacity_zero_is_clear_when_tint_and_frost_off() {
    let img = render_with(|c| {
        c.opacity = 0.0;
        c.tint_enabled = false;
        c.frost_enabled = false;
        c.effects.wired_ambient = false;
    });
    let pct = nonzero_pct(&img);
    assert!(
        pct < 3.0,
        "opacity-0 clean glass must be ~fully clear: {pct:.2}% non-transparent \
         (expected < 3% — only sparse glyph text + focus ring)"
    );
}

/// GUARD (needs a GPU): the node mesh is INDEPENDENT of the window opacity — it
/// no longer fades with the Opacity slider. At opacity 0 (fully see-through) the
/// mesh must STILL paint a visible lattice over the desktop (it used to be scaled
/// to nothing by `opacity`), so turning the mesh on adds clearly more non-
/// transparent pixels than the mesh-off clean-glass baseline.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn mesh_shows_at_opacity_zero_independent_of_opacity() {
    let off = render_with(|c| {
        c.opacity = 0.0;
        c.tint_enabled = false;
        c.frost_enabled = false;
        c.effects.wired_ambient = false;
    });
    let on = render_with(|c| {
        c.opacity = 0.0; // fully transparent glass …
        c.tint_enabled = false;
        c.frost_enabled = false;
        c.effects.animations_enabled = true;
        c.effects.wired_ambient = true; // … yet the mesh still paints
        c.effects.mesh_density = 1.5;
        c.effects.mesh_brightness = 2.0;
    });
    let (off_pct, on_pct) = (nonzero_pct(&off), nonzero_pct(&on));
    eprintln!("mesh off at opacity0 = {off_pct:.2}%, mesh on = {on_pct:.2}%");
    assert!(
        on_pct > off_pct + 1.0,
        "the mesh must remain visible at opacity 0 (independent of opacity): \
         off={off_pct:.2}% vs on={on_pct:.2}% — the mesh was scaled away by opacity"
    );
}

/// PART 1 GUARD (needs a GPU): the terminal background is painted ONCE, so the
/// pane-backing alpha is LINEAR in opacity — a single 0.7 opacity yields ≈179
/// (0.7·255), NOT the ≈125 (0.7²·255) it would if the CentralPanel fill and a
/// per-pane fill both painted at the opacity alpha and COMPOUNDED (the haze bug).
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn pane_backing_alpha_is_linear_single_paint() {
    let img = render_with(|c| {
        c.opacity = 0.7;
        c.tint_enabled = false;
        c.frost_enabled = false;
        c.effects.wired_ambient = false;
    });
    let (modal, _) = modal_pane_alpha(&img);
    let linear = (0.7 * 255.0_f32).round() as i32; // 179
    let squared = (0.7 * 0.7 * 255.0_f32).round() as i32; // 125
    eprintln!("pane backing modal alpha = {modal} (linear≈{linear}, squared≈{squared})");
    assert!(
        (i32::from(modal) - linear).abs() <= 8,
        "opacity 0.7 backing must be LINEAR (~{linear}), got {modal} — a value near \
         {squared} would mean the background is still painted twice (compounding)"
    );
}

/// PART 2 GUARD (needs a GPU): the frosted-glass wash is painted ONLY when
/// enabled, and it visibly thickens the backing (independent of opacity). At a
/// see-through opacity, turning frost ON must raise the pane-backing alpha well
/// above the frost-OFF baseline.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn frost_wash_appears_only_when_enabled() {
    let off = render_with(|c| {
        c.opacity = 0.3;
        c.tint_enabled = false;
        c.frost_enabled = false;
    });
    let on = render_with(|c| {
        c.opacity = 0.3;
        c.tint_enabled = false;
        c.frost_enabled = true;
        c.frost_amount = 0.6;
        c.frost_grain = false; // flat wash for a deterministic alpha comparison
    });
    let (off_a, _) = modal_pane_alpha(&off);
    let (on_a, _) = modal_pane_alpha(&on);
    eprintln!("frost off backing alpha = {off_a}, frost on = {on_a}");
    assert!(
        i32::from(on_a) >= i32::from(off_a) + 40,
        "enabling frost must visibly thicken the backing wash (off={off_a}, on={on_a})"
    );
}
