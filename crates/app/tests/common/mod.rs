//! Shared helpers for this crate's REAL-frame integration tests.
//!
//! Extracted VERBATIM from `qa_wide_glyph_snapshot.rs` when `egui_term_render.rs`
//! needed the same measurement discipline. Integration tests are separate
//! binaries, so a `tests/common/` module is the only way to share them without a
//! second copy — and a copy is exactly what must not happen here: the inset in
//! [`pane_content_rect_px`] is load-bearing, and it is proved load-bearing in ONE
//! place (`qa_wide_glyph_snapshot::the_pane_content_assertion_rejects_a_blank_pane`,
//! which still guards it from there). Two copies would let one drift out from
//! under that proof.
//!
//! Nothing here is new logic: bodies, comments and thresholds are unchanged.
#![allow(dead_code)] // each test binary compiles its own copy and uses a subset

use c0pl4nd::egui_app;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

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
pub fn require_gpu() {
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
pub fn isolate_config_dir() {
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

// ---------------------------------------------------------------------------
// "the pane actually shows terminal content" — the check `snapshot` cannot make
// ---------------------------------------------------------------------------

/// How long a scene waits for a freshly-spawned shell to emit its first output.
///
/// Bounded on purpose: a shell that NEVER emits must FAIL the scene, not hang it
/// and not quietly snapshot a blank pane. Generous because this covers a cold
/// `cmd.exe` on a loaded machine and a login shell reading its rc files.
pub const SHELL_OUTPUT_TIMEOUT: Duration = Duration::from_secs(20);

/// Consecutive polls on which every pane must hold output before the wait is
/// satisfied.
///
/// One sighting is not enough. The pane's PTY is resized to fit its rect on the
/// first laid-out frames, and a reflow can momentarily blank the grid — so a
/// wait that returns on the FIRST non-empty read can hand the snapshot a grid
/// that is empty again a few frames later. Measured once, on a machine still
/// loaded from a full rebuild: the wait was satisfied and the frame then rendered
/// zero painted pixels. Requiring the output to still be there several polls
/// running costs ~100ms and removes that window.
pub const STABLE_OUTPUT_POLLS: u32 = 5;

/// Consecutive polls on which the focused pane's grid text must be UNCHANGED
/// before [`await_shell_quiescent`] is satisfied.
///
/// Deliberately larger than [`STABLE_OUTPUT_POLLS`], because it answers a harder
/// question. That constant asks "is output present?", and presence is settled by
/// the first byte. This one asks "has the shell finished?", and the honest
/// failure mode is catching a PAUSE mid-banner — a loaded machine can stall
/// between a shell's banner and its prompt for longer than the ~100ms five polls
/// buy. Twenty-five polls is ~500ms of no movement, which costs ~12s across the
/// whole 25-scene suite (a ~2% add on a ~520s run) and is far longer than any
/// mid-banner gap measured here.
pub const QUIESCENT_OUTPUT_POLLS: u32 = 25;

/// Minimum painted (non-background) pixels a pane body must carry to count as
/// "showing terminal content".
///
/// Calibrated from the real frames, not guessed. On the 1100x720 harness a
/// cmd.exe banner + prompt measures in the low thousands of painted pixels, and
/// an empty pane measures ZERO once the border ring and scrollbar band are
/// excluded (see [`pane_content_rect_px`]) — so the floor sits an order of
/// magnitude below real content and far above blank. It is not a "some pixel
/// differs" check: a lone cursor block (~14x7 px) would not clear it.
pub const MIN_PAINTED_PX_PER_PANE: u64 = 400;

/// Per-channel slack when deciding a pixel is "the background". Absorbs any
/// faint dither/gradient in the pane backing so a subtle non-flat background
/// cannot be counted as painted content.
pub const BG_TOL: i32 = 8;

/// Drive the real frame loop until EVERY pane in the grid has produced terminal
/// output, then let the frame settle.
///
/// This replaces the fixed step-and-sleep waits the affected scenes used. A
/// fixed wait is a race: it captured the full banner on one run and a completely
/// empty terminal on the next, and BOTH passed. The poll is the same shape
/// `egui_term_render.rs` and [`px_harness`] already use — step the production
/// loop, check observable state, bounded by a deadline.
///
/// It is deliberately a hard FAILURE on timeout. Returning early (or skipping)
/// would put the blank frame straight back into the PNG a human is asked to
/// eyeball, which is the entire defect.
pub fn await_every_pane_has_output(h: &mut Harness<'_, egui_app::C0pl4ndApp>, scene: &str) {
    let deadline = Instant::now() + SHELL_OUTPUT_TIMEOUT;
    let mut pending = String::from("the grid has no panes at all");
    let mut stable = 0u32;
    while Instant::now() < deadline {
        h.step();
        let ids = h.state().pane_ids();
        if ids.is_empty() {
            stable = 0;
        } else {
            let waiting: Vec<String> = ids
                .iter()
                .filter(|id| {
                    h.state()
                        .pane_grid_text(**id)
                        .is_none_or(|t| t.trim().is_empty())
                })
                .map(|id| format!("pane {}", id.raw()))
                .collect();
            if waiting.is_empty() {
                stable += 1;
                if stable >= STABLE_OUTPUT_POLLS {
                    // Settle: the poll fires on the FIRST byte reaching the grid,
                    // and a shell's banner + prompt arrive over several reads.
                    // Without this the snapshot can catch a half-drawn banner.
                    for _ in 0..10 {
                        h.step();
                    }
                    return;
                }
            } else {
                stable = 0;
                pending = waiting.join(", ");
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "QA-SNAPSHOT[{scene}]: {pending} produced NO terminal output within \
         {SHELL_OUTPUT_TIMEOUT:?}. This scene renders a frame a human is asked to \
         eyeball for terminal content, so snapshotting an empty pane here would \
         be a picture of nothing that passes."
    );
}

/// Drive the real frame loop until the focused pane's shell has STOPPED writing.
///
/// [`await_every_pane_has_output`] waits for output to be PRESENT on
/// [`STABLE_OUTPUT_POLLS`] consecutive polls, which is what a snapshot scene
/// needs — it photographs whatever the shell produced. A PIXEL test needs
/// something stricter, because it does not photograph the shell's output at all:
/// it clears the grid with `ESC[2J`, feeds its OWN content, and measures that.
/// What breaks it is therefore not an empty grid but a shell write that arrives
/// AFTER the feed and clears or scrolls the row just written.
///
/// The wait this replaces returned on the FIRST non-empty read. A real shell's
/// banner and prompt arrive over several PTY writes, and the pane's PTY is
/// resized to fit its rect on the first laid-out frames — so the remaining
/// writes, plus whatever the shell redraws in response to that resize, landed
/// after a test's feed and wiped it. The grid the test then measured was the
/// shell's, or nothing at all.
///
/// Measured, on this tree, before the change:
/// `adjacent_bg_spans_tile_with_no_seam_and_no_overlap` failed its `both spans
/// must paint` precondition on 1 of 4 consecutive full-suite runs — the fed row
/// was simply gone by render time — while passing in isolation and on the other
/// three. A second batch lost `underline_strikeout_and_overline_land_at_the_
/// bottom_middle_and_top_of_the_cell` the same way, so this is a property of the
/// harness, not of either test.
///
/// Waiting for QUIESCENCE closes it at the cause: a shell that has printed its
/// prompt and is blocked reading input writes nothing more, so its grid text
/// stops changing. This returns only once that text has been non-empty AND
/// byte-identical across [`QUIESCENT_OUTPUT_POLLS`] consecutive polls.
///
/// It is NOT a retry, and deliberately so: it runs BEFORE any content is fed and
/// before any assertion exists, it re-runs no measurement, and it can turn no
/// failing assertion into a passing one. It only establishes the precondition
/// every pixel scene in this crate already claims in its comments — that the
/// grid belongs to the test.
///
/// Bounded, and a hard PANIC on timeout. A shell that never settles must fail
/// loudly: returning early would hand the test a racing grid, which is the exact
/// defect, and skipping would report green without measuring anything.
pub fn await_shell_quiescent(h: &mut Harness<'_, egui_app::C0pl4ndApp>, scene: &str) {
    let deadline = Instant::now() + SHELL_OUTPUT_TIMEOUT;
    let mut last: Option<String> = None;
    let mut stable = 0u32;
    while Instant::now() < deadline {
        h.step();
        let text = h.state().test_focused_buffer_text();
        let has_output = text.as_deref().is_some_and(|t| !t.trim().is_empty());
        if has_output && text == last {
            stable += 1;
            if stable >= QUIESCENT_OUTPUT_POLLS {
                return;
            }
        } else {
            stable = 0;
            last = text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "{scene}: the focused pane's shell never went quiet within \
         {SHELL_OUTPUT_TIMEOUT:?} — its grid text never held still for \
         {QUIESCENT_OUTPUT_POLLS} consecutive polls. These scenes clear the grid \
         and feed their own content, so a shell still writing would overwrite it \
         and every pixel assertion below would be measuring that race instead of \
         the paint path."
    );
}

/// The region of a pane a reader actually reads: its production body rect, minus
/// the window padding and minus the scrollbar overlay band on the right edge.
///
/// Excluding those is what keeps [`assert_every_pane_rendered_content`] honest
/// rather than vacuous. The focused pane draws a 2px accent ring INSIDE its body
/// rect (`egui_app::mod` "background quad (theme bg) + focus ring"), which alone
/// paints well over a thousand pixels around an EMPTY pane — a count taken over
/// the raw rect would therefore be satisfied by exactly the blank frame this
/// assertion exists to reject. The padding inset (the same
/// `config_window_padding` the production `grid_text_origin` uses) clears the
/// ring; the right-edge band clears the scrollbar.
pub fn pane_content_rect_px(
    h: &Harness<'_, egui_app::C0pl4ndApp>,
    pane: egui_app::grid::PaneId,
) -> (u32, u32, u32, u32) {
    // `egui_app::scrollbar`'s BAR_WIDTH (8) + BAR_MARGIN (3) are private to the
    // crate, so their sum is mirrored here with a point of slack.
    const SCROLLBAR_BAND: f32 = 12.0;
    let s = h.state();
    let rect = s.pane_body_rect(pane).unwrap_or_else(|| {
        panic!(
            "pane {} has no laid-out body rect — the grid has not rendered a \
             frame yet, so there is nothing to measure",
            pane.raw()
        )
    });
    // Floored at the ring's own width so a zero-padding config still excludes it.
    let pad = f32::from(s.config_window_padding()).max(4.0);
    let ppp = h.ctx.pixels_per_point();
    let x0 = ((rect.left() + pad) * ppp).round() as u32;
    let y0 = ((rect.top() + pad) * ppp).round() as u32;
    let x1 = ((rect.right() - pad - SCROLLBAR_BAND) * ppp).round() as u32;
    let y1 = ((rect.bottom() - pad) * ppp).round() as u32;
    assert!(
        x1 > x0 && y1 > y0,
        "pane {}'s content rect is degenerate after insetting {rect:?} — the pane \
         is too small to carry any terminal content",
        pane.raw()
    );
    (x0, y0, x1, y1)
}

/// `(painted, total, background)` over the half-open region `[x0,x1) x [y0,y1)`:
/// how many pixels differ from that region's MODAL colour by more than
/// [`BG_TOL`] on any channel.
///
/// The modal colour is taken from the region itself rather than from the theme,
/// so this stays correct under a tint, a transparency alpha or a theme change —
/// it measures "was anything drawn ON the pane backing", which is the property,
/// not "is the backing the colour I expected".
pub fn painted_px_in(img: &image::RgbaImage, region: (u32, u32, u32, u32)) -> (u64, u64, [u8; 4]) {
    let (x0, y0, x1, y1) = region;
    let (x1, y1) = (x1.min(img.width()), y1.min(img.height()));
    let mut hist: std::collections::HashMap<[u8; 4], u64> = std::collections::HashMap::new();
    for y in y0..y1 {
        for x in x0..x1 {
            *hist.entry(img.get_pixel(x, y).0).or_default() += 1;
        }
    }
    let bg = *hist
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(c, _)| c)
        .expect("the content region is non-empty");
    let (mut painted, mut total) = (0u64, 0u64);
    for y in y0..y1 {
        for x in x0..x1 {
            total += 1;
            let p = img.get_pixel(x, y).0;
            if (0..4).any(|i| (i32::from(p[i]) - i32::from(bg[i])).abs() > BG_TOL) {
                painted += 1;
            }
        }
    }
    (painted, total, bg)
}

/// THE assertion that stops a scene passing while showing an empty terminal:
/// EVERY pane in the grid must carry painted content inside its own body rect.
///
/// Per-pane and scoped to production geometry, both deliberately. A whole-frame
/// count cannot tell "both panes render" from "one pane renders and the other is
/// blank" — which is precisely what `qa_split_panes` was shipping — and a count
/// over the raw rect would be satisfied by the focus ring of an empty pane.
pub fn assert_every_pane_rendered_content(
    h: &Harness<'_, egui_app::C0pl4ndApp>,
    img: &image::RgbaImage,
    scene: &str,
) {
    let ids = h.state().pane_ids();
    assert!(
        !ids.is_empty(),
        "QA-SNAPSHOT[{scene}]: the grid has no panes, so the frame cannot be \
         showing the terminal this scene claims to show"
    );
    for pane in ids {
        let region = pane_content_rect_px(h, pane);
        let (painted, total, bg) = painted_px_in(img, region);
        // The pane's grid state, read AFTER the frame was captured — so it
        // narrows a failure without over-claiming. An empty grid means the shell
        // had produced nothing, full stop. A NON-empty grid is the ambiguous
        // case: either the render dropped content it had, or the content landed
        // in the window between the captured frame and this read. (Measured: with
        // the post-split wait cut, pane 1 painted 0 pixels and then reported 97
        // grid characters a few milliseconds later — the second reading.) Saying
        // which is which is the reader's job; printing both is this message's.
        let grid = h.state().pane_grid_text(pane).unwrap_or_default();
        let grid_chars = grid.chars().filter(|c| !c.is_whitespace()).count();
        eprintln!(
            "QA-SNAPSHOT[{scene}]: pane {} content rect {region:?} bg={bg:?} \
             painted={painted}/{total} grid_nonspace_chars={grid_chars}",
            pane.raw()
        );
        assert!(
            painted >= MIN_PAINTED_PX_PER_PANE,
            "QA-SNAPSHOT[{scene}]: pane {} rendered only {painted} painted pixels \
             in its {total}-pixel body (background {bg:?}, floor \
             {MIN_PAINTED_PX_PER_PANE}) — this pane is EMPTY in the PNG a human is \
             asked to eyeball. The whole-frame 'not a uniform colour' check cannot \
             see this: the titlebar and status bar satisfy it on their own. \
             Its grid holds {grid_chars} non-whitespace characters when read just \
             after the capture, so {} First 200 chars of the grid: {:?}",
            pane.raw(),
            if grid_chars > 0 {
                "the content either arrived too late for the captured frame (widen \
                 the wait) or the render dropped it (a paint defect)."
            } else {
                "the shell had produced nothing at all to draw."
            },
            grid.chars().take(200).collect::<String>()
        );
    }
}

/// Send a Ctrl+Shift+<key> chord (the default keybinding modifier on this host).
pub fn chord(h: &mut Harness<'_, egui_app::C0pl4ndApp>, key: egui::Key) {
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
