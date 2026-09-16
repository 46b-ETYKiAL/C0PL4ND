//! egui font installation: base icon stack + the user's configured monospace.
//!
//! Builds egui `FontDefinitions` (Phosphor icons merged into both families) and
//! installs the configured system monospace family into a `Context`, loading it
//! off-thread. The enumeration + font-stack primitives live in the sibling
//! `fonts` module; this is the `Context`-installation layer over them. Extracted
//! from the `egui_app` god-module; re-exported via `pub(crate) use font_setup::*`.

use super::fonts;

/// Build egui's base font set: the Phosphor icon font merged into BOTH the
/// proportional and monospace families (so the chrome's caption glyphs —
/// close/maximize/minimize/gear, split-right/down — render as crisp icons
/// instead of tofu), plus the SOLID `phosphor-fill` family used by a pinned
/// tab's pin. This is the icon-only base; [`install_chrome_fonts`] layers the
/// user's configured monospace family on top.
pub(crate) fn base_font_definitions(ui_family: &str) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    // Thin = the default chrome icon weight (registered as "phosphor").
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Thin);
    // Fill = SOLID glyphs, registered under a SEPARATE family so most icons stay
    // thin but a pinned tab can show a solid pin (`add_to_fonts` always uses the
    // "phosphor" key, so a second call would overwrite Thin with Fill). Use via
    // `RichText::new(fill_glyph).family(FontFamily::Name("phosphor-fill".into()))`.
    fonts.font_data.insert(
        "phosphor-fill".to_owned(),
        egui_phosphor::Variant::Fill.font_data().into(),
    );
    fonts.families.insert(
        egui::FontFamily::Name("phosphor-fill".into()),
        vec!["phosphor-fill".to_owned()],
    );
    // `add_to_fonts` registers the "phosphor" font_data and inserts it into the
    // Proportional family; also append it to Monospace so monospace buttons can
    // resolve the icons.
    if let Some(mono) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        if !mono.iter().any(|f| f == "phosphor") {
            mono.push("phosphor".to_owned());
        }
    }
    // Bundled JP fallback: register the embedded Noto Sans JP subset and append it
    // to BOTH families (mirrors the sibling editor SCR1B3) so Japanese glyphs
    // render out of the box on every machine — even one with no system CJK font —
    // WITHOUT a `fontdb` enumeration. Because it lives in the icon-only BASE, it
    // covers the default (built-in mono) path AND every custom-font stack layered
    // on top. Appended LAST, it is the lowest-priority fallback (used only for
    // glyphs the primary + OS-CJK faces lack). The subset is small, so shipping it
    // in the binary costs little while guaranteeing JP never tofus.
    fonts.font_data.insert(
        fonts::BUNDLED_JP_FALLBACK_KEY.to_owned(),
        egui::FontData::from_static(fonts::NOTO_SANS_JP_SUBSET).into(),
    );
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        let list = fonts.families.entry(family).or_default();
        if !list.iter().any(|k| k == fonts::BUNDLED_JP_FALLBACK_KEY) {
            list.push(fonts::BUNDLED_JP_FALLBACK_KEY.to_owned());
        }
    }
    // Swap the app-UI (Proportional) font to the configured family (default IBM
    // Plex Mono). Touches ONLY Proportional, so the terminal grid (Monospace) is
    // never changed by a UI-font swap. "System default" leaves egui's built-in
    // proportional font in place.
    fonts::apply_ui_font(&mut fonts, ui_family);
    fonts
}

/// Install the chrome icon fonts AND the user's configured monospace family +
/// fallbacks into egui's font set. The configured family (and each fallback,
/// in order) is loaded from the system font DB and PREPENDED to
/// `FontFamily::Monospace`, so the terminal grid and every monospace UI surface
/// render in the chosen font; egui's default monospace + the Phosphor icons stay
/// at the END as the ultimate fallback. A family that is the built-in label, is
/// "(none)", or is simply not installed is skipped gracefully (no panic) and the
/// built-in monospace remains in use.
///
/// Loading the system font DB is slow (100s of ms), so it runs ONLY when the
/// config names at least one real (non-built-in) family to load — the common
/// "built-in mono" path pays nothing. Called from `new()` and from the
/// first-frame gate in `frame_tick`, and re-run live by [`super::C0pl4ndApp::frame_tick`]
/// when the user changes the family/fallback in settings.
pub(crate) fn install_chrome_fonts(ctx: &egui::Context, font: &c0pl4nd_core::config::FontConfig) {
    // Fast path: nothing custom to load (default config / built-in choice) — set
    // the icon base and skip the expensive system-font enumeration entirely.
    if !system_font_load_needed(font) {
        ctx.set_fonts(base_font_definitions(&font.ui_family));
        return;
    }
    // Custom family: the (100s-of-ms) system DB load runs synchronously here. The
    // startup path avoids this by going through `install_base_fonts` +
    // `spawn_system_font_load` instead (audit #3); this synchronous form is the
    // settings-change re-install (user-initiated, expects an immediate apply) and
    // the headless-test path (deterministic, no worker thread).
    ctx.set_fonts(build_system_font_definitions(font));
}

/// Whether the configured font stack names any non-built-in family, i.e. whether
/// the (slow) system-font DB load is required. Pure → unit-testable.
pub(crate) fn system_font_load_needed(font: &c0pl4nd_core::config::FontConfig) -> bool {
    !fonts::is_builtin_family(&font.family)
        || font.fallback.iter().any(|f| !fonts::is_builtin_family(f))
}

/// Install ONLY the built-in icon/base fonts (no system-DB enumeration), so the
/// first frame paints immediately with the built-in monospace while the custom
/// system fonts load on a worker thread (audit #3).
pub(crate) fn install_base_fonts(ctx: &egui::Context, ui_family: &str) {
    ctx.set_fonts(base_font_definitions(ui_family));
}

/// Build the full [`egui::FontDefinitions`] for a custom font stack: enumerate
/// the system font DB and prepend the chosen family + fallbacks to
/// `FontFamily::Monospace`. This is the heavy (100s-of-ms) call — invoked off the
/// startup critical path on a worker thread by [`spawn_system_font_load`], and
/// synchronously by [`install_chrome_fonts`] on a settings change.
pub(crate) fn build_system_font_definitions(
    font: &c0pl4nd_core::config::FontConfig,
) -> egui::FontDefinitions {
    let base = base_font_definitions(&font.ui_family);
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let (defs, _loaded) = fonts::build_font_definitions(base, &db, &font.family, &font.fallback);
    defs
}

/// Spawn a worker thread that builds the custom-font [`egui::FontDefinitions`]
/// off the startup critical path (audit #3) and returns the receiver the frame
/// loop polls. The closure owns a clone of the font config so the thread is
/// self-contained. `frame_tick` calls `ctx.set_fonts(defs)` when the result
/// arrives — until then the window paints with the built-in mono from
/// [`install_base_fonts`]. A send failure (the app dropped the receiver, e.g. at
/// shutdown) is ignored — the result is simply discarded.
pub(crate) fn spawn_system_font_load(
    font: &c0pl4nd_core::config::FontConfig,
) -> std::sync::mpsc::Receiver<egui::FontDefinitions> {
    let (tx, rx) = std::sync::mpsc::channel();
    let font = font.clone();
    std::thread::Builder::new()
        .name("c0pl4nd-font-load".to_string())
        .spawn(move || {
            let defs = build_system_font_definitions(&font);
            let _ = tx.send(defs);
        })
        // A spawn failure (resource-exhausted) is non-fatal: fall back to the
        // built-in mono already installed; no custom font this session.
        .ok();
    rx
}

/// Fold a [`FontConfig`](c0pl4nd_core::config::FontConfig)'s family + ordered
/// fallbacks into a single stable key. Two configs produce the same key iff they
/// install the SAME monospace font stack, so the frame loop can re-install the
/// egui fonts ONLY when this key actually changes (the live-apply gate). Pure +
/// GPU-free so the gate is unit-testable. Size / line-height are deliberately
/// excluded — they do not change which font FILE is loaded, only how it is
/// drawn.
pub(crate) fn font_apply_key(font: &c0pl4nd_core::config::FontConfig) -> String {
    let mut key = font.family.trim().to_string();
    for f in &font.fallback {
        key.push('\u{1f}'); // unit-separator: cannot appear in a family name
        key.push_str(f.trim());
    }
    // The app-UI (Proportional) font is part of the installed font set, so a
    // ui_family change must re-run `set_fonts`. A distinct separator (record-sep)
    // keeps it unambiguous from the monospace family/fallback list above.
    key.push('\u{1e}');
    key.push_str(font.ui_family.trim());
    key
}

/// Everything that determines the CONTENT of the pre-warmed grid glyph atlas.
///
/// `(font-stack key, size bits, pixels-per-point bits, text-contrast bits)`.
/// Floats are compared by bit pattern rather than by value because this is an
/// identity question ("is the atlas the one we warmed?"), not a numeric one —
/// and `NaN != NaN` under value comparison would re-warm forever.
pub(crate) type AtlasWarmKey = (String, u32, u32, u32);

/// Build the [`AtlasWarmKey`] for a live font stack + coverage curve.
///
/// `text_contrast` is in the key for a concrete reason that is easy to miss:
/// changing the glyph coverage curve makes epaint RECREATE the whole font atlas
/// (its `Fonts::begin_pass` compares the incoming `TextOptions` against the
/// live ones and recreates on any difference). An atlas key blind to the curve
/// would therefore report a warm atlas that no longer exists, leaving the warmup
/// gate un-armed across exactly the kind of atlas reset that gate was built for.
///
/// Pure + GPU-free so the invalidation is unit-testable without a renderer.
pub(crate) fn atlas_warm_key(
    font_family_key: &str,
    size: f32,
    pixels_per_point: f32,
    text_contrast: f32,
) -> AtlasWarmKey {
    (
        font_family_key.to_string(),
        size.to_bits(),
        pixels_per_point.to_bits(),
        text_contrast.to_bits(),
    )
}

#[cfg(test)]
mod key_tests {
    use super::{atlas_warm_key, font_apply_key};
    use c0pl4nd_core::config::FontConfig;

    /// THE atlas-invalidation regression guard.
    ///
    /// A coverage-curve change recreates the font atlas. If the warm key cannot
    /// see the curve, the app believes its atlas is still warm across that
    /// recreation — so the warmup gate never re-arms and the cached galleys are
    /// never dropped. This is the no-GPU half of that contract; the galley-cache
    /// half is `apply_live_glyph_curve_change`.
    #[test]
    fn the_atlas_warm_key_changes_when_the_coverage_curve_changes() {
        let base = atlas_warm_key("JetBrains Mono", 13.0, 1.0, 0.0);
        assert_ne!(
            base,
            atlas_warm_key("JetBrains Mono", 13.0, 1.0, 0.35),
            "the atlas warm key must move when text_contrast moves — otherwise a \
             live curve change silently leaves the app pointing at a discarded \
             atlas"
        );
        assert_ne!(
            base,
            atlas_warm_key("JetBrains Mono", 13.0, 1.0, -0.35),
            "…in both directions"
        );
        // A curve change that is too small to move the key would be a curve
        // change the atlas gate cannot see. Bit-comparison means ANY distinct
        // float registers.
        assert_ne!(base, atlas_warm_key("JetBrains Mono", 13.0, 1.0, f32::MIN_POSITIVE));
    }

    /// The pre-existing key components must still be load-bearing — this guard
    /// would otherwise pass on a key that ONLY carried the contrast.
    #[test]
    fn the_atlas_warm_key_still_tracks_family_size_and_dpi() {
        let base = atlas_warm_key("JetBrains Mono", 13.0, 1.0, 0.0);
        assert_ne!(base, atlas_warm_key("IBM Plex Mono", 13.0, 1.0, 0.0));
        assert_ne!(base, atlas_warm_key("JetBrains Mono", 14.0, 1.0, 0.0));
        assert_ne!(base, atlas_warm_key("JetBrains Mono", 13.0, 2.0, 0.0));
        assert_eq!(
            base,
            atlas_warm_key("JetBrains Mono", 13.0, 1.0, 0.0),
            "and it must be stable for identical inputs, or the atlas would \
             re-warm every frame"
        );
    }

    #[test]
    fn font_apply_key_changes_when_ui_family_changes() {
        // A UI-font change must produce a DIFFERENT apply key so the live
        // frame-loop gate re-installs the fonts (otherwise the swap does nothing).
        let a = FontConfig {
            ui_family: "IBM Plex Mono".into(),
            ..FontConfig::default()
        };
        let b = FontConfig {
            ui_family: "System default".into(),
            ..FontConfig::default()
        };
        assert_ne!(
            font_apply_key(&a),
            font_apply_key(&b),
            "a ui_family change must change the apply key"
        );
        // Same config → same key (idempotent gate).
        assert_eq!(font_apply_key(&a), font_apply_key(&a.clone()));
        // The terminal family still participates too.
        let c = FontConfig {
            family: "Fira Mono".into(),
            ..a.clone()
        };
        assert_ne!(font_apply_key(&a), font_apply_key(&c));
    }
}
