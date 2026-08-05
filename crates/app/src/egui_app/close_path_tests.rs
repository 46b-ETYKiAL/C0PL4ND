//! Wiring tests for the window CLOSE path and the visual-QA cursor-blink pin.
//!
//! Three capabilities shipped built-and-tested but with no call site, so all
//! three were inert: `WindowConfig::close_guard` (the running-command
//! confirmation), `WindowConfig::close_action` (close/minimize to tray), and
//! `PaneTerm::spawn_program_in` (the named-profile working directory). Their
//! own unit tests were exhaustive and green the whole time — a decision function
//! nobody calls passes every test you can write about the decision.
//!
//! So these tests are deliberately about the WIRE, not the decision:
//!
//! * the busy-pane count comes from a REAL OSC 133 `;C` mark driven through the
//!   real parser, not a stubbed count;
//! * the close paths are driven through the REAL `frame_tick` (an OS
//!   `close_requested`, an Alt+F4 key event) and the REAL caption ✕ click, not
//!   by calling the decision directly — every assertion that matters is on what
//!   the app then DID;
//! * the cwd tests assert the spawned child ACTUALLY RAN in the requested
//!   directory (it prints its own cwd), never that the argument was stored — a
//!   spawn that accepted `cwd` and dropped it would satisfy any "a pane
//!   appeared" check.

use super::*;

use std::cell::RefCell;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Serialises every test that touches the process-wide explicit-quit flag —
/// directly, or via a `frame_tick` close path (which CONSUMES it). Without this
/// a parallel test's `take_explicit_quit` would eat the flag another test just
/// set, and the round-trip test would fail for a reason that is not a bug.
///
/// Poisoning is recovered from rather than propagated: one failing test must not
/// turn every later test in this file into a panic that hides its own verdict.
static CLOSE_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn close_path_guard() -> std::sync::MutexGuard<'static, ()> {
    CLOSE_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A uniquely-named directory under the OS temp dir. The unique component is
/// what the assertions match on, so Windows 8.3 short-name mangling of the
/// PARENT (`RUNNER~1`) can never make a correct spawn look wrong.
fn unique_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("c0pl4nd-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the test cwd");
    dir
}

/// A token only THIS profile's arguments can put on screen.
///
/// It is what separates "the profile ran" from "the default shell ran": on
/// Windows the platform default shell IS `cmd.exe`, and an interactive `cmd.exe`
/// started in the probe's directory prints that very directory in its prompt. A
/// cwd assertion alone would therefore pass for a spawn that ignored the profile
/// entirely — the exact regression these tests exist to catch.
const PROFILE_SENTINEL: &str = "c0pl4ndprobe";

/// A program that prints [`PROFILE_SENTINEL`] joined to its own working
/// directory. Printing the cwd is what makes the assertion about where the child
/// really ran rather than about an argument being stored.
fn print_cwd_program() -> (&'static str, Vec<&'static str>) {
    #[cfg(windows)]
    {
        ("cmd.exe", vec!["/C", "echo c0pl4ndprobe-%CD%"])
    }
    #[cfg(not(windows))]
    {
        ("/bin/sh", vec!["-c", "echo c0pl4ndprobe-$(pwd)"])
    }
}

/// A shell profile bound to an explicit program, appended to `app` and made
/// ACTIVE — the state a user is in after picking a shell from the ▾ menu.
fn activate_profile(app: &mut C0pl4ndApp, program: &str, args: &[&str]) {
    app.shell_profiles.push(shells::ShellProfile {
        label: "cwd probe".to_string(),
        program: Some(program.to_string()),
        args: args.iter().map(|a| (*a).to_string()).collect(),
    });
    app.active_shell = app.shell_profiles.len() - 1;
}

/// Does the terminal grid show `needle`, ignoring the hard wrap?
///
/// The probe prints an absolute path into an 80-column grid, so a long temp path
/// is split across rows mid-token — a plain `contains` would report a CORRECT
/// spawn as a failure. Both sides drop every space and line break before the
/// comparison; the needle is a single unique path component with neither, so
/// this cannot match anything else.
fn shows_unwrapped(grid: &str, needle: &str) -> bool {
    fn squash(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }
    squash(grid).contains(&squash(needle))
}

/// Poll a pane's visible grid for `needle` until `timeout`. The PTY reader is a
/// background thread, so output arrives asynchronously; returns the last grid
/// seen so a failure message can show what DID land.
fn wait_for_grid(pane: &PaneTerm, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    loop {
        last = pane.grid_text().unwrap_or(last);
        if shows_unwrapped(&last, needle) {
            return last;
        }
        if Instant::now() >= deadline {
            return last;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Attach a live pane to `app` whose shell has announced, through the REAL OSC
/// 133 parser, that a command's output has started (`ESC ] 133 ; C BEL`) and has
/// NOT announced that it finished. That is exactly the state
/// `PaneTerm::has_running_command` reports `true` for.
///
/// Panics rather than skipping when no shell can spawn: an absent platform is
/// not a passing close guard, and a silent early return would make this file
/// green on a host where it proved nothing.
fn attach_busy_pane(app: &mut C0pl4ndApp) -> PaneId {
    let pane = PaneTerm::spawn(app.theme.clone(), 80, 24);
    let term = pane
        .terminal_for_test()
        .expect("the platform default shell must spawn for this test to mean anything");
    term.lock().unwrap().advance(b"\x1b]133;C\x07");
    let pid = app.pane_alloc.alloc();
    app.terms.insert(pid, pane);
    assert!(
        app.terms[&pid].has_running_command(),
        "precondition: the pane must report the ;C mark as a command in flight"
    );
    pid
}

/// Drive ONE real `frame_tick` over a headless context with a real screen rect
/// (so the grid actually lays out and a deferred pane can spawn), with `raw`
/// tweaked by `prep`. Returns the frame's full output so a test can inspect what
/// was PAINTED.
fn drive_frame(
    ctx: &egui::Context,
    app: &mut C0pl4ndApp,
    prep: impl FnOnce(&mut egui::RawInput),
) -> egui::FullOutput {
    let mut raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 800.0),
        )),
        ..Default::default()
    };
    prep(&mut raw);
    ctx.begin_pass(raw);
    app.frame_tick(ctx);
    ctx.end_pass()
}

/// Mark `raw` as an OS-accepted window close — the state winit reports for a
/// taskbar → Close, the system menu, and the `WM_CLOSE` the tray menu's Quit
/// posts.
fn as_os_close(raw: &mut egui::RawInput) {
    let id = raw.viewport_id;
    raw.viewports
        .entry(id)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
}

// ---------------------------------------------------------------------------
// SEAM 1 — close_guard: a running command holds the close
// ---------------------------------------------------------------------------

/// The wire, end to end: a real `;C` mark → `has_running_command` →
/// `busy_pane_count` → `close_guard` → a HELD close.
///
/// The first assertion (an idle app exits immediately) is what stops the second
/// passing vacuously: if `close_decision` always returned `Confirm`, or if the
/// count were always non-zero, the idle case would fail here.
#[test]
fn a_running_command_reaches_close_guard_and_holds_the_close() {
    let _guard = close_path_guard();
    let mut app = C0pl4ndApp::bootstrap();
    assert!(
        app.config.window.warn_on_close_running,
        "precondition: the running-command guard ships ON"
    );
    assert_eq!(app.busy_pane_count(), 0, "a fresh app has nothing running");
    assert_eq!(
        app.close_decision(false),
        CloseOutcome::Exit,
        "with nothing running the close must go straight through"
    );

    attach_busy_pane(&mut app);

    assert_eq!(
        app.busy_pane_count(),
        1,
        "the pane's OSC 133 mark must reach the count the guard decides on"
    );
    assert_eq!(
        app.close_decision(false),
        CloseOutcome::Confirm { busy_panes: 1 },
        "a command in flight must HOLD the close for confirmation, carrying the \
         real pane count"
    );
}

/// The user's "Close anyway" must let the close through — and the count is still
/// non-zero when they say it, so a guard that re-ran unconditionally would loop
/// the prompt forever.
#[test]
fn a_confirmed_close_proceeds_while_the_command_is_still_running() {
    let _guard = close_path_guard();
    let mut app = C0pl4ndApp::bootstrap();
    attach_busy_pane(&mut app);
    assert_eq!(
        app.close_decision(false),
        CloseOutcome::Confirm { busy_panes: 1 },
        "precondition: the close is held"
    );

    app.close_confirmed = true;

    assert_eq!(
        app.busy_pane_count(),
        1,
        "the command is STILL running — that is the whole point of the \
         short-circuit"
    );
    assert_eq!(
        app.close_decision(false),
        CloseOutcome::Exit,
        "the recorded answer must reach close_guard's already_confirmed and let \
         the close through"
    );
}

/// Turning the preference off must reach the same call — a guard wired to a
/// hardcoded `true` would hold this close too.
#[test]
fn the_warn_preference_off_never_holds_a_close() {
    let _guard = close_path_guard();
    let mut app = C0pl4ndApp::bootstrap();
    attach_busy_pane(&mut app);
    assert_eq!(
        app.close_decision(false),
        CloseOutcome::Confirm { busy_panes: 1 },
        "precondition: ON holds the close"
    );

    app.config.window.warn_on_close_running = false;

    assert_eq!(
        app.close_decision(false),
        CloseOutcome::Exit,
        "the live preference must reach close_guard"
    );
}

// ---------------------------------------------------------------------------
// SEAM 2 — close_action: tray, and the two load-bearing behaviours
// ---------------------------------------------------------------------------

/// LOAD-BEARING: with NO tray icon, close-to-tray must still EXIT. Hiding to a
/// tray that does not exist strands the window invisible with nothing to restore
/// it from — the app is running, unreachable, holding the user's shells.
///
/// The second half is what makes the first half mean something: once a tray is
/// reported the SAME config hides instead. So the exit above was the missing
/// tray, not an inert preference.
#[test]
fn no_tray_always_exits_even_with_close_to_tray_on() {
    let _guard = close_path_guard();
    let mut app = C0pl4ndApp::bootstrap();
    app.config.window.close_to_tray = true;
    assert!(
        !app.tray_available(),
        "precondition: nothing has reported a tray"
    );

    assert_eq!(
        app.close_decision(false),
        CloseOutcome::Exit,
        "close-to-tray with NO tray must exit, never hide an unrecoverable window"
    );

    app.set_tray_available(true);

    assert_eq!(
        app.close_decision(false),
        CloseOutcome::HideToTray,
        "with a real tray the same preference hides — proving the exit above was \
         the ABSENT TRAY and not a preference that never reached close_action"
    );
}

/// LOAD-BEARING: an explicit quit must ALWAYS exit. The tray menu's own Quit
/// posts `WM_CLOSE`, which arrives at the same close path as an ordinary ✕ — so
/// without this, close-to-tray would swallow the one affordance that closes the
/// app and it could never be shut down at all.
#[test]
fn an_explicit_quit_always_exits_even_with_close_to_tray_on() {
    let _guard = close_path_guard();
    let mut app = C0pl4ndApp::bootstrap();
    app.config.window.close_to_tray = true;
    app.set_tray_available(true);
    assert_eq!(
        app.close_decision(false),
        CloseOutcome::HideToTray,
        "precondition: close-to-tray is live for an ordinary close"
    );

    assert_eq!(
        app.close_decision(true),
        CloseOutcome::Exit,
        "an explicit quit must never be swallowed by close-to-tray"
    );
}

/// The guard runs BEFORE the tray action: a running command holds the close even
/// when close-to-tray would otherwise hide the window. (Hiding would leave the
/// user with no prompt AND no window, while the command kept running.)
#[test]
fn the_running_command_guard_runs_before_the_tray_action() {
    let _guard = close_path_guard();
    let mut app = C0pl4ndApp::bootstrap();
    app.config.window.close_to_tray = true;
    app.set_tray_available(true);
    attach_busy_pane(&mut app);

    assert_eq!(
        app.close_decision(false),
        CloseOutcome::Confirm { busy_panes: 1 },
        "the guard must be consulted first"
    );

    app.close_confirmed = true;
    assert_eq!(
        app.close_decision(false),
        CloseOutcome::HideToTray,
        "once confirmed, the tray action gets its say"
    );
}

/// The tray's Quit flag must survive the hop from the tray's global menu handler
/// to the close path — and be CONSUMED there, so one Quit cannot make every
/// later close in the session bypass close-to-tray.
#[test]
fn the_explicit_quit_flag_is_delivered_once_and_consumed() {
    let _guard = close_path_guard();
    assert!(
        !take_explicit_quit(),
        "precondition: no quit pending (the lock serialises the consumers)"
    );

    request_explicit_quit();

    assert!(
        take_explicit_quit(),
        "the tray Quit must reach the close path"
    );
    assert!(
        !take_explicit_quit(),
        "...and be consumed — a sticky flag would make every later ✕ bypass \
         close-to-tray"
    );
}

// ---------------------------------------------------------------------------
// SEAM 1+2 driven through the REAL close paths
// ---------------------------------------------------------------------------

/// An OS-initiated close (`close_requested`) must run the decision and reach the
/// exit. `exit_requests` is the observable: in a live window the process is gone
/// by now, so the headless harness counts the branch instead.
#[test]
fn an_os_close_request_runs_the_decision_and_exits() {
    let _guard = close_path_guard();
    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();

    drive_frame(&ctx, &mut app, |_| {});
    assert_eq!(
        app.exit_requests(),
        0,
        "an ordinary frame must not close the app"
    );
    assert_eq!(app.last_close_outcome(), None, "...nor decide anything");

    drive_frame(&ctx, &mut app, as_os_close);

    assert_eq!(
        app.last_close_outcome(),
        Some(CloseOutcome::Exit),
        "the OS close must go through close_guard + close_action"
    );
    assert_eq!(app.exit_requests(), 1, "...and reach the exit");
}

/// The same OS close, with a command in flight: HELD, not exited, and the
/// confirmation is up carrying the real pane count.
#[test]
fn an_os_close_request_is_held_by_a_running_command() {
    let _guard = close_path_guard();
    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();
    drive_frame(&ctx, &mut app, |_| {});
    attach_busy_pane(&mut app);

    drive_frame(&ctx, &mut app, as_os_close);

    assert_eq!(
        app.exit_requests(),
        0,
        "a close with a command in flight must NOT kill the shells outright"
    );
    assert_eq!(
        app.close_confirm_busy_panes(),
        Some(1),
        "the confirmation must be up, naming how many panes are busy"
    );
}

/// Alt+F4 takes the same decision. The caption subclass removes `WS_SYSMENU`, so
/// the OS never turns Alt+F4 into a `close_requested` — the key event is all the
/// app gets, and it must not bypass the guard.
#[test]
fn alt_f4_runs_the_close_decision() {
    let _guard = close_path_guard();
    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();
    drive_frame(&ctx, &mut app, |_| {});
    attach_busy_pane(&mut app);

    drive_frame(&ctx, &mut app, |raw| {
        raw.modifiers.alt = true;
        raw.events.push(egui::Event::Key {
            key: egui::Key::F4,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                alt: true,
                ..Default::default()
            },
        });
    });

    assert_eq!(
        app.exit_requests(),
        0,
        "Alt+F4 must be held by the running-command guard, not fast-exit past it"
    );
    assert_eq!(
        app.close_confirm_busy_panes(),
        Some(1),
        "Alt+F4 must raise the same confirmation as every other close"
    );
}

/// Answering the confirmation ("Close anyway") must re-enter the close path and
/// actually close — driven through the REAL modal, by pressing Enter on a real
/// frame, not by calling the handler.
#[test]
fn answering_the_confirmation_closes_the_app() {
    let _guard = close_path_guard();
    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();
    drive_frame(&ctx, &mut app, |_| {});
    attach_busy_pane(&mut app);
    drive_frame(&ctx, &mut app, as_os_close);
    assert_eq!(
        app.close_confirm_busy_panes(),
        Some(1),
        "precondition: the confirmation is up"
    );

    drive_frame(&ctx, &mut app, |raw| {
        raw.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
    });

    assert_eq!(
        app.close_confirm_busy_panes(),
        None,
        "the confirmation must be dismissed"
    );
    assert_eq!(app.exit_requests(), 1, "...and the close must go through");
}

/// Declining the confirmation ("Keep working") must dismiss it and leave the app
/// running — AND must not record the answer, or the very next close would skip
/// the warning while the same command was still running.
#[test]
fn declining_the_confirmation_keeps_the_app_running() {
    let _guard = close_path_guard();
    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();
    drive_frame(&ctx, &mut app, |_| {});
    attach_busy_pane(&mut app);
    drive_frame(&ctx, &mut app, as_os_close);
    assert_eq!(app.close_confirm_busy_panes(), Some(1), "precondition");

    drive_frame(&ctx, &mut app, |raw| {
        raw.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
    });

    assert_eq!(app.close_confirm_busy_panes(), None, "dismissed");
    assert_eq!(app.exit_requests(), 0, "the app is still running");
    assert!(
        !app.close_confirmed,
        "declining must NOT be remembered as consent — the next close must warn \
         again"
    );

    drive_frame(&ctx, &mut app, as_os_close);
    assert_eq!(
        app.close_confirm_busy_panes(),
        Some(1),
        "the next close warns again"
    );
    assert_eq!(app.exit_requests(), 0);
}

/// The caption ✕ takes the same decision — driven by CLICKING the real button in
/// the real chrome, so a ✕ rewired to bypass the funnel fails here.
#[test]
fn clicking_the_caption_close_runs_the_close_decision() {
    use egui_kittest::kittest::Queryable;

    let _guard = close_path_guard();
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    #[allow(deprecated)]
    let mut h = egui_kittest::Harness::new(|ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));
    h.run();
    assert_eq!(
        app.borrow().exit_requests(),
        0,
        "precondition: still running"
    );

    h.get_by_label("close").click();
    h.run();

    assert_eq!(
        app.borrow().last_close_outcome(),
        Some(CloseOutcome::Exit),
        "the ✕ must run the close decision"
    );
    assert_eq!(
        app.borrow().exit_requests(),
        1,
        "...and reach the exit branch"
    );
}

/// …and the ✕ is held by a running command, exactly like every other close
/// surface. This is the case the guard exists for: a user clicking ✕ mid-build.
#[test]
fn clicking_the_caption_close_is_held_by_a_running_command() {
    use egui_kittest::kittest::Queryable;

    let _guard = close_path_guard();
    let mut seed = C0pl4ndApp::bootstrap();
    attach_busy_pane(&mut seed);
    let app = RefCell::new(seed);
    #[allow(deprecated)]
    let mut h = egui_kittest::Harness::new(|ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));
    h.run();

    h.get_by_label("close").click();
    h.run();

    assert_eq!(
        app.borrow().exit_requests(),
        0,
        "the ✕ must not kill an in-flight command with no prompt"
    );
    assert_eq!(
        app.borrow().close_confirm_busy_panes(),
        Some(1),
        "the confirmation must be up"
    );
}

// ---------------------------------------------------------------------------
// SEAM 3 — spawn_term_in's named-profile arm carries the cwd
// ---------------------------------------------------------------------------

/// THE regression: with a named shell profile active, `spawn_term_in` called the
/// directory-less `PaneTerm::spawn_program`, so reopening a closed pane (or
/// restoring a layout) silently landed in the default directory.
///
/// Asserted on where the child ACTUALLY RAN — it prints its own cwd — not on the
/// argument being stored. A variant that accepted `cwd` and dropped it would
/// pass any "a pane appeared" or "the field was set" check.
#[test]
fn a_named_profile_pane_opens_in_the_requested_directory() {
    let dir = unique_dir("named-profile-cwd");
    let unique = dir
        .file_name()
        .and_then(|s| s.to_str())
        .expect("unique component")
        .to_string();

    let mut app = C0pl4ndApp::bootstrap();
    let (program, args) = print_cwd_program();
    activate_profile(&mut app, program, &args);

    assert!(
        app.new_terminal_in(dir.to_str()),
        "the probe pane must open"
    );
    let pane = app
        .terms
        .get(&app.focused_pane)
        .expect("the new pane is the focused one");
    assert!(
        pane.error().is_none(),
        "the named profile must have spawned, got: {:?}",
        pane.error()
    );

    let grid = wait_for_grid(pane, &unique, Duration::from_secs(20));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        shows_unwrapped(&grid, PROFILE_SENTINEL),
        "the pane must be running the NAMED PROFILE's program — no \
         {PROFILE_SENTINEL:?} on screen means it fell back to the default shell; \
         grid:\n{grid}"
    );
    assert!(
        shows_unwrapped(&grid, &unique),
        "a named-profile pane must RUN in the requested cwd; wanted {unique:?} \
         in the grid, got:\n{grid}"
    );
}

// ---------------------------------------------------------------------------
// SEAM 4 — the deferred first-spawn honours the profile AND the cwd
// ---------------------------------------------------------------------------

/// Run frames until `pred` holds, or give up. The deferred first-spawn happens
/// inside `render_pane_body`, on the first frame the pane's rect is known, and
/// the PTY reader is a background thread — so both the spawn and its output need
/// a bounded wait rather than a fixed frame count.
fn run_until(
    h: &mut egui_kittest::Harness<'_>,
    timeout: Duration,
    mut pred: impl FnMut() -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        h.run();
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The deferred first-spawn (the initial pane, and every pane a restored layout
/// brings back) used to ignore the shell profile entirely and always spawn the
/// platform default — so a layout captured under PowerShell/WSL came back as the
/// default shell.
///
/// Pinned with a profile whose program CANNOT exist: if the profile is honoured
/// the pane carries a spawn error, and if it is ignored the default shell starts
/// perfectly. There is no timing in that distinction and no way to satisfy it by
/// accident.
#[test]
fn the_deferred_first_spawn_honours_the_active_shell_profile() {
    let mut seed = C0pl4ndApp::bootstrap();
    activate_profile(&mut seed, "c0pl4nd-no-such-program-exists", &[]);
    assert!(
        seed.terms.is_empty(),
        "precondition: the initial pane's PTY is DEFERRED, not spawned yet"
    );
    let app = RefCell::new(seed);

    #[allow(deprecated)]
    let mut h = egui_kittest::Harness::new(|ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));
    let spawned = run_until(&mut h, Duration::from_secs(10), || {
        !app.borrow().terms.is_empty()
    });
    assert!(spawned, "the deferred pane must eventually spawn");

    let borrowed = app.borrow();
    let pane = borrowed
        .terms
        .values()
        .next()
        .expect("the deferred pane exists");
    assert!(
        pane.error().is_some(),
        "the deferred spawn must use the ACTIVE profile — a pane with no error \
         means it fell back to the platform default shell and ignored it"
    );
}

/// …and it carries the restored working directory THROUGH that profile.
///
/// Both halves are asserted, and both are load-bearing: the [`PROFILE_SENTINEL`]
/// can only appear if the profile's ARGS ran, and the unique directory can only
/// appear if the restored cwd travelled with them. Dropping either one is caught
/// here — a spawn that honoured the profile but lost the cwd prints the sentinel
/// with the wrong directory, and one that ignored the profile prints no sentinel
/// at all (on Windows it would still show the right directory, in the default
/// shell's prompt).
#[test]
fn the_deferred_first_spawn_carries_the_restored_cwd_through_a_named_profile() {
    let dir = unique_dir("deferred-profile-cwd");
    let unique = dir
        .file_name()
        .and_then(|s| s.to_str())
        .expect("unique component")
        .to_string();

    let mut seed = C0pl4ndApp::bootstrap();
    let (program, args) = print_cwd_program();
    activate_profile(&mut seed, program, &args);
    let pid = seed.focused_pane;
    seed.restored_cwds
        .insert(pid, dir.to_str().expect("utf8 path").to_string());
    let app = RefCell::new(seed);

    #[allow(deprecated)]
    let mut h = egui_kittest::Harness::new(|ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));

    let showed = run_until(&mut h, Duration::from_secs(20), || {
        app.borrow()
            .terms
            .get(&pid)
            .and_then(PaneTerm::grid_text)
            .is_some_and(|g| shows_unwrapped(&g, PROFILE_SENTINEL) && shows_unwrapped(&g, &unique))
    });
    let grid = app
        .borrow()
        .terms
        .get(&pid)
        .and_then(PaneTerm::grid_text)
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        shows_unwrapped(&grid, PROFILE_SENTINEL),
        "the deferred spawn must run the ACTIVE PROFILE's program — no \
         {PROFILE_SENTINEL:?} on screen means it fell back to the platform \
         default shell; grid:\n{grid}"
    );
    assert!(
        showed,
        "the deferred spawn must run in the RESTORED cwd; wanted {unique:?} in \
         the grid, got:\n{grid}"
    );
}

// ---------------------------------------------------------------------------
// TASK B — the visual-QA cursor-blink phase pin
// ---------------------------------------------------------------------------

/// The pure phase rule, over its whole input space.
#[test]
fn cursor_blink_on_covers_every_input_combination() {
    // Unforced, and not blinking (either the setting is off or the pane is not
    // focused): steady ON. An unfocused caret is drawn as a hollow outline, so
    // it must not also vanish.
    for &blink in &[true, false] {
        for &focused in &[true, false] {
            if blink && focused {
                continue;
            }
            for &t in &[0.0, 0.3, 0.6, 1.05, 7.77] {
                assert!(
                    cursor_blink_on(None, blink, focused, t),
                    "a non-blinking caret is steady ON (blink={blink} \
                     focused={focused} t={t})"
                );
            }
        }
    }

    // Unforced, blinking on the focused pane: the first half of each period is
    // ON, the second half OFF. Period = 2 × 530 ms.
    assert!(cursor_blink_on(None, true, true, 0.0));
    assert!(cursor_blink_on(None, true, true, 0.52));
    assert!(!cursor_blink_on(None, true, true, 0.54));
    assert!(!cursor_blink_on(None, true, true, 1.05));
    assert!(
        cursor_blink_on(None, true, true, 1.07),
        "next period wraps ON"
    );

    // Forced: the pin WINS over every combination of the other three inputs —
    // which is the entire point (a scene captures whatever frame it captures).
    for &blink in &[true, false] {
        for &focused in &[true, false] {
            for &t in &[0.0, 0.54, 1.05, 42.5] {
                assert!(
                    cursor_blink_on(Some(CursorBlinkPhase::On), blink, focused, t),
                    "pinned ON must paint (blink={blink} focused={focused} t={t})"
                );
                assert!(
                    !cursor_blink_on(Some(CursorBlinkPhase::Off), blink, focused, t),
                    "pinned OFF must not paint (blink={blink} focused={focused} \
                     t={t})"
                );
            }
        }
    }
}

/// Count the filled rects painted in `out` whose fill is exactly `want`.
/// Recurses into composite shapes, since the pane body's painter emits nested
/// shape vectors.
fn filled_rects_of_colour(out: &egui::FullOutput, want: egui::Color32) -> usize {
    fn walk(shape: &egui::Shape, want: egui::Color32, n: &mut usize) {
        match shape {
            egui::Shape::Rect(r) if r.fill == want => {
                *n += 1;
            }
            egui::Shape::Vec(v) => {
                for s in v {
                    walk(s, want, n);
                }
            }
            _ => {}
        }
    }
    let mut n = 0;
    for clipped in &out.shapes {
        walk(&clipped.shape, want, &mut n);
    }
    n
}

/// The pin must reach the PAINTER, not merely the field.
///
/// Asserted on the rendered shape list: the focused pane's block caret is a
/// filled rect in the theme's cursor colour at 55% alpha. Pinning the phase OFF
/// must remove exactly that rect and pinning it ON must put it back — a pin that
/// was stored and never consulted leaves both frames identical, and the OFF
/// assertion fails.
#[test]
fn the_pinned_cursor_phase_reaches_the_painter() {
    let _guard = close_path_guard();
    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();
    assert!(
        app.config.cursor.blink,
        "precondition: the caret blinks by default — the free-running phase this \
         pin exists to control"
    );

    // The focused pane paints its block caret as the theme cursor colour at 55%.
    let cur = c0pl4nd_core::theme::parse_hex(&app.theme.cursor).unwrap_or((0, 255, 144));
    let caret = egui::Color32::from_rgb(cur.0, cur.1, cur.2).gamma_multiply(0.55);

    // Let the deferred pane spawn so there is a terminal with a cursor cell.
    let mut painted_on = 0;
    for _ in 0..8 {
        app.set_cursor_blink_phase(Some(CursorBlinkPhase::On));
        painted_on = filled_rects_of_colour(&drive_frame(&ctx, &mut app, |_| {}), caret);
        if painted_on > 0 {
            break;
        }
    }
    assert!(
        painted_on > 0,
        "precondition: with the phase pinned ON the caret must be painted (this \
         is the frame a QA snapshot captures)"
    );

    app.set_cursor_blink_phase(Some(CursorBlinkPhase::Off));
    let painted_off = filled_rects_of_colour(&drive_frame(&ctx, &mut app, |_| {}), caret);
    assert_eq!(
        painted_off, 0,
        "pinning the phase OFF must remove the caret from the painted frame — it \
         was still there, so the pin never reached the painter"
    );

    app.set_cursor_blink_phase(Some(CursorBlinkPhase::On));
    let painted_again = filled_rects_of_colour(&drive_frame(&ctx, &mut app, |_| {}), caret);
    assert!(
        painted_again > 0,
        "pinning it back ON must restore the caret"
    );
}
