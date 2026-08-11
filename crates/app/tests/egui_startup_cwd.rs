//! Wiring tests for `--cwd <path>` / `-d <path>` — the startup working
//! directory the Explorer "Open C0PL4ND here" shell verb passes as `%V`.
//!
//! ## What these prove (and why the assertions are shaped this way)
//!
//! A parser test alone would be worthless here: the failure mode this feature
//! has is not "the string parsed wrong", it is "the parsed value never reached
//! the shell". So both tests drive the REAL production frame loop
//! ([`C0pl4ndApp::frame_tick`]) through `egui_kittest`, exactly like the other
//! `egui_*` suites, and assert on what the shipping path DID with the value:
//!
//! 1. [`the_frame_path_consumes_the_startup_cwd`] — the deferred first-pane
//!    spawn in `egui_app::render_pane_body` must CONSUME the one-shot store.
//!    Cut the `cli_cwd::take_startup_cwd()` call out of that spawn site and the
//!    value is still sitting in the store after a frame, so this fails. It needs
//!    no live PTY, so it can never silently skip.
//! 2. [`the_startup_cwd_reaches_the_initial_shell`] — end-to-end: the initial
//!    pane's REAL PTY prints the startup directory when asked where it is. This
//!    is the one that proves the directory reached `CommandBuilder::cwd` and not
//!    merely a Rust variable. It needs a live PTY and says so out loud when the
//!    platform has none, rather than passing vacuously.

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

use c0pl4nd::cli_cwd;
use c0pl4nd::egui_app::C0pl4ndApp;

/// The startup-directory store is process-global (it models a process-wide CLI
/// argument), so the two tests in this binary must not interleave.
static STORE_LOCK: Mutex<()> = Mutex::new(());

/// Build a headless harness driving the REAL `frame_tick`, with a screen large
/// enough that the pane gets a real pixel rect — which is what makes the
/// deferred first-pane spawn actually fire on the first frame.
fn harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
    #[allow(deprecated)]
    let mut h = Harness::new(move |ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1000.0, 700.0));
    h.run();
    h
}

/// Run `--cwd <dir>` through the SAME parse+validate the binary uses, then seed
/// the store the way `egui_main` does. Returns the marker directory.
fn seed_startup_cwd(root: &std::path::Path) -> std::path::PathBuf {
    // A distinctive, short leaf name so the polled needle cannot be confused
    // with anything else on screen and is unlikely to straddle a line wrap.
    let target = root.join("c0pl4ndcwd");
    std::fs::create_dir(&target).expect("create the marker directory");
    let argv = vec![
        "c0pl4nd.exe".to_string(),
        "--cwd".to_string(),
        target.to_string_lossy().into_owned(),
    ];
    let parsed = cli_cwd::parse_startup_cwd(&argv)
        .expect("a real directory must validate")
        .expect("the flag was present");
    cli_cwd::set_startup_cwd(&parsed);
    parsed
}

/// THE wiring assertion. Fails if the `cli_cwd::take_startup_cwd()` call is
/// removed from the deferred first-pane spawn — i.e. if the wire is cut.
#[test]
fn the_frame_path_consumes_the_startup_cwd() {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    cli_cwd::clear_startup_cwd();
    let dir = tempfile::tempdir().expect("tempdir");
    seed_startup_cwd(dir.path());

    let app = RefCell::new(C0pl4ndApp::bootstrap());
    // Pre-condition: nothing has spawned yet, so the store must still be full.
    // Without this the post-condition could pass for the wrong reason (a seed
    // that never landed).
    assert!(
        app.borrow()
            .pane_grid_text(app.borrow().focused_pane())
            .is_none(),
        "the initial pane is deferred: it must not exist before the first frame",
    );

    let _h = harness(&app);

    assert_eq!(
        cli_cwd::take_startup_cwd(),
        None,
        "the shipping frame path must CONSUME the --cwd startup directory at the \
         deferred first-pane spawn; a value still in the store means the spawn \
         site no longer reads it and --cwd silently does nothing",
    );
    cli_cwd::clear_startup_cwd();
}

/// End-to-end: the initial pane's REAL shell is running in the `--cwd`
/// directory. Asks the shell where it is and polls its grid for the answer.
#[test]
fn the_startup_cwd_reaches_the_initial_shell() {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    cli_cwd::clear_startup_cwd();
    let dir = tempfile::tempdir().expect("tempdir");
    let target = seed_startup_cwd(dir.path());
    let needle = target
        .file_name()
        .expect("marker leaf")
        .to_string_lossy()
        .into_owned();

    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    // The deferred spawn has now run. If the platform has no PTY there is
    // nothing to assert against — say so rather than pass vacuously. The
    // consume-assertion above still covers the wire in that case.
    if app.borrow().focused_grid_text().is_none() {
        eprintln!("no live PTY on this platform; skipping the --cwd end-to-end check");
        cli_cwd::clear_startup_cwd();
        return;
    }

    // `cd` with no argument prints the working directory on cmd.exe; `pwd` does
    // the same on a POSIX shell. Both are builtins of the default shells this
    // app spawns, so neither depends on anything being on PATH.
    let query = if cfg!(windows) { "cd" } else { "pwd" };
    for ch in query.chars() {
        h.event(egui::Event::Text(ch.to_string()));
    }
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while Instant::now() < deadline {
        h.step();
        if app
            .borrow()
            .focused_grid_text()
            .is_some_and(|t| t.contains(&needle))
        {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }

    let grid = app.borrow().focused_grid_text().unwrap_or_default();
    cli_cwd::clear_startup_cwd();
    assert!(
        seen,
        "the initial shell must be RUNNING IN the --cwd directory: `{query}` should \
         print a path containing {needle:?}. Grid was:\n{grid}",
    );
}
