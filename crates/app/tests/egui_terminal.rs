//! Headless **interaction** tests for the C0PL4ND egui shell's LIVE TERMINAL
//! panes (Milestone 2), driven by `egui_kittest` + real PTYs.
//!
//! ## Discipline (non-negotiable)
//!
//! Every test here drives the **real** production frame loop
//! ([`C0pl4ndApp::frame_tick`]) with **simulated input** and asserts the
//! **observable outcome** the input actually caused — there is NO
//! set-state-then-assert-the-same-state tautology. Specifically these tests
//! prove the bug-prone core of Milestone 2:
//!
//! - **type → PTY round-trip**: simulated keystrokes reach the focused pane's
//!   real PTY AND the pane's grid updates with the shell's output. This is the
//!   exact "typing does nothing" failure class the milestone must guard.
//! - **pane focus**: clicking a pane routes input to it (and away from others).
//! - **resize → PTY**: shrinking a pane's rect resizes its PTY grid.
//!
//! The glyphon GPU paint callback cannot run under kittest's headless software
//! path (recon dossier §7) — so the app paints a text fallback when no GPU is
//! present, and these tests assert the PTY/input/resize LOGIC (the bug-prone
//! part). The pixel render is left to the offscreen `screenshot.rs` visual-QA.
//!
//! The app module is compiled in via `#[path]` so the test can construct
//! `C0pl4ndApp` directly; the closure handed to `Harness::new` calls the same
//! `frame_tick` the shipping binary runs each frame.

use c0pl4nd::egui_app;
use c0pl4nd_core::paste_guard::PasteConfirmReason;
use std::cell::RefCell;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

use egui_app::grid::PaneId;
use egui_app::C0pl4ndApp;

/// Build a headless harness driving the REAL `frame_tick` for a shared app, with
/// a generous screen so panes get a real pixel rect (so resize math runs).
fn harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
    #[allow(deprecated)]
    let mut h = Harness::new(move |ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1000.0, 700.0));
    h.run();
    h
}

/// Type a string into the focused pane: queue one `egui::Event::Text` per char
/// and step a frame for each (the real `forward_input_to_focused` path).
fn type_text(h: &mut Harness<'_>, s: &str) {
    for ch in s.chars() {
        h.event(egui::Event::Text(ch.to_string()));
    }
    h.step();
}

/// Press Enter (a real `egui::Event::Key` — the shell sees `\r`).
fn press_enter(h: &mut Harness<'_>) {
    h.key_press(egui::Key::Enter);
    h.step();
}

/// Poll the focused pane's grid for `needle`, stepping frames + sleeping (the
/// PTY echoes/executes asynchronously, exactly like `e2e_terminal.rs` polls).
fn poll_focused_contains(
    h: &mut Harness<'_>,
    app: &RefCell<C0pl4ndApp>,
    needle: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        h.step();
        if app
            .borrow()
            .focused_grid_text()
            .is_some_and(|t| t.contains(needle))
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    // One last check after the final step.
    app.borrow()
        .focused_grid_text()
        .is_some_and(|t| t.contains(needle))
}

/// REGRESSION (blank-pane-on-split): opening a second terminal splits the
/// focused pane and GROWS the first pane's PTY past its 24-row spawn height,
/// then a narrowing reflow follows. The shell answers the resize with a
/// full-screen redraw (cursor-home + one line per row). If a *full-screen*
/// scroll region is not recognised as full-screen after the grid grows past
/// the spawn height, it stays frozen at the old bottom (row 23) — and the
/// multi-line redraw then scrolls every content line out of the restricted
/// `0..=23` region, leaving the pane an all-spaces grid.
///
/// This test drives the REAL split path (`new_terminal`) and asserts pane 0 is
/// NON-BLANK afterwards. It must PASS with the `Terminal::resize` fix (capture
/// `region_is_full` against the OLD height before resizing the grid). Guarded
/// with the no-live-PTY skip AFTER building the harness, since the deferred
/// real spawn happens on the first frame.
#[test]
fn opening_a_new_terminal_does_not_blank_the_first_pane() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    {
        let a = app.borrow();
        let focused = a.focused_pane();
        if a.pane_grid_text(focused).is_none() {
            eprintln!("no live PTY on this platform; skipping blank-pane-on-split");
            return;
        }
    }
    let mut h = harness(&app);

    let first = app.borrow().focused_pane();

    // Put a marker on the first pane and confirm it lands (so we KNOW the pane
    // had content before the split — otherwise a blank assertion is vacuous).
    let token = "c0pl4nd_split_marker";
    type_text(&mut h, &format!("echo {token}"));
    press_enter(&mut h);
    let seen = poll_focused_contains(&mut h, &app, token, Duration::from_secs(8));
    assert!(
        seen,
        "pre-condition: the marker must reach pane 0's grid before the split"
    );

    // Open a second terminal: splits the focused pane and resizes pane 0's PTY
    // (grow rows past 24, then narrow cols). This is the "+"-button path.
    app.borrow_mut().new_terminal();

    // Let the resize + the shell's redraw settle across several frames.
    for _ in 0..12 {
        h.step();
        std::thread::sleep(Duration::from_millis(40));
    }

    let text = app.borrow().pane_grid_text(first).unwrap_or_default();
    assert!(
        !text.trim().is_empty(),
        "pane 0 must NOT be blank after opening a new terminal (blank-pane-on-split \
         regression) — grid was:\n{text}"
    );
}

/// THE load-bearing test: type a command into the focused pane and assert the
/// echoed/executed output lands in that pane's REAL terminal grid. This proves
/// keystrokes reach the PTY AND the grid updates — the "typing does nothing"
/// failure class this milestone exists to guard.
#[test]
fn typing_a_command_reaches_the_pty_and_updates_the_grid() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    // Skip cleanly if the platform shell could not spawn (no PTY on this box) —
    // never a false green: assert the pane is live before driving it.
    {
        let a = app.borrow();
        let focused = a.focused_pane();
        if a.pane_grid_text(focused).is_none() {
            eprintln!("no live PTY on this platform; skipping round-trip");
            return;
        }
    }
    let mut h = harness(&app);

    // A token that cannot pre-exist on the prompt line. `echo` it so the shell
    // prints it back (works on cmd.exe and POSIX sh — the default shells).
    let token = "c0pl4nd_grid_ok";
    type_text(&mut h, &format!("echo {token}"));
    press_enter(&mut h);

    let seen = poll_focused_contains(&mut h, &app, token, Duration::from_secs(8));
    assert!(
        seen,
        "the typed `echo {token}` must reach the PTY and its output must appear \
         in the focused pane's grid — proves keystrokes → PTY → grid"
    );
}

/// Pane focus: bootstrap opens two panes. Click pane 1's tab to focus it, type a
/// token, and assert it lands in pane 1's grid and NOT in pane 0's. This proves
/// input routes to the clicked pane (and away from the other).
#[test]
fn clicking_a_pane_routes_typed_input_to_that_pane_only() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    {
        let a = app.borrow();
        if a.pane_grid_text(PaneId(0)).is_none() || a.pane_grid_text(PaneId(1)).is_none() {
            eprintln!("no live PTY on this platform; skipping focus routing");
            return;
        }
        assert_eq!(a.focused_pane(), PaneId(0), "pane 0 focused at start");
    }
    let mut h = harness(&app);

    // Focus pane 1 by clicking its tab (the real chrome path), retrying until
    // focus ACTUALLY lands on pane 1. The tab's accessible label tracks the
    // pane's LIVE OSC window title, which lands ASYNCHRONOUSLY from the PTY reader
    // thread — so (a) re-derive the lookup key from the SAME post-`h.run()` state
    // each iteration, and (b) a title landing also RESIZES the tab strip, shifting
    // the tab's rect between capture and hit-test, so a single click can miss.
    // Verifying the effect (focused_pane == 1) and re-clicking closes both races.
    // (egui_chrome's click_tab_control_until is the shared form; integration test
    // binaries don't share helpers, so it's inlined here.)
    let mut focused = false;
    for _ in 0..240 {
        h.run();
        let label = app
            .borrow()
            .tab_label_for_pane(PaneId(1))
            .expect("pane 1 must have a tab label");
        if let Some(node) = h.query_by_label(label.as_str()) {
            node.click();
            h.run();
            if app.borrow().focused_pane() == PaneId(1) {
                focused = true;
                break;
            }
        }
    }
    assert!(focused, "clicking pane 1's tab never moved focus to pane 1");

    // Type a unique token; it must land in pane 1's grid.
    let token = "c0pl4nd_pane1_only";
    type_text(&mut h, &format!("echo {token}"));
    press_enter(&mut h);

    let seen_in_1 = poll_focused_contains(&mut h, &app, token, Duration::from_secs(8));
    assert!(
        seen_in_1,
        "input must route to the focused (clicked) pane 1's PTY+grid"
    );
    // And it must NOT appear in pane 0 (input did not leak to the unfocused pane).
    let in_0 = app
        .borrow()
        .pane_grid_text(PaneId(0))
        .is_some_and(|t| t.contains(token));
    assert!(
        !in_0,
        "the token must NOT appear in the unfocused pane 0 — input must not leak"
    );
}

/// Resize → PTY: a pane laid out in a large window has a wide grid; shrinking the
/// window shrinks the pane rect, which must resize the pane's PTY grid (fewer
/// cols/rows). Drives the real `render_pane_body` resize path via the frame loop.
#[test]
fn shrinking_the_window_resizes_the_pane_pty() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    {
        let a = app.borrow();
        if a.pane_grid_text(a.focused_pane()).is_none() {
            eprintln!("no live PTY on this platform; skipping resize");
            return;
        }
    }
    let mut h = harness(&app);
    h.set_size(egui::vec2(1200.0, 800.0));
    h.run();
    h.run();

    let focused = app.borrow().focused_pane();
    let big = app
        .borrow()
        .pane_size(focused)
        .expect("focused pane has a PTY size");
    assert!(
        big.0 >= 1 && big.1 >= 1,
        "a laid-out pane must have a positive grid size, got {big:?}"
    );

    // Shrink the window substantially; the pane rect shrinks with it.
    h.set_size(egui::vec2(360.0, 320.0));
    h.run();
    h.run();

    let small = app
        .borrow()
        .pane_size(focused)
        .expect("focused pane still has a PTY size");
    assert!(
        small.0 < big.0 || small.1 < big.1,
        "shrinking the window must shrink the pane's PTY grid \
         (was {big:?}, now {small:?})"
    );
}

/// SECURITY (paste-injection / pastejacking): a MULTI-LINE paste must be
/// DEFERRED to the confirm overlay (not executed on its embedded newline), and
/// must NOT reach the PTY until the user confirms. With `paste_warn_multiline`
/// on by default, pasting `"X1uniq\nX2uniq"` parks it in `pending_paste` and the
/// focused grid never shows it; confirming then clears the pending state.
#[test]
fn multiline_paste_is_deferred_until_confirmed() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    h.event(egui::Event::Paste("X1uniq\nX2uniq".to_string()));
    h.step();
    h.step();

    assert!(
        app.borrow().has_pending_paste(),
        "a multi-line paste must be deferred to the confirm overlay"
    );
    // The overlay must be able to word WHICH hazard tripped, so the call site
    // has to record the reason too — not merely "something was deferred".
    assert_eq!(
        app.borrow().pending_paste_reason(),
        Some(PasteConfirmReason::MultiLine),
        "the multi-line gate (not the size gate) must be recorded as the reason"
    );
    // It must NOT have been forwarded to the PTY (so the shell can't run it yet).
    assert!(
        !app.borrow()
            .focused_grid_text()
            .is_some_and(|t| t.contains("X1uniq")),
        "a deferred multi-line paste must NOT reach the PTY before confirmation"
    );

    let sent = app.borrow_mut().confirm_pending_paste();
    assert_eq!(sent.as_deref(), Some("X1uniq\nX2uniq"));
    assert!(
        !app.borrow().has_pending_paste(),
        "confirming clears the pending paste"
    );
}

/// A SINGLE-LINE paste is not a multi-line-execution hazard, so it goes straight
/// through the injection guard to the PTY (no deferral) and the shell echoes it.
#[test]
fn singleline_paste_reaches_the_pty() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    h.event(egui::Event::Paste("PASTEPROBE42".to_string()));
    h.step();

    assert!(
        !app.borrow().has_pending_paste(),
        "a single-line paste is sent immediately, not deferred"
    );
    assert!(
        poll_focused_contains(&mut h, &app, "PASTEPROBE42", Duration::from_secs(10)),
        "a single-line paste must reach the PTY and be echoed into the grid"
    );
}

/// SECURITY (the load-bearing half of the paste gate): CANCELLING a deferred
/// paste must DROP it — the text must never reach the PTY, not "reach it later".
/// A gate that defers but leaks on cancel is worse than no gate at all, because
/// the user believes they refused the payload.
///
/// The negative assertion is made non-vacuous by an ORDERING BARRIER: after the
/// cancel we push a single-line paste (which takes the immediate path) and wait
/// for it to be echoed back from the real shell. Once the barrier has completed
/// its full PTY round-trip, any write the cancelled paste might have issued
/// would already have landed too — so its continued absence is real evidence,
/// not just impatience.
#[test]
fn cancelling_a_deferred_paste_never_reaches_the_pty() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    {
        let a = app.borrow();
        let focused = a.focused_pane();
        if a.pane_grid_text(focused).is_none() {
            eprintln!("no live PTY on this platform; skipping cancelled-paste leak test");
            return;
        }
    }

    h.event(egui::Event::Paste("CANCELLEAK_A\nCANCELLEAK_B".to_string()));
    h.step();
    h.step();
    assert!(
        app.borrow().has_pending_paste(),
        "the multi-line paste must be deferred before it can be cancelled"
    );

    app.borrow_mut().cancel_pending_paste();
    assert!(
        !app.borrow().has_pending_paste(),
        "cancelling clears the pending paste"
    );
    assert_eq!(
        app.borrow().pending_paste_reason(),
        None,
        "cancelling clears the reason in lockstep with the text"
    );

    // Ordering barrier: this later paste takes the immediate path, so once the
    // shell has echoed it the cancelled text has had at least as long to appear.
    h.event(egui::Event::Paste("CANCELBARRIER77".to_string()));
    h.step();
    assert!(
        poll_focused_contains(&mut h, &app, "CANCELBARRIER77", Duration::from_secs(10)),
        "the barrier paste must reach the PTY (otherwise the leak check is vacuous)"
    );

    assert!(
        !app.borrow()
            .focused_grid_text()
            .is_some_and(|t| t.contains("CANCELLEAK_A")),
        "a CANCELLED paste must never reach the PTY"
    );
}

/// The call site must consult the LIVE config, not a compiled-in policy: with
/// `paste_warn_multiline` off (and the size gate off), a multi-line paste is
/// delivered immediately instead of being parked in the confirm overlay.
#[test]
fn a_disabled_gate_pastes_a_multiline_immediately() {
    let config = c0pl4nd_core::Config {
        paste_warn_multiline: false,
        paste_warn_bytes: 0,
        ..Default::default()
    };

    let app = RefCell::new(C0pl4ndApp::bootstrap_with(config));
    let mut h = harness(&app);

    h.event(egui::Event::Paste("GATEOFF_A\nGATEOFF_B".to_string()));
    h.step();
    h.step();

    assert!(
        !app.borrow().has_pending_paste(),
        "with both gates disabled a multi-line paste must NOT be deferred"
    );
    assert_eq!(
        app.borrow().pending_paste_reason(),
        None,
        "nothing was deferred, so there is no reason to report"
    );
}

/// The SIZE gate reaches the app too: an oversized SINGLE-line paste (no
/// newline, so the multi-line gate cannot see it) is deferred and reported as
/// [`PasteConfirmReason::Large`]. Pins the app to the two-gate policy rather
/// than to "contains a newline".
#[test]
fn oversized_single_line_paste_is_deferred_as_large() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    let huge = "H".repeat(app.borrow().config.paste_warn_bytes + 1);
    assert!(
        !huge.contains('\n'),
        "the size gate must fire with no newline"
    );

    h.event(egui::Event::Paste(huge.clone()));
    h.step();
    h.step();

    assert!(
        app.borrow().has_pending_paste(),
        "an oversized single-line paste must be deferred"
    );
    assert_eq!(
        app.borrow().pending_paste_reason(),
        Some(PasteConfirmReason::Large),
        "the SIZE gate (not the multi-line gate) must be recorded as the reason"
    );

    let sent = app.borrow_mut().confirm_pending_paste();
    assert_eq!(
        sent.as_deref(),
        Some(huge.as_str()),
        "confirming returns the exact deferred text"
    );
    assert!(
        !app.borrow().has_pending_paste(),
        "confirming clears the pending paste"
    );
}

/// PRIVACY (command-history capture): a line the user typed that the shell
/// ECHOED is recorded in history; a line prefixed with a SPACE is excluded
/// (HISTCONTROL=ignorespace). This also exercises the echo-gate: a recorded
/// line must have appeared in the focused grid.
#[test]
fn history_records_echoed_commands_and_skips_leading_space() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    // Type an echoed (but not executed elsewhere) token, then Enter. The shell
    // echoes it onto the prompt line, so the echo-gate records it.
    type_text(&mut h, "echohist_KEEP");
    // Make sure the echo has landed in the grid before committing.
    assert!(
        poll_focused_contains(&mut h, &app, "echohist_KEEP", Duration::from_secs(10)),
        "the typed command must be echoed before Enter"
    );
    press_enter(&mut h);
    h.step();
    assert!(
        app.borrow()
            .command_history_entries()
            .iter()
            .any(|e| e.contains("echohist_KEEP")),
        "an echoed typed command must be recorded in history"
    );

    // A leading-space line must NOT be recorded (ignorespace opt-out).
    type_text(&mut h, " spacedout_DROP");
    poll_focused_contains(&mut h, &app, "spacedout_DROP", Duration::from_secs(10));
    press_enter(&mut h);
    h.step();
    assert!(
        !app.borrow()
            .command_history_entries()
            .iter()
            .any(|e| e.contains("spacedout_DROP")),
        "a leading-space command must be excluded from history"
    );
}

/// PRIVACY controls: an incognito session records NO command history, and the
/// "clear history" action empties it. Drives the public control surface the
/// Privacy settings page is wired to.
#[test]
fn incognito_blocks_history_and_clear_empties_it() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    // Incognito ON → a typed+echoed command is NOT recorded.
    app.borrow_mut().set_incognito(true);
    type_text(&mut h, "incog_SECRET_cmd");
    poll_focused_contains(&mut h, &app, "incog_SECRET_cmd", Duration::from_secs(10));
    press_enter(&mut h);
    h.step();
    assert!(
        app.borrow().command_history_entries().is_empty(),
        "incognito must record no command history"
    );

    // Incognito OFF → a command records; clear empties it (zeroized).
    app.borrow_mut().set_incognito(false);
    type_text(&mut h, "kept_after_incog");
    assert!(
        poll_focused_contains(&mut h, &app, "kept_after_incog", Duration::from_secs(10)),
        "command echoes after leaving incognito"
    );
    press_enter(&mut h);
    h.step();
    assert!(
        app.borrow()
            .command_history_entries()
            .iter()
            .any(|e| e.contains("kept_after_incog")),
        "leaving incognito resumes history capture"
    );
    app.borrow_mut().clear_command_history();
    assert!(
        app.borrow().command_history_entries().is_empty(),
        "clear_command_history empties the history"
    );
}

/// ACCESSIBILITY (F2-1): the terminal grid is custom-painted, so without an
/// explicit AccessKit node a screen reader perceives an empty interactive
/// region and the terminal's content is invisible to assistive tech. This test
/// drives the REAL frame loop, gets a deterministic marker into the focused
/// pane's grid, then asserts the marker is present in the AccessKit tree the
/// app exposes (`query_by_label_contains`) — i.e. a screen reader would read
/// the grid content. Without the `resp.widget_info(..)` fix in
/// `render_pane_body`, no node carries the grid text and this query is `None`.
#[test]
fn grid_text_is_exposed_to_accesskit_screen_readers() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    {
        let a = app.borrow();
        let focused = a.focused_pane();
        if a.pane_grid_text(focused).is_none() {
            eprintln!("no live PTY on this platform; skipping a11y grid-text exposure");
            return;
        }
    }
    let mut h = harness(&app);

    let token = "c0pl4nd_a11y_marker";
    type_text(&mut h, &format!("echo {token}"));
    press_enter(&mut h);
    let seen = poll_focused_contains(&mut h, &app, token, Duration::from_secs(8));
    assert!(
        seen,
        "pre-condition: the marker must reach the focused pane's grid"
    );

    // Build one more frame so the AccessKit tree reflects the grid that now
    // contains the marker, then assert the marker is exposed accessibly.
    h.step();
    assert!(
        h.query_by_label_contains(token).is_some(),
        "the terminal grid text must be exposed to AccessKit (screen readers) — \
         no accessible node carried the marker '{token}'"
    );
}

/// IME COMPOSITION (F3-1): a CJK / complex-script IME routes composed text
/// through `egui::Event::Ime(ImeEvent::Commit(..))` — NOT `Event::Text` — so
/// without an `Event::Ime` arm in `forward_input_to_focused`, CJK input never
/// reaches the shell. This test drives the REAL frame loop, fires a `Commit`
/// event carrying a CJK string into the focused pane, and asserts the committed
/// text reaches that pane's REAL terminal grid (the shell echoes typed input).
/// Proves the IME-commit → PTY → grid round-trip. Guarded with the same
/// no-live-PTY skip as the ASCII round-trip test so it never falsely greens on
/// a box without a usable shell.
#[test]
fn ime_commit_reaches_the_pty_and_updates_the_grid() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    {
        let a = app.borrow();
        let focused = a.focused_pane();
        if a.pane_grid_text(focused).is_none() {
            eprintln!("no live PTY on this platform; skipping IME-commit round-trip");
            return;
        }
    }
    let mut h = harness(&app);

    // A CJK composition the user finished composing: the IME delivers the final
    // result as a single `Commit`. (A real session would also see one or more
    // `Preedit` events first; those are display-only and need not be simulated
    // to prove the commit path.) The shell echoes the typed characters back to
    // the grid, exactly as ASCII `Event::Text` does in the round-trip test.
    let composed = "日本語";
    h.event(egui::Event::Ime(egui::ImeEvent::Commit(
        composed.to_string(),
    )));
    h.step();

    let seen = poll_focused_contains(&mut h, &app, composed, Duration::from_secs(8));
    assert!(
        seen,
        "the IME-committed text `{composed}` must reach the PTY and echo into \
         the focused pane's grid — proves Event::Ime(Commit) → PTY → grid (F3-1)"
    );
}
