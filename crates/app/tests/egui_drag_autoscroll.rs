//! Headless **interaction** tests for DRAG-SELECT AUTOSCROLL, driven by
//! `egui_kittest` over the REAL production frame loop ([`C0pl4ndApp::frame_tick`]).
//!
//! ## The defect these guard
//!
//! Selecting text and dragging the pointer past the top or bottom edge of a pane
//! did nothing at all: the view did not scroll and the selection head froze at
//! the last cell that happened to be inside the grid, so a user could never
//! select more than one screenful. The whole feature was absent (`grep -rni
//! autoscroll crates/` returned nothing).
//!
//! ## Discipline (non-negotiable)
//!
//! Each test asserts **BOTH halves** of the fix, because either alone is a fake
//! green:
//!
//! - **(a) the view scrolled** — the focused pane's `view_offset` moved in the
//!   direction the drag demanded, and
//! - **(b) the selection GREW to cover the newly revealed content** — the head
//!   landed on an absolute scrollback line that was NOT inside the visible window
//!   when the drag began, the selection now spans MORE than one screenful (which
//!   is impossible without autoscroll), and the grid row the head names really
//!   does hold a marker line that was NOT on screen at press time.
//!
//! A test that asserted only (a) — "a pending-scroll value was set" — would pass
//! against a build that scrolls the view while leaving the selection frozen,
//! which is exactly the half-fix this file exists to reject.
//!
//! Scrollback is built with [`C0pl4ndApp::test_feed_focused`] (straight into the
//! emulator, bypassing the PTY) so the content is deterministic; the pane itself
//! is a real spawned terminal, and [`ensure_focused_spawned`] waits out both the
//! deferred first-frame spawn and the shell's startup banner so a fed row is
//! neither dropped nor overwritten. A box with no usable PTY fails LOUD rather
//! than skipping silently.

use c0pl4nd::egui_app;
use std::cell::RefCell;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

use egui_app::C0pl4ndApp;

/// Number of marker lines fed into the pane — comfortably more than any plausible
/// screenful, so there is room to autoscroll well past one viewport in either
/// direction.
const FED_LINES: usize = 300;

/// Frames to hold the pointer outside the grid. The pointer is deliberately NOT
/// moved again during these frames: a real user parks the pointer past the edge
/// and expects the view to keep scrolling, which only works because the drag
/// branch requests a repaint each frame.
///
/// Sized to travel well past one screenful (the per-frame cap is a handful of
/// lines) while staying short of `FED_LINES`, so the view does NOT run all the
/// way to a scrollback end — a run that bottoms out would park the selection
/// head on the blank tail of the live screen and make the content assertion
/// vacuous.
const HOLD_FRAMES: usize = 16;

/// Build a headless harness driving the REAL `frame_tick` for a shared app.
fn harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
    #[allow(deprecated)]
    let mut h = Harness::new(move |ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));
    h.run();
    h
}

/// Press or release the primary pointer button at `pos`.
fn pointer_primary(h: &mut Harness<'_>, pos: egui::Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    });
}

/// Wait out the focused pane's deferred first-frame PTY spawn AND the spawned
/// shell's own startup output, so a subsequent `test_feed_focused` is neither
/// silently dropped nor wiped out from under the test. Both waits are
/// load-bearing; a timeout panics rather than returning a quietly-unseeded app.
fn ensure_focused_spawned(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>) {
    for _ in 0..120 {
        if app.borrow().test_focused_alive() {
            break;
        }
        h.run();
    }
    if !app.borrow().test_focused_alive() {
        panic!("the focused pane never spawned its emulator");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        h.step();
        let landed = app
            .borrow()
            .test_focused_buffer_text()
            .is_some_and(|t| !t.trim().is_empty());
        if landed {
            for _ in 0..20 {
                h.step();
            }
            return;
        }
    }
    panic!(
        "the spawned shell never produced startup output — the pane is alive but \
         silent, so a fed row could still be wiped by a late banner"
    );
}

/// A snapshot of everything an autoscroll assertion needs about the focused pane.
///
/// `buffer` is the WHOLE buffer (scrollback history then the live screen), so its
/// line index IS the absolute scrollback line a [`Selection`] endpoint is
/// expressed in. That is the surface the content assertions read; the *visible
/// grid* text is not view-aware (it always renders the live screen) and so cannot
/// answer "what content is under the head after scrolling".
struct View {
    /// Visible row count of the pane grid.
    rows: usize,
    /// Scroll-up offset (0 = pinned to the live bottom).
    offset: usize,
    /// Absolute scrollback line currently at the TOP of the visible window.
    window_start: usize,
    /// Whole-buffer text, one line per absolute scrollback line.
    buffer: String,
}

impl View {
    /// The content of absolute scrollback line `abs`, if the buffer has one.
    fn line(&self, abs: usize) -> Option<&str> {
        self.buffer.lines().nth(abs)
    }

    /// Every buffer line currently inside the visible window.
    fn visible_lines(&self) -> Vec<String> {
        (self.window_start..self.window_start + self.rows)
            .filter_map(|abs| self.line(abs).map(str::to_string))
            .collect()
    }
}

fn view(app: &RefCell<C0pl4ndApp>) -> View {
    let a = app.borrow();
    let focused = a.focused_pane();
    let rows = a
        .pane_size(focused)
        .expect("the focused pane has a live terminal")
        .1 as usize;
    let offset = a
        .test_focused_view_offset()
        .expect("the focused pane has a live terminal");
    let scrollback = a
        .test_focused_scrollback_len()
        .expect("the focused pane has a live terminal");
    View {
        rows,
        offset,
        window_start: scrollback.saturating_sub(offset),
        buffer: a
            .test_focused_buffer_text()
            .expect("the focused pane has a live terminal"),
    }
}

/// Feed `FED_LINES` uniquely-numbered marker lines into the focused emulator, so
/// every scrollback line is individually identifiable in the grid text.
fn feed_markers(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>) {
    {
        let mut a = app.borrow_mut();
        let mut buf = String::new();
        for i in 0..FED_LINES {
            buf.push_str(&format!("AS_LINE_{i:04}\r\n"));
        }
        a.test_feed_focused(buf.as_bytes());
    }
    h.run();
}

/// Assert that the selection head landed on a fed marker line that was NOT
/// inside the visible window when the drag began — the "the selection grew to
/// cover the newly revealed content" half of the fix, checked against real
/// terminal content rather than a coordinate alone.
fn assert_head_is_on_newly_revealed_content(before: &View, after: &View, head_line: usize) {
    let landed = after.line(head_line).unwrap_or_else(|| {
        panic!(
            "the buffer must have a line at the head's absolute line {head_line} \
             (buffer has {} lines) — a head on the blank tail would make this \
             assertion vacuous",
            after.buffer.lines().count()
        )
    });
    assert!(
        landed.contains("AS_LINE_"),
        "the line under the selection head must be one of the fed marker lines, got {landed:?}"
    );
    let was_visible = before.visible_lines();
    assert!(
        !was_visible.iter().any(|l| l == landed),
        "{landed:?} (the line under the selection head) was ALREADY on screen \
         before the drag — the selection never reached newly revealed content, \
         which is the half-fix this test exists to reject. Window at press: \
         [{}, {})",
        before.window_start,
        before.window_start + before.rows
    );
}

/// Dragging BELOW the bottom edge must scroll the view FORWARD (toward the live
/// bottom) and keep extending the selection over the lines that scroll in.
#[test]
fn drag_past_the_bottom_edge_autoscrolls_forward_and_extends_the_selection() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_markers(&mut h, &app);

    // Park the view at the OLDEST retained line so there is a long way to scroll
    // FORWARD. Ctrl+Shift+Home is the real scroll-to-top chord.
    h.event(egui::Event::Key {
        key: egui::Key::Home,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        },
    });
    h.run();

    let before = view(&app);
    assert!(
        before.offset > before.rows,
        "precondition: the view must sit more than one screenful back in history \
         (offset {}, rows {}) or there is nothing to autoscroll into",
        before.offset,
        before.rows
    );

    // Press inside the grid, then drag FAR below the pane and hold still.
    let press = egui::pos2(120.0, 200.0);
    let past_bottom = egui::pos2(120.0, 5000.0);
    h.event(egui::Event::PointerMoved(press));
    pointer_primary(&mut h, press, true);
    h.step();
    h.event(egui::Event::PointerMoved(past_bottom));
    for _ in 0..HOLD_FRAMES {
        h.step();
    }

    let after = view(&app);
    let (anchor, head, _) = app
        .borrow()
        .test_selection()
        .expect("the drag must have produced a selection");

    // (a) THE VIEW SCROLLED — forward, toward the live bottom.
    assert!(
        after.offset < before.offset,
        "dragging past the BOTTOM edge must scroll the view forward: offset went \
         {} -> {} (no movement means autoscroll never fired)",
        before.offset,
        after.offset
    );

    // (b) THE SELECTION GREW ONTO THE NEWLY REVEALED CONTENT.
    let last_visible_at_press = before.window_start + before.rows - 1;
    assert!(
        head.0 > last_visible_at_press,
        "the selection head must extend PAST the last line that was on screen when \
         the drag began (head line {}, last visible at press {}) — a head that \
         stops at the old viewport edge means the view scrolled but the selection \
         did not follow, which is the half-fix this test rejects",
        head.0,
        last_visible_at_press
    );
    assert!(
        head.0.saturating_sub(anchor.0) + 1 > before.rows,
        "the selection must now span MORE than one screenful ({} lines over a \
         {}-row viewport) — that is only reachable by autoscrolling",
        head.0.saturating_sub(anchor.0) + 1,
        before.rows
    );

    // The head must stay INSIDE the scrolled viewport (it tracks the pointer,
    // clamped to the bottom edge) — never a row that does not exist.
    let head_row = head
        .0
        .checked_sub(after.window_start)
        .expect("the head must land inside the scrolled window");
    assert!(
        head_row < after.rows,
        "the head row {head_row} must be inside the {}-row viewport",
        after.rows
    );
    // ...and the content it names must be a line that was OFF SCREEN at press.
    assert!(
        after.offset > 0,
        "the drag must not have run all the way to the live bottom, or the head \
         would sit on the screen's blank tail and prove nothing"
    );
    assert_head_is_on_newly_revealed_content(&before, &after, head.0);

    pointer_primary(&mut h, past_bottom, false);
    h.step();
}

/// Dragging ABOVE the top edge must scroll the view BACK into history and keep
/// extending the selection over the older lines that scroll in.
#[test]
fn drag_past_the_top_edge_autoscrolls_back_and_extends_the_selection() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_markers(&mut h, &app);

    // The view is pinned to the live bottom after output, so there is a long way
    // to scroll BACK.
    let before = view(&app);
    assert_eq!(
        before.offset, 0,
        "precondition: fresh output pins the view to the live bottom"
    );
    assert!(
        before.window_start > before.rows,
        "precondition: there must be more than one screenful of history above the \
         window (window_start {}, rows {})",
        before.window_start,
        before.rows
    );

    // Press inside the grid, then drag FAR above the pane and hold still.
    let press = egui::pos2(120.0, 200.0);
    let past_top = egui::pos2(120.0, -5000.0);
    h.event(egui::Event::PointerMoved(press));
    pointer_primary(&mut h, press, true);
    h.step();
    h.event(egui::Event::PointerMoved(past_top));
    for _ in 0..HOLD_FRAMES {
        h.step();
    }

    let after = view(&app);
    let (anchor, head, _) = app
        .borrow()
        .test_selection()
        .expect("the drag must have produced a selection");

    // (a) THE VIEW SCROLLED — back into history.
    assert!(
        after.offset > before.offset,
        "dragging past the TOP edge must scroll the view back into history: offset \
         went {} -> {} (no movement means autoscroll never fired)",
        before.offset,
        after.offset
    );

    // (b) THE SELECTION GREW ONTO THE NEWLY REVEALED (OLDER) CONTENT.
    assert!(
        head.0 < before.window_start,
        "the selection head must extend ABOVE the first line that was on screen \
         when the drag began (head line {}, window top at press {}) — a head that \
         stops at the old viewport edge means the view scrolled but the selection \
         did not follow",
        head.0,
        before.window_start
    );
    assert!(
        anchor.0.saturating_sub(head.0) + 1 > before.rows,
        "the selection must now span MORE than one screenful ({} lines over a \
         {}-row viewport) — that is only reachable by autoscrolling",
        anchor.0.saturating_sub(head.0) + 1,
        before.rows
    );

    let head_row = head
        .0
        .checked_sub(after.window_start)
        .expect("the head must land inside the scrolled window");
    assert!(
        head_row < after.rows,
        "the head row {head_row} must be inside the {}-row viewport",
        after.rows
    );
    assert_head_is_on_newly_revealed_content(&before, &after, head.0);

    pointer_primary(&mut h, past_top, false);
    h.step();
}

/// Frames to hold the pointer past an edge when the point of the test is to run
/// the view all the way INTO a scrollback bound. At the per-frame cap this
/// traverses `FED_LINES` several times over, so the view is unambiguously parked
/// against the end rather than merely still travelling when the test stops
/// looking.
const HOLD_FRAMES_TO_BOUND: usize = 200;

/// Autoscroll must RESPECT THE SCROLLBACK BOUNDS: held past the top edge long
/// enough to consume the entire history, the view parks at the oldest retained
/// line and STAYS there, and the selection head lands on that oldest line — a
/// real absolute line, never a wrapped or out-of-buffer one.
///
/// This is the sustained-run counterpart to the single-screenful edge tests: it
/// is the only test that drives autoscroll to a hard limit, so it is what proves
/// the loop terminates cleanly at the end of history instead of running away.
#[test]
fn drag_past_the_top_edge_stops_at_the_oldest_line_and_stays_there() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_markers(&mut h, &app);

    let before = view(&app);
    assert!(
        before.window_start > before.rows,
        "precondition: there must be more than one screenful of history to consume \
         (window_start {}, rows {})",
        before.window_start,
        before.rows
    );

    let press = egui::pos2(120.0, 200.0);
    let past_top = egui::pos2(120.0, -5000.0);
    h.event(egui::Event::PointerMoved(press));
    pointer_primary(&mut h, press, true);
    h.step();
    h.event(egui::Event::PointerMoved(past_top));
    for _ in 0..HOLD_FRAMES_TO_BOUND {
        h.step();
    }

    let at_bound = view(&app);
    let (anchor, head, _) = app
        .borrow()
        .test_selection()
        .expect("the drag must have produced a selection");

    // (a) THE VIEW SCROLLED, and stopped exactly AT the oldest retained line —
    // clamped by the scrollback bound rather than running past it.
    assert!(
        at_bound.offset > before.offset,
        "the view must have scrolled back into history (offset {} -> {})",
        before.offset,
        at_bound.offset
    );
    assert_eq!(
        at_bound.window_start, 0,
        "held past the top edge for {HOLD_FRAMES_TO_BOUND} frames, the view must \
         reach the OLDEST retained line (window_start 0), not stall part-way"
    );

    // (b) THE SELECTION FOLLOWED ALL THE WAY DOWN to that oldest line, and the
    // line it names is a real one in the buffer — not a wrapped or phantom index.
    assert_eq!(
        head.0, 0,
        "the selection head must reach the oldest line (0) once the view is parked \
         against the scrollback bound — a head that stops short means the selection \
         stopped following the view"
    );
    assert!(
        anchor.0 > before.rows,
        "the selection must span far MORE than one screenful ({} lines) — only \
         reachable by sustained autoscroll",
        anchor.0 + 1
    );
    assert!(
        at_bound.line(head.0).is_some(),
        "the head's absolute line {} must exist in the buffer",
        head.0
    );

    // (c) STABILITY AT THE BOUND: more frames of the same held pointer must not
    // move the view or the head. A clamp that silently kept "scrolling" would
    // drift here.
    for _ in 0..40 {
        h.step();
    }
    let after_bound = view(&app);
    let (_, head_after, _) = app
        .borrow()
        .test_selection()
        .expect("the selection survives the extra frames");
    assert_eq!(
        after_bound.window_start, at_bound.window_start,
        "the view must stay parked at the scrollback bound"
    );
    assert_eq!(
        head_after, head,
        "the selection head must stay put once the view is clamped"
    );

    pointer_primary(&mut h, past_top, false);
    h.step();
}

/// A drag that stays INSIDE the grid must NOT scroll — autoscroll is an
/// edge-overshoot behaviour, not a side effect of every drag. Without this, a
/// runaway rate function that scrolled on any drag would still satisfy the two
/// tests above.
#[test]
fn a_drag_that_stays_inside_the_grid_never_autoscrolls() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_markers(&mut h, &app);

    // Park the view MID-history first: at either scrollback end a spurious scroll
    // would be silently clamped away and this test could not see it. Jump to the
    // top, then wheel forward a little, and confirm we really are between the ends.
    h.event(egui::Event::Key {
        key: egui::Key::Home,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        },
    });
    h.run();
    let top = view(&app);
    h.event(egui::Event::PointerMoved(egui::pos2(120.0, 200.0)));
    h.step();
    for _ in 0..4 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -1.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::default(),
        });
        h.step();
    }

    let before = view(&app);
    assert!(
        before.offset > 0 && before.offset < top.offset,
        "precondition: the view must sit strictly BETWEEN the scrollback ends \
         (offset {} of a maximum {}) so a spurious scroll in EITHER direction is \
         observable rather than clamped",
        before.offset,
        top.offset
    );

    // Drag horizontally, well inside the pane body, for as many frames as the
    // edge tests hold.
    let press = egui::pos2(120.0, 200.0);
    let inside = egui::pos2(420.0, 260.0);
    h.event(egui::Event::PointerMoved(press));
    pointer_primary(&mut h, press, true);
    h.step();
    h.event(egui::Event::PointerMoved(inside));
    for _ in 0..HOLD_FRAMES {
        h.step();
    }

    let after = view(&app);
    assert_eq!(
        after.offset, before.offset,
        "a drag held INSIDE the grid must not scroll the view at all"
    );
    let (anchor, head, _) = app
        .borrow()
        .test_selection()
        .expect("the in-grid drag still selects");
    assert_ne!(anchor, head, "the in-grid drag still moves the head");

    pointer_primary(&mut h, inside, false);
    h.step();
}
