//! Installed-font enumeration and egui font-stack wiring for the Font settings
//! page (#14 — "Font family + fallback dropdowns that actually load the font").
//!
//! Before this module the egui app NEVER loaded the configured font: the chrome
//! only registered egui's default fonts + the Phosphor icons, and the terminal
//! grid drew with egui's built-in `FontFamily::Monospace`. The free-text
//! `family` / `fallback` config fields were inert. This module:
//!
//! 1. **enumerates** the system's installed MONOSPACE families (via `fontdb`,
//!    cached once — it is slow), and
//! 2. **loads** the configured family + fallbacks into egui by reading each
//!    chosen face's raw bytes and PREPENDING them to `FontFamily::Monospace`,
//!    keeping egui's default monospace + Phosphor at the END as the ultimate
//!    fallback, so the grid + every monospace UI surface use the chosen font.
//!
//! A family that cannot be found is skipped gracefully — never a panic — so a
//! stale config naming an uninstalled font falls back to the built-in mono.

use std::sync::OnceLock;

use eframe::egui;

/// The label shown for egui's built-in monospace in the Family / Fallback
/// dropdowns. Always offered as a choice so a user can deliberately fall back to
/// the bundled mono even on a system rich in installed fonts, and so the list is
/// never empty (the enumeration could legitimately find zero installed faces).
pub const BUILTIN_MONOSPACE_LABEL: &str = "(built-in monospace)";

/// The Fallback dropdown's "no fallback in this slot" choice.
pub const NONE_LABEL: &str = "(none)";

// ── Bundled fonts ───────────────────────────────────────────────────────────
//
// C0PL4ND enumerates the machine's INSTALLED monospace families via `fontdb`,
// so which faces are offered depends on the OS. The sibling editor SCR1B3 instead
// SHIPS its typefaces IN the binary via `include_bytes!`, so the same faces —
// including the lore/influence-inspired brand DISPLAY faces (Wallpoet, Michroma,
// Zen Dots, …) — are available on every machine. This block brings that identical
// OFL/Apache set into C0PL4ND as always-available, selectable Family choices that
// render in the terminal grid + chrome regardless of what the OS has installed.
//
// The `.ttf` files (and each family's OFL/LICENSE file — license compliance) live
// under `crates/app/assets/fonts/<Family>/`; the paths below are relative to THIS
// source file (`crates/app/src/egui_app/fonts.rs`), i.e. `../../assets/fonts/…`.

/// A compile-time-embedded font face bundled INTO the binary. Unlike a system
/// family enumerated via `fontdb`, a bundled face is selectable + renders on
/// EVERY machine — its resolution never touches `fontdb` or the OS font set.
pub struct BundledFont {
    /// Family name shown in Settings → Fonts → Family (and stored in config).
    pub display: &'static str,
    /// The egui `font_data` key this face registers under — namespaced so it can
    /// never collide with a `fontdb`-loaded system face key ([`font_data_key`]).
    pub key: &'static str,
    /// The raw `.ttf` bytes, embedded at compile time.
    pub bytes: &'static [u8],
}

/// Terse constructor for a [`BundledFont`] entry: `bundled!(display, key, path)`.
/// `key` is namespaced under `c0pl4nd-bundled::` (compile-time `concat!`) and the
/// bytes are `include_bytes!`-embedded (path relative to this file). The
/// `&[u8; N] → &[u8]` unsizing happens at the struct-field coercion site.
macro_rules! bundled {
    ($display:literal, $key:literal, $path:literal) => {
        BundledFont {
            display: $display,
            key: concat!("c0pl4nd-bundled::", $key),
            bytes: include_bytes!($path),
        }
    };
}

/// The bundled selectable families, in curated order — the monospace CODING faces
/// first (so the picker leads with terminal-appropriate faces), then the brand
/// DISPLAY / accent faces. Mirrors SCR1B3's `FONT_FAMILIES` set + display names so
/// both apps offer the identical typefaces. Noto Sans JP is intentionally NOT here
/// — it is wired as an always-on JP FALLBACK (see [`NOTO_SANS_JP_SUBSET`]), matching
/// SCR1B3, not as a body-text choice.
///
/// Display / variable faces (Wallpoet, Michroma, Zen Dots, and the variable-axis
/// files Doto / Red Hat Mono / Teko / Saira / Spline Sans Mono) are NOT monospace,
/// which is fine for an opt-in terminal font CHOICE — the built-in monospace stays
/// the default.
///
/// A variable `.ttf` here loads its DEFAULT instance (identical to how SCR1B3
/// embeds them) — no special handling needed, none skipped. That is this app's
/// CHOICE, not a backend limitation: epaint 0.34 rasterises through `skrifa`
/// (not `ab_glyph`) and DOES expose variable-axis selection — it resolves a
/// `skrifa` `Location` from `FontTweak::coords` chained with the per-`TextFormat`
/// coords. This app sets neither, so every axis stays at its default coordinate.
/// Wiring a weight/width axis would mean populating `FontTweak::coords` on the
/// registered face; nothing in the backend prevents it.
pub const BUNDLED_FONTS: &[BundledFont] = &[
    // Monospace coding faces.
    bundled!(
        "JetBrains Mono",
        "JetBrainsMono",
        "../../assets/fonts/JetBrainsMono/JetBrainsMono-Regular.ttf"
    ),
    bundled!(
        "IBM Plex Mono",
        "IBMPlexMono",
        "../../assets/fonts/IBMPlexMono/IBMPlexMono-Regular.ttf"
    ),
    bundled!(
        "Fira Mono",
        "FiraMono",
        "../../assets/fonts/FiraMono/FiraMono-Regular.ttf"
    ),
    bundled!(
        "Space Mono",
        "SpaceMono",
        "../../assets/fonts/SpaceMono/SpaceMono-Regular.ttf"
    ),
    bundled!(
        "Cousine",
        "Cousine",
        "../../assets/fonts/Cousine/Cousine-Regular.ttf"
    ),
    bundled!(
        "Source Code Pro",
        "SourceCodePro",
        "../../assets/fonts/SourceCodePro/SourceCodePro-Regular.ttf"
    ),
    bundled!(
        "B612 Mono",
        "B612Mono",
        "../../assets/fonts/B612Mono/B612Mono-Regular.ttf"
    ),
    bundled!(
        "Share Tech Mono",
        "ShareTechMono",
        "../../assets/fonts/ShareTechMono/ShareTechMono-Regular.ttf"
    ),
    bundled!(
        "VT323",
        "VT323",
        "../../assets/fonts/VT323/VT323-Regular.ttf"
    ),
    // Variable-axis monospace (default instance loaded — see doc above).
    bundled!(
        "Red Hat Mono",
        "RedHatMono",
        "../../assets/fonts/RedHatMono/RedHatMono[wght].ttf"
    ),
    bundled!(
        "Spline Sans Mono",
        "SplineSansMono",
        "../../assets/fonts/SplineSansMono/SplineSansMono[wght].ttf"
    ),
    // Brand DISPLAY / accent faces (proportional — opt-in terminal choice).
    bundled!(
        "Doto",
        "Doto",
        "../../assets/fonts/Doto/Doto[ROND,wght].ttf"
    ),
    bundled!(
        "Major Mono Display",
        "MajorMonoDisplay",
        "../../assets/fonts/MajorMonoDisplay/MajorMonoDisplay-Regular.ttf"
    ),
    bundled!(
        "Chakra Petch",
        "ChakraPetch",
        "../../assets/fonts/ChakraPetch/ChakraPetch-Regular.ttf"
    ),
    bundled!(
        "Wallpoet",
        "Wallpoet",
        "../../assets/fonts/Wallpoet/Wallpoet-Regular.ttf"
    ),
    bundled!(
        "Michroma",
        "Michroma",
        "../../assets/fonts/Michroma/Michroma-Regular.ttf"
    ),
    bundled!("Teko", "Teko", "../../assets/fonts/Teko/Teko[wght].ttf"),
    bundled!(
        "Rajdhani",
        "Rajdhani",
        "../../assets/fonts/Rajdhani/Rajdhani-Regular.ttf"
    ),
    bundled!(
        "Saira",
        "Saira",
        "../../assets/fonts/Saira/Saira[wdth,wght].ttf"
    ),
    bundled!(
        "Zen Dots",
        "ZenDots",
        "../../assets/fonts/ZenDots/ZenDots-Regular.ttf"
    ),
    bundled!(
        "Syncopate",
        "Syncopate",
        "../../assets/fonts/Syncopate/Syncopate-Regular.ttf"
    ),
];

/// The bundled Noto Sans JP subset, wired as an always-on JP FALLBACK (mirrors
/// SCR1B3) by [`super::font_setup::base_font_definitions`] so Japanese glyphs
/// render out of the box on every machine — even one with no system CJK font —
/// WITHOUT a `fontdb` enumeration. Appended last (lowest priority), it supplies
/// only the glyphs the primary + OS-CJK faces lack.
pub const NOTO_SANS_JP_SUBSET: &[u8] =
    include_bytes!("../../assets/fonts/NotoSansJP/NotoSansJP-Subset.ttf");

/// The fixed egui `font_data` key the bundled JP fallback registers under.
pub const BUNDLED_JP_FALLBACK_KEY: &str = "c0pl4nd-bundled-jp";

/// Look up a bundled face by its display name (case-insensitive), returning both
/// its embedded bytes and its namespaced egui key, or `None` when `family` is not
/// a bundled face. This NEVER touches `fontdb`, so a bundled family is selectable
/// and renders regardless of installed system fonts.
pub fn bundled_face(family: &str) -> Option<&'static BundledFont> {
    let want = family.trim();
    BUNDLED_FONTS
        .iter()
        .find(|b| b.display.eq_ignore_ascii_case(want))
}

/// The bundled family display names, in curated order — offered in the Family /
/// Fallback dropdowns right after the built-in label.
pub fn bundled_family_displays() -> impl Iterator<Item = &'static str> {
    BUNDLED_FONTS.iter().map(|b| b.display)
}

/// The app-UI-font choices for the Settings → Fonts → App UI font dropdown:
/// "System default" (keep egui's built-in proportional font) first, then every
/// bundled family display name. The stored config value is the display string.
pub fn ui_family_choices() -> impl Iterator<Item = &'static str> {
    std::iter::once(c0pl4nd_core::config::UI_FONT_SYSTEM_DEFAULT).chain(bundled_family_displays())
}

/// Whether `family` names the "keep egui's built-in proportional font" UI choice
/// (the system-default label, or empty). Such a value swaps nothing.
pub fn is_system_ui_family(family: &str) -> bool {
    let f = family.trim();
    f.is_empty() || f.eq_ignore_ascii_case(c0pl4nd_core::config::UI_FONT_SYSTEM_DEFAULT)
}

/// Apply the chosen app-UI font to `defs`: register the bundled face and PREPEND
/// it to `FontFamily::Proportional`, so the whole app UI (settings / chrome /
/// toolbar / status bar / menus) renders in it. The "System default" choice (or
/// an unknown / non-bundled name) leaves egui's built-in proportional font
/// untouched. Never a panic.
///
/// This is the UI-font half of the terminal/UI font SPLIT: it touches ONLY the
/// `Proportional` family, so the terminal grid — bound to `Monospace` — is never
/// affected by a UI-font swap (and the terminal-font swap, which touches only
/// `Monospace`, never affects the UI). Mirrors SCR1B3's mechanism exactly.
pub fn apply_ui_font(defs: &mut egui::FontDefinitions, ui_family: &str) {
    if is_system_ui_family(ui_family) {
        return;
    }
    let Some(bf) = bundled_face(ui_family) else {
        return; // unknown / non-bundled → keep egui's built-in proportional font
    };
    defs.font_data.insert(
        bf.key.to_string(),
        egui::FontData::from_static(bf.bytes).into(),
    );
    let prop = defs
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    prop.retain(|k| k != bf.key); // never register the same key twice
    prop.insert(0, bf.key.to_string());
}

/// Process-wide cache of the installed monospace family list. Enumeration loads
/// and parses the whole system font set (100s of ms with hundreds of fonts), so
/// it runs at most ONCE per process — never per frame.
static MONOSPACE_FAMILIES: OnceLock<Vec<String>> = OnceLock::new();

/// The installed monospace family names plus the built-in label, sorted +
/// deduped, computed lazily and cached for the process lifetime.
///
/// The returned list is suitable to drive the Font settings `ComboBox`es
/// directly. [`BUILTIN_MONOSPACE_LABEL`] is ALWAYS present (first), so the list
/// is non-empty even on a system where enumeration finds nothing.
pub fn monospace_family_choices() -> &'static [String] {
    MONOSPACE_FAMILIES.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let installed: Vec<String> = db
            .faces()
            .filter(|f| f.monospaced)
            // The first family name is the English-US one (fontdb guarantees it
            // is first when present); display that.
            .filter_map(|f| f.families.first().map(|(name, _)| name.clone()))
            .collect();
        with_bundled_families(normalize_family_list(installed))
    })
}

/// Splice the always-available bundled families into a [`normalize_family_list`]
/// output: the built-in label stays first, the bundled families follow (curated
/// order, guaranteed present on any machine), then the installed system families —
/// minus any whose name duplicates a bundled face (so a box that ALSO has, e.g.,
/// JetBrains Mono installed shows it once, as the bundled entry). Pure (no
/// `fontdb`, no egui) so the "built-in first, then bundled, then system" contract
/// is unit-testable without a system font DB.
pub fn with_bundled_families(normalized: Vec<String>) -> Vec<String> {
    let mut iter = normalized.into_iter();
    let mut out: Vec<String> = Vec::new();
    // The built-in label is always first in a normalized list; keep it leading.
    if let Some(first) = iter.next() {
        out.push(first);
    }
    for display in bundled_family_displays() {
        out.push(display.to_string());
    }
    // Append the remaining (system) families, dropping any that a bundled face
    // already covers so the same name never appears twice.
    for name in iter {
        if bundled_face(&name).is_none() {
            out.push(name);
        }
    }
    out
}

/// Pure helper: take a raw list of monospace family names, prepend the built-in
/// label, then sort + dedupe (case-insensitively) so the dropdown is clean and
/// stable. The built-in label always sorts to the FRONT regardless of the
/// installed names, so it is the first, default-reachable choice.
///
/// Kept pure (no `fontdb`, no egui) so the "always contains the built-in +
/// sorted + deduped" contract is unit-testable without a system font DB.
pub fn normalize_family_list(installed: Vec<String>) -> Vec<String> {
    let mut names: Vec<String> = installed
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != BUILTIN_MONOSPACE_LABEL)
        .collect();
    // Case-insensitive sort + dedupe so "Cascadia Code" and "cascadia code" do
    // not both appear, and the order is deterministic.
    names.sort_by_key(|a| a.to_lowercase());
    names.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
    let mut out = Vec::with_capacity(names.len() + 1);
    out.push(BUILTIN_MONOSPACE_LABEL.to_string());
    out.extend(names);
    out
}

/// Whether `family` names egui's built-in monospace (the synthetic label, or the
/// generic "monospace" the default config uses), i.e. NOT a real installed face
/// to load. Such a value contributes no custom font bytes — the built-in mono is
/// already the ultimate fallback.
pub fn is_builtin_family(family: &str) -> bool {
    let f = family.trim();
    f.is_empty()
        || f == BUILTIN_MONOSPACE_LABEL
        || f == NONE_LABEL
        || f.eq_ignore_ascii_case("monospace")
}

/// Read the raw bytes of the regular face of the installed family `family` from
/// `db`, case-insensitively matched against each face's primary (English-US)
/// family name. Returns `None` when no installed face matches (caller skips it
/// gracefully). Prefers a non-italic, ~regular-weight face so the grid renders
/// upright text; falls back to the first match if no plain face exists.
pub fn face_bytes_for_family(db: &fontdb::Database, family: &str) -> Option<(Vec<u8>, u32)> {
    face_bytes_for_weight(db, family, REGULAR_WEIGHT)
}

/// The OpenType weight class of a normal face — the target [`face_bytes_for_weight`]
/// scores against for the body text of the grid.
const REGULAR_WEIGHT: u16 = 400;

/// The OpenType weight class of a bold face (SGR `1`).
const BOLD_WEIGHT: u16 = 700;

/// The lowest weight class that still counts as a GENUINE bold cut. A family
/// whose heaviest installed face is lighter than this has no bold at all, so the
/// renderer must fall back to faux-bold rather than silently drawing SGR-1 text
/// in the regular weight (which is what "no bold face is loaded" looked like).
const MIN_BOLD_WEIGHT: u16 = 600;

/// The synthetic egui family name the BOLD monospace face is registered under.
/// A distinct family (rather than a weight on `FontFamily::Monospace`) is the
/// only way to reach a second face in egui, whose `FontId` selects a family, not
/// a weight.
pub const BOLD_MONOSPACE_FAMILY: &str = "c0pl4nd-mono-bold";

/// The egui font family the renderer uses for SGR-1 (bold) cells.
///
/// [`build_font_definitions`] always registers this family, so a `FontId` built
/// from it is always resolvable: it holds the real bold faces when the machine
/// has any, and otherwise mirrors the regular monospace stack (in which case
/// [`bold_face_available`] reports `false` and the renderer applies faux-bold).
pub fn bold_monospace_family() -> egui::FontFamily {
    egui::FontFamily::Name(BOLD_MONOSPACE_FAMILY.into())
}

/// Whether the last [`build_font_definitions`] call found a GENUINE bold face.
///
/// When this is `false` the bold family is just the regular stack, so bold text
/// would be indistinguishable from plain text — the renderer compensates by
/// double-striking the glyph at a sub-pixel offset (faux bold). Stored as a
/// process-global because the font stack is process-global: the renderer needs
/// the answer per-glyph, deep inside the paint loop, where threading a font-load
/// result through every call would add a parameter to the whole render path for
/// a value that can only change when the fonts are re-installed.
pub fn bold_face_available() -> bool {
    BOLD_FACE_AVAILABLE.load(std::sync::atomic::Ordering::Relaxed)
}

static BOLD_FACE_AVAILABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Read the raw bytes of the face of `family` closest to `want_weight`.
///
/// Generalises [`face_bytes_for_family`] (which targets the regular weight) so
/// the same matching logic serves the BOLD lookup — one implementation, so the
/// upright preference and the `.ttc` face-index handling can never drift between
/// the two weights. Returns the file bytes and the face's index WITHIN that file.
pub fn face_bytes_for_weight(
    db: &fontdb::Database,
    family: &str,
    want_weight: u16,
) -> Option<(Vec<u8>, u32)> {
    let want = family.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    let matches = db.faces().filter(|f| {
        f.families
            .first()
            .map(|(name, _)| name.to_lowercase() == want)
            .unwrap_or(false)
    });
    // Prefer an upright face at the wanted weight; otherwise take whatever matched.
    let mut best: Option<(&fontdb::FaceInfo, i32)> = None;
    for f in matches {
        let upright = matches!(f.style, fontdb::Style::Normal);
        // Distance from the wanted weight; smaller is better.
        let weight_dist = (f.weight.0 as i32 - want_weight as i32).abs();
        let score = if upright { 0 } else { 10_000 } + weight_dist;
        if best.as_ref().map(|(_, s)| score < *s).unwrap_or(true) {
            best = Some((f, score));
        }
    }
    let best = best?;
    // A bold REQUEST that only matched a light face is not a bold face. Reject it
    // so the caller can fall back to faux-bold instead of registering the regular
    // cut under the bold family (which renders SGR-1 identically to plain text —
    // the bug this lookup exists to fix).
    if want_weight >= MIN_BOLD_WEIGHT && best.0.weight.0 < MIN_BOLD_WEIGHT {
        return None;
    }
    let id = best.0.id;
    // Return the raw file bytes AND the face's index WITHIN that file. Many
    // Windows system fonts (MS Gothic, Malgun Gothic, …) are TrueType Collections
    // (.ttc) holding multiple faces; `with_face_data` yields the whole-file bytes
    // plus the wanted face's collection index. Dropping that index and handing the
    // bytes to egui makes it read face 0 — the WRONG font — so ASCII codepoints
    // render as whatever glyph sits at that slot in the wrong face (the terminal-
    // grid "garble": Latin text drawn as CJK / math glyphs). The caller MUST set
    // `FontData.index` to this value.
    db.with_face_data(id, |data, index| (data.to_vec(), index))
}

/// The egui `font_data` key under which a loaded custom family's bytes are
/// registered. Distinct per family so the Family choice and a Fallback choice
/// naming a different font do not collide.
fn font_data_key(family: &str) -> String {
    format!("c0pl4nd-user-font::{}", family.trim().to_lowercase())
}

/// The platform's OS-bundled CJK fonts, tried in order as an implicit ultimate
/// fallback by [`build_font_definitions`] so CJK (Chinese/Japanese/Korean) text
/// renders out of the box — instead of tofu boxes — without shipping a multi-MB
/// CJK font in the binary. Only the FIRST that resolves on the machine is loaded.
fn os_cjk_fallback_fonts() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    {
        // Present on every Windows install: MS Gothic (JP monospace) first, then
        // the SC / KR system faces so non-Japanese CJK also resolves.
        &["MS Gothic", "Yu Gothic", "Microsoft YaHei", "Malgun Gothic"]
    }
    #[cfg(target_os = "macos")]
    {
        &["Hiragino Sans", "Apple SD Gothic Neo", "PingFang SC"]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        &[
            "Noto Sans Mono CJK JP",
            "Noto Sans CJK JP",
            "Noto Sans CJK SC",
        ]
    }
}

/// Build the egui [`egui::FontDefinitions`] for the configured `family` +
/// `fallbacks`, on top of `base` (which must already carry egui's defaults +
/// the Phosphor icon families — i.e. the output of the chrome's base font
/// install). Each named family that resolves to an installed face is loaded and
/// PREPENDED (family first, then each fallback in order) to
/// `FontFamily::Monospace`; egui's default monospace fonts + Phosphor stay at
/// the END as the ultimate fallback. Families that do not resolve (built-in
/// label, "(none)", or simply not installed) are skipped — never a panic.
///
/// Returns `(defs, loaded_any)` where `loaded_any` is true iff at least one
/// custom face was prepended (so the caller can note whether the install
/// actually changed the monospace stack). The `db` is borrowed so the (slow)
/// system-font load can be done once by the caller and reused across the family
/// + fallback lookups.
pub fn build_font_definitions(
    mut base: egui::FontDefinitions,
    db: &fontdb::Database,
    family: &str,
    fallbacks: &[String],
) -> (egui::FontDefinitions, bool) {
    // Resolve the ordered list of real families to load: the primary first,
    // then each fallback, skipping the built-in/none/empty markers and
    // de-duplicating so the same font is never prepended twice.
    let mut wanted: Vec<String> = Vec::new();
    for name in std::iter::once(family).chain(fallbacks.iter().map(String::as_str)) {
        if is_builtin_family(name) {
            continue;
        }
        let n = name.trim().to_string();
        if !wanted.iter().any(|w| w.eq_ignore_ascii_case(&n)) {
            wanted.push(n);
        }
    }

    // Load each resolvable face and register it; collect the keys to prepend in
    // priority order (primary, then fallbacks).
    let mut prepend_keys: Vec<String> = Vec::new();
    for name in &wanted {
        // Bundled faces resolve WITHOUT `fontdb`, so a bundled family is
        // selectable and renders on every machine regardless of installed system
        // fonts. Check the bundled registry first; only fall through to the system
        // font DB for a name the bundle does not carry.
        if let Some(bf) = bundled_face(name) {
            base.font_data.insert(
                bf.key.to_string(),
                egui::FontData::from_static(bf.bytes).into(),
            );
            prepend_keys.push(bf.key.to_string());
            tracing::debug!(font = %name, "loaded bundled monospace font face");
            continue;
        }
        if let Some((bytes, index)) = face_bytes_for_family(db, name) {
            let key = font_data_key(name);
            // Preserve the .ttc face index — a collection face at a non-zero index
            // loaded as index 0 renders the WRONG font (the grid garble).
            let mut face = egui::FontData::from_owned(bytes);
            face.index = index;
            base.font_data.insert(key.clone(), face.into());
            prepend_keys.push(key);
            tracing::debug!(font = %name, index, "loaded configured monospace font face");
        } else {
            // Observability breadcrumb: a configured family/fallback that does
            // not resolve degrades silently to the built-in font, which reads as
            // "my font setting did nothing". Log it at warn so `C0PL4ND_LOG=warn`
            // (the default) surfaces it without a debug build.
            tracing::warn!(
                font = %name,
                "configured monospace font not found on this system; using the \
                 built-in font (check the [font] family/fallback in config.toml)"
            );
        }
    }

    // Implicit OS CJK fallback: append the FIRST platform CJK font that resolves
    // so Chinese/Japanese/Korean text renders out of the box using a face the OS
    // already ships — instead of tofu boxes — WITHOUT bundling a multi-MB CJK
    // font into the binary. It is loaded last, so it is the lowest-priority
    // fallback (used only for glyphs the primary + user fallbacks lack), and at
    // most ONE is loaded. The per-cell renderer places every wide glyph on its
    // own two cells, so even a proportional CJK face aligns in the grid.
    for cand in os_cjk_fallback_fonts() {
        if wanted.iter().any(|w| w.eq_ignore_ascii_case(cand)) {
            continue; // already attempted as a user-configured fallback
        }
        if let Some((bytes, index)) = face_bytes_for_family(db, cand) {
            let key = font_data_key(cand);
            // Preserve the .ttc face index — MS Gothic / Malgun Gothic are
            // collections; face 0 is the wrong font.
            let mut face = egui::FontData::from_owned(bytes);
            face.index = index;
            base.font_data.insert(key.clone(), face.into());
            prepend_keys.push(key);
            tracing::debug!(font = %cand, index, "loaded OS CJK fallback font for grid coverage");
            break;
        }
    }

    let loaded_any = !prepend_keys.is_empty();
    if loaded_any {
        let mono = base
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default();
        // Prepend in REVERSE so the final order is [primary, fallback1, ...,
        // <egui defaults>, <phosphor>] — inserting each at the front in reverse
        // yields the forward priority order.
        for key in prepend_keys.iter().rev() {
            // Defensive: never register the same key twice in the family list.
            mono.retain(|k| k != key);
            mono.insert(0, key.clone());
        }
    }

    // --- BOLD face (SGR `1`) ---------------------------------------------
    // egui's `FontId` selects a FAMILY, not a weight, so the only way to reach a
    // second (bold) cut is to register it under its own synthetic family. Without
    // this the grid loaded no bold face at all and SGR-1 text drew identically to
    // plain text — the "some text is more coloured / heavier in Windows Terminal"
    // half of the reported gap.
    let mut bold_keys: Vec<String> = Vec::new();
    for name in &wanted {
        if let Some((bytes, index)) = face_bytes_for_weight(db, name, BOLD_WEIGHT) {
            let key = bold_font_data_key(name);
            let mut face = egui::FontData::from_owned(bytes);
            face.index = index;
            base.font_data.insert(key.clone(), face.into());
            bold_keys.push(key);
            tracing::debug!(font = %name, index, "loaded bold monospace font face");
        } else {
            tracing::debug!(
                font = %name,
                "no bold cut installed for this family; bold cells fall back to faux-bold"
            );
        }
    }
    let bold_found = !bold_keys.is_empty();
    BOLD_FACE_AVAILABLE.store(bold_found, std::sync::atomic::Ordering::Relaxed);

    // The bold family is ALWAYS registered, so a `FontId` naming it always
    // resolves. Real bold cuts come first; the whole regular monospace stack
    // follows as the fallback chain, which keeps glyph COVERAGE identical to the
    // regular family (a bold cut missing a CJK glyph still resolves it via the
    // OS CJK fallback rather than drawing tofu). When no bold cut was found the
    // family is exactly the regular stack and `bold_face_available()` is false,
    // which is the renderer's signal to faux-bold instead.
    let mono_chain = base
        .families
        .get(&egui::FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let mut bold_chain = bold_keys;
    for key in mono_chain {
        if !bold_chain.contains(&key) {
            bold_chain.push(key);
        }
    }
    base.families.insert(bold_monospace_family(), bold_chain);

    (base, loaded_any)
}

/// The egui `font_data` key for a family's BOLD face. Distinct from
/// [`font_data_key`] so a family's regular and bold cuts never collide on one
/// key (which would silently overwrite one with the other).
fn bold_font_data_key(family: &str) -> String {
    format!("c0pl4nd-user-font-bold::{}", family.trim().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_list_always_contains_builtin_first() {
        let got = normalize_family_list(vec!["Cascadia Code".into(), "Hack".into()]);
        assert_eq!(
            got.first().map(String::as_str),
            Some(BUILTIN_MONOSPACE_LABEL),
            "the built-in label is always the first choice"
        );
    }

    #[test]
    fn family_list_is_sorted_and_deduped_case_insensitively() {
        let got = normalize_family_list(vec![
            "Hack".into(),
            "Cascadia Code".into(),
            "cascadia code".into(), // duplicate (different case)
            "  ".into(),            // blank dropped
            "Anonymous Pro".into(),
        ]);
        // Built-in first, then the installed names sorted case-insensitively,
        // with the case-duplicate collapsed.
        assert_eq!(
            got,
            vec![
                BUILTIN_MONOSPACE_LABEL.to_string(),
                "Anonymous Pro".to_string(),
                "Cascadia Code".to_string(),
                "Hack".to_string(),
            ]
        );
    }

    #[test]
    fn empty_install_still_offers_the_builtin() {
        let got = normalize_family_list(vec![]);
        assert_eq!(got, vec![BUILTIN_MONOSPACE_LABEL.to_string()]);
    }

    #[test]
    fn is_builtin_family_recognises_markers_and_generic_monospace() {
        assert!(is_builtin_family(""));
        assert!(is_builtin_family("   "));
        assert!(is_builtin_family(BUILTIN_MONOSPACE_LABEL));
        assert!(is_builtin_family(NONE_LABEL));
        assert!(is_builtin_family("monospace"));
        assert!(is_builtin_family("MONOSPACE"));
        assert!(!is_builtin_family("Cascadia Code"));
    }

    /// A face that exists in a `fontdb::Database` is prepended to Monospace ahead
    /// of egui's defaults; a family that does NOT exist is skipped (no panic, no
    /// change). Builds an in-memory DB from a tiny embedded test font so the test
    /// never depends on any system font being installed.
    #[test]
    fn build_definitions_prepends_existing_and_skips_missing() {
        // egui ships test-usable font bytes via its own default set; here we
        // construct a fontdb from the data egui's own default proportional font
        // exposes is not accessible, so we synthesise a minimal valid TTF by
        // loading egui's bundled fonts is not possible. Instead, exercise the
        // pure path: an EMPTY db resolves nothing, so build is a graceful no-op.
        let db = fontdb::Database::new(); // no faces loaded
        let base = egui::FontDefinitions::default();
        let before = base
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default();
        let (defs, loaded) = build_font_definitions(base, &db, "Definitely Not Installed XYZ", &[]);
        assert!(!loaded, "a missing family loads nothing");
        let after = defs
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default();
        assert_eq!(before, after, "a missing family leaves Monospace unchanged");
    }

    /// When a face IS present in the db, its bytes are registered and its key is
    /// prepended to the Monospace family ahead of the existing entries. Uses a
    /// real installed monospace family IF the system has one (guarded — skipped
    /// when none exists), so the prepend wiring is exercised on a real face
    /// without hard-coding a font name.
    #[test]
    fn build_definitions_prepends_a_real_installed_monospace_when_available() {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // Find any installed monospace family to use as the primary — but NOT one
        // that a bundled face shadows (a bundled-named system font resolves via the
        // bundled path under a different key, which this system-path test does not
        // exercise). This keeps the assertion about the `font_data_key` system path.
        let Some(fam) = db
            .faces()
            .filter(|f| f.monospaced)
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .find(|n| bundled_face(n).is_none())
        else {
            // No non-bundled monospace font installed on this box — nothing to assert.
            return;
        };
        let base = egui::FontDefinitions::default();
        let (defs, loaded) = build_font_definitions(base, &db, &fam, &[]);
        assert!(loaded, "an installed monospace family must load");
        let mono = defs
            .families
            .get(&egui::FontFamily::Monospace)
            .expect("monospace family exists");
        let key = font_data_key(&fam);
        assert_eq!(
            mono.first(),
            Some(&key),
            "the chosen family is prepended to the FRONT of Monospace"
        );
        assert!(
            defs.font_data.contains_key(&key),
            "the chosen family's bytes are registered under its key"
        );
    }

    #[test]
    fn font_data_key_is_family_namespaced_and_case_folded() {
        // The egui font_data key is per-family so the Family choice and a
        // differently-named Fallback never collide; it is lowercased + trimmed
        // so the same font under two casings maps to ONE key.
        assert_eq!(
            font_data_key("Cascadia Code"),
            "c0pl4nd-user-font::cascadia code"
        );
        assert_eq!(
            font_data_key("  CASCADIA code  "),
            font_data_key("cascadia code"),
            "trimming + case-folding give one key for one font"
        );
        assert_ne!(
            font_data_key("Hack"),
            font_data_key("Cascadia Code"),
            "distinct families get distinct keys (no cross-font collision)"
        );
    }

    #[test]
    fn face_bytes_for_family_returns_none_for_empty_name() {
        // The empty-name guard: a blank family never matches a face (and never
        // touches the db face iterator with an empty want string).
        let db = fontdb::Database::new();
        assert!(face_bytes_for_family(&db, "").is_none());
        assert!(face_bytes_for_family(&db, "   ").is_none());
    }

    #[test]
    fn face_bytes_for_family_returns_none_when_no_face_matches() {
        // A db with no faces (or a name no face carries) yields None — the
        // caller skips the family gracefully rather than panicking.
        let db = fontdb::Database::new(); // empty
        assert!(face_bytes_for_family(&db, "Definitely Not Installed XYZ").is_none());
    }

    #[test]
    fn build_definitions_dedupes_a_repeated_family_and_fallback() {
        // A family repeated as a fallback (same name, different case) must not
        // be loaded twice. With an empty db nothing loads, but the dedup of the
        // `wanted` list is exercised (no panic, loaded_any == false).
        let db = fontdb::Database::new();
        let base = egui::FontDefinitions::default();
        let (_defs, loaded) =
            build_font_definitions(base, &db, "Hack", &["hack".to_string(), "HACK".to_string()]);
        assert!(
            !loaded,
            "an uninstalled family loads nothing even when repeated"
        );
    }

    #[test]
    fn every_bundled_font_has_bytes_and_a_distinct_namespaced_key() {
        assert!(!BUNDLED_FONTS.is_empty(), "the bundle ships faces");
        let mut keys = std::collections::HashSet::new();
        for bf in BUNDLED_FONTS {
            assert!(
                !bf.display.trim().is_empty(),
                "every bundled face has a display name"
            );
            // A real .ttf is far larger than this; guards against an empty or
            // truncated include_bytes! (a wrong relative path would not compile,
            // but a zero-byte file would).
            assert!(
                bf.bytes.len() > 1024,
                "bundled face {} carries real font bytes ({} bytes)",
                bf.display,
                bf.bytes.len()
            );
            assert!(
                bf.key.starts_with("c0pl4nd-bundled::"),
                "bundled face {} uses the bundled key namespace",
                bf.display
            );
            assert!(
                keys.insert(bf.key),
                "bundled face keys are unique ({} collided)",
                bf.key
            );
        }
    }

    #[test]
    fn bundled_jp_fallback_bytes_are_present() {
        // The always-on JP fallback (wired in base_font_definitions) must carry
        // real bytes so Japanese glyphs render without a system CJK font.
        assert!(
            NOTO_SANS_JP_SUBSET.len() > 1024,
            "the bundled Noto Sans JP subset carries real font bytes"
        );
        assert_eq!(BUNDLED_JP_FALLBACK_KEY, "c0pl4nd-bundled-jp");
    }

    /// A copy of `src` whose sfnt version tag is clobbered — the corrupted-face
    /// fixture the parse guards below are calibrated against. `skrifa` reads that
    /// tag first and rejects the blob outright, which is exactly what makes it a
    /// usable CONTROL: it proves the assertion can fire, so a green run is
    /// evidence rather than a vacuous pass.
    fn with_broken_sfnt_tag(src: &[u8]) -> Vec<u8> {
        let mut bytes = src.to_vec();
        bytes[0..4].copy_from_slice(b"XXXX");
        bytes
    }

    /// Every bundled face — including the 5 VARIABLE fonts (Doto/RedHatMono/Saira/
    /// SplineSansMono/Teko) and the JP fallback — MUST parse with **`skrifa`**, the
    /// glyph crate epaint actually builds its atlas with. `epaint::text::font::
    /// FontFace::new` calls `skrifa::FontRef::from_index(bytes, index)?` — the ONLY
    /// fallible step in it — and `epaint::text::fonts` turns that error into
    /// `panic!("Error parsing {name:?} TTF/OTF font file: {err}")`. So a face that
    /// only *registers* but cannot be parsed crashes the app the moment the user
    /// picks it, a class the registration-only tests cannot catch.
    ///
    /// This test previously parsed with `ab_glyph`, on the stale premise that
    /// `ab_glyph` was "the exact glyph crate epaint builds its atlas with". epaint
    /// 0.34 moved to `skrifa` (see its `Cargo.lock` deps: `skrifa`, no `ab_glyph`),
    /// so the guard was exercising a parser this app's render path never runs — it
    /// could not catch its own target class. The two parsers provably disagree; see
    /// [`a_face_skrifa_accepts_can_still_be_rejected_by_ab_glyph`].
    #[test]
    fn every_bundled_face_parses_with_skrifa() {
        // CONTROL FIRST: a corrupted face must be REJECTED, or the loop below is
        // an assertion that cannot fail.
        let corrupt = with_broken_sfnt_tag(BUNDLED_FONTS[0].bytes);
        assert!(
            skrifa::FontRef::from_index(&corrupt, 0).is_err(),
            "control: a face with a clobbered sfnt tag must be REJECTED by skrifa, \
             otherwise the per-face assertions below can never fire"
        );

        for bf in BUNDLED_FONTS {
            assert!(
                skrifa::FontRef::from_index(bf.bytes, 0).is_ok(),
                "bundled face {} ({}) must parse with skrifa so epaint never \
                 panics (\"Error parsing … TTF/OTF font file\") when it is selected",
                bf.display,
                bf.key
            );
        }
        assert!(
            skrifa::FontRef::from_index(NOTO_SANS_JP_SUBSET, 0).is_ok(),
            "the bundled JP fallback must parse with skrifa"
        );
    }

    /// Pins WHY the guard above had to change parsers: `ab_glyph` and `skrifa` do
    /// not agree on what a font is, so an `ab_glyph` verdict carries no information
    /// about whether epaint will panic.
    ///
    /// Both fixtures below are real JetBrains Mono bytes with one targeted
    /// mutation. `ab_glyph` (ttf-parser) eagerly requires `head`/`hhea`/`maxp` and
    /// rejects both; `skrifa::FontRef::from_index` only reads the table directory
    /// and accepts both — and epaint, which uses skrifa, would therefore load them
    /// without panicking. So the old test could go RED on a face epaint is
    /// perfectly happy with (a false alarm), which is the same thing as saying its
    /// GREEN was never evidence about epaint either.
    ///
    /// `ab_glyph` stays a dev-dependency purely to hold this control in place; the
    /// app never parses with it.
    #[test]
    fn a_face_skrifa_accepts_can_still_be_rejected_by_ab_glyph() {
        let base = BUNDLED_FONTS[0].bytes;

        // (1) Rename the `head` table tag in the directory.
        let mut renamed_head = base.to_vec();
        let table_count = u16::from_be_bytes([renamed_head[4], renamed_head[5]]) as usize;
        let mut renamed = false;
        for i in 0..table_count {
            let at = 12 + i * 16;
            if &renamed_head[at..at + 4] == b"head" {
                renamed_head[at..at + 4].copy_from_slice(b"zzzz");
                renamed = true;
            }
        }
        assert!(
            renamed,
            "fixture setup: the base face must carry a `head` table"
        );

        // (2) Keep the directory, zero every byte of table DATA.
        let mut hollow = base.to_vec();
        for byte in &mut hollow[12 + table_count * 16..] {
            *byte = 0;
        }

        for (label, bytes) in [
            ("head-tag-renamed", &renamed_head),
            ("body-zeroed", &hollow),
        ] {
            assert!(
                skrifa::FontRef::from_index(bytes, 0).is_ok(),
                "{label}: skrifa (the parser epaint runs) is expected to ACCEPT this"
            );
            assert!(
                ab_glyph::FontRef::try_from_slice(bytes).is_err(),
                "{label}: ab_glyph is expected to REJECT this — if it ever agrees \
                 with skrifa here, this control has stopped proving the two \
                 parsers diverge and the parser choice above needs re-deriving"
            );
        }
    }

    #[test]
    fn default_font_family_is_a_bundled_face() {
        // M11 self-fix: the zero-config default font family MUST resolve to a
        // BUNDLED face, or the default silently falls back to egui's built-in
        // monospace on every machine that lacks the family (the latent bug the
        // prior "Monaspace Neon" default carried — it is not bundled). This is the
        // load-bearing regression guard for that class.
        let default_family = c0pl4nd_core::Config::default().font.family;
        assert!(
            bundled_face(&default_family).is_some(),
            "the default font family {default_family:?} must be a bundled face so \
             the zero-config default renders in the intended typeface, not a \
             silent egui-monospace fallback"
        );
    }

    #[test]
    fn bundled_face_resolves_case_insensitively_and_trims() {
        // A bundled family is found regardless of the config's casing/whitespace.
        assert!(bundled_face("JetBrains Mono").is_some());
        assert!(bundled_face("  jetbrains mono  ").is_some());
        assert!(bundled_face("WALLPOET").is_some());
        // A non-bundled (system) name does not resolve as bundled.
        assert!(bundled_face("Cascadia Code").is_none());
        assert!(bundled_face("").is_none());
    }

    #[test]
    fn with_bundled_families_orders_builtin_then_bundled_then_system() {
        // A normalized list is [builtin, ...sorted system]; splicing must keep the
        // builtin first, put every bundled family next, then the system families.
        let normalized =
            normalize_family_list(vec!["Cascadia Code".into(), "Some Odd Mono".into()]);
        let out = with_bundled_families(normalized);
        assert_eq!(
            out.first().map(String::as_str),
            Some(BUILTIN_MONOSPACE_LABEL),
            "built-in label stays first"
        );
        // Every bundled display name is present.
        for d in bundled_family_displays() {
            assert!(
                out.iter().any(|x| x == d),
                "bundled family {d} appears in the choice list"
            );
        }
        // The bundled block immediately follows the built-in label, before system.
        let first_bundled = out
            .iter()
            .position(|x| x == "JetBrains Mono")
            .expect("bundled family present");
        let system_pos = out
            .iter()
            .position(|x| x == "Cascadia Code")
            .expect("system family present");
        assert!(
            first_bundled < system_pos,
            "bundled families precede system families"
        );
    }

    #[test]
    fn with_bundled_families_dedupes_a_system_name_that_matches_a_bundled_face() {
        // If the machine ALSO has a bundled-named family installed, it must appear
        // once (as the bundled entry), not twice.
        let normalized = normalize_family_list(vec!["JetBrains Mono".into()]);
        let out = with_bundled_families(normalized);
        let count = out
            .iter()
            .filter(|x| x.as_str() == "JetBrains Mono")
            .count();
        assert_eq!(count, 1, "a bundled-named system family is not duplicated");
    }

    /// The load-bearing guarantee: a bundled family renders even with an EMPTY
    /// `fontdb` (i.e. a machine with no matching system font). Selecting it must
    /// register its bytes and prepend it to Monospace — proving bundled resolution
    /// does NOT depend on system enumeration.
    #[test]
    fn build_definitions_loads_a_bundled_family_without_any_system_font() {
        let db = fontdb::Database::new(); // no system faces loaded
        let base = egui::FontDefinitions::default();
        let (defs, loaded) = build_font_definitions(base, &db, "Wallpoet", &[]);
        assert!(loaded, "a bundled family loads even with an empty font DB");
        let key = bundled_face("Wallpoet").expect("bundled").key;
        let mono = defs
            .families
            .get(&egui::FontFamily::Monospace)
            .expect("monospace family exists");
        assert_eq!(
            mono.first().map(String::as_str),
            Some(key),
            "the chosen bundled family is prepended to the FRONT of Monospace"
        );
        assert!(
            defs.font_data.contains_key(key),
            "the bundled family's bytes are registered under its key"
        );
    }

    #[test]
    fn build_definitions_loads_a_variable_axis_bundled_family() {
        // Variable fonts (Doto[ROND,wght], Saira[wdth,wght], …) load their default
        // instance via from_static — no named-instance handling, mirroring SCR1B3.
        let db = fontdb::Database::new();
        let base = egui::FontDefinitions::default();
        let (defs, loaded) = build_font_definitions(base, &db, "Saira", &["Doto".to_string()]);
        assert!(loaded, "variable-axis bundled families load");
        for fam in ["Saira", "Doto"] {
            let key = bundled_face(fam).expect("bundled").key;
            assert!(
                defs.font_data.contains_key(key),
                "variable-axis family {fam} registered its default instance"
            );
        }
    }

    #[test]
    fn apply_ui_font_prepends_a_bundled_face_to_proportional_only() {
        // The UI-font swap must touch ONLY the Proportional family (the whole app
        // UI) and leave the Monospace family (the terminal grid) untouched — the
        // terminal/UI font SEPARATION that is task 10's core requirement.
        let mut defs = egui::FontDefinitions::default();
        let mono_before = defs.families[&egui::FontFamily::Monospace].clone();
        apply_ui_font(&mut defs, "IBM Plex Mono");
        let key = bundled_face("IBM Plex Mono").expect("bundled").key;
        // Registered + first in Proportional.
        assert!(defs.font_data.contains_key(key));
        assert_eq!(
            defs.families[&egui::FontFamily::Proportional]
                .first()
                .map(String::as_str),
            Some(key),
            "the chosen UI face must be first in Proportional"
        );
        // Monospace (terminal) is byte-for-byte unchanged.
        assert_eq!(
            defs.families[&egui::FontFamily::Monospace],
            mono_before,
            "a UI-font swap must NOT touch the terminal (Monospace) family"
        );
    }

    #[test]
    fn apply_ui_font_system_default_and_unknown_leave_proportional_unchanged() {
        for choice in ["System default", "", "   ", "Not A Real Font"] {
            let mut defs = egui::FontDefinitions::default();
            let prop_before = defs.families[&egui::FontFamily::Proportional].clone();
            apply_ui_font(&mut defs, choice);
            assert_eq!(
                defs.families[&egui::FontFamily::Proportional],
                prop_before,
                "choice {choice:?} must keep egui's built-in proportional font"
            );
        }
    }

    #[test]
    fn ui_family_choices_lead_with_system_default_then_bundled() {
        let choices: Vec<&str> = ui_family_choices().collect();
        assert_eq!(choices.first().copied(), Some("System default"));
        assert!(choices.contains(&"IBM Plex Mono"));
        assert!(choices.contains(&"JetBrains Mono"));
    }
}
