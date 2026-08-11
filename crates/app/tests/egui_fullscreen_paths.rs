//! Headless **interaction** tests for the two surfaces that toggle frameless
//! fullscreen (#36) — the F11 chord and the command-palette ROW CLICK — driven
//! through the real `frame_tick`.
//!
//! ## Why this file exists
//!
//! `Action::ToggleFullscreen` is REGISTERED on two surfaces: the keybinding
//! dispatcher and the command palette. Only the first had ever been driven
//! (`egui_chrome.rs::f11_toggles_frameless_fullscreen_and_hides_the_chrome`).
//! "The action is in `Action::ALL`, therefore the palette row works" is a
//! registration argument, not evidence — the palette row is a real widget whose
//! click has to reach `run_palette_selection` → `dispatch_action`, and nothing
//! asserted that it did.
//!
//! ## The reconcile branch, and why it was invisible
//!
//! `frame_tick` mirrors the OS fullscreen state back into `self.fullscreen` each
//! frame, guarded so it is SKIPPED on a frame that just commanded a change (the
//! OS reports the OLD state until it has applied the command, and that stale read
//! would undo the toggle).
//!
//! `egui_kittest` leaves `RawInput.viewports[ROOT].fullscreen` at `None`, so in
//! every other test in this repo that reconcile branch is **dead** — `if let
//! Some(os)` never matches. A test that never populates it cannot say anything
//! about the guard.
//!
//! [`FakeOs`] populates it the way the real backend does: it reports a change
//! ONLY because the app emitted `ViewportCommand::Fullscreen`, so the reported
//! state is derived from what the app actually commanded rather than from what
//! the app's own mirror says (which would be circular). `lag` is how many extra
//! frames the report takes.
//!
//! `lag = 0` is what ships: winit's Windows backend writes
//! `window_state.fullscreen` SYNCHRONOUSLY inside `set_fullscreen`
//! (`winit-0.30.13` `platform_impl/windows/window.rs:718`) and `window.fullscreen()`
//! reads that same field (`:692`), which is what `egui-winit`'s
//! `update_viewport_info` publishes (`egui-winit-0.34.3` `lib.rs:1277`). So the
//! next frame already sees the new state.

use c0pl4nd::egui_app;
use std::cell::RefCell;
use std::collections::VecDeque;

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

use egui_app::C0pl4ndApp;

fn harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
    // `Harness::new` (the Context-closure form) is deprecated in egui_kittest
    // 0.34 in favour of `new_ui`, but `new_ui` hands out only a `&mut Ui` and
    // `frame_tick` builds panels + windows. Same deliberate allow as
    // `egui_chrome.rs`.
    #[allow(deprecated)]
    let mut h = Harness::new(move |ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1000.0, 700.0));
    h
}

/// A window manager that moves ONLY because the app commanded it, and takes
/// `lag` extra frames to report the move back.
struct FakeOs {
    fullscreen: bool,
    pipeline: VecDeque<Option<bool>>,
}

impl FakeOs {
    fn new(lag: usize) -> Self {
        Self {
            fullscreen: false,
            pipeline: (0..lag).map(|_| None).collect(),
        }
    }

    /// One frame: publish what the OS currently reports, step the app, capture
    /// any `Fullscreen` command it emitted, and advance the report pipeline.
    fn frame(&mut self, h: &mut Harness<'_>) {
        h.input_mut()
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .expect("the harness always has a ROOT viewport")
            .fullscreen = Some(self.fullscreen);
        h.step();
        let mut commanded = None;
        for out in h.output().viewport_output.values() {
            for cmd in &out.commands {
                if let egui::ViewportCommand::Fullscreen(want) = cmd {
                    commanded = Some(*want);
                }
            }
        }
        self.pipeline.push_back(commanded);
        if let Some(Some(want)) = self.pipeline.pop_front() {
            self.fullscreen = want;
        }
    }

    fn frames(&mut self, h: &mut Harness<'_>, n: usize) {
        for _ in 0..n {
            self.frame(h);
        }
    }
}

/// Open the palette, filter it to actions matching "fullscreen", and return the
/// single row's label. Asserting the row set here is what stops a later click
/// landing on some other action if the fuzzy match ever widens.
fn palette_fullscreen_row(
    h: &mut Harness<'_>,
    os: &mut FakeOs,
    app: &RefCell<C0pl4ndApp>,
) -> String {
    h.event(egui::Event::Key {
        key: egui::Key::P,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        },
    });
    os.frame(h);
    assert!(
        app.borrow().palette_open(),
        "precondition: Ctrl+Shift+P must open the palette"
    );
    // A leading `>` filters to ACTIONS only, so no shell-history row can be
    // selected instead.
    for ch in ">fullscreen".chars() {
        h.event(egui::Event::Text(ch.to_string()));
    }
    os.frames(h, 2);
    let rows = app.borrow().palette_row_labels();
    assert_eq!(
        rows.len(),
        1,
        "the query must isolate exactly one action row, got {rows:?}"
    );
    rows.into_iter().next().expect("the one row")
}

/// Press F11 as a real, modifier-free key event (one event, one frame — the
/// keybinding dispatcher only acts on the press).
fn press_f11(h: &mut Harness<'_>, os: &mut FakeOs) {
    h.event(egui::Event::Key {
        key: egui::Key::F11,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    os.frame(h);
}

/// THE CLICK PATH. Clicking the palette's "Fullscreen" ROW must do what F11
/// does: dispatch through the shared action layer, enter fullscreen, drop the
/// titlebar, and close the palette. Clicking it again must come back out.
///
/// The chrome-presence flip is asserted alongside the state flag so this cannot
/// pass on a mirror that flipped while the frame kept drawing the titlebar.
#[test]
fn the_palette_fullscreen_row_click_toggles_fullscreen_and_the_chrome() {
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    let mut os = FakeOs::new(0);
    os.frames(&mut h, 2);

    assert!(
        !app.borrow().fullscreen(),
        "the app starts windowed (not fullscreen)"
    );
    assert!(
        h.query_by_label("settings").is_some(),
        "the titlebar caption cluster (settings gear) is present while windowed"
    );

    let row = palette_fullscreen_row(&mut h, &mut os, &app);
    h.get_by_label(row.as_str()).click();
    os.frame(&mut h);

    assert_eq!(
        app.borrow().last_palette_action(),
        Some(egui_app::Action::ToggleFullscreen),
        "the row click must reach the SHARED dispatch path, not a palette-local branch"
    );
    assert!(
        app.borrow().fullscreen(),
        "clicking the palette's Fullscreen row must enter fullscreen"
    );
    assert!(
        !app.borrow().palette_open(),
        "running a palette row closes the palette"
    );
    // The palette renders AFTER the titlebar within a frame, so the click frame
    // itself still drew the (pre-toggle) chrome. The chrome-presence flip is
    // observable from the NEXT frame on.
    os.frame(&mut h);
    assert!(
        h.query_by_label("settings").is_none(),
        "entering fullscreen hides the titlebar (its gear is no longer rendered)"
    );

    // …and the same click again leaves it.
    let row = palette_fullscreen_row(&mut h, &mut os, &app);
    h.get_by_label(row.as_str()).click();
    os.frame(&mut h);
    assert!(
        !app.borrow().fullscreen(),
        "a second palette row click must exit fullscreen"
    );
    os.frame(&mut h);
    assert!(
        h.query_by_label("settings").is_some(),
        "exiting fullscreen restores the titlebar (the gear is back)"
    );
}

/// The OS-reconcile branch must never undo a toggle the app itself just
/// commanded — on EITHER surface.
///
/// This is the first test in the repo that populates `viewport().fullscreen` at
/// all, so it is the first one in which that branch executes. Both surfaces are
/// asserted because the guard is keyed on the KEYBINDING dispatcher's fired-action
/// set, which a palette click is not a member of; if that ever became the thing
/// the guard depends on, the palette arm here is what says so.
#[test]
fn the_os_reconcile_never_undoes_a_toggle_the_app_just_commanded() {
    for surface in ["f11", "palette"] {
        let app = RefCell::new(C0pl4ndApp::bootstrap());
        let mut h = harness(&app);
        let mut os = FakeOs::new(0);
        os.frames(&mut h, 2);
        assert_eq!(
            h.input()
                .viewports
                .get(&egui::ViewportId::ROOT)
                .and_then(|v| v.fullscreen),
            Some(false),
            "{surface}: precondition — the OS must be REPORTING a state, or the \
             reconcile branch this test is about never runs"
        );

        if surface == "f11" {
            press_f11(&mut h, &mut os);
        } else {
            let row = palette_fullscreen_row(&mut h, &mut os, &app);
            h.get_by_label(row.as_str()).click();
            os.frame(&mut h);
        }
        assert!(
            app.borrow().fullscreen(),
            "{surface}: the toggle must take on the frame it is commanded"
        );

        // Every later frame reads the OS report. None of them may flip it back.
        for i in 0..4 {
            os.frame(&mut h);
            assert!(
                app.borrow().fullscreen(),
                "{surface}: frame +{i} after the toggle read the OS state back and \
                 UNDID it — the reconcile guard let a stale/other read win"
            );
        }
    }
}
