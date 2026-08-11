//! Headless tests for SHIFT+WHEEL scrolling, driven by `egui_kittest` over the
//! REAL production frame loop ([`C0pl4ndApp::frame_tick`]).
//!
//! ## The defect these guard
//!
//! egui folds a wheel notch into a SINGLE axis before the app ever sees it: with
//! `InputOptions::horizontal_scroll_modifier` held — **Shift**, by default — it
//! rewrites the delta as `vec2(x + y, 0.0)` and leaves `smooth_scroll_delta.y` at
//! ZERO. The pane read only `.y`, so **Shift+wheel did nothing at all**.
//!
//! That silently broke the shell's own documented escape hatch. `render_pane_body`
//! forces LOCAL handling (rather than reporting the wheel to the program) when
//! Shift is held, precisely so a user can reach the scrollback while vim / tmux /
//! htop has grabbed the mouse — and then the local branch dropped the notch on the
//! floor. Shift+wheel was the ONLY route into the scrollback in that state, and it
//! was inert.
//!
//! ## Discipline (non-negotiable)
//!
//! Every test asserts the **observable outcome** — the focused pane's scroll
//! offset actually moved, measured through the same accessors a user's eyes would
//! check — never "a pending value was set". Each also carries a control that
//! fails a plausible over-fix:
//!
//! - a Shift notch must move the view **as far as** a plain notch (a build that
//!   consumed the axis but halved/doubled the magnitude fails);
//! - a horizontal delta **without** the modifier must move nothing (a build that
//!   blindly added `.x` to `.y` fails);
//! - Shift+wheel must still work **while a program has grabbed the mouse** (the
//!   actual user-facing bug), and a plain wheel in that state must NOT scroll
//!   locally (proving the mouse grab is real and the test is not just scrolling a
//!   pane that ignores mouse mode).

use c0pl4nd::egui_app;
use std::cell::RefCell;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

use egui_app::C0pl4ndApp;

/// Scrollback lines fed before scrolling, so there is a long runway in both
/// directions and no assertion is silently clamped at a scrollback end.
const FILLER_LINES: usize = 600;

/// Frames stepped after each wheel event. egui SMOOTHS a discrete wheel notch
/// over several frames (`WheelState::after_events`), so a single-frame read would
/// see only the first slice of the notch.
const SETTLE_FRAMES: usize = 24;

/// A point well inside the pane body and far from the right-edge scrollbar, so
/// the wheel lands on the grid rather than on the bar.
const HOVER: egui::Pos2 = egui::pos2(200.0, 300.0);

fn harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
    #[allow(deprecated)]
    let mut h = Harness::new(move |ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));
    h.run();
    h
}

/// Wait out the deferred first-frame PTY spawn and the shell's startup banner, so
/// fed content is neither dropped nor overwritten. Fails LOUD rather than skipping.
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
    panic!("the spawned shell never produced startup output");
}

fn feed_filler(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>) {
    {
        let mut a = app.borrow_mut();
        let mut buf = String::new();
        for i in 0..FILLER_LINES {
            buf.push_str(&format!("WHEEL_LINE_{i:04}\r\n"));
        }
        a.test_feed_focused(buf.as_bytes());
    }
    h.run();
}

fn offset(app: &RefCell<C0pl4ndApp>) -> usize {
    app.borrow()
        .test_focused_view_offset()
        .expect("the focused pane has a live terminal")
}

/// Park the view roughly mid-history, so a scroll in EITHER direction is
/// observable instead of being clamped away at a scrollback end.
fn park_mid_history(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>) {
    // Ctrl+Shift+Home is the real scroll-to-top chord.
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
    let top = offset(app);
    assert!(
        top > 200,
        "precondition: the fed scrollback must be deep (offset at top {top})"
    );
    // Then wheel forward a chunk so we sit between the two ends.
    h.event(egui::Event::PointerMoved(HOVER));
    h.step();
    for _ in 0..12 {
        wheel(h, egui::vec2(0.0, -1.0), false);
    }
    let mid = offset(app);
    assert!(
        mid > 0 && mid < top,
        "precondition: the view must sit strictly BETWEEN the scrollback ends \
         (offset {mid} of a maximum {top}) so a scroll in either direction is \
         observable rather than clamped"
    );
}

/// Send one wheel event of `delta` LINES with (or without) Shift held, exactly as
/// the platform would, then let egui's scroll smoothing settle.
///
/// Shift is set on BOTH the event (which is what egui's `WheelState` folds the
/// axis on) and the harness's held-modifier state (which is what the app reads
/// via `ui.input(|i| i.modifiers)`) — a real user holding Shift produces both.
fn wheel(h: &mut Harness<'_>, delta: egui::Vec2, shift: bool) {
    let modifiers = egui::Modifiers {
        shift,
        ..Default::default()
    };
    h.input_mut().modifiers = modifiers;
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta,
        phase: egui::TouchPhase::Move,
        modifiers,
    });
    for _ in 0..SETTLE_FRAMES {
        h.step();
    }
    h.input_mut().modifiers = egui::Modifiers::default();
    h.step();
}

/// Enable mouse reporting in the focused pane (`?1000`), the state in which a
/// plain wheel is sent to the PROGRAM and Shift+wheel is the user's only way back
/// into the scrollback.
fn grab_the_mouse(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>) {
    app.borrow_mut().test_feed_focused(b"\x1b[?1000h");
    h.run();
}

/// A Shift-held notch must scroll the scrollback — and by the SAME amount as a
/// plain notch. Reading only `.y` (the defect) makes the shifted half zero.
#[test]
fn shift_wheel_scrolls_the_scrollback_as_far_as_a_plain_wheel() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_filler(&mut h, &app);
    park_mid_history(&mut h, &app);

    // A PLAIN notch back into history — the reference distance.
    let before_plain = offset(&app);
    wheel(&mut h, egui::vec2(0.0, 1.0), false);
    let plain_moved = offset(&app).saturating_sub(before_plain);
    assert!(
        plain_moved > 0,
        "precondition: a plain wheel notch must scroll back into history \
         (offset stayed at {before_plain})"
    );

    // The SAME notch with Shift held.
    let before_shift = offset(&app);
    wheel(&mut h, egui::vec2(0.0, 1.0), true);
    let shift_moved = offset(&app).saturating_sub(before_shift);

    assert!(
        shift_moved > 0,
        "Shift+wheel must scroll the scrollback, but the view did not move at all \
         (offset stayed at {before_shift}). egui folds a Shift-held notch onto the \
         x-axis and zeroes y, so a pane that reads only `smooth_scroll_delta.y` \
         sees nothing — that is the dead Shift+wheel this test exists to reject."
    );
    assert_eq!(
        shift_moved, plain_moved,
        "a Shift-held notch must move the view exactly as far as a plain notch \
         (plain {plain_moved} rows, shifted {shift_moved} rows)"
    );
}

/// Shift+wheel must scroll DOWN as well as up — a one-direction fix is a half fix.
#[test]
fn shift_wheel_scrolls_both_directions() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_filler(&mut h, &app);
    park_mid_history(&mut h, &app);

    let start = offset(&app);
    wheel(&mut h, egui::vec2(0.0, 1.0), true);
    let up = offset(&app);
    assert!(
        up > start,
        "Shift+wheel UP must go BACK into history ({start} -> {up})"
    );

    wheel(&mut h, egui::vec2(0.0, -1.0), true);
    let down = offset(&app);
    assert!(
        down < up,
        "Shift+wheel DOWN must go FORWARD toward the live output ({up} -> {down})"
    );
}

/// THE ACTUAL USER-FACING BUG: while a program has grabbed the mouse, a plain
/// wheel is reported to the program (no local scroll) and Shift+wheel is the
/// documented escape into the scrollback. Both halves are asserted, so a build
/// that simply ignored mouse mode would fail the first.
#[test]
fn shift_wheel_reaches_the_scrollback_while_a_program_has_grabbed_the_mouse() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_filler(&mut h, &app);
    park_mid_history(&mut h, &app);
    grab_the_mouse(&mut h, &app);

    // A PLAIN wheel now belongs to the program: the local view must NOT move.
    let before_plain = offset(&app);
    wheel(&mut h, egui::vec2(0.0, 1.0), false);
    assert_eq!(
        offset(&app),
        before_plain,
        "with the mouse grabbed (?1000) a plain wheel is reported to the program, \
         not scrolled locally — if this moves, the mouse grab is not in effect and \
         the Shift assertion below proves nothing"
    );

    // Shift forces LOCAL handling — this is the only way back into the scrollback.
    let before_shift = offset(&app);
    wheel(&mut h, egui::vec2(0.0, 1.0), true);
    assert!(
        offset(&app) > before_shift,
        "Shift+wheel must reach the local scrollback even while a program has \
         grabbed the mouse (offset stayed at {before_shift}) — with the program \
         owning the pointer this is the user's ONLY route into their history"
    );
}

/// A genuine SIDEWAYS gesture (tilt wheel / two-finger horizontal trackpad swipe)
/// with no modifier must scroll nothing: the grid is exactly as wide as its pane,
/// so there is no horizontal viewport, and repurposing the gesture into vertical
/// motion would be a surprise. This is the control that rejects a naive
/// "just add `.x` to `.y`" fix.
#[test]
fn an_unmodified_horizontal_wheel_does_not_scroll_the_scrollback() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    ensure_focused_spawned(&mut h, &app);
    feed_filler(&mut h, &app);
    park_mid_history(&mut h, &app);

    let before = offset(&app);
    for _ in 0..4 {
        wheel(&mut h, egui::vec2(3.0, 0.0), false);
    }
    assert_eq!(
        offset(&app),
        before,
        "an unmodified horizontal wheel delta must leave the scrollback alone"
    );

    // ...and the very same magnitude WITH the modifier does scroll, proving the
    // pane is not simply deaf to wheel input in this state.
    wheel(&mut h, egui::vec2(0.0, 1.0), true);
    assert!(
        offset(&app) != before,
        "the modifier-held notch after it must still scroll — otherwise the \
         assertion above passes for the wrong reason"
    );
}
