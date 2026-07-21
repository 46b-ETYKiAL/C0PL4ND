//! Headless **interaction** tests for the two C0PL4ND settings sections the
//! existing `egui_settings` suite never visits: **Motion** (the largest single
//! block of `settings.rs` — the master animation switch, the CRT/mesh/VHS
//! effect gating, and the chromatic-aberration first-enable seed) and
//! **Privacy** (history capture, incognito, clear-now), driven by
//! `egui_kittest`.
//!
//! ## Discipline (non-negotiable)
//!
//! Every test drives the **real** production frame loop
//! ([`C0pl4ndApp::frame_tick`] → `settings::show`) and operates the **real**
//! widgets by their accessible label, then asserts an **observable** outcome the
//! interaction actually caused. There is NO set-state-then-assert-the-same-state
//! tautology and NO test-only mirror of the settings UI.
//!
//! Two observation channels are used, both reading state DERIVED from the live
//! config each frame (never a test mirror):
//!
//! 1. **The accessibility tree** — a widget's `numeric_value` / `is_disabled` is
//!    rebuilt from `config.effects.*` on every frame, so asserting on it observes
//!    the live config. This is the only channel available for the Motion section:
//!    the `config_*` accessors live in `app_config.rs`, and no accessor exists for
//!    `config.effects.*`. It is the same channel the existing suite's
//!    `mode_off_disables_the_channel_combo` / `picking_an_update_channel_changes_
//!    the_combo_value` tests use.
//! 2. **The host's public accessors** (`is_incognito`, `command_history_entries`)
//!    for Privacy, which drives the full production chain: widget → `Outcome` →
//!    host handler → accessor.
//!
//! ## Why several tests drive the SEARCH box to isolate a row
//!
//! The Motion section renders ~14 sliders. Targeting one by
//! `get_all_by_role(Slider).nth(N)` would silently re-target if a row were added
//! above it — the test would keep passing while asserting about the wrong widget.
//! Instead these tests type the row's canonical label into the REAL search box,
//! which filters to that single row (`row_visible`), making "the only slider on
//! screen" a deterministic, refactor-proof handle. That also exercises the real
//! cross-category search path.
//!
//! ## PTY dependency
//!
//! The three history tests need a live shell: they seed the command history
//! through the REAL echo-gated type-then-Enter capture path (the same path the
//! palette/window-mgmt suites seed with), so `wait_for_echo` blocks on the
//! shell's real echo before pressing Enter. Feeding synthetic rows is
//! deliberately NOT used here, so the Windows-banner screen-reset race that
//! `ensure_focused_spawned` guards does not apply: waiting for the echo of the
//! typed line is itself proof the shell is up and responsive.

use c0pl4nd::egui_app;
use std::cell::RefCell;
use std::time::{Duration, Instant};

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;

use egui_app::C0pl4ndApp;

/// The intensity `settings.rs` seeds on the FIRST enable of chromatic
/// aberration. Read from the production constant (not a hardcoded copy) so this
/// suite tracks the real default rather than pinning a stale literal.
const SEEDED_CHROMATIC: f32 = c0pl4nd_core::config::DEFAULT_CHROMATIC_INTENSITY;

/// Build a headless harness driving the REAL `frame_tick` for a shared app, with
/// a generous screen so the settings window fits fully.
fn harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
    #[allow(deprecated)]
    let mut h = Harness::new(move |ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));
    h.run();
    h
}

/// Open the settings window by clicking the real gear caption button.
fn open_settings(h: &mut Harness<'_>) {
    h.get_by_label("settings").click();
    h.run();
}

/// Click a left-nav category by its label. The nav item is a selectable (role
/// `Button`); querying by role disambiguates it from the section `heading` of the
/// same name (role `Label`).
fn select_category(h: &mut Harness<'_>, category: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::Button, category)
        .click();
    h.run();
}

/// Type `query` into the REAL settings search box, filtering the pane to the
/// matching rows. The search box is the first `TextInput` in the pane (the
/// Motion/Privacy views render no text field of their own, so it is the only
/// one). Node borrows are scoped so they end before each `h.run()`.
fn search_for(h: &mut Harness<'_>, query: &str) {
    {
        let search = h
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the settings pane must render a search box");
        search.focus();
    }
    h.run();
    {
        let search = h
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the search box must still be present after focus");
        search.type_text(query);
    }
    h.run();
}

/// The `(value, is_disabled)` of the slider whose accessible label is `label`.
///
/// egui builds a `Slider`'s accessible label from its `.text(…)`
/// (`WidgetInfo::slider(enabled, value, self.text.text())`), and rebuilds its
/// numeric value + enabled-ness from the LIVE config every frame. So targeting by
/// label — rather than `get_all_by_role(Slider).nth(N)` — both observes real
/// state and cannot silently re-target if a row is inserted above it.
fn slider_state(h: &mut Harness<'_>, label: &str) -> (f64, bool) {
    let node = h
        .get_by_role_and_label(egui::accesskit::Role::Slider, label)
        .accesskit_node();
    (
        node.numeric_value()
            .expect("an egui Slider exposes a numeric value"),
        node.is_disabled(),
    )
}

// ---- Motion: chromatic-aberration first-enable seed --------------------------

#[test]
fn first_enabling_chromatic_aberration_seeds_a_visible_intensity() {
    // The stored intensity is 0.0 while the effect has never been enabled, so a
    // user who ticked the checkbox saw the effect switch on at no strength and
    // read the control as broken. `settings.rs` seeds `DEFAULT_CHROMATIC_INTENSITY`
    // on the FIRST enable, but only while the stored intensity is still at the
    // floor, so a user's own value is never clobbered (the sibling test below).
    //
    // This test FOUND A REAL BUG: the seed was guarded on `<= 0.0`, which is
    // unreachable. egui's Slider (`SliderClamping::Always`) clamps its bound value
    // into the slider's range and writes it back ON EVERY RENDER, so simply
    // drawing the Motion page rewrote the 0.0 sentinel to the range floor (0.1)
    // BEFORE the user ever ticked the box. The seed never fired and the effect
    // enabled at the FAINTEST setting — the very symptom the seed exists to cure.
    // The guard is now `<= CHROMATIC_MIN`; this test is its regression gate.
    //
    // It drives the REAL checkbox and observes the REAL intensity slider's
    // accessible numeric value — which `settings.rs` rebuilds from
    // `config.effects.chromatic_aberration` every frame. Not a tautology: nothing
    // in the test writes the intensity; only the production seed can move it to
    // the seeded default.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Motion");
    // Isolate the chromatic row: its canonical label is unique to the Motion
    // section, so the pane filters down to that one row (checkbox + slider + ↺).
    // This also disambiguates the slider's label — "intensity" is shared with the
    // VHS row, which this filter leaves off screen.
    search_for(&mut h, "chromatic aberration");

    // Precondition, read from the LIVE widget: the effect is off, so its slider is
    // disabled (gated on `ca_on`) and shows no user-chosen intensity.
    //
    // NOTE the displayed value is NOT the stored 0.0: egui's Slider clamps to its
    // 0.1..=1.5 range, so the widget reads 0.1. That clamp is exactly what this
    // test is about (see the assertion below).
    let (before, disabled_before) = slider_state(&mut h, "intensity");
    assert!(
        disabled_before,
        "precondition: the intensity slider is disabled while the effect is off"
    );
    assert!(
        (before - f64::from(SEEDED_CHROMATIC)).abs() > 1e-6,
        "precondition: the intensity does not already sit at the seeded default, \
         or the assertion below would pass vacuously (got {before})"
    );

    // Tick the REAL checkbox.
    h.get_by_label("Chromatic aberration").click();
    h.run();

    let (after, disabled_after) = slider_state(&mut h, "intensity");
    assert!(
        (after - f64::from(SEEDED_CHROMATIC)).abs() < 1e-6,
        "first-enabling chromatic aberration must seed the intensity to the \
         visible default {SEEDED_CHROMATIC} (got {after}) — otherwise the effect \
         switches on at 0 strength and reads as broken"
    );
    assert!(
        !disabled_after,
        "enabling the effect must ENABLE its intensity slider"
    );
}

#[test]
fn re_enabling_chromatic_aberration_keeps_the_users_own_intensity() {
    // The seed is guarded by `chromatic_aberration <= CHROMATIC_MIN` so it only
    // ever fills a value still sitting at the slider's floor. A user who set their
    // own intensity, turned the effect off, and turned it back on must get THEIR
    // value back — not the default stomped over it.
    //
    // This is the negative half of the seed contract and it is what keeps the
    // guard honest: it pins the UPPER edge of the seed condition, so widening the
    // guard (e.g. to `<= CHROMATIC_MAX`, which would make the first-enable test
    // pass by seeding unconditionally) fails HERE. The two tests only pass
    // together for a guard that fires at the floor and nowhere else.
    let mut config = c0pl4nd_core::Config::default();
    // A distinct, non-default intensity the seed must never overwrite.
    config.effects.chromatic_aberration = 1.4;
    config.effects.chromatic_aberration_enabled = false;
    let app = RefCell::new(C0pl4ndApp::bootstrap_with(config));
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Motion");
    search_for(&mut h, "chromatic aberration");

    let (before, _) = slider_state(&mut h, "intensity");
    assert!(
        (before - 1.4).abs() < 1e-6,
        "precondition: the user's stored intensity is 1.4 (got {before})"
    );

    h.get_by_label("Chromatic aberration").click();
    h.run();

    let (after, disabled) = slider_state(&mut h, "intensity");
    assert!(
        (after - 1.4).abs() < 1e-6,
        "re-enabling must PRESERVE the user's own intensity 1.4, not reseed the \
         default {SEEDED_CHROMATIC} (got {after})"
    );
    assert!(!disabled, "the slider is enabled again with the effect on");
}

// ---- Motion: the master switch gates every effect ----------------------------

#[test]
fn the_master_animation_switch_gates_the_effect_rows() {
    // Every Motion effect is rendered `add_enabled(on, …)` where `on` is the LIVE
    // `config.effects.animations_enabled`. Turning the master switch off must
    // therefore disable the effect controls — the "fully static UI" contract in
    // the section's help text.
    //
    // Observed via the a11y tree's enabled-ness, which is derived from the live
    // config each frame (the same channel `mode_off_disables_the_channel_combo`
    // uses). Not a tautology: the test never writes the enabled-ness it asserts.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Motion");

    // The master switch defaults ON, so the CRT scan-lines checkbox is enabled.
    assert!(
        !h.get_by_label("CRT scan lines")
            .accesskit_node()
            .is_disabled(),
        "precondition: with animations enabled (the default) the CRT scan-lines \
         checkbox is operable"
    );

    // Flip the REAL master switch off.
    h.get_by_label("Enable animations").click();
    h.run();

    assert!(
        h.get_by_label("CRT scan lines")
            .accesskit_node()
            .is_disabled(),
        "turning the master animation switch OFF must disable the effect \
         controls below it (the fully-static-UI contract)"
    );
    assert!(
        h.get_by_label("Node-mesh background")
            .accesskit_node()
            .is_disabled(),
        "the master switch must gate the node-mesh toggle too, not just the CRT row"
    );

    // And back ON — proving the gate tracks BOTH directions, not a one-way latch.
    h.get_by_label("Enable animations").click();
    h.run();
    assert!(
        !h.get_by_label("CRT scan lines")
            .accesskit_node()
            .is_disabled(),
        "turning the master switch back ON must re-enable the effect controls"
    );
}

#[test]
fn scanline_darkness_is_gated_behind_the_scanlines_toggle() {
    // The Scanline-darkness slider is `add_enabled(on && crt_scanlines, …)`: it is
    // inert until scan lines are switched on ("Enable scan lines first." in its
    // hover). Drives the REAL checkbox and observes the REAL slider's enabled-ness.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Motion");
    // "scanline" matches the "crt scanlines" row AND the "scanline darkness" row,
    // so ONE filter shows the checkbox and the slider it gates together. (The
    // query lives in ctx temp-data and persists across frames, so re-searching
    // mid-test would APPEND to it — a single query avoids that trap entirely.)
    search_for(&mut h, "scanline");

    // With `crt_scanlines` off by default the darkness slider is disabled.
    let (_, disabled_before) = slider_state(&mut h, "darkness");
    assert!(
        disabled_before,
        "precondition: scanline darkness is disabled while scan lines are off"
    );

    h.get_by_label("CRT scan lines").click();
    // `step`, not `run`: with scan lines ON the app animates their drift and so
    // requests a repaint every frame, which makes `Harness::run` (which loops
    // until the UI settles, capped at 4 steps) panic on a UI that never settles.
    // Two steps: one to process the click, one to rebuild the a11y tree from the
    // new config.
    h.step();
    h.step();

    let (_, disabled_after) = slider_state(&mut h, "darkness");
    assert!(
        !disabled_after,
        "switching CRT scan lines ON must enable the scanline-darkness slider"
    );
}

// ---- Motion: mesh colour override / follow-theme -----------------------------

#[test]
fn the_mesh_colour_row_offers_reset_to_theme_only_once_overridden() {
    // `effects.mesh_color` is `None` by default → the mesh follows the theme
    // accent and the row shows a muted "following theme" note. Once a colour is
    // pinned, the row instead offers "Reset to theme", whose click clears the
    // override back to None.
    //
    // The presence/absence of that button is derived from the live
    // `config.effects.mesh_color.is_some()` each frame, so it is an honest
    // observable. We start from a config that already pins an override (the
    // colour-picker popup itself is not driveable headlessly — see the note in
    // the "remains untestable" report), and assert the button appears, that
    // clicking it clears the override, and that the "following theme" note
    // returns — the full round trip.
    let mut config = c0pl4nd_core::Config::default();
    config.effects.wired_ambient = true; // the row is gated on the mesh being on
    config.effects.mesh_color = Some([10, 200, 30]);
    let app = RefCell::new(C0pl4ndApp::bootstrap_with(config));
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Motion");
    search_for(&mut h, "mesh color");

    // Overridden → the revert button renders and the note does not.
    assert!(
        h.query_by_label("Reset to theme").is_some(),
        "a pinned mesh colour must offer the 'Reset to theme' revert"
    );
    assert!(
        h.query_by_label("following theme").is_none(),
        "a pinned mesh colour must NOT claim it is following the theme"
    );

    h.get_by_label("Reset to theme").click();
    h.run();

    // Cleared → the button is gone and the muted note is back.
    assert!(
        h.query_by_label("Reset to theme").is_none(),
        "clicking 'Reset to theme' must clear the override, retiring the button"
    );
    assert!(
        h.query_by_label("following theme").is_some(),
        "with the override cleared the row must report it follows the theme accent"
    );
}

#[test]
fn the_mesh_rows_are_gated_behind_the_node_mesh_toggle() {
    // Mesh density/brightness/drift are `add_enabled(on && wired_ambient, …)`:
    // inert until the node-mesh background is switched on. Drives the REAL
    // checkbox and observes the REAL density slider's enabled-ness.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Motion");
    // "mesh" matches the "wired mesh background" row AND the "mesh density" row,
    // so ONE filter shows the checkbox and the slider it gates together.
    search_for(&mut h, "mesh");

    let (_, disabled_before) = slider_state(&mut h, "density");
    assert!(
        disabled_before,
        "precondition: mesh density is disabled while the node mesh is off"
    );

    h.get_by_label("Node-mesh background").click();
    h.run();

    let (_, disabled_after) = slider_state(&mut h, "density");
    assert!(
        !disabled_after,
        "switching the node-mesh background ON must enable the mesh-density slider"
    );
}

// ---- Privacy: history capture, incognito, clear-now --------------------------

/// Type a string into the focused pane (one `Event::Text` per char), then step —
/// the real capture path the command history records from.
fn type_text(h: &mut Harness<'_>, s: &str) {
    for ch in s.chars() {
        h.event(egui::Event::Text(ch.to_string()));
    }
    h.step();
}

/// How long to wait for the real shell to echo a typed line.
///
/// Generous on purpose: the wait ends the instant the echo lands, so a large bound
/// costs a healthy run NOTHING. It only decides how much scheduler starvation is
/// tolerated before the suite declares the shell dead. Measured: at 10s this suite
/// failed 2/10 under ~27 concurrent rustc processes, with no product defect — a
/// flake that would make the loop's K-consecutive-clean fixed point unreachable.
/// The assertions are unchanged and a shell that never echoes still fails; it just
/// waits longer before saying so.
const ECHO_TIMEOUT: Duration = Duration::from_secs(45);

/// Poll until the focused pane's grid shows `needle` (the shell echoed it), so
/// the echo-gated history capture records the line deterministically. Waiting on
/// the REAL echo (not a fixed sleep) is also what proves the shell is up, so no
/// separate spawn/banner wait is needed for the type-then-Enter path.
fn wait_for_echo(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>, needle: &str) {
    let deadline = Instant::now() + ECHO_TIMEOUT;
    while Instant::now() < deadline {
        h.step();
        if app
            .borrow()
            .focused_grid_text()
            .is_some_and(|t| t.contains(needle))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!(
        "the shell never echoed {needle:?} within {ECHO_TIMEOUT:?} — cannot seed \
         the command history (the shell is not up, not a product bug)"
    );
}

/// Type-then-Enter a command into the focused pane (records it in the history).
fn run_line(h: &mut Harness<'_>, app: &RefCell<C0pl4ndApp>, line: &str) {
    type_text(h, line);
    wait_for_echo(h, app, line);
    h.key_press(egui::Key::Enter);
    h.step();
}

#[test]
fn turning_off_history_capture_stops_recording_commands() {
    // The Privacy "Record command history" toggle is the no-history posture. It
    // must reach the REAL capture gate (`should_record_history`), not merely store
    // a config bool nothing reads — the dead-setting bug class.
    //
    // End-to-end through the whole production chain: the REAL checkbox →
    // `config.history_capture_enabled` → `should_record_history` → the observable
    // `command_history_entries()`. Not a tautology: a command run AFTER the toggle
    // must be absent while one run BEFORE it is still present (proving the
    // recorder was working and the toggle is what stopped it — not a broken
    // seeding path).
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    // Seed one command with capture ON (the default) — proves capture works here.
    run_line(&mut h, &app, "echo before");
    assert!(
        app.borrow()
            .command_history_entries()
            .iter()
            .any(|e| e == "echo before"),
        "precondition: with capture on (the default) a run command IS recorded"
    );

    // Turn capture off via the REAL Privacy checkbox, then close settings so the
    // keystrokes below go to the pane.
    open_settings(&mut h);
    select_category(&mut h, "Privacy");
    h.get_by_label("Record command history").click();
    h.run();
    h.get_by_label("close settings").click();
    h.run();

    run_line(&mut h, &app, "echo after");

    let entries = app.borrow().command_history_entries();
    assert!(
        !entries.iter().any(|e| e == "echo after"),
        "with history capture switched OFF a newly-run command must NOT be \
         recorded (history: {entries:?})"
    );
    assert!(
        entries.iter().any(|e| e == "echo before"),
        "switching capture off must not retroactively erase what was already \
         recorded — that is the separate 'Clear now' action (history: {entries:?})"
    );
}

#[test]
fn the_incognito_toggle_stops_capture_and_clears_what_was_recorded() {
    // "No history this session" is RUNTIME state (never persisted): the checkbox
    // reports through `Outcome::set_incognito` to the host's `set_incognito`,
    // which both flips the capture gate AND clears what is already recorded ("a
    // clean break", per its doc).
    //
    // End-to-end: REAL checkbox → Outcome → host → the observable `is_incognito()`
    // + `command_history_entries()`.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    run_line(&mut h, &app, "echo secret");
    assert!(
        !app.borrow().command_history_entries().is_empty(),
        "precondition: the history is populated before going incognito"
    );
    assert!(
        !app.borrow().is_incognito(),
        "precondition: a session starts non-incognito"
    );

    open_settings(&mut h);
    select_category(&mut h, "Privacy");
    h.get_by_label("No history this session").click();
    h.run();

    assert!(
        app.borrow().is_incognito(),
        "ticking 'No history this session' must put the live session in incognito"
    );
    assert!(
        app.borrow().command_history_entries().is_empty(),
        "entering incognito must CLEAR the already-recorded history (the clean \
         break its contract promises), not just stop future capture"
    );
}

#[test]
fn privacy_clear_now_erases_the_recorded_history() {
    // The Privacy → "Clear now" button reports through `Outcome::clear_history` to
    // the host's `clear_command_history`. End-to-end: REAL button → Outcome → host
    // → the observable `command_history_entries()`.
    //
    // NOTE: the sibling "Clear saved state" button is deliberately NOT clicked by
    // any test here — it calls `clear_saved_ui_state`, which deletes the REAL
    // `%APPDATA%\c0pl4nd\app.ron`. Driving it would mutate real user state.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    run_line(&mut h, &app, "echo keepme");
    assert!(
        app.borrow()
            .command_history_entries()
            .iter()
            .any(|e| e == "echo keepme"),
        "precondition: the command was recorded so there is something to clear"
    );

    open_settings(&mut h);
    select_category(&mut h, "Privacy");
    h.get_by_label("Clear now").click();
    h.run();

    assert!(
        app.borrow().command_history_entries().is_empty(),
        "clicking Privacy → 'Clear now' must erase every recorded command"
    );
    assert!(
        !app.borrow().is_incognito(),
        "'Clear now' is a one-shot erase — it must NOT silently flip the session \
         into incognito (that is the separate toggle)"
    );
}

// ---- settings search reaches the nav-less Config section ----------------------

#[test]
fn the_config_section_is_reachable_from_the_nav_and_by_search() {
    // The Config section (config-file path + "Open config folder") is rendered by
    // `render_sections` under `section_visible(sel, q, "Config", …)`.
    //
    // This test previously pinned a BUG as intended behaviour: `"Config"` was
    // missing from `CATEGORIES`, so no left-nav item could ever select it and
    // search was the section's only door — a fully built page the user could not
    // click to. The old test even carried the instruction "if one was added, this
    // test should be updated to select it directly". The category has now been
    // added, so this asserts the corrected behaviour: reachable BOTH ways.
    //
    // The section's buttons are NOT clicked: "Open config folder" calls
    // `reveal_in_file_manager`, which spawns a real `explorer` process, and
    // `create_dir_all`s the real config dir.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    open_settings(&mut h);

    // A left-nav item labelled "Config" now exists...
    assert!(
        h.query_by_role_and_label(egui::accesskit::Role::Button, "Config")
            .is_some(),
        "the Config section must have a left-nav item — without one the page is \
         built but unclickable, which is indistinguishable from a missing feature"
    );

    // ...and selecting it reveals the section heading (role Label).
    select_category(&mut h, "Config");
    assert!(
        h.query_by_role_and_label(egui::accesskit::Role::Label, "Config")
            .is_some(),
        "selecting the Config nav item must show the Config section"
    );

    // Search remains a valid second door — the nav entry must not have replaced it.
    search_for(&mut h, "config");
    assert!(
        h.query_by_role_and_label(egui::accesskit::Role::Label, "Config")
            .is_some(),
        "searching 'config' must still reveal the Config section"
    );
}
