//! Headless WIRING tests for config HOT RELOAD: editing `config.toml` while the
//! app is running takes effect live, with no relaunch.
//!
//! ## Discipline
//!
//! Every test here drives the REAL production frame loop
//! ([`C0pl4ndApp::frame_tick`]) through `egui_kittest` and asserts state the app
//! only reaches by actually reloading and APPLYING the file. Nothing calls
//! `config_hot_reload_tick` directly — if the call is ever removed from
//! `frame_tick`, `config_toml_edit_is_picked_up_live` fails, because
//! `app.config` and `app.theme` are the app's own live state and stepping frames
//! is the only thing driving them here.
//!
//! Each test writes into its OWN temp dir and repoints the watcher there with
//! [`C0pl4ndApp::watch_config_at`], so no test ever reads or writes the user's
//! real `%APPDATA%\c0pl4nd\config.toml`.

use c0pl4nd::egui_app;
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

use egui_app::C0pl4ndApp;

/// The watcher stats the file at most once per 400 ms, so a hot reload is not
/// expected on the very next frame. Everything here polls up to this long.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(6);

fn harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
    #[allow(deprecated)]
    let mut h = Harness::new(move |ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1000.0, 700.0));
    h.run();
    h
}

/// A private temp dir for one test, named so concurrent test binaries and
/// repeated runs never collide.
fn temp_config_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "c0pl4nd_hotreload_{tag}_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Step frames (with small sleeps, since the watcher's throttle is wall-clock)
/// until `pred` holds or the timeout expires. Returns whether it held.
fn step_until(
    h: &mut Harness<'_>,
    app: &RefCell<C0pl4ndApp>,
    timeout: Duration,
    pred: impl Fn(&C0pl4ndApp) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        h.step();
        if pred(&app.borrow()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    pred(&app.borrow())
}

/// THE WIRE TEST. An external edit to `config.toml` reaches the running app:
/// the live `Config` carries the new values AND the change is APPLIED (the
/// terminal `Theme` — which is what the PTY grid draws its glyphs from — swaps
/// to the newly-named theme).
///
/// Asserting the applied `theme` as well as the parsed `config` is deliberate:
/// a reload that swapped `self.config` but never called `apply_config_live`
/// would leave the panes rendering the OLD colours, which is a half-wire that a
/// config-only assertion would pass.
///
/// This test fails if the `config_hot_reload_tick` call is removed from
/// `frame_tick`: stepping frames is the ONLY thing that can change this app's
/// config, and the file is edited after the app was built.
#[test]
fn config_toml_edit_is_picked_up_live() {
    let dir = temp_config_dir("live");
    let path = dir.join("config.toml");
    // Start from a file that matches the app's starting state, so the ONLY
    // change the app can observe is the edit made below.
    std::fs::write(&path, "theme = \"itasha-corp\"\n").unwrap();

    let app = RefCell::new(C0pl4ndApp::bootstrap());
    app.borrow_mut().watch_config_at(path.clone());
    let mut h = harness(&app);

    assert_eq!(
        app.borrow().config.theme,
        "itasha-corp",
        "precondition: the app starts on the default theme"
    );
    let before_theme = app.borrow().theme.name.clone();

    // The external edit — exactly what a user typing in an editor produces.
    std::fs::write(
        &path,
        "theme = \"ghost-paper\"\nalways_on_top = true\nopacity = 0.75\n",
    )
    .unwrap();

    assert!(
        step_until(&mut h, &app, RELOAD_TIMEOUT, |a| a.config.theme
            == "ghost-paper"),
        "an external config.toml edit must reach the running app's live Config"
    );
    assert!(
        app.borrow().config.always_on_top,
        "every edited field is reloaded, not just the theme"
    );
    assert!(
        (app.borrow().config.opacity - 0.75).abs() < f32::EPSILON,
        "the reloaded opacity must be the value written to the file"
    );

    // APPLIED, not merely parsed: the terminal theme the grid draws from must
    // have swapped too.
    let after_theme = app.borrow().theme.name.clone();
    assert_ne!(
        after_theme, before_theme,
        "the reload must APPLY the theme (reloading Config alone leaves the \
         panes drawing the old colours)"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A config file that fails to PARSE must never clobber the running settings.
/// The user keeps working with what they had; a toast reports the problem.
///
/// This is the load-bearing half: reverting a live session to defaults because
/// someone saved a half-typed TOML line would be strictly worse than ignoring
/// the file until it parses again.
#[test]
fn an_unparseable_edit_keeps_the_running_config() {
    let dir = temp_config_dir("badparse");
    let path = dir.join("config.toml");
    std::fs::write(&path, "theme = \"itasha-corp\"\n").unwrap();

    // Start from a NON-default in-memory config so "kept" is distinguishable
    // from "reset to defaults".
    let config = c0pl4nd_core::Config {
        always_on_top: true,
        ..Default::default()
    };
    let app = RefCell::new(C0pl4ndApp::bootstrap_with(config));
    app.borrow_mut().watch_config_at(path.clone());
    let mut h = harness(&app);

    std::fs::write(&path, "theme = = = not valid toml\n").unwrap();

    // Wait for the app to NOTICE the change (the toast is the observable that
    // proves the reload path ran at all — without it this test would pass even
    // if hot reload were entirely absent).
    assert!(
        step_until(&mut h, &app, RELOAD_TIMEOUT, |a| a
            .toast_text()
            .is_some_and(|t| t.contains("couldn't be read"))),
        "an unparseable edit must surface a toast"
    );
    assert!(
        app.borrow().config.always_on_top,
        "the running config must survive an unparseable edit unchanged"
    );
    assert_eq!(
        app.borrow().config.theme,
        c0pl4nd_core::Config::default().theme,
        "nothing from the broken file may be partially applied"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A file that is TOUCHED but semantically unchanged must not churn the app.
/// Re-applying the theme + visuals on every save of an unmodified file would be
/// a visible flicker for no change, so the reload is short-circuited on an
/// equal `Config`.
#[test]
fn a_touched_but_equivalent_file_does_not_announce_a_reload() {
    let dir = temp_config_dir("equiv");
    let path = dir.join("config.toml");
    let body = c0pl4nd_core::Config::default().to_toml().unwrap();
    std::fs::write(&path, &body).unwrap();

    let app = RefCell::new(C0pl4ndApp::bootstrap());
    app.borrow_mut().watch_config_at(path.clone());
    let mut h = harness(&app);

    // Same settings, different bytes (a trailing comment) — a real "touch".
    std::fs::write(&path, format!("{body}\n# just a comment\n")).unwrap();

    let announced = step_until(&mut h, &app, Duration::from_secs(3), |a| {
        a.toast_text().is_some_and(|t| t.contains("Reloaded"))
    });
    assert!(
        !announced,
        "a touched-but-equivalent config must not announce a reload"
    );

    std::fs::remove_dir_all(&dir).ok();
}
