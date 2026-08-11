//! `C0pl4ndApp` config + theme observation accessors.
//!
//! Small getters over the live `config`/`theme` that the egui_kittest settings
//! tests use to assert observable changes after driving the real settings UI
//! through `frame_tick` — the same production path the app uses. Grouped out of
//! the `C0pl4ndApp` impl in the god-module; behaviour unchanged.

use super::theme;

impl super::C0pl4ndApp {
    /// The live toast text, if one is currently showing.
    ///
    /// `self.toast` is `pub(crate)`, so the in-crate unit tests can read it
    /// directly — but `crates/app/tests/` is a SEPARATE crate and cannot. The
    /// config-hot-reload test asserts on the user-visible reload notice, which
    /// is the only observable proof the reload actually applied, so it needs a
    /// public accessor rather than a `pub(crate)` field.
    #[allow(dead_code)]
    pub fn toast_text(&self) -> Option<&str> {
        self.toast.as_deref()
    }

    /// The current font size (pt) from the live config. Used by the settings
    /// slider interaction test.
    #[allow(dead_code)]
    pub fn config_font_size(&self) -> f32 {
        self.config.font.size
    }

    /// The current primary font family from the live config. Observation accessor
    /// for the Font-family dropdown interaction test.
    #[allow(dead_code)]
    pub fn config_font_family(&self) -> String {
        self.config.font.family.clone()
    }

    /// The current ordered fallback font families from the live config.
    /// Observation accessor for the Fallback dropdown interaction test.
    #[allow(dead_code)]
    pub fn config_font_fallback(&self) -> Vec<String> {
        self.config.font.fallback.clone()
    }

    /// The font-stack key (family + fallbacks) most recently INSTALLED into egui.
    /// Observation accessor for the live-apply interaction test: after a family
    /// change is driven through the real frame loop, this MUST reflect the new
    /// family (proving the font was actually re-installed, not just stored in
    /// config). Mirrors [`font_apply_key`].
    #[allow(dead_code)]
    pub fn applied_font_key(&self) -> String {
        self.applied_font_family.clone()
    }

    /// The current cursor blink flag from the live config.
    #[allow(dead_code)]
    pub fn config_cursor_blink(&self) -> bool {
        self.config.cursor.blink
    }

    /// The customizable toolbar's LEFT-group action ids from the live config.
    /// Observation accessor for the Settings → Toolbar interaction tests.
    #[allow(dead_code)]
    pub fn config_toolbar_left(&self) -> Vec<String> {
        self.config.toolbar.left.clone()
    }

    /// The customizable toolbar's RIGHT-cluster action ids from the live config.
    #[allow(dead_code)]
    pub fn config_toolbar_right(&self) -> Vec<String> {
        self.config.toolbar.right.clone()
    }

    /// The customizable toolbar's overflow-menu ids from the live config.
    #[allow(dead_code)]
    pub fn config_toolbar_menu(&self) -> Vec<String> {
        self.config.toolbar.menu.clone()
    }

    /// Whether the toolbar's overflow "⋯" button is enabled in the live config.
    #[allow(dead_code)]
    pub fn config_toolbar_show_overflow(&self) -> bool {
        self.config.toolbar.show_overflow
    }

    /// The current window opacity (0.0..=1.0) from the live config — the single
    /// see-through control. `1.0` = solid, `0.0` = fully transparent.
    #[allow(dead_code)]
    pub fn config_opacity(&self) -> f32 {
        self.config.opacity
    }

    /// The current window tint strength (0.0..=1.0) from the live config.
    #[allow(dead_code)]
    pub fn config_tint_strength(&self) -> f32 {
        self.config.tint_strength
    }

    /// Whether the tint colour wash is enabled in the live config.
    #[allow(dead_code)]
    pub fn config_tint_enabled(&self) -> bool {
        self.config.tint_enabled
    }

    /// The current terminal color theme stem from the live config.
    #[allow(dead_code)]
    pub fn config_theme(&self) -> &str {
        &self.config.theme
    }

    /// Whether the egui Visuals DERIVED from the active terminal theme read as
    /// LIGHT (window-fill luminance > 0.5). Observation accessor for the
    /// whole-app-theming interaction test: it asserts the chrome flips light
    /// after picking a light theme (ghost-paper) and dark after a dark one,
    /// exercising the same `visuals_from_theme` derivation the live app applies.
    #[allow(dead_code)]
    pub fn visuals_are_light(&self) -> bool {
        theme::is_light(theme::visuals_from_theme(&self.theme).window_fill)
    }

    /// The current scrollback line count from the live config.
    #[allow(dead_code)]
    pub fn config_scrollback_lines(&self) -> usize {
        self.config.scrollback_lines
    }

    /// The current multi-line-paste-warning flag from the live config.
    #[allow(dead_code)]
    pub fn config_paste_warn_multiline(&self) -> bool {
        self.config.paste_warn_multiline
    }
}

#[cfg(test)]
mod tests {
    use super::super::C0pl4ndApp;

    fn app_with(config: c0pl4nd_core::Config) -> C0pl4ndApp {
        C0pl4ndApp::bootstrap_with(config)
    }

    /// Every accessor in this module is a one-line getter, so there is no logic to
    /// test — but there IS a wiring question worth answering: does each one read its
    /// OWN field? These accessors are the observation surface the settings
    /// interaction tests assert through, so one silently reading a neighbouring
    /// field would make those tests assert the wrong thing while still passing.
    ///
    /// Each field below is set to a value DISTINCT from every other field's (and
    /// from the default), so an accessor wired to the wrong field fails here. This
    /// is deliberately a wiring check, not a claim to be testing behaviour.
    #[test]
    fn every_config_accessor_reads_its_own_field() {
        let mut config = c0pl4nd_core::Config::default();
        config.font.size = 21.5;
        config.font.family = "Sentinel Family".to_string();
        config.font.fallback = vec!["Sentinel Fallback".to_string()];
        config.cursor.blink = !config.cursor.blink;
        config.toolbar.left = vec!["sentinel-left".to_string()];
        config.toolbar.right = vec!["sentinel-right".to_string()];
        config.toolbar.menu = vec!["sentinel-menu".to_string()];
        config.toolbar.show_overflow = !config.toolbar.show_overflow;
        config.opacity = 0.37;
        config.tint_strength = 0.62;
        config.tint_enabled = !config.tint_enabled;
        config.scrollback_lines = 4321;
        config.paste_warn_multiline = !config.paste_warn_multiline;

        // Snapshot the flipped booleans before `config` is moved into the app.
        let blink = config.cursor.blink;
        let show_overflow = config.toolbar.show_overflow;
        let tint_enabled = config.tint_enabled;
        let paste_warn = config.paste_warn_multiline;

        let app = app_with(config);

        assert_eq!(app.config_font_size(), 21.5);
        assert_eq!(app.config_font_family(), "Sentinel Family");
        assert_eq!(app.config_font_fallback(), vec!["Sentinel Fallback"]);
        assert_eq!(app.config_cursor_blink(), blink);
        assert_eq!(app.config_toolbar_left(), vec!["sentinel-left"]);
        assert_eq!(app.config_toolbar_right(), vec!["sentinel-right"]);
        assert_eq!(app.config_toolbar_menu(), vec!["sentinel-menu"]);
        assert_eq!(app.config_toolbar_show_overflow(), show_overflow);
        assert_eq!(app.config_opacity(), 0.37);
        assert_eq!(app.config_tint_strength(), 0.62);
        assert_eq!(app.config_tint_enabled(), tint_enabled);
        assert_eq!(app.config_scrollback_lines(), 4321);
        assert_eq!(app.config_paste_warn_multiline(), paste_warn);
    }

    /// The theme accessor reports the configured theme stem.
    #[test]
    fn config_theme_reports_the_selected_theme() {
        let config = c0pl4nd_core::Config {
            theme: "ghost-paper".to_string(),
            ..Default::default()
        };
        assert_eq!(app_with(config).config_theme(), "ghost-paper");
    }

    /// `applied_font_key` reports the font stack actually INSTALLED into egui, which
    /// is NOT the configured family: a freshly bootstrapped app has installed
    /// nothing yet, so it must be empty even though a family IS configured. This is
    /// what lets the settings test prove a family change was really re-installed
    /// rather than only stored in the config.
    #[test]
    fn applied_font_key_is_empty_until_a_font_is_installed() {
        let config = c0pl4nd_core::Config {
            font: c0pl4nd_core::config::FontConfig {
                family: "Sentinel Family".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let app = app_with(config);
        assert_eq!(
            app.applied_font_key(),
            "",
            "bootstrap installs no font, so the applied key must not echo the config"
        );
        assert_eq!(
            app.config_font_family(),
            "Sentinel Family",
            "precondition: a family IS configured, so the assertion above is not vacuous"
        );
    }

    /// The whole-app chrome follows the selected terminal theme: a LIGHT theme
    /// derives light egui visuals and a DARK theme derives dark ones. Both
    /// directions are asserted, so this cannot pass on a constant.
    #[test]
    fn visuals_follow_the_selected_theme_in_both_directions() {
        let light = app_with(c0pl4nd_core::Config {
            theme: "ghost-paper".to_string(),
            ..Default::default()
        });
        assert!(
            light.visuals_are_light(),
            "a light theme (ghost-paper) must derive LIGHT egui visuals"
        );

        let dark = app_with(c0pl4nd_core::Config {
            theme: "void".to_string(),
            ..Default::default()
        });
        assert!(
            !dark.visuals_are_light(),
            "a dark theme (void) must derive DARK egui visuals"
        );
    }
}
