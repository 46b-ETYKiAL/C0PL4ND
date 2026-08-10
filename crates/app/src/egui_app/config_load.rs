//! User config + terminal-theme loading.
//!
//! Loads the persisted config from its canonical path and resolves the active
//! terminal theme (built-in or on-disk), each returning an optional parse-error
//! notice for the caller to surface as a toast. Pure `core` APIs (no App state),
//! extracted from the `egui_app` god-module. Re-exported via `pub(crate) use config_load::*`.

/// Load the persisted user config from its canonical path, returning the config
/// AND a parse-error message (F5-2) when a config file EXISTS but fails to
/// read/parse — so the caller can surface it as a visible toast instead of the
/// silent fallback-to-defaults that previously only `eprintln`'d (invisible to a
/// GUI-launched user). Without loading at all the egui app would start from
/// `Config::default()` every launch, so on-disk settings the panel WROTE never
/// took effect. `None` error means the file was absent (normal zero-config
/// start) or parsed cleanly. Pure `core` APIs, available in every binary that
/// includes this module (incl. the `#[path]`-included test bins).
pub(crate) fn load_config_with_status() -> (c0pl4nd_core::Config, Option<String>) {
    load_config_from(c0pl4nd_core::Config::default_path().filter(|p| p.exists()))
}

/// Pure core of config loading, parameterised on the path so it is unit-testable
/// (the real entry resolves `Config::default_path()`). An absent path → defaults
/// with no error; a present-but-invalid file → defaults WITH an error message
/// (the F5-2 surfacing contract).
pub(crate) fn load_config_from(
    path: Option<std::path::PathBuf>,
) -> (c0pl4nd_core::Config, Option<String>) {
    match path {
        Some(p) => match std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|s| c0pl4nd_core::Config::from_toml(&s, &p).map_err(|e| e.to_string()))
        {
            Ok(cfg) => (cfg, None),
            Err(e) => {
                tracing::warn!(
                    target: "c0pl4nd::config",
                    path = ?p,
                    "config load failed; using defaults"
                );
                (
                    c0pl4nd_core::Config::default(),
                    Some(crate::user_error::config_load_failed(e)),
                )
            }
        },
        None => (c0pl4nd_core::Config::default(), None),
    }
}

/// Honour an OS forced-colors / high-contrast request on a freshly loaded
/// config, returning `true` when the theme was auto-selected.
///
/// C0PL4ND ships high-contrast themes but, until this seam existed, never asked
/// the OS whether the user had turned high contrast ON — so a user running in
/// High Contrast mode got the ordinary brand theme and had to find the setting
/// by hand.
///
/// The precedence decision itself lives in
/// [`c0pl4nd_core::forced_colors::auto_theme_override`] as a pure function with
/// its own tests: **an explicit theme choice always wins**, and auto-selection
/// only applies when the stored theme is still provably the shipped default.
/// This wrapper does nothing but sample the OS and apply that verdict.
///
/// The assignment mutates the in-memory config exactly as `follow_os_theme_tick`
/// does for the dark/light follow, which means it can later be persisted by an
/// unrelated save. That is deliberate and consistent with the existing
/// auto-theming behaviour: once written, the value is no longer the default, so
/// it reads as a deliberate choice and sticks — which is also what makes it
/// straightforward for the user to override from the theme picker at any time.
pub(crate) fn apply_forced_colors_auto_theme(config: &mut c0pl4nd_core::Config) -> bool {
    apply_forced_colors_auto_theme_with(config, c0pl4nd_core::forced_colors::forced_colors())
}

/// Pure core of [`apply_forced_colors_auto_theme`], parameterised on the OS
/// answer so the wiring is unit-testable without a machine in High Contrast
/// mode (the real entry samples the OS).
pub(crate) fn apply_forced_colors_auto_theme_with(
    config: &mut c0pl4nd_core::Config,
    high_contrast: bool,
) -> bool {
    let shipped_default = c0pl4nd_core::Config::default().theme;
    match c0pl4nd_core::forced_colors::auto_theme_override(
        &config.theme,
        &shipped_default,
        high_contrast,
    ) {
        Some(theme) => {
            tracing::info!(
                target: "c0pl4nd::a11y",
                theme,
                "OS high contrast is on and no theme was chosen; selecting the accessible theme"
            );
            config.theme = theme.to_string();
            true
        }
        None => false,
    }
}

/// Load the terminal colour theme named by `config.theme` from the bundled
/// themes dir (next to the binary or in the source tree during development),
/// falling back to the built-in Itasha.Corp void theme when the file is absent.
/// The terminal grid's glyph colours come from this theme — NOT egui Visuals.
/// On-disk theme-file lookup order for a theme named `name`, highest priority
/// first. The user's config-dir `themes/<name>.toml` comes FIRST so it overrides
/// a built-in (and a shipped `assets/themes`) of the same name — the behavior the
/// settings hint promises ("a user theme TOML under the config dir's themes
/// folder overrides the built-in of the same name"). Pure so the ordering is
/// unit-testable without touching the real filesystem.
pub(crate) fn theme_candidate_paths(
    name: &str,
    config_dir: Option<&std::path::Path>,
    exe_dir: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    let file = format!("{name}.toml");
    let mut candidates = Vec::new();
    // 1. User override: <config dir>/themes/<name>.toml
    if let Some(d) = config_dir {
        candidates.push(d.join("themes").join(&file));
    }
    // 2. Dev tree / current working dir: assets/themes/<name>.toml
    candidates.push(std::path::PathBuf::from("assets/themes").join(&file));
    // 3. Shipped next to the executable: <exe dir>/assets/themes/<name>.toml
    if let Some(d) = exe_dir {
        candidates.push(d.join("assets/themes").join(&file));
    }
    candidates
}

/// Resolve the active terminal theme, returning the theme AND an optional
/// user-facing notice when a theme file that EXISTS failed to load (a bad hex /
/// broken TOML in a hand-authored override). The notice is surfaced as a toast
/// by the caller — previously such a failure was swallowed by `if let Ok` and
/// the user silently got the wrong (fallback) colors with no explanation. An
/// ABSENT file is the normal case and stays silent.
pub(crate) fn load_terminal_theme(
    config: &c0pl4nd_core::Config,
) -> (c0pl4nd_core::Theme, Option<String>) {
    let config_path = c0pl4nd_core::Config::default_path();
    let config_dir = config_path.as_deref().and_then(|p| p.parent());
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(|p| p.parent());
    let mut notice = None;
    for c in theme_candidate_paths(&config.theme, config_dir, exe_dir) {
        match c0pl4nd_core::Theme::load_from(&c) {
            Ok(t) => return (t, None),
            Err(e) => {
                // A candidate that EXISTS but failed to parse is a real user
                // error worth surfacing (distinct from the common "file absent"
                // skip). Capture the first such error and keep falling through to
                // a valid theme so the app never wedges on a bad override.
                if notice.is_none() && c.exists() {
                    notice = Some(crate::user_error::theme_load_failed(e, &config.theme));
                }
            }
        }
    }
    // No on-disk file matched (the common case in the INSTALLED app, whose CWD
    // is not the source tree and which ships no `assets/themes/` next to the
    // exe). Resolve from the COMPILED-IN theme set so selection still works —
    // this is the fix for "the theme doesn't change". On-disk files above still
    // win when present, so a user can override a built-in or add their own.
    if let Some(t) = c0pl4nd_core::Theme::builtin_named(&config.theme) {
        return (t, notice);
    }
    (c0pl4nd_core::Theme::builtin_void(), notice)
}
