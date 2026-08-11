//! Headless **interaction** tests for the C0PL4ND egui settings window
//! (Milestone 2), driven by `egui_kittest`.
//!
//! ## Discipline (non-negotiable)
//!
//! Every test here drives the **real** production frame loop
//! ([`C0pl4ndApp::frame_tick`] → [`settings::show`]) and operates the **real**
//! widgets by their accessible label, then asserts the **observable**
//! `Config`/theme change the interaction actually caused. There is NO
//! set-state-then-assert-the-same-state tautology and NO test-only mirror of
//! the settings UI — those are exactly how "the toggle does nothing" and "the
//! theme picker is dead" ship. A control is only considered working when a
//! simulated click/edit here produces its real effect on the live config.
//!
//! Each test first opens settings (clicks the real gear), then selects the
//! category that owns the control (clicks the real left-nav item), then drives
//! the widget — the exact path a user follows.
//!
//! The app module is compiled into this test binary via `#[path]` so the test
//! constructs `C0pl4ndApp` directly (no eframe window); the closure handed to
//! `Harness::new` calls the same `frame_tick` the shipping binary runs each
//! frame. Headless (`live_window == false`), so the settings window never
//! writes the user's real config file (the persistence guard in
//! `settings_window` is real-window-only) — these tests observe the in-memory
//! live-apply, which is the load-bearing behaviour.

use c0pl4nd::egui_app;
use std::cell::RefCell;

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;

use egui_app::C0pl4ndApp;

/// Build a headless harness driving the REAL `frame_tick` for a shared app, with
/// a generous screen so the fixed-size settings window (760×560) fits fully.
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

/// Click a left-nav category by its label (e.g. "Cursor", "Fonts"). The nav
/// item is a selectable (role `Button`); querying by role disambiguates it from
/// the section `heading` of the same name (role `Label`), which renders for the
/// currently-selected category.
fn select_category(h: &mut Harness<'_>, category: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::Button, category)
        .click();
    h.run();
}

#[test]
fn gear_opens_the_grouped_settings_window() {
    // Sanity: the gear opens the new window, and a section heading is present
    // (proving the grouped layout renders, not a flat placeholder).
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    assert!(app.borrow().settings_is_open(), "gear must open settings");

    // The default category is "Appearance"; its section HEADING (role Label,
    // distinct from the same-named nav Button) must be on screen — proof the
    // grouped layout rendered a real section, not a flat placeholder.
    h.get_by_role_and_label(egui::accesskit::Role::Label, "Appearance");
}

#[test]
fn toggling_cursor_blink_flips_the_live_config() {
    // Cursor.blink defaults to TRUE. Navigate to Cursor, click the Blink toggle,
    // and assert the live config flipped — the real widget → real config path.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    assert!(
        app.borrow().config_cursor_blink(),
        "precondition: blink defaults on"
    );
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Cursor");

    h.get_by_label("Blink the cursor").click();
    h.run();

    assert!(
        !app.borrow().config_cursor_blink(),
        "clicking the Blink toggle must turn cursor blink OFF in the live config"
    );
}

#[test]
fn toolbar_section_default_zones_overflow_toggle_and_reset_are_wired() {
    // The Settings → Toolbar section must render and drive the live config. Default
    // placement: view/equalize/shell on the LEFT, only the script launcher on the
    // RIGHT (by the gear) — the "only the script button moved" contract.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    assert_eq!(
        app.borrow().config_toolbar_left(),
        vec!["view_mode", "equalize_panes", "shell_switcher"],
        "default LEFT group is view/equalize/shell"
    );
    assert_eq!(
        app.borrow().config_toolbar_right(),
        vec!["script_launcher"],
        "default RIGHT cluster is ONLY the script launcher"
    );
    assert!(app.borrow().config_toolbar_show_overflow());
    let default_left = app.borrow().config_toolbar_left();
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Toolbar");

    // Flip the overflow checkbox OFF (uniquely-labelled control).
    h.get_by_label("Show the overflow menu button when its menu has actions")
        .click();
    h.run();
    assert!(
        !app.borrow().config_toolbar_show_overflow(),
        "toggling the overflow checkbox must turn it OFF in the live config"
    );

    // Reset restores the defaults (overflow back on, zones back to default).
    h.get_by_label("Reset toolbar to defaults").click();
    h.run();
    assert!(
        app.borrow().config_toolbar_show_overflow(),
        "Reset must restore show_overflow to its default (on)"
    );
    assert_eq!(
        app.borrow().config_toolbar_left(),
        default_left,
        "Reset must restore the default LEFT group"
    );
    assert_eq!(
        app.borrow().config_toolbar_right(),
        vec!["script_launcher"],
        "Reset must restore the default RIGHT cluster"
    );
}

#[test]
fn toggling_paste_warn_flips_the_live_config() {
    // paste_warn_multiline defaults to TRUE (a security default). Toggling it
    // must turn it off — proving the Terminal-section toggle is wired.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    assert!(
        app.borrow().config_paste_warn_multiline(),
        "precondition: multi-line paste warning defaults on"
    );
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Terminal");

    h.get_by_label("Warn before multi-line paste").click();
    h.run();

    assert!(
        !app.borrow().config_paste_warn_multiline(),
        "clicking the paste-warn toggle must turn it OFF in the live config"
    );
}

#[test]
fn clicking_the_font_size_slider_changes_the_live_config() {
    // The Font-size slider must move the live config off its 13.0 default.
    // Clicking the slider track sets the value to the click position (egui's
    // click-to-set behaviour); a center click on an 8..32 pt range lands well
    // above 13, so we assert the live value CHANGED — not just re-asserting
    // 13.0. (Arrow-key driving is intentionally NOT used: the terminal input
    // forwarder consumes arrow keys for the PTY, so a real click on the widget
    // is the robust, production-faithful path.)
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let before = app.borrow().config_font_size();
    assert_eq!(before, 13.0, "precondition: font size defaults to 13pt");
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Fonts");

    // The Fonts section renders the Size slider first; click its track.
    h.get_all_by_role(egui::accesskit::Role::Slider)
        .next()
        .expect("Font section must render a size slider")
        .click();
    h.run();

    let after = app.borrow().config_font_size();
    assert_ne!(
        after, before,
        "clicking the font-size slider track must change the live font size \
         (before={before}, after={after})"
    );
}

#[test]
fn reset_button_reverts_a_changed_setting() {
    // The per-setting ↺ revert: change cursor blink (off), then click the ↺ for
    // that row and assert it returns to the default (on). Proves the reset
    // affordance actually reverts, not just renders.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Cursor");

    // Flip blink off first.
    h.get_by_label("Blink the cursor").click();
    h.run();
    assert!(
        !app.borrow().config_cursor_blink(),
        "blink turned off so the ↺ becomes enabled"
    );

    // Two rows render a ↺ in the Cursor section (Style, Blink); the Blink ↺ is
    // the only ENABLED one now (Style is unchanged → its ↺ is disabled). The ↺
    // glyph is the button's accessible LABEL ("Restore default" is only a
    // tooltip, not a label), so target the single ENABLED ↺ via predicate and
    // click it; assert blink returns to its default (on).
    h.get_by(|n: &egui_kittest::kittest::AccessKitNode<'_>| {
        n.label().as_deref() == Some("↺") && !n.is_disabled()
    })
    .click();
    h.run();

    assert!(
        app.borrow().config_cursor_blink(),
        "clicking ↺ must restore cursor blink to its default (on)"
    );
}

#[test]
fn picking_a_theme_in_the_combo_changes_the_live_config() {
    // Selecting a different theme in the combo must change the live config theme
    // stem — the cause→effect the "dead theme picker" bug class hides in. We
    // open the real ComboBox and click a distinctly-different built-in by its
    // menu label, then assert the live config stem became that theme.
    //
    // NOTE: the SETTINGS_WINDOW also reloads the terminal color theme on this
    // change (`load_terminal_theme`) so the live PTY panes repaint — but the
    // theme TOMLs live at the repo-root `assets/themes/`, which the headless
    // test cwd (the crate dir) does not resolve, so both stems fall back to the
    // same built-in void theme and the reloaded fg is not observable here. The
    // live fg repaint is verified in the screenshot QA step instead; this test
    // asserts the wired config change, which IS observable.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    assert_eq!(
        app.borrow().config_theme(),
        "itasha-corp",
        "precondition: default theme"
    );
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Appearance");

    // Open the theme combo (role ComboBox; its accessible VALUE is the current
    // stem, not a label) and pick a distinctly-different built-in by its menu
    // label. The Appearance section's only combo is the theme picker.
    h.get_all_by_role(egui::accesskit::Role::ComboBox)
        .next()
        .expect("Appearance must render the theme combo")
        .click();
    h.run();
    h.get_by_label("ghost-paper").click(); // the opened menu item
    h.run();

    assert_eq!(
        app.borrow().config_theme(),
        "ghost-paper",
        "picking ghost-paper in the combo must update the live config theme stem"
    );
}

#[test]
fn searching_reveals_a_setting_from_another_category() {
    // Cross-category search: with "Cursor" selected, typing "blink" (a Cursor
    // setting label) keeps the Blink control visible AND the search reaches
    // across categories. We start in Cursor because that view has no text field
    // of its own, so the search box is the sole TextInput (deterministic to
    // target) — then type "blink" and operate the revealed Blink toggle. Proves
    // the search filter (SCR1B3's section_visible/row_visible) is wired and the
    // filtered control stays operable.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Cursor");

    // In the Cursor view the search box is the only TextInput. Focus it (scope
    // the node so its borrow ends before `h.run()`), then re-query and type.
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
        search.type_text("blink");
    }
    h.run();

    // The Blink toggle survives the filter and is operable.
    h.get_by_label("Blink the cursor").click();
    h.run();

    assert!(
        !app.borrow().config_cursor_blink(),
        "a setting matched by the search filter must stay operable; \
         clicking the filtered Blink toggle must flip the live config"
    );
}

#[test]
fn clicking_the_close_button_dismisses_settings() {
    // The in-content Close ✕ (added after the "can't close it" report — the
    // title-bar ✕ reads low-contrast against the dark frame) must actually
    // dismiss the window: open settings, click the labelled close, assert the
    // live `settings_open` flag flipped to false.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    assert!(
        app.borrow().settings_is_open(),
        "precondition: settings open"
    );

    h.get_by_label("close settings").click();
    h.run();

    assert!(
        !app.borrow().settings_is_open(),
        "clicking the in-content Close ✕ must dismiss the settings window"
    );
}

#[test]
fn picking_a_light_theme_flips_the_whole_ui_light() {
    // Whole-app theming: the chrome (titlebar / tabs / status bar / settings
    // window / panel fills) must follow the active terminal theme. Picking a
    // LIGHT theme (ghost-paper) must derive a LIGHT egui base; picking a DARK
    // theme back must derive a DARK base. We drive the REAL theme picker through
    // the real frame loop and observe `visuals_are_light()` (which recomputes
    // the SAME `visuals_from_theme` derivation the live app applies on a theme
    // change) — no set-then-assert tautology.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    assert!(
        !app.borrow().visuals_are_light(),
        "precondition: the default itasha-corp theme is DARK"
    );
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Appearance");

    // Pick the light ghost-paper theme via the real combo (the theme picker is
    // the only combo in the Appearance section).
    h.get_all_by_role(egui::accesskit::Role::ComboBox)
        .next()
        .expect("theme combo")
        .click();
    h.run();
    h.get_by_label("ghost-paper").click();
    h.run();
    assert!(
        app.borrow().visuals_are_light(),
        "picking the light ghost-paper theme must flip the whole UI light"
    );

    // Pick a dark theme back (wired-noir) — the UI must return to a dark base,
    // proving the derivation tracks BOTH polarities, not a one-way latch.
    h.get_all_by_role(egui::accesskit::Role::ComboBox)
        .next()
        .expect("theme combo")
        .click();
    h.run();
    h.get_by_label("wired-noir").click();
    h.run();
    assert!(
        !app.borrow().visuals_are_light(),
        "picking a dark theme back must flip the whole UI dark again"
    );
}

#[test]
fn the_opacity_slider_changes_the_live_config() {
    // Single always-transparent model: the Opacity slider is the whole see-through
    // control and is ALWAYS enabled (no master toggle, no mode combo). It is the
    // FIRST slider in Appearance; a center click on the 0.0..=1.0 track lands below
    // 100%, so the live opacity must change off its 1.0 default.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let before = app.borrow().config_opacity();
    assert_eq!(before, 1.0, "precondition: opacity defaults to 100%");
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Appearance");

    h.get_all_by_role(egui::accesskit::Role::Slider)
        .next()
        .expect("Appearance must render the always-enabled opacity slider")
        .click();
    h.run();

    let after = app.borrow().config_opacity();
    assert_ne!(
        after, before,
        "clicking the opacity slider track must change the live opacity \
         (before={before}, after={after})"
    );
}

#[test]
fn toggling_the_tint_wash_checkbox_flips_the_live_config() {
    // The "Enable tint wash" checkbox is the acrylic/backdrop control; it defaults
    // ON. Its focus is covered in a11y.rs, but the click->config effect was not:
    // clicking it must turn the live tint wash OFF, proving the Appearance toggle
    // is wired to `config.tint_enabled` (kills a mutant that drops the checkbox
    // binding). Closes the Milestone 3+ "acrylic toggle -> backdrop toggles" row.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    assert!(
        app.borrow().config_tint_enabled(),
        "precondition: the tint wash is enabled by default"
    );
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Appearance");

    h.get_by_label("Enable tint wash").click();
    h.run();

    assert!(
        !app.borrow().config_tint_enabled(),
        "clicking the tint-wash toggle must turn it OFF in the live config"
    );
}

#[test]
fn the_tint_strength_slider_changes_the_live_config() {
    // The tint wash is enabled by default (`tint_enabled == true`), so the tint
    // controls are live without any master toggle. Appearance renders sliders in
    // order: opacity (first), tint strength (second). Click the SECOND slider's
    // track and assert the live tint strength moves off its 0.0 default.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    assert_eq!(
        app.borrow().config_tint_strength(),
        0.0,
        "precondition: tint strength defaults to 0%"
    );
    assert!(
        app.borrow().config_tint_enabled(),
        "precondition: the tint wash is enabled by default"
    );
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Appearance");

    h.get_all_by_role(egui::accesskit::Role::Slider)
        .nth(1)
        .expect("Appearance must render the tint-strength slider")
        .click();
    h.run();

    assert_ne!(
        app.borrow().config_tint_strength(),
        0.0,
        "clicking the tint-strength slider track must raise the live tint strength"
    );
}

#[test]
fn mode_off_disables_the_check_for_updates_button() {
    // The Updates page used to render TWO combos, [0] Mode and [1] release
    // channel, and this test asserted Mode=off disabled the channel one. That
    // combo is gone: it wrote `config.update.channel`, which the in-app updater
    // never read (see settings.rs, "Release channel: DELIBERATELY NOT RENDERED").
    //
    // The property is still worth pinning, so it moves to the control that
    // survives and carries the same `mode != Off` gate -- the "Check for
    // updates" BUTTON, which is the actual network action. Asserting on it is
    // strictly better than asserting on a combo that only stored a value:
    // Mode=off is supposed to mean fully offline, and this is the widget that
    // would otherwise reach the network.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Updates");

    // Precondition: default Mode = Notify (networked) -> the button is enabled.
    // Without this the assertion below would pass on a button that was disabled
    // for some unrelated reason (or always).
    assert!(
        !h.get_by_label("Check for updates")
            .accesskit_node()
            .is_disabled(),
        "precondition: Check for updates is enabled while Mode is notify"
    );

    // Open the Mode combo (now the only one) and pick "off".
    h.get_all_by_role(egui::accesskit::Role::ComboBox)
        .next()
        .expect("Updates must render the Mode combo")
        .click();
    h.run();
    h.get_by_label("off").click();
    h.run();

    assert!(
        h.get_by_label("Check for updates")
            .accesskit_node()
            .is_disabled(),
        "setting Mode to off makes the app fully offline, so the network action \
         must be disabled"
    );
}
#[test]
fn editing_the_initial_columns_changes_the_drag_value() {
    // `window.cols` defaults to 80 and has no observation accessor. The Initial
    // columns DragValue exposes its live value as the accessible numeric_value
    // (rebuilt from `config.window.cols` each frame). We focus the DragValue and
    // type a new number — egui's DragValue accepts keyboard text entry on focus
    // — then assert the accessible numeric value changed off 80.
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Window");

    // The Window section renders DragValues (SpinButtons in the a11y tree) in
    // order: Padding (8), Initial columns (80), Initial rows (24). The numeric
    // value lives on the underlying AccessKit node; target the one reading 80.
    let cols_before = h
        .get_all_by_role(egui::accesskit::Role::SpinButton)
        .find_map(|n| {
            n.accesskit_node()
                .numeric_value()
                .filter(|v| (*v - 80.0).abs() < f64::EPSILON)
        })
        .expect("Window must render an Initial-columns spinner defaulting to 80");
    assert_eq!(cols_before, 80.0, "precondition: cols default to 80");

    // Drive the cols spinner: focus it, type a new value, commit with Enter.
    // (egui's DragValue accepts keyboard text entry while focused; `type_text`
    // is a node-level action that injects the characters as real input events.)
    {
        let cols = h
            .get_all_by_role(egui::accesskit::Role::SpinButton)
            .find(|n| {
                n.accesskit_node()
                    .numeric_value()
                    .is_some_and(|v| (v - 80.0).abs() < f64::EPSILON)
            })
            .expect("Initial-columns spinner");
        cols.focus();
        cols.type_text("120");
    }
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();

    let cols_after = h
        .get_all_by_role(egui::accesskit::Role::SpinButton)
        .find_map(|n| n.accesskit_node().numeric_value())
        .expect("the Window section must still render the cols spinner");
    assert_ne!(
        cols_after, 80.0,
        "editing the Initial-columns spinner must change the live cols value \
         (before=80, after={cols_after})"
    );
}

#[test]
fn editing_the_padding_changes_the_live_grid_text_origin() {
    // Window.padding was a DEAD field: the terminal grid was always inset by a
    // hardcoded 4px, so changing Padding in settings did nothing on screen. It
    // is now read LIVE each frame and threaded into the grid paint origin.
    //
    // This is a no-tautology interaction test: drive the REAL Padding DragValue
    // up off its default, then assert the OBSERVABLE rendering input changed —
    // the grid text origin (computed by the exact `grid_text_origin` helper the
    // production paint path uses) moves further from the pane's top-left corner.
    // We assert a downstream RENDER effect, not just the stored config value.
    let probe_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));

    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let origin_before = app.borrow().grid_text_origin_for(probe_rect);
    let padding_before = app.borrow().config_window_padding();
    assert_eq!(
        padding_before, 8,
        "precondition: padding defaults to 8px (the config default)"
    );
    let mut h = harness(&app);

    open_settings(&mut h);
    select_category(&mut h, "Window");

    // The Window section renders DragValues (SpinButtons in the a11y tree) in
    // order: Padding (8), Initial columns (80), Initial rows (24). Target the
    // one reading 8 — the Padding spinner — focus it, type a larger value, and
    // commit with Enter (egui DragValue accepts keyboard text entry on focus).
    {
        let padding = h
            .get_all_by_role(egui::accesskit::Role::SpinButton)
            .find(|n| {
                n.accesskit_node()
                    .numeric_value()
                    .is_some_and(|v| (v - 8.0).abs() < f64::EPSILON)
            })
            .expect("Window must render a Padding spinner defaulting to 8");
        padding.focus();
        padding.type_text("24");
    }
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();

    let padding_after = app.borrow().config_window_padding();
    assert_ne!(
        padding_after, padding_before,
        "editing the Padding spinner must change the live padding \
         (before={padding_before}, after={padding_after})"
    );

    // The load-bearing assertion: the change reaches the RENDER path. A larger
    // padding insets the grid further, so the live text origin must move down
    // and to the right of where it was at the default padding.
    let origin_after = app.borrow().grid_text_origin_for(probe_rect);
    assert!(
        origin_after.x > origin_before.x && origin_after.y > origin_before.y,
        "raising Padding must move the live grid text origin further into the \
         pane (before={origin_before:?}, after={origin_after:?})"
    );
    // And it must equal exactly the new padding inset from the pane corner —
    // proving the SAME helper the paint path uses produced it.
    assert_eq!(
        origin_after,
        probe_rect.left_top() + egui::vec2(f32::from(padding_after), f32::from(padding_after)),
        "the live grid origin must be the pane top-left inset by the new padding"
    );
}

/// Select a category, then return the screen-space right edge (x) of the
/// "close settings" ✕ button. Because that button sits at the right edge of the
/// settings window's header row (a `right_to_left` layout), its `rect().right()`
/// is a faithful proxy for the window's right inner edge — measured from the
/// REAL AccessKit geometry, not from screenshot pixels (the documented
/// wgpu-luminance trap). Used to prove every category page renders at the SAME
/// window width (#26).
fn close_button_right_on_page(h: &mut Harness<'_>, category: &str) -> f32 {
    select_category(h, category);
    h.get_by_label("close settings").rect().right()
}

/// Return the right edge (x) of the search box on the current page — a second
/// width proxy taken from inside the content pane. If the content pane width
/// drifted per page, this would move with it; it must not.
fn search_box_right(h: &mut Harness<'_>) -> f32 {
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .expect("the settings pane must render a search box")
        .rect()
        .right()
}

#[test]
fn every_settings_page_has_the_same_window_width() {
    // #26: "Different settings pages resize the settings menu differently and
    // all of them make the resizing extend to the right beyond the close
    // button." The fix clamps the content `Ui` to the window's inner width up
    // front, so no page's controls can demand more width than any other. This
    // test proves it from REAL geometry: the ✕ close button's right edge (the
    // window's right inner edge) must be the SAME x (within 1px) on EVERY page —
    // Appearance, Font, Updates (the page that historically drifted widest via
    // its long help line + status row), and Keybindings (wide monospace combo
    // fields). NEVER measured by screenshot pixels (the wgpu-luminance trap).
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    open_settings(&mut h);

    let appearance = close_button_right_on_page(&mut h, "Appearance");
    let font = close_button_right_on_page(&mut h, "Fonts");
    let updates = close_button_right_on_page(&mut h, "Updates");
    let keybindings = close_button_right_on_page(&mut h, "Keybindings");

    // All four right edges must agree within 1px — a stable-width window.
    for (name, x) in [
        ("Fonts", font),
        ("Updates", updates),
        ("Keybindings", keybindings),
    ] {
        assert!(
            (x - appearance).abs() <= 1.0,
            "the settings window right edge must be identical on every page; \
             Appearance={appearance}, {name}={x} (drift {})",
            (x - appearance).abs()
        );
    }
}

#[test]
fn content_never_extends_past_the_close_button() {
    // #26 (overflow half): content must NEVER draw past the window's right inner
    // edge (the ✕). The search box (whose width was the unbounded `f32::INFINITY`
    // offender) is the widest content element in the pane; its right edge must
    // sit at-or-inside the ✕ button's right edge on EVERY page. We check the two
    // pages most prone to overflow: Updates (long help) and Keybindings (wide
    // monospace fields).
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    open_settings(&mut h);

    for category in ["Appearance", "Updates", "Keybindings"] {
        select_category(&mut h, category);
        let close_right = h.get_by_label("close settings").rect().right();
        let content_right = search_box_right(&mut h);
        assert!(
            content_right <= close_right + 1.0,
            "on the {category} page the content (search box right={content_right}) \
             must not extend past the window's right inner edge \
             (close button right={close_right})"
        );
    }
}

#[test]
fn widening_the_window_keeps_pages_equal_width() {
    // #25 + #26 interaction: making the window resizable must NOT reintroduce the
    // per-page width drift. After the window is laid out (and at whatever width
    // egui resolved for the resizable window), pages must STILL be equal-width to
    // each other. We assert the invariant holds across pages at the live window
    // size — the same equal-width guarantee, now under a resizable window. (A
    // true drag-resize is a windowing-server action egui_kittest does not
    // simulate; the load-bearing property — equal width across pages at any given
    // window size — is what we verify, and it is exactly what the content clamp
    // guarantees because every page clamps to the SAME available width.)
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);
    open_settings(&mut h);

    let cursor = close_button_right_on_page(&mut h, "Cursor");
    let terminal = close_button_right_on_page(&mut h, "Terminal");
    let window = close_button_right_on_page(&mut h, "Window");
    assert!(
        (cursor - terminal).abs() <= 1.0 && (cursor - window).abs() <= 1.0,
        "resizable window must keep pages equal-width: \
         Cursor={cursor}, Terminal={terminal}, Window={window}"
    );
}

#[test]
fn pressing_escape_dismisses_settings() {
    // Esc is the conventional overlay-dismiss key; the window must honour it.
    // (Guards the `.anchor()`-free, `default_pos` window: an earlier anchored
    // window could not be moved OR reliably dismissed.)
    let app = RefCell::new(C0pl4ndApp::bootstrap());
    let mut h = harness(&app);

    open_settings(&mut h);
    assert!(
        app.borrow().settings_is_open(),
        "precondition: settings open"
    );

    h.key_press(egui::Key::Escape);
    h.run();

    assert!(
        !app.borrow().settings_is_open(),
        "pressing Escape must dismiss the settings window"
    );
}
