//! Pump → taskbar-ATTENTION wiring for OSC 9 / OSC 777.
//!
//! # The gap these close
//!
//! `taskbar::should_request_attention` was covered only as a pure predicate.
//! Nothing asserted that `pump_pane_effects` ever CALLS it, that it feeds it the
//! real focus state, or that the resulting `ViewportCommand::RequestUserAttention`
//! is actually sent — so deleting the `send_viewport_cmd` line (i.e. removing
//! the entire user-visible effect of an OSC 9 notification) left the suite
//! green. That is the same dormant shape the OSC 9;4 progress wiring was in
//! before `taskbar_wiring_tests` in `mod_tests.rs` pinned it, and the sibling
//! this file mirrors.
//!
//! These drive the REAL `pump_pane_effects` over a REAL PTY-backed pane and
//! read egui's viewport output, so they fail if the pump stops draining
//! `fx.notified`, stops consulting the predicate, ignores focus, or stops
//! sending the command.
//!
//! Lives in `egui_app` (as a `taskbar` child) rather than beside the rest of
//! the notification tests in `crate::notify_tests`, because `pump_pane_effects`
//! and `PaneTerm::terminal_for_test` are private to `egui_app` and a lib-root
//! module cannot reach them.

use crate::egui_app::grid::PaneId;
use crate::egui_app::pane_term::PaneTerm;
use crate::egui_app::C0pl4ndApp;

/// Drive one real frame with a known focus state and return the root viewport's
/// commands. `focused` maps straight onto `ViewportInfo::focused`, which is what
/// the pump reads.
fn pump_frame(app: &mut C0pl4ndApp, focused: Option<bool>) -> Vec<egui::ViewportCommand> {
    let ctx = egui::Context::default();
    let mut viewports = egui::ViewportIdMap::default();
    viewports.insert(
        egui::ViewportId::ROOT,
        egui::ViewportInfo {
            focused,
            ..Default::default()
        },
    );
    let raw = egui::RawInput {
        viewports,
        ..Default::default()
    };
    ctx.begin_pass(raw);
    app.pump_pane_effects(&ctx);
    let out = ctx.end_pass();
    out.viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

/// Build an app whose single pane has just parsed `osc`.
fn app_with_osc(osc: &[u8]) -> C0pl4ndApp {
    let mut app = C0pl4ndApp::bootstrap();
    let pane = PaneTerm::spawn(app.theme.clone(), 80, 24);
    let term = pane
        .terminal_for_test()
        .expect("PTY spawn must succeed — a skipped wiring test proves nothing");
    term.lock().unwrap().advance(osc);
    app.terms.insert(PaneId(0), pane);
    app
}

fn requested_attention(cmds: &[egui::ViewportCommand]) -> bool {
    cmds.iter()
        .any(|c| matches!(c, egui::ViewportCommand::RequestUserAttention(_)))
}

/// An OSC 9 arriving while the window is unfocused must actually SEND the
/// viewport command — not merely satisfy the predicate.
#[test]
fn pump_sends_request_user_attention_for_osc9_while_unfocused() {
    let mut app = app_with_osc(b"\x1b]9;build done\x07");
    let cmds = pump_frame(&mut app, Some(false));
    assert!(
        requested_attention(&cmds),
        "the pump must emit ViewportCommand::RequestUserAttention; got {cmds:?}"
    );
}

/// OSC 777 (title + body) reaches the same wire. Without this, a pump that
/// drained only the OSC 9 queue would still pass the test above.
#[test]
fn pump_sends_request_user_attention_for_osc777_while_unfocused() {
    let mut app = app_with_osc(b"\x1b]777;notify;Heads up;done\x07");
    let cmds = pump_frame(&mut app, Some(false));
    assert!(requested_attention(&cmds), "got {cmds:?}");
}

/// The suppression half of the wire: focused means no command at all. Without
/// it, a `should_request_attention` hard-wired to `true` — or a pump that
/// ignored focus entirely — would pass the two tests above.
#[test]
fn pump_sends_no_attention_while_focused() {
    let mut app = app_with_osc(b"\x1b]9;build done\x07");
    let cmds = pump_frame(&mut app, Some(true));
    assert!(
        !requested_attention(&cmds),
        "a focused window must not be flashed; got {cmds:?}"
    );
}

/// And ordinary output must not flash, so the positive tests cannot be passing
/// merely because the pump emits the command unconditionally.
#[test]
fn pump_sends_no_attention_without_a_notification() {
    let mut app = app_with_osc(b"echo hello\r\n");
    let cmds = pump_frame(&mut app, Some(false));
    assert!(
        !requested_attention(&cmds),
        "ordinary output must not flash the taskbar; got {cmds:?}"
    );
}

/// Startup (focus not yet reported) must not flash — the `None` arm of the
/// predicate, driven through the real pump rather than called directly.
#[test]
fn pump_sends_no_attention_before_the_first_focus_event() {
    let mut app = app_with_osc(b"\x1b]9;rc-file says hi\x07");
    let cmds = pump_frame(&mut app, None);
    assert!(
        !requested_attention(&cmds),
        "a notification during launch must not flash; got {cmds:?}"
    );
}
