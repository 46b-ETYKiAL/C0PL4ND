//! Headless tests for the terminal scrollbar's SEMANTIC MARKS, driven by
//! `egui_kittest` over the REAL production frame loop ([`C0pl4ndApp::frame_tick`]).
//!
//! ## The defect these guard
//!
//! The scrollbar painted marks for find-overlay hits and nothing else, even
//! though core has always captured OSC 133 shell-prompt marks (`;A`/`;B` — the
//! set the Ctrl+Shift+Up/Down jump-to-prompt chord walks) and command-end marks
//! with exit codes (`;D` — the set the status bar's exit-code indicator reads).
//! A long scrollback gave the user no map of where commands began or which of
//! them failed.
//!
//! ## Discipline (non-negotiable)
//!
//! These tests assert the **painted frame output** — the `egui::Shape`s the app
//! actually emitted this frame, read back from `Harness::output()` — not an
//! intermediate "the marks vector was populated" value. That distinction is the
//! whole point: the mark-building call site in `render_pane_body` was previously
//! untested even though the `scrollbar` geometry helpers were well covered, so a
//! test that only exercised a helper would have proved nothing about whether the
//! marks ever reach the bar.
//!
//! The scrollbar column is located from the frame output itself (the tall thin
//! track rect on the pane's right edge), never from hard-coded layout constants,
//! so the tests do not silently pass by re-deriving the production geometry.
//!
//! Every assertion has a matching NEGATIVE control: the same measurement is taken
//! BEFORE any OSC 133 output, and must be empty. Without that, a test counting
//! "thin rects near the right edge" could be satisfied by unrelated chrome.

use c0pl4nd::egui_app;
use std::cell::RefCell;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

use egui_app::C0pl4ndApp;

/// Scrollback lines fed before any mark, so the bar is scrollable (it auto-hides
/// when everything fits) and the marks land at spread-out track positions.
const FILLER_LINES: usize = 400;

/// Build a headless harness driving the REAL `frame_tick` for a shared app.
fn harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
    #[allow(deprecated)]
    let mut h = Harness::new(move |ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));
    h.run();
    h
}

/// Wait out the focused pane's deferred first-frame PTY spawn AND the spawned
/// shell's own startup output, so a subsequent `test_feed_focused` is neither
/// silently dropped nor wiped out from under the test. A timeout panics rather
/// than returning a quietly-unseeded app (a skip here would be a fake green).
fn ensure_focused_spawned(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>) {
    for _ in 0..120 {
        if app.borrow().test_focused_alive() {
            break;
        }
        h.run();
    }
    assert!(
        app.borrow().test_focused_alive(),
        "the focused pane never spawned its emulator"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        h.step();
        if app
            .borrow()
            .test_focused_buffer_text()
            .is_some_and(|t| !t.trim().is_empty())
        {
            for _ in 0..20 {
                h.step();
            }
            return;
        }
    }
    panic!(
        "the spawned shell never produced startup output — the pane is alive but \
         silent, so fed marks could still be wiped by a late banner"
    );
}

/// A filled rect the app painted this frame.
#[derive(Clone, Copy, Debug)]
struct PaintedRect {
    rect: egui::Rect,
    fill: egui::Color32,
}

/// Flatten every filled rect out of this frame's shape list (recursing into
/// `Shape::Vec`, which is how egui nests batched shapes).
fn painted_rects(h: &Harness<'_>) -> Vec<PaintedRect> {
    fn walk(shape: &egui::Shape, out: &mut Vec<PaintedRect>) {
        match shape {
            egui::Shape::Rect(r) => out.push(PaintedRect {
                rect: r.rect,
                fill: r.fill,
            }),
            egui::Shape::Vec(v) => {
                for s in v {
                    walk(s, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &h.output().shapes {
        walk(&clipped.shape, &mut out);
    }
    out
}

/// The scrollbar TRACK rect for `pane`, found in the painted output rather than
/// re-derived from the production layout constants: it is the tall, narrow rect
/// hugging the pane's right edge. `None` when the bar is not painted at all
/// (nothing to scroll).
fn find_track(rects: &[PaintedRect], pane: egui::Rect) -> Option<egui::Rect> {
    rects
        .iter()
        .map(|p| p.rect)
        .filter(|r| {
            r.width() > 2.0
                && r.width() < 24.0
                && r.height() > pane.height() * 0.5
                && r.right() <= pane.right() + 0.5
                && r.right() >= pane.right() - 24.0
                && r.top() >= pane.top() - 0.5
        })
        // The track is the TALLEST such rect (the thumb shares the column but is
        // never taller than the track).
        .max_by(|a, b| a.height().total_cmp(&b.height()))
}

/// Every MARK TICK painted on `track`: a rect inside the bar's column that is
/// only a few points tall (the thumb and the track itself are far taller).
fn mark_ticks(rects: &[PaintedRect], track: egui::Rect) -> Vec<PaintedRect> {
    let mut ticks: Vec<PaintedRect> = rects
        .iter()
        .copied()
        .filter(|p| {
            p.rect.height() <= 4.5
                && p.rect.width() > 0.5
                && p.rect.left() >= track.left() - 0.5
                && p.rect.right() <= track.right() + 0.5
                && p.rect.top() >= track.top() - 2.0
                && p.rect.bottom() <= track.bottom() + 2.0
        })
        .collect();
    ticks.sort_by(|a, b| a.rect.top().total_cmp(&b.rect.top()));
    ticks
}

/// Locate the focused pane's bar and read back its ticks for the current frame.
fn ticks_now(h: &Harness<'_>, app: &RefCell<C0pl4ndApp>) -> (egui::Rect, Vec<PaintedRect>) {
    let pane = {
        let a = app.borrow();
        a.pane_body_rect(a.focused_pane())
            .expect("the focused pane has been laid out")
    };
    let rects = painted_rects(h);
    let track = find_track(&rects, pane).expect(
        "the scrollbar track must be painted — it auto-hides only when there is \
         no scrollback, and these tests fill the scrollback first",
    );
    let ticks = mark_ticks(&rects, track);
    (track, ticks)
}

/// Feed filler scrollback so the bar is scrollable, with NO OSC 133 anywhere.
fn feed_filler(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>) {
    {
        let mut a = app.borrow_mut();
        let mut buf = String::new();
        for i in 0..FILLER_LINES {
            buf.push_str(&format!("FILLER_{i:04}\r\n"));
        }
        a.test_feed_focused(buf.as_bytes());
    }
    h.run();
}

/// Emit `count` OSC 133 prompt-start marks, each on its own line and spaced
/// `gap` lines apart so they land at clearly separated track positions.
fn feed_prompt_marks(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>, count: usize, gap: usize) {
    {
        let mut a = app.borrow_mut();
        let mut buf = String::new();
        for i in 0..count {
            // `OSC 133 ; A BEL` — prompt start. Core records it at the line the
            // cursor is on, then the printed text + CRLF advances past it.
            buf.push_str(&format!("\x1b]133;A\x07PROMPT_{i:03}\r\n"));
            for j in 0..gap {
                buf.push_str(&format!("out_{i:03}_{j:03}\r\n"));
            }
        }
        a.test_feed_focused(buf.as_bytes());
    }
    h.run();
}

/// Emit `count` OSC 133 command-end marks with a NON-ZERO exit code (failures),
/// spaced `gap` lines apart.
fn feed_failed_commands(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>, count: usize, gap: usize) {
    {
        let mut a = app.borrow_mut();
        let mut buf = String::new();
        for i in 0..count {
            buf.push_str(&format!("\x1b]133;D;1\x07FAILED_{i:03}\r\n"));
            for j in 0..gap {
                buf.push_str(&format!("after_{i:03}_{j:03}\r\n"));
            }
        }
        a.test_feed_focused(buf.as_bytes());
    }
    h.run();
}

/// OSC 133 PROMPT marks must reach the painted scrollbar.
///
/// The negative control (no marks fed → no ticks painted) is what makes the
/// positive half meaningful: it proves the tick measurement is not picking up
/// unrelated chrome that would be present either way.
#[test]
fn osc133_prompt_marks_are_painted_on_the_scrollbar() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_filler(&mut h, &app);

    // NEGATIVE CONTROL: a scrollable bar with no OSC 133 output and no open find
    // overlay paints NO marks at all.
    let (track, before) = ticks_now(&h, &app);
    assert!(
        track.height() > 0.0,
        "precondition: the bar is painted so ticks are locatable"
    );
    assert!(
        before.is_empty(),
        "with no OSC 133 output and no search, the bar must paint no marks — got \
         {before:?}; the tick measurement is picking up unrelated chrome and every \
         positive assertion below would be vacuous"
    );

    const PROMPTS: usize = 6;
    feed_prompt_marks(&mut h, &app, PROMPTS, 25);

    let (track, after) = ticks_now(&h, &app);
    assert_eq!(
        after.len(),
        PROMPTS,
        "each of the {PROMPTS} shell-prompt marks must paint exactly one tick on \
         the bar — got {}. Zero means the prompt marks never reach the scrollbar \
         (the defect); a different count means they are mis-counted.",
        after.len()
    );

    // Positions must be ORDERED and spread: prompt N is further down the history
    // than prompt N-1, so its tick sits lower on the track. A build that mapped
    // every mark to one constant y would pass a bare count assertion but fail here.
    let ys: Vec<f32> = after.iter().map(|p| p.rect.center().y).collect();
    for w in ys.windows(2) {
        assert!(
            w[1] > w[0],
            "prompt ticks must descend the track in emission order, got {ys:?}"
        );
    }
    assert!(
        ys.last().unwrap() - ys.first().unwrap() > track.height() * 0.05,
        "the prompt ticks must be SPREAD over the track (first {:.1}, last {:.1}, \
         track {:.1}pt) — all marks collapsing onto one position would mean the \
         absolute-line → track-y mapping is not being applied",
        ys.first().unwrap(),
        ys.last().unwrap(),
        track.height()
    );
    for p in &after {
        assert!(
            p.rect.top() >= track.top() - 2.0 && p.rect.bottom() <= track.bottom() + 2.0,
            "a tick escaped the track: {:?} vs {track:?}",
            p.rect
        );
    }
}

/// A command that FAILED (OSC 133 `;D` with a non-zero exit code) must paint its
/// own mark, visually distinct from a prompt mark: a different colour AND a
/// different, non-overlapping slice of the bar's width — so the kinds are
/// distinguishable rather than merged into one undifferentiated set.
#[test]
fn failed_commands_paint_marks_distinguishable_from_prompt_marks() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_filler(&mut h, &app);

    let (_, before) = ticks_now(&h, &app);
    assert!(
        before.is_empty(),
        "negative control: no marks before any OSC 133"
    );

    const PROMPTS: usize = 3;
    const FAILURES: usize = 4;
    feed_prompt_marks(&mut h, &app, PROMPTS, 20);
    feed_failed_commands(&mut h, &app, FAILURES, 20);

    let (track, ticks) = ticks_now(&h, &app);
    assert_eq!(
        ticks.len(),
        PROMPTS + FAILURES,
        "both kinds must be painted ({PROMPTS} prompts + {FAILURES} failures)"
    );

    // Split by which half of the bar each tick occupies — the geometry that makes
    // the kinds tell-apart-able without relying on colour vision.
    let mid = track.center().x;
    let left: Vec<PaintedRect> = ticks
        .iter()
        .copied()
        .filter(|p| p.rect.right() <= mid + 0.5)
        .collect();
    let right: Vec<PaintedRect> = ticks
        .iter()
        .copied()
        .filter(|p| p.rect.left() >= mid - 0.5)
        .collect();
    assert_eq!(
        left.len(),
        PROMPTS,
        "the {PROMPTS} prompt ticks must occupy the LEFT half of the bar"
    );
    assert_eq!(
        right.len(),
        FAILURES,
        "the {FAILURES} failure ticks must occupy the RIGHT half of the bar"
    );
    assert_eq!(
        left.len() + right.len(),
        ticks.len(),
        "no tick may straddle the midline — the halves are what separates the kinds"
    );

    // ...and the two kinds are painted in different colours.
    let prompt_fill = left[0].fill;
    let failure_fill = right[0].fill;
    assert!(
        left.iter().all(|p| p.fill == prompt_fill),
        "all prompt ticks share one colour"
    );
    assert!(
        right.iter().all(|p| p.fill == failure_fill),
        "all failure ticks share one colour"
    );
    assert_ne!(
        prompt_fill, failure_fill,
        "a prompt tick and a failed-command tick must not be painted in the same \
         colour — merging them is exactly what this feature exists to avoid"
    );

    // A failure is drawn heavier than a prompt, so it is the louder signal.
    assert!(
        right[0].rect.height() > left[0].rect.height(),
        "a failed-command tick ({:.1}pt) must be heavier than a prompt tick ({:.1}pt)",
        right[0].rect.height(),
        left[0].rect.height()
    );
}

/// A command that SUCCEEDED (`;D;0`) or reported no code at all must NOT paint a
/// failure mark. Without this, "paint a tick for every `;D` mark" would satisfy
/// the failure test above while turning the bar into noise on a healthy session.
#[test]
fn successful_and_codeless_commands_paint_no_failure_mark() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_filler(&mut h, &app);

    {
        let mut a = app.borrow_mut();
        let mut buf = String::new();
        for i in 0..5 {
            // `;C` output-start (no code), `;D;0` success, and a bare `;D` with no
            // code at all — none of these is a failure.
            buf.push_str(&format!("\x1b]133;C\x07running_{i}\r\n"));
            buf.push_str(&format!("\x1b]133;D;0\x07ok_{i}\r\n"));
            buf.push_str(&format!("\x1b]133;D\x07done_{i}\r\n"));
            for j in 0..10 {
                buf.push_str(&format!("pad_{i}_{j}\r\n"));
            }
        }
        a.test_feed_focused(buf.as_bytes());
    }
    h.run();

    let (_, ticks) = ticks_now(&h, &app);
    assert!(
        ticks.is_empty(),
        "successful / code-less command-end marks and output-start marks must \
         paint NOTHING on the bar — got {} tick(s). Marking every `;D` would fill \
         the scrollbar of a perfectly healthy session with failure ticks.",
        ticks.len()
    );

    // ...and the very same feed WITH a non-zero code does paint one, proving the
    // emptiness above is a real discrimination and not a dead code path.
    feed_failed_commands(&mut h, &app, 1, 5);
    let (_, after) = ticks_now(&h, &app);
    assert_eq!(
        after.len(),
        1,
        "one non-zero `;D` after the same feed must paint exactly one failure tick"
    );
}
