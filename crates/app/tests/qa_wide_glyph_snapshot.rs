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
/// covered by construction.
///
/// The dir is a `tempfile` dir, NOT a name derived from the process id: pids
/// recycle, and a recycled name is a dir a PREVIOUS run already saved a config
/// into — which is the same read-contamination this function exists to remove,
/// just sourced from an old test run instead of the developer's profile.
///
/// # A FRESH dir PER HARNESS, not one per process
///
/// This used to hand out ONE `OnceLock` dir for the whole process, and that
/// re-created the very contamination it exists to prevent — only sourced from a
/// SIBLING SCENE instead of the developer's profile. The app SAVES its config,
/// so with a shared dir scene N's persisted settings are scene N+1's loaded
/// settings, and the scenes leak into each other in name order.
///
/// That was not hypothetical. Under a plain `cargo test ... -- --ignored`
/// (ONE process — the exact command this module's header tells you to run),
/// `qa_tint_transparent_low_opacity` persisted `opacity = 0.10` with
/// `tint_enabled` and `tint = #ff0040`, and every alphabetically-later scene
/// rendered through it: `qa_wide_glyph_frame`'s centre pane pixel was
/// `(67, 2, 18, 91)` —
/// a red-washed, 36%-alpha frame — where the same scene run ALONE renders
/// `(18, 18, 18, 255)`. The wide-glyph, toolbar-settings and tint-settings PNGs
/// a human is asked to eyeball were all being produced through another scene's
/// transparency.
///
/// It went unseen because CI runs this file under nextest (process-per-test),
/// where each process gets its own dir and the leak cannot occur — so the CI
/// path and the local eyeball path disagreed, and only the local one was wrong.
/// The structural guard below proves every scene goes through ONE constructor;
/// it could not see that the constructor pointed them all at one MUTABLE dir.
///
/// Each `TempDir` is parked in a process-lifetime `static` rather than returned,
/// because the app keeps writing to it for as long as its harness lives — it
/// must never be dropped mid-run.
fn isolate_config_dir() {
    use std::sync::Mutex;
    static DIRS: Mutex<Vec<tempfile::TempDir>> = Mutex::new(Vec::new());
    let dir = tempfile::Builder::new()
        .prefix("c0pl4nd-qa-cfg-")
        .tempdir()
        .expect("create the QA config dir");
    // Edition 2021: `set_var` is safe here. All these scenes run single-threaded
    // (`--test-threads=1`, and the wgpu renders serialise anyway), so there is no
    // racing writer.
    std::env::set_var("APPDATA", dir.path());
    std::env::set_var("XDG_CONFIG_HOME", dir.path());
    std::env::set_var("HOME", dir.path());
    DIRS.lock()
        .expect("the QA config-dir registry is never poisoned")
        .push(dir);
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

// =============================================================================
// PIXEL-ASSERTING GRID-PAINT TESTS
// =============================================================================
//
// Everything above this line renders a frame and eyeballs it (plus the two
// driver-independent guards in `snapshot`). That is not enough for the paint
// contract itself: the headline per-cell BACKGROUND fix, the styled underlines,
// the SGR-58 underline colour, strikeout and overline were all verified only by
// TYPE-PLUMBING tests (does a `RunStyle` carry a `bg`? does `row_cell_spans`
// merge?). A cut wire in `paint_grid_native`'s PASS 1 / PASS 3 emit would leave
// every one of those green — the grid would silently go back to drawing plain
// text on the window background, which is the exact defect that was fixed.
//
// The tests below close that gap by asserting SPECIFIC COLOURS AT SPECIFIC
// PIXELS. They are deliberately not "the frame is not uniform" checks: each one
// feeds a known SGR sequence, computes where the paint MUST land from the
// production geometry (`pane_body_rect` + `grid_text_origin_for`), and asserts
// the exact RGB there — plus a CONTROL render of the same content WITHOUT the
// SGR, which must contain ZERO pixels of that colour. The control is what makes
// them differential rather than "some pixel happened to be orange".
//
// Content is fed with `C0pl4ndApp::test_feed_focused` (straight into the
// emulator, bypassing the PTY) so the grid contents are deterministic and do not
// depend on a shell being willing to emit ANSI.
//
// Why exact equality is legitimate here: the backgrounds and the straight
// decorations are opaque `rect_filled` quads snapped to the physical pixel grid,
// so their interiors rasterise BYTE-EXACT on any driver (measured: a 4-cell quad
// covers exactly `width_px * height_px` pixels of exactly the requested RGB, at
// alpha 255). Only the analytic CURLY underline is antialiased, and that one
// case uses a tolerance — and asserts the property antialiasing cannot fake (a
// multi-scanline amplitude).

/// Grid-paint probe colours. Chosen to be absent from the chrome + the default
/// theme so an exact-match count is unambiguous — every one of them measures 0
/// in the control renders below.
const PX_BG_A: [u8; 3] = [173, 41, 209];
const PX_BG_B: [u8; 3] = [17, 191, 153];
/// Per-row calibration backgrounds — painted to the RIGHT of the decorated span
/// so each row carries its own MEASURED y-extent (see [`px_row_band`]). One
/// colour PER ROW, deliberately: consecutive rows tile with no gap, so a single
/// shared colour would merge into one indivisible block and a per-row band could
/// only be recovered by dividing by an ASSUMED row count. A distinct colour per
/// row keeps every band directly measured.
const PX_CAL_ROWS: [[u8; 3]; 4] = [[61, 59, 199], [199, 61, 59], [59, 199, 137], [183, 199, 59]];
/// Decoration colour (used as an SGR-58 underline colour and as a foreground).
const PX_DECO: [u8; 3] = [255, 128, 0];
/// A second foreground, for the "SGR 58 is underline-scoped" separation.
const PX_TEXT: [u8; 3] = [0, 255, 68];

/// The set of pixels matching one colour inside a y-slice of the frame.
#[derive(Debug, Clone)]
struct PxMask {
    /// How many pixels matched.
    n: u64,
    /// Leftmost / rightmost matching x (inclusive). Meaningless when `n == 0`.
    x0: u32,
    x1: u32,
    /// Every DISTINCT scanline that carries at least one match, ascending. This
    /// is the discriminator between the underline variants: a single underline
    /// occupies exactly ONE scanline, a double occupies TWO, a curl at least
    /// three.
    ys: Vec<u32>,
}

impl PxMask {
    fn is_empty(&self) -> bool {
        self.n == 0
    }
    /// Width of the matching span in pixels.
    fn w(&self) -> u32 {
        if self.n == 0 {
            0
        } else {
            self.x1 - self.x0 + 1
        }
    }
    fn y0(&self) -> u32 {
        *self.ys.first().expect("mask is empty")
    }
    fn y1(&self) -> u32 {
        *self.ys.last().expect("mask is empty")
    }
    /// Height of the matching span in pixels (inclusive of both ends).
    fn h(&self) -> u32 {
        self.y1() - self.y0() + 1
    }
}

/// Every pixel within `tol` (per channel, L-inf) of `want`, restricted to the
/// half-open scanline range `y0..y1`. `tol == 0` is exact equality.
fn px_mask(img: &image::RgbaImage, want: [u8; 3], tol: i32, y0: u32, y1: u32) -> PxMask {
    let (mut n, mut x0, mut x1) = (0u64, u32::MAX, 0u32);
    let mut ys: Vec<u32> = Vec::new();
    for y in y0..y1.min(img.height()) {
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

/// Exact-RGB variant of [`px_mask`] over the whole frame.
fn px_exact(img: &image::RgbaImage, want: [u8; 3]) -> PxMask {
    px_mask(img, want, 0, 0, img.height())
}

/// THE harness for these tests: the real app, rendered at 1 physical pixel per
/// point, with every state that would tint, fade or overlay the grid turned OFF.
///
/// Those overrides are not cosmetic — they are what makes an EXACT colour
/// assertion legitimate. A persisted `opacity < 1`, a tint, frost, scanlines,
/// flicker, VHS bands, the ambient mesh or chromatic aberration each composite
/// over (or under) the grid and would shift the quad's bytes, so an exact match
/// would fail for a reason that has nothing to do with the paint contract under
/// test. `build_harness` already isolates the config dir, so these start from
/// the DEFAULTS and not from whatever the developer last saved.
fn px_harness() -> Harness<'static, egui_app::C0pl4ndApp> {
    let mut h = build_harness(None, |app| {
        app.config.opacity = 1.0;
        app.config.tint_enabled = false;
        app.config.frost_enabled = false;
        app.config.effects.wired_ambient = false;
        app.config.effects.crt_scanlines = false;
        app.config.effects.flicker = false;
        app.config.effects.vhs_tracking = false;
        app.config.effects.chromatic_aberration_enabled = false;
    });
    // Let the pane spawn and its shell emit its banner, so a late line of
    // startup output cannot land on top of the fed content mid-assert.
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
    panic!(
        "the focused pane never produced startup output — a still-booting shell \
         can overwrite the fed grid mid-render, so these pixel asserts would be \
         measuring a race rather than the paint path"
    );
}

/// Clear the screen, feed `payload` into the focused pane's emulator, and render
/// one real frame. The `ESC[2J ESC[H` prefix makes each call independent, so one
/// harness can serve both a test payload and its CONTROL.
fn px_feed(h: &mut Harness<'_, egui_app::C0pl4ndApp>, payload: &str) -> image::RgbaImage {
    let mut buf = String::from("\x1b[2J\x1b[H");
    buf.push_str(payload);
    h.state_mut().test_feed_focused(buf.as_bytes());
    for _ in 0..5 {
        h.step();
    }
    h.render().expect("kittest wgpu render must succeed")
}

/// The grid's text origin in PHYSICAL pixels, taken from the PRODUCTION geometry
/// (`pane_body_rect` fed through `grid_text_origin_for`, the same helper
/// `paint_grid_native` calls) rather than re-derived here.
///
/// This is what turns "an orange pixel exists somewhere" into "the paint landed
/// where the layout says cell (0,0) is". A regression that painted the grid at
/// the pane origin instead of the padded text origin fails on it.
fn px_grid_origin(h: &Harness<'_, egui_app::C0pl4ndApp>) -> (u32, u32) {
    let s = h.state();
    let rect = s
        .pane_body_rect(s.focused_pane())
        .expect("the focused pane has been laid out");
    let o = s.grid_text_origin_for(rect);
    let ppp = h.ctx.pixels_per_point();
    // `paint_grid_native` snaps every quad edge with `snap_to_physical`, which is
    // a round to the physical pixel grid — mirror that here.
    ((o.x * ppp).round() as u32, (o.y * ppp).round() as u32)
}

/// The exact scanline range `[y0, y1)` of grid row `row`, MEASURED from that
/// row's own calibration background quad rather than computed from a guessed row
/// pitch. Every decoration payload below paints [`PX_CAL_ROWS`]`[row]` over
/// columns to the RIGHT of the decorated span, so each row carries its own ruler
/// and a decoration's position is checked against real geometry.
fn px_row_band(img: &image::RgbaImage, row: usize) -> (u32, u32) {
    let cal = px_exact(img, PX_CAL_ROWS[row]);
    assert!(
        !cal.is_empty(),
        "row {row}'s calibration background is missing from the frame — either \
         PASS 1 does not paint backgrounds at all, or the payload never reached \
         the grid"
    );
    assert_eq!(
        cal.ys.len() as u32,
        cal.h(),
        "row {row}'s calibration band must be contiguous scanlines, got {:?}",
        cal.ys
    );
    (cal.y0(), cal.y1() + 1)
}

// ---------------------------------------------------------------------------
// PASS 1 — per-cell background quads (the headline fix)
// ---------------------------------------------------------------------------

/// THE background regression guard. The #1 reported defect was that per-cell
/// background colour was discarded; the fix gives `ColorRun` a `RunStyle` with a
/// `bg` and emits a filled quad per span in PASS 1. Nothing pixel-level covered
/// it, so cutting the `painter.rect_filled` in PASS 1 stayed green.
///
/// This asserts the quad lands at the cell rectangle the layout computes, is
/// SOLID (every interior pixel is exactly the requested RGB at alpha 255), does
/// not bleed outside the grid, and that a second row tiles onto the first with
/// no seam and no overlap.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn bg_quads_paint_the_exact_sgr_colour_at_the_computed_cell_rect() {
    let mut h = px_harness();
    let (ox, oy) = px_grid_origin(&h);

    // Row 0: 4 cells of A. Row 1: EIGHT cells of B — twice the columns, so the
    // measured widths pin that x is computed from the cell COLUMN.
    let img = px_feed(
        &mut h,
        &format!(
            "{}    \x1b[0m\r\n{}        \x1b[0m\r\n",
            px_sgr_bg(PX_BG_A),
            px_sgr_bg(PX_BG_B)
        ),
    );

    let a = px_exact(&img, PX_BG_A);
    let b = px_exact(&img, PX_BG_B);
    eprintln!("bg quads: A={a:?} B={b:?} origin=({ox},{oy})");
    assert!(
        !a.is_empty() && !b.is_empty(),
        "PASS 1 painted NO background quad — a cell with an explicit SGR 48 \
         background rendered on the window default. This is the reported defect: \
         A={a:?} B={b:?}"
    );

    // 1. The quad starts EXACTLY at the grid text origin (cell 0,0), not at the
    //    pane origin and not one padding unit off.
    assert_eq!(
        (a.x0, a.y0()),
        (ox, oy),
        "the row-0 background quad must start at the production grid text origin \
         ({ox},{oy}); it started at ({},{})",
        a.x0,
        a.y0()
    );

    // 2. It is SOLID: exactly width*height pixels of exactly that colour, i.e.
    //    no gaps, no partial-alpha edges eating the interior.
    assert_eq!(
        a.n,
        u64::from(a.w()) * u64::from(a.h()),
        "the background quad must be a solid rectangle: {} matching pixels for a \
         {}x{} bounding box",
        a.n,
        a.w(),
        a.h()
    );
    // …and fully opaque, so text drawn on top composites against the SGR colour
    // and not against a half-transparent version of it.
    assert_eq!(
        img.get_pixel(a.x0 + 1, a.y0() + 1).0,
        [PX_BG_A[0], PX_BG_A[1], PX_BG_A[2], 255],
        "the background quad must be fully opaque at the requested RGB"
    );

    // 3. No bleed OUTSIDE the grid: the padding column immediately left of the
    //    text origin is untouched.
    assert!(
        ox >= 1,
        "the grid origin must be inset by the window padding"
    );
    let left_of_grid = img.get_pixel(ox - 1, oy + 1).0;
    assert_ne!(
        [left_of_grid[0], left_of_grid[1], left_of_grid[2]],
        PX_BG_A,
        "the background quad bled into the window padding at x={}",
        ox - 1
    );

    // 4. Row 1 tiles onto row 0 with NO seam and NO overlap — the property the
    //    physical-pixel snapping exists for.
    assert_eq!(
        b.y0(),
        a.y1() + 1,
        "row 1's quad must start on the scanline immediately after row 0's last \
         (rows {:?} then {:?}) — a gap is a hairline seam, an overlap is a \
         double-painted row",
        (a.y0(), a.y1()),
        (b.y0(), b.y1())
    );
    assert_eq!(
        a.h(),
        b.h(),
        "both rows must be exactly one row pitch tall ({} vs {})",
        a.h(),
        b.h()
    );

    // 5. Eight cells are twice as wide as four (±1px of snapping slack): x is
    //    computed from the cell column, not accumulated from glyph advances.
    let (w4, w8) = (i64::from(a.w()), i64::from(b.w()));
    assert!(
        (w8 - 2 * w4).abs() <= 1,
        "an 8-cell background span must be twice a 4-cell one: {w4}px vs {w8}px"
    );
    assert_eq!(
        b.x0, ox,
        "row 1's quad must also start at column 0 ({ox}), got {}",
        b.x0
    );

    // 6. CONTROL — the same two rows of spaces with NO background SGR paint
    //    nothing. Without this, "some pixel is magenta" could be satisfied by any
    //    chrome accident; with it, the quads are attributable to the SGR alone.
    let control = px_feed(&mut h, "    \r\n        \r\n");
    for (name, want) in [("A", PX_BG_A), ("B", PX_BG_B)] {
        let m = px_exact(&control, want);
        assert!(
            m.is_empty(),
            "CONTROL: with no SGR 48 the frame must contain zero {name} pixels, \
             found {} — the assertion above is not attributable to the background \
             paint",
            m.n
        );
    }
}

/// Adjacent background spans on ONE row must tile exactly: the merge in
/// `row_cell_spans` plus the physical-pixel snapping is specifically there so a
/// highlighted region shows no hairline seam and no double-painted column. A
/// seam is the visible artefact users report as "the selection has stripes".
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn adjacent_bg_spans_tile_with_no_seam_and_no_overlap() {
    let mut h = px_harness();
    let img = px_feed(
        &mut h,
        &format!(
            "{}    {}    \x1b[0m\r\n",
            px_sgr_bg(PX_BG_A),
            px_sgr_bg(PX_BG_B)
        ),
    );
    let a = px_exact(&img, PX_BG_A);
    let b = px_exact(&img, PX_BG_B);
    assert!(!a.is_empty() && !b.is_empty(), "both spans must paint");
    eprintln!(
        "adjacent spans: A x=[{},{}] B x=[{},{}]",
        a.x0, a.x1, b.x0, b.x1
    );

    assert_eq!(
        (a.y0(), a.y1()),
        (b.y0(), b.y1()),
        "two spans on the same grid row must occupy the same scanlines"
    );
    assert_eq!(
        b.x0,
        a.x1 + 1,
        "the second span must begin on the column immediately after the first \
         ends (A ends at {}, B starts at {}) — any other value is a seam or an \
         overlap",
        a.x1,
        b.x0
    );

    // Scan the row's middle scanline end-to-end: EVERY pixel from the first
    // span's left edge to the second's right edge must be one of the two
    // colours. A single background-coloured pixel in between is the hairline.
    let mid = (a.y0() + a.y1()) / 2;
    for x in a.x0..=b.x1 {
        let p = img.get_pixel(x, mid).0;
        let rgb = [p[0], p[1], p[2]];
        assert!(
            rgb == PX_BG_A || rgb == PX_BG_B,
            "seam at ({x},{mid}): {rgb:?} is neither span colour — the two \
             background quads do not tile"
        );
    }
}

/// A coloured background must sit BEHIND the glyph, not instead of it. This is
/// the `grep --color` / `git diff` / fzf-selected-row case: both the block and
/// the text have to be visible in the same cells.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn bg_quad_is_painted_behind_the_glyph_not_over_it() {
    let mut h = px_harness();
    let img = px_feed(
        &mut h,
        &format!(
            "{}\x1b[38;2;255;255;255mABCD\x1b[0m\r\n",
            px_sgr_bg(PX_BG_A)
        ),
    );
    let bg = px_exact(&img, PX_BG_A);
    assert!(
        !bg.is_empty(),
        "the background quad is missing behind the text"
    );
    // The glyph core is matched with a TOLERANCE, unlike everything else in this
    // module. Backgrounds and straight decorations are opaque axis-aligned quads
    // and rasterise byte-exact anywhere; a GLYPH's coverage is decided by
    // epaint's font rasteriser, so how many pixels reach FULL white depends on
    // the face and hinting. The tolerance keeps this portable to the CI software
    // rasteriser without weakening what is asserted: a glyph that is not drawn
    // at all still yields ZERO matches, which is the regression this guards.
    //
    // It is scoped to the BACKGROUND ROW'S scanlines, not the whole frame. The
    // chrome (titlebar labels, status bar) is near-white text too, so a tolerant
    // whole-frame mask matches it and the "glyphs sit inside the block" check
    // below then compares the block against the titlebar. Exact-white happened
    // not to match the chrome, which is why the unscoped form survived until the
    // tolerance was introduced.
    let fg = px_mask(&img, [255, 255, 255], 60, bg.y0(), bg.y1() + 1);
    eprintln!("bg-behind-glyph: bg n={} fg n={}", bg.n, fg.n);
    assert!(
        !fg.is_empty(),
        "the glyphs are missing — the background quad was painted OVER the text \
         (PASS 1 must run before PASS 2)"
    );
    // The glyph pixels must lie INSIDE the background span's columns (the mask is
    // already bounded to its scanlines).
    assert!(
        fg.x0 >= bg.x0 && fg.x1 <= bg.x1,
        "the glyphs must render within the cells that carry the background: \
         glyphs x=[{},{}] vs background x=[{},{}]",
        fg.x0,
        fg.x1,
        bg.x0,
        bg.x1
    );
    // The background is NOT solid any more — the glyphs punched through it.
    assert!(
        bg.n < u64::from(bg.w()) * u64::from(bg.h()),
        "the glyphs did not displace any background pixel, so nothing was drawn \
         on top of the quad"
    );
}

/// Reverse video (SGR 7) is only visible BECAUSE of the background quad: with no
/// quad an inverse cell drew its background colour as text onto an unchanged
/// background, i.e. nothing. Asserting it pins the inverse path end-to-end.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn reverse_video_paints_the_foreground_colour_as_a_background_quad() {
    let mut h = px_harness();
    let sgr_fg = format!("\x1b[38;2;{};{};{}m", PX_BG_A[0], PX_BG_A[1], PX_BG_A[2]);
    // Four SPACES: with SGR 7 the (empty) cells must still be filled with the
    // foreground colour, because inverse swaps fg and bg.
    let img = px_feed(&mut h, &format!("{sgr_fg}\x1b[7m    \x1b[0m\r\n"));
    let m = px_exact(&img, PX_BG_A);
    eprintln!("reverse video: {m:?}");
    assert!(
        !m.is_empty(),
        "SGR 7 on blank cells painted nothing — reverse video is invisible \
         without the background quad"
    );
    assert_eq!(
        m.n,
        u64::from(m.w()) * u64::from(m.h()),
        "the inverse block must be a solid filled rectangle"
    );

    // CONTROL: the same colour as a plain FOREGROUND on blank cells paints
    // nothing at all (blank cells emit no glyph), so the block above is
    // attributable to the inverse-background path and not to text.
    let control = px_feed(&mut h, &format!("{sgr_fg}    \x1b[0m\r\n"));
    assert!(
        px_exact(&control, PX_BG_A).is_empty(),
        "CONTROL: blank cells with only a foreground colour must paint nothing"
    );
}

// ---------------------------------------------------------------------------
// PASS 3 — line decorations
// ---------------------------------------------------------------------------

/// Build the SGR that sets a 24-bit background.
fn px_sgr_bg(c: [u8; 3]) -> String {
    format!("\x1b[48;2;{};{};{}m", c[0], c[1], c[2])
}
/// Build the SGR that sets a 24-bit foreground.
fn px_sgr_fg(c: [u8; 3]) -> String {
    format!("\x1b[38;2;{};{};{}m", c[0], c[1], c[2])
}
/// Build the SGR 58 that sets a 24-bit UNDERLINE colour.
fn px_sgr_ul(c: [u8; 3]) -> String {
    format!("\x1b[58;2;{};{};{}m", c[0], c[1], c[2])
}

/// One decorated row: 8 SPACES carrying `sgr`, then a 4-cell calibration
/// background so the row's exact scanline band can be measured from the frame.
///
/// Spaces matter: `row_glyph_cells` skips blank cells, so the row emits NO
/// glyph — every decoration-coloured pixel in it is therefore the decoration
/// itself, with no need to separate it from antialiased glyph edges.
fn px_deco_row(sgr: &str, row: usize) -> String {
    format!(
        "{sgr}        \x1b[0m{}    \x1b[0m\r\n",
        px_sgr_bg(PX_CAL_ROWS[row])
    )
}

/// Underline, strikeout and overline must land at the BOTTOM, MIDDLE and TOP of
/// the cell respectively — and in that vertical order. Position is the whole
/// point of a decoration: an underline drawn at the cell top is an overline.
///
/// All three are rendered in one frame so the ordering is asserted against a
/// single geometry, and each row carries its own calibration band so the
/// positions are checked against the row's MEASURED extent rather than a
/// hardcoded row pitch.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn underline_strikeout_and_overline_land_at_the_bottom_middle_and_top_of_the_cell() {
    let mut h = px_harness();
    let fg = px_sgr_fg(PX_TEXT);
    let img = px_feed(
        &mut h,
        &format!(
            "{}{}{}",
            // row 0: overline, row 1: strikeout, row 2: underline
            px_deco_row(&format!("\x1b[53m{fg}"), 0),
            px_deco_row(&format!("\x1b[9m{fg}"), 1),
            px_deco_row(&format!("\x1b[4m{fg}"), 2),
        ),
    );

    let mut ys = Vec::new();
    for (row, name) in [(0usize, "overline"), (1, "strikeout"), (2, "underline")] {
        let (b0, b1) = px_row_band(&img, row);
        let m = px_mask(&img, PX_TEXT, 0, b0, b1);
        eprintln!("{name}: row band [{b0},{b1}) mask={m:?}");
        assert!(
            !m.is_empty(),
            "{name} painted NO pixels in its row band [{b0},{b1}) — the PASS 3 \
             emit for it is not reached"
        );
        assert_eq!(
            m.ys.len(),
            1,
            "{name} must be a single 1px rule; it occupies {} scanlines {:?}",
            m.ys.len(),
            m.ys
        );
        // It must cover the whole 8-cell span, not one dash per glyph.
        assert_eq!(
            m.n,
            u64::from(m.w()),
            "{name} must be a continuous horizontal rule across its span \
             ({} pixels over a {}px width)",
            m.n,
            m.w()
        );
        let y = m.y0();
        let h_band = b1 - b0;
        // Where in the cell, as a fraction of the row pitch.
        let frac = f64::from(y - b0) / f64::from(h_band);
        match name {
            // SGR 53 draws at `snap(row_y)` — the very first scanline of the cell.
            "overline" => assert_eq!(
                y, b0,
                "the overline must sit on the cell's TOP scanline ({b0}), got {y}"
            ),
            // SGR 9 draws at `row_y + ch * 0.55`.
            "strikeout" => assert!(
                (0.35..0.75).contains(&frac),
                "the strikeout must cross the cell MIDDLE; it is at {frac:.2} of \
                 the row pitch (y={y} in band [{b0},{b1}))"
            ),
            // SGR 4 draws at `row_y + ch - thickness*2`.
            _ => assert!(
                frac >= 0.80,
                "the underline must sit near the cell BOTTOM; it is at {frac:.2} \
                 of the row pitch (y={y} in band [{b0},{b1}))"
            ),
        }
        ys.push((name, y));
    }
    assert!(
        ys[0].1 < ys[1].1 && ys[1].1 < ys[2].1,
        "the three decorations must stack overline < strikeout < underline \
         within their equal-height rows: {ys:?}"
    );

    // CONTROL — the same three rows of spaces in the same colour with NO
    // decoration SGR paint nothing at all.
    let control = px_feed(
        &mut h,
        &format!(
            "{}{}{}",
            px_deco_row(&fg, 0),
            px_deco_row(&fg, 1),
            px_deco_row(&fg, 2)
        ),
    );
    let m = px_exact(&control, PX_TEXT);
    assert!(
        m.is_empty(),
        "CONTROL: blank cells with only a foreground colour and no decoration \
         SGR must paint zero pixels of it, found {}",
        m.n
    );
}

/// SGR 58 sets the UNDERLINE's own colour, and it is scoped to the underline:
/// strikeout and overline keep taking the TEXT colour. Both halves matter —
/// dropping the `underline_color` lookup makes the underline take the fg (which
/// still "works", so a type test passes), and widening it to the other
/// decorations silently recolours them.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn sgr58_colours_the_underline_only_and_falls_back_to_the_foreground() {
    let mut h = px_harness();
    let img = px_feed(
        &mut h,
        &format!(
            "{}{}{}{}",
            // row 0: underline, DECO as the foreground, no SGR 58 -> underline
            //        must fall back to the foreground colour.
            px_deco_row(&format!("\x1b[4m{}", px_sgr_fg(PX_DECO)), 0),
            // row 1: underline, TEXT foreground + SGR 58 DECO -> the underline
            //        takes the SGR-58 colour, NOT the foreground.
            px_deco_row(
                &format!("\x1b[4m{}{}", px_sgr_fg(PX_TEXT), px_sgr_ul(PX_DECO)),
                1,
            ),
            // row 2: strikeout with the same pair -> must be the FOREGROUND.
            px_deco_row(
                &format!("\x1b[9m{}{}", px_sgr_fg(PX_TEXT), px_sgr_ul(PX_DECO)),
                2,
            ),
            // row 3: overline with the same pair -> must be the FOREGROUND.
            px_deco_row(
                &format!("\x1b[53m{}{}", px_sgr_fg(PX_TEXT), px_sgr_ul(PX_DECO)),
                3,
            ),
        ),
    );

    // Row 0 — no SGR 58: the underline is the foreground colour.
    let (b0, b1) = px_row_band(&img, 0);
    let fallback = px_mask(&img, PX_DECO, 0, b0, b1);
    assert!(
        !fallback.is_empty(),
        "with no SGR 58 the underline must fall back to the foreground colour; \
         nothing was painted in row 0"
    );

    // Row 1 — SGR 58 wins over the foreground.
    let (b0, b1) = px_row_band(&img, 1);
    let deco = px_mask(&img, PX_DECO, 0, b0, b1);
    let text = px_mask(&img, PX_TEXT, 0, b0, b1);
    eprintln!("sgr58 row: deco n={} text n={}", deco.n, text.n);
    assert!(
        !deco.is_empty(),
        "SGR 58 must colour the underline: no {PX_DECO:?} pixels in row 1"
    );
    assert!(
        text.is_empty(),
        "the underline took the FOREGROUND colour instead of the SGR-58 colour: \
         {} {PX_TEXT:?} pixels in row 1",
        text.n
    );

    // Rows 2 and 3 — SGR 58 must NOT reach strikeout or overline.
    for (row, name) in [(2usize, "strikeout"), (3, "overline")] {
        let (b0, b1) = px_row_band(&img, row);
        let deco = px_mask(&img, PX_DECO, 0, b0, b1);
        let text = px_mask(&img, PX_TEXT, 0, b0, b1);
        assert!(
            !text.is_empty(),
            "the {name} must be drawn in the TEXT colour; nothing was painted in \
             row {row}"
        );
        assert!(
            deco.is_empty(),
            "SGR 58 leaked onto the {name}: {} underline-coloured pixels in row \
             {row}. SGR 58 scopes the custom colour to the UNDERLINE only",
            deco.n
        );
    }
}

/// The `4:n` styled-underline variants must be VISUALLY DISTINCT, not merely
/// parsed into distinct enum values. Each is identified by the property its
/// paint arm is supposed to produce:
///
/// | SGR     | style  | signature asserted                                    |
/// |---------|--------|-------------------------------------------------------|
/// | `4`     | single | exactly 1 scanline, continuous across the span         |
/// | `4:0`   | none   | nothing painted                                        |
/// | `4:1`   | single | pixel-identical to plain `4`                           |
/// | `4:2`   | double | exactly 2 scanlines, straddling the single's            |
/// | `4:3`   | curly  | >= 3 scanlines (the sine amplitude)                     |
/// | `4:5`   | dashed | 1 scanline, GAPPED (interior columns with no paint)     |
///
/// `4:4` (dotted) is deliberately absent — it currently paints nothing at all.
/// That is a real, reproducible defect in the renderer, NOT an omission of
/// convenience: see `underline_dotted_4_4_defect.rs`, which carries the failing
/// reproduction and the shape-level evidence. Adding it here would make this
/// test red for a cause the test file cannot fix.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn styled_underline_variants_are_visually_distinct() {
    let mut h = px_harness();
    let ul = px_sgr_ul(PX_DECO);

    // Measure each variant on its own frame so one variant's paint can never be
    // mistaken for another's.
    let measure = |h: &mut Harness<'_, egui_app::C0pl4ndApp>, sgr: &str| -> (PxMask, PxMask) {
        let img = px_feed(h, &px_deco_row(&format!("{sgr}{ul}"), 0));
        let (b0, b1) = px_row_band(&img, 0);
        (
            px_mask(&img, PX_DECO, 0, b0, b1),
            // A tolerant mask for the antialiased curl, which has almost no
            // byte-exact pixel.
            px_mask(&img, PX_DECO, 90, b0, b1),
        )
    };

    let (single, _) = measure(&mut h, "\x1b[4m");
    assert!(
        !single.is_empty(),
        "the plain SGR 4 underline painted nothing"
    );
    assert_eq!(
        single.ys.len(),
        1,
        "a single underline occupies exactly one scanline, got {:?}",
        single.ys
    );
    assert_eq!(
        single.n,
        u64::from(single.w()),
        "a single underline is continuous across its span: {} pixels over {}px",
        single.n,
        single.w()
    );

    let (none, none_tol) = measure(&mut h, "\x1b[4:0m");
    assert!(
        none.is_empty() && none_tol.is_empty(),
        "`4:0` means NO underline; it painted {} pixels ({} within tolerance)",
        none.n,
        none_tol.n
    );

    let (s1, _) = measure(&mut h, "\x1b[4:1m");
    assert_eq!(
        (s1.n, s1.x0, s1.x1, s1.ys.clone()),
        (single.n, single.x0, single.x1, single.ys.clone()),
        "`4:1` is the same single underline as plain `4`; the two renders differ"
    );

    let (double, _) = measure(&mut h, "\x1b[4:2m");
    assert_eq!(
        double.ys.len(),
        2,
        "`4:2` must paint TWO separated hairlines, got scanlines {:?}",
        double.ys
    );
    assert!(
        double.ys[0] < single.ys[0] && double.ys[1] > single.ys[0],
        "the double underline's two rules must straddle the single's baseline \
         (single at {}, double at {:?})",
        single.ys[0],
        double.ys
    );
    assert_eq!(
        double.n,
        2 * u64::from(double.w()),
        "both rules of the double underline must span the full width"
    );

    let (_, curly) = measure(&mut h, "\x1b[4:3m");
    assert!(
        curly.ys.len() >= 3,
        "`4:3` is an undercurl: its sine must occupy at least three scanlines, \
         got {:?} — a flat line here means the curly arm degenerated to a single \
         rule",
        curly.ys
    );
    assert!(
        curly.w() >= single.w() - 2,
        "the curl must span the same run width as a single underline ({} vs {})",
        curly.w(),
        single.w()
    );

    let (dashed, _) = measure(&mut h, "\x1b[4:5m");
    assert!(!dashed.is_empty(), "`4:5` painted nothing");
    assert_eq!(
        dashed.ys.len(),
        1,
        "a dashed underline stays on one scanline, got {:?}",
        dashed.ys
    );
    assert!(
        dashed.n * 5 < single.n * 4,
        "`4:5` must be GAPPED — it covered {} of the {} pixels a continuous \
         underline covers, which is indistinguishable from solid",
        dashed.n,
        single.n
    );
    assert!(
        dashed.n * 4 > single.n,
        "`4:5` must still be substantially drawn ({} of {} pixels)",
        dashed.n,
        single.n
    );
}

/// REGRESSION GUARD for the cross-scene config leak (needs a GPU).
///
/// [`no_scene_can_bypass_the_config_isolation`] proves every scene goes through
/// ONE constructor. It cannot prove that constructor hands each scene a
/// *separate* dir — and when it did not, scenes leaked into each other in name
/// order and the PNGs a human eyeballs were rendered through another scene's
/// transparency. See [`isolate_config_dir`] for the measured evidence.
///
/// This asserts the property end-to-end along the REAL leak path: opening
/// Settings persists the live config (that write is the leak's source), and a
/// subsequent scene must still start from the defaults.
///
/// The `config.toml`-exists check is a VACUITY GUARD, not decoration: if scene
/// one never persisted anything, scene two would see defaults no matter how
/// broken the isolation was, and this test would pass while asserting nothing.
#[test]
#[ignore = "needs a real GPU; run with --ignored"]
fn one_scenes_persisted_config_cannot_leak_into_the_next_scene() {
    let defaults = c0pl4nd_core::Config::default();
    // PREMISE GUARD: the three fields scene one sets must actually differ from
    // the defaults, or "scene two sees the defaults" would be satisfied by
    // scene one's values too and the test would prove nothing.
    assert!(
        (defaults.opacity - 0.10).abs() > 0.01
            && defaults.tint != "#ff0040"
            && (defaults.tint_strength - 0.8).abs() > 0.01,
        "this test's premise is that scene one's transparency differs from the \
         defaults; the defaults are opacity={} tint={} strength={}",
        defaults.opacity,
        defaults.tint,
        defaults.tint_strength
    );

    // --- scene one: the tint scene, with Settings opened so the config is
    //     actually written to disk (the module doc records that opening
    //     Settings persists `settings_win_w`; that write is the leak source).
    let scene_one_dir = {
        let mut h = build_tinted(0.10, 0.8);
        for _ in 0..10 {
            h.step();
        }
        h.get_by_label("settings").click();
        for _ in 0..6 {
            h.step();
        }
        std::path::PathBuf::from(std::env::var("APPDATA").expect("the harness redirected APPDATA"))
    };
    let persisted = scene_one_dir.join("c0pl4nd").join("config.toml");
    assert!(
        persisted.is_file(),
        "VACUOUS TEST GUARD: scene one did not persist a config to {} — without a \
         write there is nothing for scene two to inherit, so this test would pass \
         regardless of the isolation. Re-establish a scene that writes.",
        persisted.display()
    );
    let written = std::fs::read_to_string(&persisted).expect("read the persisted config");
    assert!(
        written.contains("#ff0040"),
        "VACUOUS TEST GUARD: scene one's persisted config does not carry the tint \
         colour it set, so inheriting it would be harmless. Contents:\n{written}"
    );

    // --- scene two: a plain harness must NOT inherit any of it.
    let h2 = build_harness(None, |_| {});
    let scene_two_dir =
        std::path::PathBuf::from(std::env::var("APPDATA").expect("the harness redirected APPDATA"));
    assert_ne!(
        scene_one_dir,
        scene_two_dir,
        "each scene must get its OWN throwaway config dir; both resolved to {}",
        scene_one_dir.display()
    );
    let cfg = &h2.state().config;
    assert_eq!(
        cfg.tint, defaults.tint,
        "scene two inherited scene one's tint colour ({} instead of the default \
         {}) — the config dir is shared again, so every scene after a tint scene \
         renders through it",
        cfg.tint, defaults.tint
    );
    assert_eq!(
        cfg.opacity, defaults.opacity,
        "scene two inherited scene one's opacity ({} instead of the default {})",
        cfg.opacity, defaults.opacity
    );
    assert_eq!(
        cfg.tint_strength, defaults.tint_strength,
        "scene two inherited scene one's tint strength ({} instead of the default \
         {})",
        cfg.tint_strength, defaults.tint_strength
    );
}
