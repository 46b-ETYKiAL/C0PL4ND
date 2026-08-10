//! Headless egui-shell unit tests, split out of `egui_app/mod.rs`
//! (F6-1 decomposition) — pure structural move, no test changes.

use super::*;

#[test]
fn bootstrap_opens_with_initial_panes() {
    let app = C0pl4ndApp::bootstrap();
    assert_eq!(app.pane_count(), INITIAL_PANES);
    assert!(!app.settings_is_open());
}

/// The app boots windowed: the transient fullscreen flag (#36) is never
/// persisted, so a fresh app is always non-fullscreen.
#[test]
fn bootstrap_is_not_fullscreen() {
    assert!(!C0pl4ndApp::bootstrap().fullscreen());
}

/// Splitting beyond the pane cap is refused with a PLAIN, actionable toast —
/// not the old terse "max N panes" internal-constant phrasing (inventory
/// C0-015). Drives the REAL `split` path and observes the real toast.
#[test]
fn splitting_past_the_pane_cap_shows_a_plain_toast() {
    let mut app = C0pl4ndApp::bootstrap();
    while app.pane_count() < grid::MAX_PANES {
        app.split(egui_tiles::LinearDir::Horizontal);
    }
    assert_eq!(app.pane_count(), grid::MAX_PANES, "filled to the cap");
    app.toast = None;

    // One split too many: refused, with a user-facing explanation + recovery.
    app.split(egui_tiles::LinearDir::Horizontal);
    assert_eq!(app.pane_count(), grid::MAX_PANES, "the cap held");
    let toast = app.toast.as_deref().expect("a refused split shows a toast");
    assert!(
        toast.contains(&format!("maximum of {} panes", grid::MAX_PANES)),
        "toast: {toast}"
    );
    assert!(
        toast.contains("Close one"),
        "toast carries a recovery step: {toast}"
    );
    // No leaked internal-constant phrasing.
    assert!(!toast.starts_with("max "), "toast: {toast}");
}

/// #35: PowerShell uses the call operator `& "<path>"`; cmd / a Windows
/// "Default shell" uses a plain double-quoted path; POSIX shells use a
/// single-quoted path with the `'\''` escape. Keyed off the active shell
/// LABEL so the form matches what the user's shell actually expects.
#[test]
fn quote_path_for_shell_matches_the_active_shell() {
    let p = std::path::Path::new("/tmp/my script.sh");
    // PowerShell → call operator (matched by the label containing "PowerShell").
    assert_eq!(
        quote_path_for_shell(p, "PowerShell 7"),
        "& \"/tmp/my script.sh\"",
        "PowerShell must use the call operator so the path is invoked, not echoed"
    );
    assert_eq!(
        quote_path_for_shell(p, "Windows PowerShell"),
        "& \"/tmp/my script.sh\"",
        "Windows PowerShell uses the same call-operator form"
    );
    // A PowerShell path with an embedded double-quote backtick-escapes it.
    assert_eq!(
        quote_path_for_shell(std::path::Path::new(r#"C:\a"b.ps1"#), "PowerShell 7"),
        "& \"C:\\a`\"b.ps1\"",
        "PowerShell escapes an embedded double-quote with a backtick"
    );
    // Non-PowerShell on the host platform: Windows → cmd double-quote; POSIX
    // → single-quote. Assert the branch that matches THIS build's `cfg`.
    let cmd_or_posix = quote_path_for_shell(p, "Default shell");
    if cfg!(windows) {
        assert_eq!(
            cmd_or_posix, "\"/tmp/my script.sh\"",
            "a non-PowerShell Windows shell (cmd) uses a plain double-quoted path"
        );
    } else {
        assert_eq!(
            cmd_or_posix, "'/tmp/my script.sh'",
            "a POSIX shell uses a single-quoted path"
        );
    }
}

#[cfg(not(windows))]
#[test]
fn quote_path_for_shell_posix_escapes_single_quote() {
    // POSIX `'\''` escape: a path with an embedded single quote.
    assert_eq!(
        quote_path_for_shell(std::path::Path::new("/x/it's.sh"), "Bash"),
        "'/x/it'\\''s.sh'",
        "an embedded single quote is escaped as '\\'' for POSIX shells"
    );
}

/// The per-glyph cache key is content+pass+style sensitive and stable: the same
/// glyph+colour+pass+style hashes equal (a cache HIT, shared across cells); any
/// change to the glyph, its colour, the pass, or the style seed changes the key.
#[test]
fn glyph_cache_key_is_content_pass_and_style_sensitive() {
    let style = row_style_key(14.0, (200, 200, 200));
    let base = glyph_cache_key(
        'a',
        (255, 0, 0),
        RowPass::Main,
        style,
        glyph_cache::GlyphAttrs::default(),
    );
    assert_eq!(
        base,
        glyph_cache_key(
            'a',
            (255, 0, 0),
            RowPass::Main,
            style,
            glyph_cache::GlyphAttrs::default()
        ),
        "identical glyph+colour+pass+style → same key (a cache HIT, reused per cell)"
    );
    assert_ne!(
        base,
        glyph_cache_key(
            'b',
            (255, 0, 0),
            RowPass::Main,
            style,
            glyph_cache::GlyphAttrs::default()
        ),
        "a different glyph must change the key"
    );
    assert_ne!(
        base,
        glyph_cache_key(
            'a',
            (0, 255, 0),
            RowPass::Main,
            style,
            glyph_cache::GlyphAttrs::default()
        ),
        "a colour change must change the key"
    );
    assert_ne!(
        base,
        glyph_cache_key(
            'a',
            (255, 0, 0),
            RowPass::GhostRed,
            style,
            glyph_cache::GlyphAttrs::default()
        ),
        "a different pass (chromatic ghost) must change the key"
    );
    let style2 = row_style_key(18.0, (200, 200, 200));
    assert_ne!(
        base,
        glyph_cache_key(
            'a',
            (255, 0, 0),
            RowPass::Main,
            style2,
            glyph_cache::GlyphAttrs::default()
        ),
        "a font-size change must change the key"
    );
}

/// `system_font_load_needed` (audit #3) is true only when a non-built-in
/// family or fallback is configured — the gate that decides whether the
/// off-thread system-font load runs.
#[test]
fn system_font_load_needed_tracks_custom_families() {
    let mut font = c0pl4nd_core::config::FontConfig {
        family: "monospace".to_string(),
        // Clear the default fallback list (which names CJK system faces) so
        // this case isolates the FAMILY gate.
        fallback: Vec::new(),
        ..Default::default()
    };
    // A built-in family with no custom fallback needs no system load.
    if fonts::is_builtin_family(&font.family) {
        assert!(
            !system_font_load_needed(&font),
            "a built-in family with built-in fallbacks needs no system-font load"
        );
    }
    // A built-in family BUT with a custom fallback DOES need the load (the
    // default config's exact shape — the audit #3 case).
    font.fallback = vec!["Some Custom CJK Face".to_string()];
    assert!(
        system_font_load_needed(&font),
        "a custom fallback alone requires the system-font DB load"
    );
    font.fallback = Vec::new();
    // A clearly-custom family always needs the load.
    font.family = "Some Custom Face That Is Not Built In".to_string();
    assert!(
        system_font_load_needed(&font),
        "a non-built-in family requires the system-font DB load"
    );
}

/// A short title passes through unchanged (trimmed); a title longer than the
/// cap is shortened to exactly `MAX_TAB_TITLE` chars plus a `…` suffix, so a
/// verbose program title cannot blow out the tab strip.
#[test]
fn cap_tab_title_trims_and_truncates() {
    assert_eq!(
        C0pl4ndApp::cap_tab_title("  vim  "),
        "vim",
        "a short title is trimmed and passed through verbatim"
    );
    // Exactly at the cap → no ellipsis.
    let at_cap: String = "a".repeat(C0pl4ndApp::MAX_TAB_TITLE);
    assert_eq!(
        C0pl4ndApp::cap_tab_title(&at_cap),
        at_cap,
        "a title exactly at the cap is not truncated"
    );
    // One over the cap → truncated to MAX_TAB_TITLE chars + ellipsis.
    let over_cap: String = "b".repeat(C0pl4ndApp::MAX_TAB_TITLE + 5);
    let capped = C0pl4ndApp::cap_tab_title(&over_cap);
    assert_eq!(
        capped.chars().count(),
        C0pl4ndApp::MAX_TAB_TITLE + 1,
        "an over-length title keeps MAX_TAB_TITLE chars plus one ellipsis char"
    );
    assert!(
        capped.ends_with('…') && capped.starts_with('b'),
        "the truncated title keeps the leading chars and ends with an ellipsis"
    );
}

/// `scrub_display_text` removes the bidi/zero-width/control spoofing set but
/// preserves ordinary printable text, including non-ASCII printable glyphs.
#[test]
fn scrub_display_text_strips_dangerous_and_keeps_printable() {
    // RLO bidi override (the classic "evil.com\u{202E}gpj.exe" spoof).
    assert_eq!(
        scrub_display_text("evil.com\u{202E}gpj.exe"),
        "evil.comgpj.exe",
        "the RLO bidi override is removed"
    );
    // Zero-width space.
    assert_eq!(
        scrub_display_text("ab\u{200B}cd"),
        "abcd",
        "the zero-width space is removed"
    );
    // A bidi isolate (FSI here, in the U+2066..=U+2069 range).
    assert_eq!(
        scrub_display_text("x\u{2066}y\u{2069}z"),
        "xyz",
        "bidi isolates are removed"
    );
    // A C0 control (BEL).
    assert_eq!(
        scrub_display_text("title\u{07}here"),
        "titlehere",
        "the C0 BEL control char is removed"
    );
    // The whole dangerous set at once, plus other family members.
    assert_eq!(
        scrub_display_text(
            "\u{202A}\u{202D}\u{200E}\u{200F}\u{200C}\u{200D}\u{FEFF}\u{2068}clean\u{0000}\u{009F}"
        ),
        "clean",
        "embeddings, marks, joiners, BOM, isolate, NUL, and C1 are all removed"
    );
    // Printable text — ASCII, accented Latin, and CJK — is PRESERVED.
    assert_eq!(
        scrub_display_text("café 日本語 ~/projects"),
        "café 日本語 ~/projects",
        "ordinary printable text including non-ASCII is preserved verbatim"
    );
    // A clean string is returned unchanged.
    assert_eq!(scrub_display_text("vim"), "vim", "clean input is a no-op");
}

/// The scrub is applied at the `cap_tab_title` display boundary, so a tab
/// label can never carry a bidi/zero-width/control spoof even before
/// trimming and capping run.
#[test]
fn cap_tab_title_scrubs_spoofing_chars() {
    assert_eq!(
        C0pl4ndApp::cap_tab_title("  evil.com\u{202E}gpj.exe\u{200B}  "),
        "evil.comgpj.exe",
        "the tab label is scrubbed of bidi/zero-width chars then trimmed"
    );
}

/// Two panes whose shells set the SAME OSC title still get DISTINCT
/// accessible tab labels. The visible tab text may collide (real terminals
/// allow two same-named tabs), but the accessibility tree — and the
/// `get_by_label` lookups the interaction tests rely on — must never have
/// two nodes sharing one name. Every label is anchored on the unique
/// `pane {id}`. Regression guard for the Windows-CI failure where both
/// bootstrap shells set the same cwd title and the tab lookup went ambiguous.
#[test]
fn tab_a11y_label_is_unique_even_when_titles_collide() {
    // Identical display title for two different panes → distinct labels.
    let a = C0pl4ndApp::tab_a11y_label(PaneId(0), "make");
    let b = C0pl4ndApp::tab_a11y_label(PaneId(1), "make");
    assert_ne!(
        a, b,
        "colliding titles must still yield distinct accessible labels"
    );
    assert_eq!(a, "make (pane 0)");
    assert_eq!(b, "make (pane 1)");
    // WCAG 2.5.3 "Label in Name": the visible title is a prefix of the label.
    assert!(
        a.starts_with("make"),
        "the title leads the accessible label"
    );
    // The untitled fallback is already unique → not doubled into
    // "pane 2 (pane 2)".
    assert_eq!(
        C0pl4ndApp::tab_a11y_label(PaneId(2), "pane 2"),
        "pane 2",
        "the bare pane-id fallback carries no redundant suffix"
    );
}

/// A pane whose running program has not set an OSC title falls back to the
/// generic `pane {id}` label — so untitled panes read exactly as before this
/// feature, keeping the visual-order tab strip stable. (A fresh bootstrap
/// shell has not emitted a title escape, so every label is the fallback.)
#[test]
fn pane_titles_fall_back_to_pane_id_without_osc_title() {
    let app = C0pl4ndApp::bootstrap();
    let titles = app.pane_titles();
    assert_eq!(titles.len(), app.pane_count());
    for (id, label) in titles {
        assert_eq!(
            label,
            format!("pane {}", id.raw()),
            "an untitled pane must use the pane-id fallback label"
        );
    }
}

#[test]
fn grid_text_origin_insets_by_padding() {
    // The grid text origin is the pane top-left inset by the padding on
    // BOTH axes; a larger padding moves it further into the pane.
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 300.0));
    assert_eq!(
        grid_text_origin(rect, 8.0),
        egui::pos2(18.0, 28.0),
        "padding must inset the origin from the pane top-left on both axes"
    );
    let near = grid_text_origin(rect, 4.0);
    let far = grid_text_origin(rect, 16.0);
    assert!(
        far.x > near.x && far.y > near.y,
        "a larger padding must move the origin further into the pane"
    );
}

#[test]
fn grid_text_origin_clamps_negative_padding() {
    // A bad (negative) config can never push the origin outside the pane —
    // it clamps to the pane top-left (zero inset).
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
    assert_eq!(
        grid_text_origin(rect, -5.0),
        rect.left_top(),
        "negative padding must clamp to zero inset (origin == pane top-left)"
    );
}

#[test]
fn cell_at_pos_maps_pointer_to_grid_cell() {
    // Origin (10,20), 8×16-point cells. A point inside cell (row 2, col 3)
    // maps to it; a point above/left of the origin is off-grid → None.
    let origin = egui::pos2(10.0, 20.0);
    let (cw, ch) = (8.0, 16.0);
    // Cell (2,3) spans x∈[34,42), y∈[52,68); pick a point inside.
    assert_eq!(
        cell_at_pos(egui::pos2(36.0, 60.0), origin, cw, ch),
        Some((2, 3))
    );
    // Exactly on the origin → cell (0,0).
    assert_eq!(cell_at_pos(origin, origin, cw, ch), Some((0, 0)));
    // Left of / above the grid → off-grid.
    assert_eq!(cell_at_pos(egui::pos2(9.0, 60.0), origin, cw, ch), None);
    assert_eq!(cell_at_pos(egui::pos2(36.0, 19.0), origin, cw, ch), None);
    // Degenerate cell size never divides by zero.
    assert_eq!(cell_at_pos(egui::pos2(36.0, 60.0), origin, 0.0, ch), None);
}

// ---- drag-select autoscroll rate + edge clamping ----------------------------
//
// The geometry half of drag-select autoscroll. The behavioural half — that the
// view actually scrolls AND the selection extends over the newly revealed lines
// — is driven end-to-end through the real frame loop in
// `crates/app/tests/egui_drag_autoscroll.rs`.

#[test]
fn autoscroll_is_silent_while_the_pointer_is_inside_the_grid() {
    // Grid spans y ∈ [20, 180) (10 rows of 16pt). Anywhere inside — including
    // exactly on either boundary — must not scroll: autoscroll is an
    // edge-OVERSHOOT behaviour, not a side effect of dragging.
    let (top, bottom, ch) = (20.0, 180.0, 16.0);
    assert_eq!(autoscroll_lines(100.0, top, bottom, ch), 0, "mid-grid");
    assert_eq!(autoscroll_lines(top, top, bottom, ch), 0, "on the top edge");
    assert_eq!(
        autoscroll_lines(bottom, top, bottom, ch),
        0,
        "on the bottom edge"
    );
}

#[test]
fn autoscroll_direction_matches_the_edge_the_pointer_left() {
    // Positive == back into history (`scroll_view(+n)`), so dragging ABOVE the
    // top must be positive and BELOW the bottom negative. A swapped sign would
    // scroll the view AWAY from the content the user is reaching for.
    let (top, bottom, ch) = (20.0, 180.0, 16.0);
    assert!(
        autoscroll_lines(top - 1.0, top, bottom, ch) > 0,
        "above the top scrolls BACK into history"
    );
    assert!(
        autoscroll_lines(bottom + 1.0, top, bottom, ch) < 0,
        "below the bottom scrolls FORWARD toward the live bottom"
    );
}

#[test]
fn autoscroll_rate_scales_with_the_overshoot_and_is_capped() {
    // One row past → 1 line; three rows past → 3; a flung pointer saturates at
    // the cap instead of teleporting across the whole history.
    let (top, bottom, ch) = (20.0, 180.0, 16.0);
    assert_eq!(
        autoscroll_lines(bottom + 1.0, top, bottom, ch),
        -1,
        "a single pixel of overshoot still moves one line"
    );
    assert_eq!(autoscroll_lines(bottom + ch, top, bottom, ch), -1);
    assert_eq!(
        autoscroll_lines(bottom + 3.0 * ch, top, bottom, ch),
        -3,
        "three rows of overshoot must move three lines, not one"
    );
    assert_eq!(
        autoscroll_lines(top - 3.0 * ch, top, bottom, ch),
        3,
        "the same scaling applies above the top edge"
    );
    assert_eq!(
        autoscroll_lines(bottom + 10_000.0, top, bottom, ch),
        -AUTOSCROLL_MAX_LINES,
        "a far-flung pointer clamps to the per-frame cap"
    );
    assert_eq!(
        autoscroll_lines(top - 10_000.0, top, bottom, ch),
        AUTOSCROLL_MAX_LINES
    );
}

#[test]
fn autoscroll_never_divides_by_zero_or_wraps_on_degenerate_input() {
    // Degenerate cell height / inverted span / non-finite pointer are all
    // "no scroll", never a panic and never a saturated negative from a bad cast.
    assert_eq!(
        autoscroll_lines(0.0, 20.0, 180.0, 0.0),
        0,
        "zero cell height"
    );
    assert_eq!(
        autoscroll_lines(0.0, 20.0, 180.0, -4.0),
        0,
        "negative height"
    );
    assert_eq!(autoscroll_lines(0.0, 180.0, 20.0, 16.0), 0, "inverted span");
    assert_eq!(autoscroll_lines(f32::NAN, 20.0, 180.0, 16.0), 0);
    assert_eq!(
        autoscroll_lines(f32::NEG_INFINITY, 20.0, 180.0, 16.0),
        0,
        "an infinite pointer is not a scroll request"
    );
}

#[test]
fn clamping_maps_an_off_grid_pointer_to_the_nearest_edge_cell() {
    // Origin (10,20), 8×16 cells, a 4-col × 10-row grid. Inside, the clamped
    // mapping agrees with `cell_at_pos`; outside, it pins to the edge cell
    // instead of returning None (above/left) or a row that does not exist
    // (below/right) — the latter would place the selection head off the grid.
    let origin = egui::pos2(10.0, 20.0);
    let (cw, ch) = (8.0, 16.0);
    let (cols, rows) = (4, 10);
    assert_eq!(
        clamp_pos_to_grid_cell(egui::pos2(36.0, 60.0), origin, cw, ch, cols, rows),
        Some((2, 3)),
        "inside the grid it agrees with the unclamped hit test"
    );
    assert_eq!(
        cell_at_pos(egui::pos2(36.0, 60.0), origin, cw, ch),
        Some((2, 3))
    );
    // Above / left of the origin — `cell_at_pos` gives up here, which is exactly
    // why a drag past the TOP edge used to freeze the selection head.
    assert_eq!(
        cell_at_pos(egui::pos2(-900.0, -900.0), origin, cw, ch),
        None
    );
    assert_eq!(
        clamp_pos_to_grid_cell(egui::pos2(-900.0, -900.0), origin, cw, ch, cols, rows),
        Some((0, 0)),
        "off the top-left pins to the first cell"
    );
    // Below / right of the last cell pins to the LAST cell, never past it.
    assert_eq!(
        clamp_pos_to_grid_cell(egui::pos2(9_000.0, 9_000.0), origin, cw, ch, cols, rows),
        Some((rows - 1, cols - 1)),
        "off the bottom-right pins to the last cell"
    );
    // A non-finite coordinate must not saturate into a bogus index.
    assert_eq!(
        clamp_pos_to_grid_cell(
            egui::pos2(f32::NAN, f32::INFINITY),
            origin,
            cw,
            ch,
            cols,
            rows
        ),
        Some((0, 0))
    );
    // Degenerate grids yield no cell at all rather than underflowing `count - 1`.
    assert_eq!(
        clamp_pos_to_grid_cell(egui::pos2(36.0, 60.0), origin, cw, ch, 0, rows),
        None
    );
    assert_eq!(
        clamp_pos_to_grid_cell(egui::pos2(36.0, 60.0), origin, cw, ch, cols, 0),
        None
    );
    assert_eq!(
        clamp_pos_to_grid_cell(egui::pos2(36.0, 60.0), origin, 0.0, ch, cols, rows),
        None
    );
}

#[test]
fn link_url_at_cell_matches_half_open_span() {
    // One link on row 0 spanning cols [4, 25). A col inside hits; the
    // exclusive end col does not; a different row does not.
    let links = vec![(
        CellSpan {
            line: 0,
            col_start: 4,
            col_end: 25,
        },
        "https://example.com".to_string(),
    )];
    assert_eq!(link_url_at_cell(&links, 0, 4), Some("https://example.com"));
    assert_eq!(link_url_at_cell(&links, 0, 24), Some("https://example.com"));
    assert_eq!(
        link_url_at_cell(&links, 0, 25),
        None,
        "end col is exclusive"
    );
    assert_eq!(link_url_at_cell(&links, 0, 3), None, "before the span");
    assert_eq!(link_url_at_cell(&links, 1, 10), None, "wrong row");
}

#[test]
fn link_span_at_cell_returns_the_hovered_span_geometry() {
    // The hover-underline affordance hit-tests with link_span_at_cell, which must
    // share link_url_at_cell's half-open semantics but return the SPAN (the
    // geometry the underline paints) rather than the URL string.
    let links = vec![(
        CellSpan {
            line: 0,
            col_start: 4,
            col_end: 25,
        },
        "https://example.com".to_string(),
    )];
    let hit = link_span_at_cell(&links, 0, 4).expect("col_start is inside");
    assert_eq!(
        (hit.line, hit.col_start, hit.col_end),
        (0, 4, 25),
        "returns the matched span's geometry"
    );
    assert!(
        link_span_at_cell(&links, 0, 24).is_some(),
        "last col is inside"
    );
    assert!(
        link_span_at_cell(&links, 0, 25).is_none(),
        "end col is exclusive (matches link_url_at_cell)"
    );
    assert!(link_span_at_cell(&links, 0, 3).is_none(), "before the span");
    assert!(link_span_at_cell(&links, 1, 10).is_none(), "wrong row");
}

#[test]
fn ctrl_click_on_a_seeded_url_records_it() {
    // Drive the SAME span-build + hit-test path a real Ctrl-click uses, over a
    // KNOWN seeded grid (PTY-independent). The URL "https://example.com" sits
    // at byte 4 on row 0 → char cols [4, 23).
    let mut app = C0pl4ndApp::bootstrap();
    app.test_seed_focused_grid("see https://example.com here\nplain line, no link");
    assert_eq!(app.last_opened_url(), None, "nothing opened yet");

    // A cell inside the URL span opens it and records it.
    let opened = app.test_open_url_at_cell(0, 8);
    assert_eq!(opened.as_deref(), Some("https://example.com"));
    assert_eq!(
        app.last_opened_url().as_deref(),
        Some("https://example.com"),
        "a Ctrl-click on a URL must record it as opened"
    );

    // A cell on the no-link line opens nothing (and does not clobber the
    // last-opened record).
    assert_eq!(app.test_open_url_at_cell(1, 2), None);
    assert_eq!(
        app.last_opened_url().as_deref(),
        Some("https://example.com"),
        "clicking a non-URL cell must not open or change the record"
    );
}

#[test]
fn update_notice_surfaces_as_a_toast_and_is_recorded() {
    let mut app = C0pl4ndApp::bootstrap();
    assert_eq!(app.last_update_notice(), None, "no notice at start");
    assert!(app.toast.is_none(), "no toast at start");
    app.apply_update_notice("C0PL4ND 9.9.9 is available".to_string());
    assert_eq!(
        app.last_update_notice().as_deref(),
        Some("C0PL4ND 9.9.9 is available"),
        "the notice is recorded (observable)"
    );
    assert_eq!(
        app.toast.as_deref(),
        Some("C0PL4ND 9.9.9 is available"),
        "the notice is shown as a transient status-bar toast"
    );
}

#[test]
fn launch_check_channel_polls_into_a_toast_then_stops() {
    // Simulates the background launch check: a notice sent on the attached
    // channel is picked up by `poll_update_check` (called each frame) and
    // surfaced; the channel is then dropped (one-shot).
    let mut app = C0pl4ndApp::bootstrap();
    let (tx, rx) = std::sync::mpsc::channel();
    app.attach_update_check(rx);
    assert!(app.update_rx.is_some(), "channel attached");
    // Nothing sent yet → poll is a no-op.
    app.poll_update_check();
    assert_eq!(app.last_update_notice(), None);
    // The background thread finds an update and sends one notice.
    tx.send("C0PL4ND 2.0.0 is available".to_string()).unwrap();
    app.poll_update_check();
    assert_eq!(
        app.last_update_notice().as_deref(),
        Some("C0PL4ND 2.0.0 is available"),
        "a received notice surfaces via the per-frame poll"
    );
    assert!(
        app.update_rx.is_none(),
        "the check is one-shot: the receiver is dropped after delivery"
    );
}

#[test]
fn split_increases_pane_count() {
    let mut app = C0pl4ndApp::bootstrap();
    let before = app.pane_count();
    app.split(egui_tiles::LinearDir::Horizontal);
    assert_eq!(app.pane_count(), before + 1);
}

#[test]
fn split_refuses_above_cap() {
    let mut app = C0pl4ndApp::bootstrap();
    while app.pane_count() < grid::MAX_PANES {
        app.split(egui_tiles::LinearDir::Horizontal);
    }
    assert_eq!(app.pane_count(), grid::MAX_PANES);
    app.split(egui_tiles::LinearDir::Vertical);
    assert_eq!(app.pane_count(), grid::MAX_PANES, "cap must hold");
    assert!(app.toast.is_some());
}

/// Regression for "the existing terminal goes black after I close one and
/// open a new one". An orphaned pane (in storage but unreachable from the
/// tree root) is COUNTED by `pane_count` but rendered NOWHERE — i.e. black.
/// After close+new-terminal, EVERY pane must be reachable from the root.
#[test]
fn close_then_new_terminal_keeps_every_pane_reachable() {
    fn reachable(tree: &egui_tiles::Tree<Pane>) -> Vec<PaneId> {
        fn walk(tree: &egui_tiles::Tree<Pane>, id: egui_tiles::TileId, out: &mut Vec<PaneId>) {
            match tree.tiles.get(id) {
                Some(egui_tiles::Tile::Pane(p)) => out.push(p.pane_id),
                Some(egui_tiles::Tile::Container(c)) => {
                    for ch in c.children() {
                        walk(tree, *ch, out);
                    }
                }
                None => {}
            }
        }
        let mut out = Vec::new();
        if let Some(root) = tree.root {
            walk(tree, root, &mut out);
        }
        out
    }

    let mut app = C0pl4ndApp::bootstrap(); // 1 pane (id 0)
    app.new_terminal(); // → 0, 1
    assert_eq!(app.pane_count(), 2);
    app.close_pane(app.focused_pane); // close the new one
    assert_eq!(app.pane_count(), 1, "back to one pane after close");
    app.new_terminal(); // → survivor + a fresh pane
    assert_eq!(app.pane_count(), 2, "two panes after re-adding");

    let reachable = reachable(&app.grid_tree);
    assert_eq!(
        reachable.len(),
        app.pane_count(),
        "every pane must be reachable from the root after close+new (an \
             orphaned pane renders black); reachable={reachable:?}"
    );
}

/// Fast-close contract: `prepare_shutdown` must reap EVERY live terminal so
/// no PTY child is orphaned after the window closes, while NOT terminating
/// the process (the real Close handler calls `process::exit(0)` AFTER this;
/// the test exercises only the cleanup so it does not kill the test runner).
///
/// The config save is real-window-only (`live_window`); `bootstrap()` leaves
/// it false, so this test deliberately does NOT write the user's real config
/// (no test pollution) — it asserts the no-orphan child-reaping side effect,
/// which is the load-bearing correctness guarantee of the fast exit.
#[test]
fn prepare_shutdown_kills_shells_without_dropping_panes_or_exiting() {
    let mut app = C0pl4ndApp::bootstrap();
    // Open a couple more panes so there are several live shells to kill.
    app.new_terminal();
    app.new_terminal();
    let n = app.term_count();
    assert!(
        n > 0,
        "precondition: at least one live terminal before shutdown"
    );
    assert!(
        !app.live_window,
        "bootstrap() is headless: the config save is skipped (no test pollution)"
    );

    // The cleanup the real Close handler runs before process::exit(0). It kills
    // every pane's shell (`PaneTerm::kill_child` → `Session::kill_child` →
    // `PtyProcess::kill`; that the kill actually terminates the child is proven
    // deterministically by `pty::tests::kill_terminates_interactive_child`).
    app.prepare_shutdown();

    // The load-bearing FAST-CLOSE contract: prepare_shutdown must NOT drop the
    // panes. Dropping runs the per-pane `ClosePseudoConsole` that BLOCKS until
    // each child exits, sequentially — the slow-to-close latency this change
    // removes. `process::exit(0)` (which the real Close handler calls right after)
    // runs no destructors, so the blocking drop never fires; the shells are still
    // reaped because they were killed above, and the OS reclaims the pseudoconsole
    // handles on process exit.
    assert_eq!(
        app.term_count(),
        n,
        "prepare_shutdown must NOT drop the panes (the blocking per-pane \
             ClosePseudoConsole is skipped on the fast-exit path)"
    );
    // Reaching here proves prepare_shutdown returned normally — it did NOT
    // call process::exit (which would abort the test runner) and did NOT block.
}

// ---- Transparency clear-color (single always-transparent opacity model) ----

#[test]
fn clear_color_is_always_fully_transparent() {
    // The window is always created transparent-capable, so the clear is
    // unconditionally [0,0,0,0]: the opacity slider is folded ONLY into the panel
    // fills ([`pane_bg_alpha`]) + resting chrome, never the clear. A non-zero
    // clear compounded with the panel alpha (0.6 over 0.6 ≈ 0.84) and darkened the
    // window to near-opaque black; a fully-transparent clear makes the panel alpha
    // the sole determinant of see-through. At opacity 1.0 the opaque panels cover
    // the transparent clear, so the window still reads as a solid surface.
    let [_, _, _, a] = window_clear_color();
    assert_eq!(a, 0.0, "the clear is always fully transparent");
    assert_eq!(window_clear_color(), [0.0, 0.0, 0.0, 0.0]);
}

// ---- Line-height row pitch ----

#[test]
fn line_height_multiplier_anchors_default_to_one() {
    // The 20.0-px default maps to a 1.0 multiplier (natural spacing), so the
    // default config reproduces the pre-feature row pitch exactly.
    assert!(
        (line_height_multiplier(LINE_HEIGHT_ANCHOR_PX) - 1.0).abs() < 1e-6,
        "the default line-height must yield a 1.0 (natural) pitch multiplier"
    );
    // A larger configured line-height opens the rows up (> 1.0); a smaller
    // one tightens them (< 1.0).
    assert!(line_height_multiplier(40.0) > 1.0, "40px loosens the pitch");
    assert!(
        line_height_multiplier(12.0) < 1.0,
        "12px tightens the pitch"
    );
}

#[test]
fn line_height_multiplier_clamps_and_guards_bad_values() {
    // A degenerate / non-finite config can neither collapse rows nor scatter
    // them: the multiplier is clamped to a sane band, and 0 / negative /
    // non-finite fall back to the natural 1.0.
    assert_eq!(line_height_multiplier(0.0), 1.0, "zero → natural");
    assert_eq!(line_height_multiplier(-5.0), 1.0, "negative → natural");
    assert_eq!(line_height_multiplier(f32::NAN), 1.0, "NaN → natural");
    assert!(
        (line_height_multiplier(1000.0) - 4.0).abs() < 1e-6,
        "a huge line-height clamps to the 4.0 ceiling"
    );
    assert!(
        (line_height_multiplier(1.0) - 0.5).abs() < 1e-6,
        "a tiny line-height clamps to the 0.5 floor"
    );
}

#[test]
fn effective_row_pitch_scales_natural_height_by_the_multiplier() {
    // At the default line-height the pitch equals the natural galley height;
    // doubling the line-height (40px) doubles the pitch; the pitch is never
    // below 1px (a degenerate natural height still yields a drawable row).
    let natural = 16.0;
    assert!(
        (effective_row_pitch(natural, LINE_HEIGHT_ANCHOR_PX) - natural).abs() < 1e-6,
        "default line-height keeps the natural pitch"
    );
    assert!(
        (effective_row_pitch(natural, 40.0) - natural * 2.0).abs() < 1e-3,
        "40px (2× the 20px anchor) doubles the row pitch"
    );
    assert!(
        effective_row_pitch(0.0, LINE_HEIGHT_ANCHOR_PX) >= 1.0,
        "the pitch floors at 1px so a row is always drawable"
    );
}

// ---- CRT / chromatic-aberration helpers ----

#[test]
fn chromatic_offset_is_zero_when_off_and_clears_the_glyph_on_hidpi() {
    // 0.0 (the default) → no ghost offset at all (the OFF fast-path).
    assert_eq!(
        chromatic_offset(0.0, 1.0),
        0.0,
        "0 intensity = no aberration"
    );
    assert_eq!(chromatic_offset(-1.0, 1.0), 0.0, "negative = off");
    assert_eq!(chromatic_offset(f32::NAN, 1.0), 0.0, "NaN = off");
    // ON at 1× → the PHYSICAL-px offset is the logical offset; the minimum is
    // ≥2 physical px so the fringe clears the opaque glyph (#28).
    let low = chromatic_offset(0.0001, 1.0);
    assert!(
        low >= CHROMATIC_MIN_OFFSET_PHYS_PX - 1e-3,
        "even a tiny intensity floors at the ≥2-physical-px visible minimum"
    );
    // Full intensity at 1× → the cap in physical px.
    assert!(
        (chromatic_offset(1.0, 1.0) - CHROMATIC_MAX_OFFSET_PHYS_PX).abs() < 1e-3,
        "intensity 1.0 reaches the physical-px cap at 1×"
    );
    // HiDPI (2×): the LOGICAL offset halves, but it still represents ≥2
    // PHYSICAL px — the whole point of the ppp-aware fix. At the floor the
    // logical offset is MIN/2 but ×ppp == the physical floor.
    let phys_at_2x = chromatic_offset(0.0001, 2.0) * 2.0;
    assert!(
        phys_at_2x >= CHROMATIC_MIN_OFFSET_PHYS_PX - 1e-3,
        "on a 2× panel the fringe is still ≥2 physical px (clears the glyph)"
    );
    // A wild intensity still clamps to the cap (never smears to illegibility).
    assert!(
        (chromatic_offset(99.0, 1.0) - CHROMATIC_MAX_OFFSET_PHYS_PX).abs() < 1e-3,
        "clamped to the physical-px cap"
    );
    // A bad ppp is treated as 1× (never NaN / div-by-zero).
    assert!(chromatic_offset(1.0, 0.0).is_finite());
    assert!(chromatic_offset(1.0, f32::NAN).is_finite());
}

#[test]
fn chromatic_ghost_alpha_scales_with_intensity_and_is_zero_when_off() {
    // OFF → no ghost alpha (so no ghost passes are even drawn).
    assert_eq!(chromatic_ghost_alpha(0.0), 0, "0 intensity = no ghost");
    assert_eq!(chromatic_ghost_alpha(-1.0), 0, "negative = off");
    // ON → saturated 150..=220 band (#28: pure-channel ghosts behind the
    // glyph must POP, not wash to grey like the old 100..=140 tinted galleys).
    assert_eq!(
        chromatic_ghost_alpha(0.0001),
        150,
        "a low intensity starts at the visible 150 floor"
    );
    assert_eq!(
        chromatic_ghost_alpha(1.0),
        220,
        "full intensity reaches the 220 cap"
    );
    assert_eq!(
        chromatic_ghost_alpha(99.0),
        220,
        "alpha is capped at 220 even for a wild intensity"
    );
    assert!(
        chromatic_ghost_alpha(0.3) <= chromatic_ghost_alpha(0.9),
        "ghost alpha grows with intensity"
    );
    // Every visible ghost is firmly in the saturated 150..=220 band.
    for i in [0.2_f32, 0.5, 1.0, 3.0, 9.0] {
        let a = chromatic_ghost_alpha(i);
        assert!((150..=220).contains(&a), "alpha {a} in the visible band");
    }
}

#[test]
fn chromatic_edge_weight_is_zero_at_centre_and_full_at_edge() {
    // Edge-weighting: a glyph at the vertical centre fringes faintly (40% of
    // the base offset), the edges fringe at the full base offset (#28 / §2b).
    let base = 2.0;
    let (lo, hi) = (0.0, 100.0);
    let centre = chromatic_edge_weighted_offset(base, 50.0, lo, hi);
    let edge = chromatic_edge_weighted_offset(base, 100.0, lo, hi);
    assert!(
        (centre - base * 0.4).abs() < 1e-4,
        "centre keeps 40% of the offset (a faint fringe, never fully crisp)"
    );
    assert!(
        (edge - base).abs() < 1e-4,
        "the edge gets the full base offset"
    );
    assert!(edge > centre, "the edge separates more than the centre");
    // OFF / degenerate span → no offset, never NaN.
    assert_eq!(chromatic_edge_weighted_offset(0.0, 5.0, 0.0, 100.0), 0.0);
    assert_eq!(chromatic_edge_weighted_offset(2.0, 5.0, 10.0, 10.0), 2.0);
}

#[test]
fn scanline_period_is_physical_px_anchored() {
    // The period is PHYSICAL-px-anchored: at 1× it is the raw physical px; at
    // 2× HiDPI the LOGICAL period halves but still spans the same physical px
    // (the fix for "reads as a flat film on HiDPI", #28).
    assert!((scanline_period_pts(1.0) - CRT_SCANLINE_PERIOD_PHYS_PX).abs() < 1e-6);
    assert!(
        (scanline_period_pts(2.0) - CRT_SCANLINE_PERIOD_PHYS_PX / 2.0).abs() < 1e-6,
        "2× panel halves the logical period (same physical pitch)"
    );
    // A bad ppp is treated as 1× (never div-by-zero / NaN).
    assert!((scanline_period_pts(0.0) - CRT_SCANLINE_PERIOD_PHYS_PX).abs() < 1e-6);
    assert!(scanline_period_pts(f32::NAN).is_finite());
}

#[test]
fn scanline_count_fills_the_whole_rect_and_is_zero_for_empty() {
    // Bands fill the WHOLE content height every period — not a vignette box.
    let n = scanline_count(300.0, 1.0);
    assert_eq!(n, (300.0_f32 / scanline_period_pts(1.0)).ceil() as usize);
    assert!(
        n >= 90,
        "a tall pane is covered by many bands, not a 4-edge box"
    );
    // A HiDPI panel has MORE (thinner-logical) bands for the same height.
    assert!(
        scanline_count(300.0, 2.0) > scanline_count(300.0, 1.0),
        "a 2× panel packs more logical bands into the same height"
    );
    // Degenerate heights paint nothing (no panic, no negative loop).
    assert_eq!(scanline_count(0.0, 1.0), 0, "empty rect → no bands");
    assert_eq!(scanline_count(-5.0, 1.0), 0, "negative → no bands");
    assert_eq!(scanline_count(f32::NAN, 1.0), 0, "NaN → no bands");
}

#[test]
fn scanline_dark_alpha_maps_darkness_to_a_visible_band() {
    // Darkness 0 → no band; darkness 1 → the strong cap; monotone between.
    assert_eq!(scanline_dark_alpha(0.0), 0, "no darkness = no band");
    assert_eq!(scanline_dark_alpha(-1.0), 0, "negative = no band");
    assert_eq!(scanline_dark_alpha(f32::NAN), 0, "NaN = no band");
    assert_eq!(
        scanline_dark_alpha(1.0),
        CRT_SCANLINE_MAX_DARK_ALPHA as u8,
        "full darkness reaches the dark-band cap"
    );
    // The default darkness reads as a band (well above a near-invisible film).
    let def = scanline_dark_alpha(c0pl4nd_core::config::DEFAULT_SCANLINE_DARKNESS);
    assert!(
        def >= 80,
        "the default darkness paints a clearly-visible band (alpha {def})"
    );
    assert!(
        scanline_dark_alpha(0.2) < scanline_dark_alpha(0.8),
        "darker config => darker band"
    );
}

#[test]
fn scanline_field_drifts_with_time_and_wraps() {
    // The whole dark-band field creeps DOWN as time advances (the visible proof
    // of animation) and wraps seamlessly every period — SCR1B3-style calm drift,
    // replacing the old bright rolling-scan bar.
    let period = 3.0_f32;
    let d0 = scanline_drift(period, 0.0);
    let d1 = scanline_drift(period, 0.1);
    assert_eq!(d0, 0.0, "at t=0 the field sits at its base offset");
    assert!(d1 > d0, "the field drifts DOWN as time advances");
    // The offset stays within one period, never running away unbounded.
    assert!(scanline_drift(period, 123.4) < period);
    // One full period returns the field to its start (seamless wrap).
    let one_period_t = period / CRT_SCANLINE_DRIFT_PTS_PER_SEC;
    assert!(
        scanline_drift(period, one_period_t).abs() < 1e-3,
        "one full period wraps back to the base offset"
    );
    // Degenerate period never panics.
    assert_eq!(scanline_drift(0.0, 1.0), 0.0);
}

// ---- Pane background alpha (single always-transparent opacity model) ----

#[test]
fn pane_bg_alpha_is_solid_at_default_opacity() {
    // The default opacity (1.0) paints the pane fill fully opaque so the window
    // reads as a solid surface even though it is always transparent-capable.
    let app = C0pl4ndApp::bootstrap();
    assert_eq!(app.config.opacity, 1.0, "default opacity is 100%");
    assert_eq!(pane_bg_alpha(&app.config), 255, "opacity 1.0 → solid fill");
}

#[test]
fn pane_bg_alpha_folds_opacity_across_the_full_range() {
    // The single Opacity slider drives the pane-fill alpha directly across its
    // whole range — 1.0 → 255 (solid), 0.0 → 0 (fully see-through), monotonic in
    // between. This is the one lever that makes the window transparent.
    let mk = |o: f32| {
        pane_bg_alpha(&c0pl4nd_core::Config {
            opacity: o,
            ..Default::default()
        })
    };
    assert_eq!(
        mk(0.6),
        (0.6 * 255.0_f32).round() as u8,
        "alpha tracks opacity"
    );
    assert!(mk(0.6) < 255, "a translucent pane fill is non-opaque");
    // The floor is 0.0 (no dead band), so opacity 0 is genuinely fully transparent.
    assert_eq!(
        mk(0.0),
        (TRANSLUCENT_ALPHA_FLOOR * 255.0_f32).round() as u8,
        "opacity 0.0 → fully transparent (floor 0.0)"
    );
    assert_eq!(mk(0.0), 0);
    // Monotonic + reaches solid at the top.
    assert!(mk(0.2) < mk(0.5) && mk(0.5) < mk(0.85) && mk(0.85) < mk(1.0));
    assert_eq!(mk(1.0), 255, "opacity 1.0 is a solid fill");
}

// --- pure-function coverage (Wave H): the keyboard→PTY map, the UTF-8
//     search-highlight column map, and the acrylic tint parser were untested; a
//     regression in any silently corrupts input / highlights / theming. ---

#[test]
fn egui_key_to_logical_maps_keys_and_ctrl_chords() {
    use c0pl4nd_core::term::{KeyModifiers, LogicalKey};
    let none = KeyModifiers::default();
    let ctrl = KeyModifiers {
        ctrl: true,
        ..Default::default()
    };
    // Named keys map straight through.
    assert_eq!(
        egui_key_to_logical(egui::Key::ArrowUp, none),
        Some(LogicalKey::ArrowUp)
    );
    assert_eq!(
        egui_key_to_logical(egui::Key::Enter, none),
        Some(LogicalKey::Enter)
    );
    assert_eq!(
        egui_key_to_logical(egui::Key::F5, none),
        Some(LogicalKey::Function(5))
    );
    // Ctrl+Space → NUL.
    assert_eq!(
        egui_key_to_logical(egui::Key::Space, ctrl),
        Some(LogicalKey::Text("\0".to_string()))
    );
    // Ctrl+letter → the C0 control byte (Ctrl+C=0x03, Ctrl+A=0x01,
    // Ctrl+M=0x0D='\r'). A regression in the `& 0x1f` mask corrupts every
    // control chord.
    assert_eq!(
        egui_key_to_logical(egui::Key::C, ctrl),
        Some(LogicalKey::Text("\u{3}".to_string()))
    );
    assert_eq!(
        egui_key_to_logical(egui::Key::A, ctrl),
        Some(LogicalKey::Text("\u{1}".to_string()))
    );
    assert_eq!(
        egui_key_to_logical(egui::Key::M, ctrl),
        Some(LogicalKey::Text("\r".to_string()))
    );
    // A bare (non-ctrl) letter is delivered via Event::Text, not here → None.
    assert_eq!(egui_key_to_logical(egui::Key::C, none), None);
}

#[test]
fn byte_to_col_counts_cell_width_to_the_byte_boundary() {
    // 'é' is 2 bytes but a single-width cell: byte 3 (start of 'l') is column 2.
    assert_eq!(byte_to_col("héllo", 3), 2);
    // '日' is 3 bytes AND a WIDE (2-cell) glyph: byte 3 (start of '本') is cell
    // column 2 — the per-cell renderer positions '本' two cells past '日', so a
    // span/highlight must too.
    assert_eq!(byte_to_col("日本", 3), 2);
    // A wide glyph mid-string: byte 4 is 'b' after "a日" → cells 1 (a) + 2 (日).
    assert_eq!(byte_to_col("a日b", 4), 3);
    // Past the end clamps to the total cell width.
    assert_eq!(byte_to_col("abc", 99), 3);
    assert_eq!(byte_to_col("日本", 99), 4);
    assert_eq!(byte_to_col("", 0), 0);
    assert_eq!(byte_to_col("abc", 0), 0);
}

#[test]
fn theme_candidate_paths_prioritizes_the_config_dir() {
    use std::path::{Path, PathBuf};
    let cfg = Path::new("/cfg");
    let exe = Path::new("/app");
    let paths = theme_candidate_paths("nord", Some(cfg), Some(exe));
    // User override (config dir) first, then cwd assets, then exe-adjacent assets.
    assert_eq!(
        paths,
        vec![
            PathBuf::from("/cfg/themes/nord.toml"),
            PathBuf::from("assets/themes/nord.toml"),
            PathBuf::from("/app/assets/themes/nord.toml"),
        ],
        "config-dir user theme must be the highest-priority candidate"
    );
    // With no config dir / no exe dir, only the cwd assets path remains.
    assert_eq!(
        theme_candidate_paths("nord", None, None),
        vec![PathBuf::from("assets/themes/nord.toml")]
    );
}

// --- layout persistence (Wave: restore split-pane layout + per-pane cwd) ---

use crate::egui_app::grid as grid_mod;
use crate::egui_app::layout_state::LayoutSnapshot;
use crate::egui_app::pty_gate::expect_live_pty;

/// Build a snapshot over a default horizontal grid of the given pane ids.
fn snapshot_for(panes: &[PaneId], focused: PaneId, next_id: u64) -> LayoutSnapshot {
    LayoutSnapshot {
        tree: grid_mod::build_default_grid(panes),
        cwds: std::collections::HashMap::new(),
        focused,
        pinned: Vec::new(),
        next_id,
    }
}

/// A structurally-valid snapshot replaces the default grid: the panes become the
/// (deferred) pending set, focus + allocator + pinned are restored, and only
/// in-tree cwds survive the filter.
#[test]
fn apply_layout_snapshot_restores_a_valid_layout() {
    let mut app = C0pl4ndApp::bootstrap(); // default: 1 pane (id 0)
    let panes = [PaneId(10), PaneId(11), PaneId(12)];
    let mut snap = snapshot_for(&panes, PaneId(11), 13);
    snap.cwds.insert(PaneId(10), "/home/op/work".to_string());
    // A cwd for a pane NOT in the tree must be dropped on restore.
    snap.cwds
        .insert(PaneId(99), "/should/be/dropped".to_string());
    snap.pinned = vec![PaneId(12), PaneId(99)]; // 99 not in tree → filtered

    app.apply_layout_snapshot(snap);

    assert_eq!(app.pane_count(), 3, "restored tree has 3 panes");
    assert_eq!(app.focused_pane, PaneId(11), "focus restored");
    // All restored panes are deferred (spawned on first frame at measured size).
    assert_eq!(app.pending_spawn.len(), 3);
    assert!(app.terms.is_empty(), "no panes spawned yet (deferred)");
    // Only the in-tree cwd survived the filter.
    assert_eq!(app.restored_cwds.len(), 1);
    assert_eq!(
        app.restored_cwds.get(&PaneId(10)).map(String::as_str),
        Some("/home/op/work")
    );
    assert_eq!(
        app.pinned.iter().copied().collect::<Vec<_>>(),
        vec![PaneId(12)]
    );
    // Allocator resumes past every restored id (max present 12 + 1 = 13).
    assert_eq!(app.pane_alloc.peek_next(), 13);
}

/// A snapshot whose pane count exceeds the live cap is rejected — the default
/// grid stands untouched (a corrupt/over-cap blob must never brick launch).
#[test]
fn apply_layout_snapshot_rejects_over_cap() {
    let mut app = C0pl4ndApp::bootstrap();
    let before = app.pane_count();
    let too_many: Vec<PaneId> = (0..(grid_mod::MAX_PANES as u64 + 1)).map(PaneId).collect();
    app.apply_layout_snapshot(snapshot_for(&too_many, PaneId(0), 99));
    assert_eq!(app.pane_count(), before, "over-cap snapshot ignored");
}

/// An empty snapshot (zero panes) is rejected for the same reason.
#[test]
fn apply_layout_snapshot_rejects_empty() {
    let mut app = C0pl4ndApp::bootstrap();
    let before = app.pane_count();
    app.apply_layout_snapshot(snapshot_for(&[], PaneId(0), 0));
    assert_eq!(app.pane_count(), before, "empty snapshot ignored");
}

/// `capture_layout` of a freshly-bootstrapped app round-trips through
/// `apply_layout_snapshot` with the pane structure preserved (the deferred
/// initial pane has no live term, so its cwd is simply absent — not an error).
#[test]
fn capture_then_apply_round_trips_pane_structure() {
    let app = C0pl4ndApp::bootstrap();
    let snap = app.capture_layout();
    let captured_panes = grid_mod::panes_in_visual_order(&snap.tree);
    let mut app2 = C0pl4ndApp::bootstrap();
    app2.apply_layout_snapshot(snap);
    assert_eq!(
        grid_mod::panes_in_visual_order(&app2.grid_tree),
        captured_panes
    );
}

// --- selection absolute-line → display-row mapping (scroll-stable selection) ---

#[test]
fn selection_maps_absolute_lines_to_display_rows() {
    // Window shows absolute lines 100..=123 (window_start=100, rows=24).
    // A selection of absolute lines 105..=110 → display rows 5..=10.
    let r = selection_visible_rows((105, 2), (110, 7), 100, 24);
    assert_eq!(r, Some(((5, 2), (10, 7))));
}

#[test]
fn selection_orders_endpoints_and_survives_reverse_drag() {
    // head before anchor (dragged upward) still yields ordered display rows.
    let r = selection_visible_rows((110, 7), (105, 2), 100, 24);
    assert_eq!(r, Some(((5, 2), (10, 7))));
}

#[test]
fn selection_clamps_a_partly_scrolled_out_selection_to_the_visible_window() {
    // Selection absolute 90..=110, window starts at 100 (rows 24): the top
    // (90..100) has scrolled above → clamp start to the visible top-left (0,0);
    // the visible remainder maps 100..=110 → rows 0..=10.
    let r = selection_visible_rows((90, 3), (110, 5), 100, 24);
    assert_eq!(r, Some(((0, 0), (10, 5))));

    // Selection whose END scrolled below the bottom → clamp to last row + EOL.
    let r = selection_visible_rows((110, 1), (200, 4), 100, 24);
    assert_eq!(r, Some(((10, 1), (23, usize::MAX))));
}

#[test]
fn selection_fully_scrolled_out_is_none() {
    // Entirely above the window.
    assert_eq!(selection_visible_rows((10, 0), (20, 0), 100, 24), None);
    // Entirely below the window (window covers 100..124).
    assert_eq!(selection_visible_rows((130, 0), (140, 0), 100, 24), None);
    // Zero-row window.
    assert_eq!(selection_visible_rows((100, 0), (110, 0), 100, 0), None);
}

// ---- double-click word selection bounds -------------------------------------

#[test]
fn word_bounds_expands_to_the_whole_word_from_any_interior_column() {
    // "  hello world  " — clicking any column inside "hello" (cols 2..=6) must
    // grab the whole word, regardless of which interior cell was hit.
    let row: Vec<char> = "  hello world  ".chars().collect();
    for col in 2..=6 {
        assert_eq!(
            word_bounds(&row, col),
            (2, 6),
            "double-click at col {col} selects all of 'hello' (cols 2..=6)"
        );
    }
    // "world" is cols 8..=12.
    for col in 8..=12 {
        assert_eq!(word_bounds(&row, col), (8, 12), "selects all of 'world'");
    }
}

#[test]
fn word_bounds_on_whitespace_selects_only_that_cell() {
    // Clicking the gap between words (a space) selects just that one cell, so a
    // double-click on empty space copies nothing rather than a whole line.
    let row: Vec<char> = "ab cd".chars().collect();
    assert_eq!(
        word_bounds(&row, 2),
        (2, 2),
        "the space at col 2 is its own cell"
    );
}

#[test]
fn word_bounds_keeps_path_and_url_punctuation_in_one_word() {
    // The word class includes path / URL punctuation so a double-click grabs a
    // whole filename or URL instead of stopping at the first dot or slash.
    // "see " is cols 0..=3 (space at 3); "/usr/local/bin/foo.sh" is cols 4..=24.
    let row: Vec<char> = "see /usr/local/bin/foo.sh now".chars().collect();
    assert_eq!(
        word_bounds(&row, 10),
        (4, 24),
        "the whole path /usr/local/bin/foo.sh is one word (cols 4..=24)"
    );
    // "http://example.com/p" is cols 0..=19 (space at 20).
    let url: Vec<char> = "http://example.com/p done".chars().collect();
    assert_eq!(
        word_bounds(&url, 5),
        (0, 19),
        "the whole URL is one word (cols 0..=19)"
    );
}

#[test]
fn word_bounds_single_char_word_is_itself() {
    // A one-character word still selects that single cell (and the gesture path
    // copies it directly, so a single-char word is not lost).
    let row: Vec<char> = "a bc".chars().collect();
    assert_eq!(
        word_bounds(&row, 0),
        (0, 0),
        "single-char word 'a' is (0,0)"
    );
}

#[test]
fn word_bounds_out_of_range_column_is_inert() {
    // A column past the row length is treated as a non-word cell: it returns
    // itself and never panics (defensive against a stale hit-test).
    let row: Vec<char> = "abc".chars().collect();
    assert_eq!(word_bounds(&row, 99), (99, 99));
}

// ---- right-click context-menu actions ---------------------------------------

#[test]
fn context_menu_actions_change_the_pane_count() {
    // The split / new-tab / close items drive the same tree mutations as the
    // keyboard chords; assert the observable pane-count effect of each.
    let mut app = C0pl4ndApp::bootstrap();
    let base = app.pane_count();

    app.apply_context_menu_action(ContextMenuAction::SplitRight);
    assert_eq!(app.pane_count(), base + 1, "Split right adds one pane");
    app.apply_context_menu_action(ContextMenuAction::SplitDown);
    assert_eq!(app.pane_count(), base + 2, "Split down adds one pane");
    app.apply_context_menu_action(ContextMenuAction::NewTerminal);
    assert_eq!(app.pane_count(), base + 3, "New tab adds one pane");

    let focused = app.focused_pane();
    app.apply_context_menu_action(ContextMenuAction::ClosePane(focused));
    assert_eq!(app.pane_count(), base + 2, "Close pane removes one pane");
}

#[test]
fn context_menu_close_never_removes_the_last_pane() {
    // The Close-Pane item routes through close_pane, which keeps at least one
    // pane alive — so the menu can never leave the app with zero panes.
    let mut app = C0pl4ndApp::bootstrap();
    while app.pane_count() > 1 {
        let f = app.focused_pane();
        app.apply_context_menu_action(ContextMenuAction::ClosePane(f));
    }
    assert_eq!(app.pane_count(), 1, "closed down to a single pane");
    let f = app.focused_pane();
    app.apply_context_menu_action(ContextMenuAction::ClosePane(f));
    assert_eq!(app.pane_count(), 1, "the last pane is never closed");
}

// ---- directional-focus geometry ---------------------------------------------

#[test]
fn neighbor_in_rects_picks_the_correct_directional_neighbour() {
    // A 2x2 grid of pane rects (screen coords, y DOWN):
    //   TL(0) TR(1)
    //   BL(2) BR(3)
    // The geometry must pick the TRUE directional neighbour — not merely "a
    // different pane" (which a focus-cycle would also satisfy) — and must respect
    // the orthogonal-overlap requirement (Right from TL is TR, never the
    // diagonal BR).
    let r = |x0: f32, y0: f32| {
        egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x0 + 100.0, y0 + 100.0))
    };
    let (tl, tr, bl, br) = (PaneId(0), PaneId(1), PaneId(2), PaneId(3));
    let mut rects: HashMap<PaneId, egui::Rect> = HashMap::new();
    rects.insert(tl, r(0.0, 0.0));
    rects.insert(tr, r(100.0, 0.0));
    rects.insert(bl, r(0.0, 100.0));
    rects.insert(br, r(100.0, 100.0));

    // From the top-left pane.
    assert_eq!(neighbor_in_rects(&rects, tl, Direction::Right), Some(tr));
    assert_eq!(neighbor_in_rects(&rects, tl, Direction::Down), Some(bl));
    assert_eq!(
        neighbor_in_rects(&rects, tl, Direction::Left),
        None,
        "no pane to the left of the top-left pane"
    );
    assert_eq!(
        neighbor_in_rects(&rects, tl, Direction::Up),
        None,
        "no pane above the top-left pane"
    );

    // From the bottom-right pane (the mirror image) — proves Left/Up are not
    // swapped with Right/Down.
    assert_eq!(neighbor_in_rects(&rects, br, Direction::Left), Some(bl));
    assert_eq!(neighbor_in_rects(&rects, br, Direction::Up), Some(tr));
    assert_eq!(neighbor_in_rects(&rects, br, Direction::Right), None);
    assert_eq!(neighbor_in_rects(&rects, br, Direction::Down), None);

    // A focus id with no rect has no neighbour (defensive, never panics).
    assert_eq!(neighbor_in_rects(&rects, PaneId(99), Direction::Left), None);
}

/// The shell's [`FramePolicy`](c0pl4nd_renderer::FramePolicy) is `Continuous`
/// ONLY for the CRT scanline animation (and only when reduced-motion is off);
/// every other state is `OnDamage` (render-on-input). Locks the typed
/// frame-scheduling contract the renderer crate exposes against the shell's
/// actual repaint decision.
#[test]
fn frame_policy_is_continuous_only_for_crt_animation() {
    use c0pl4nd_renderer::FramePolicy;
    let mut app = C0pl4ndApp::bootstrap();

    // CRT off => always render-on-damage, regardless of the motion setting.
    app.config.effects.crt_scanlines = false;
    assert_eq!(app.frame_policy(), FramePolicy::OnDamage);

    // CRT on => Continuous iff reduced-motion is off (the animation pumps the
    // frame clock); under reduced-motion the roll band freezes and the policy
    // stays OnDamage. Compare against the same predicate the policy uses so the
    // assertion is deterministic on any host.
    app.config.effects.crt_scanlines = true;
    let expected = if c0pl4nd_core::reduced_motion::reduced_motion() {
        FramePolicy::OnDamage
    } else {
        FramePolicy::Continuous
    };
    assert_eq!(app.frame_policy(), expected);
}

/// Build a headless egui context whose reported OS appearance is `system` and
/// run `f` inside a pass, so `follow_os_theme_tick` reads a controlled
/// `ctx.system_theme()`. Reuse ONE context across calls so its per-frame theme
/// tracking is realistic.
#[cfg(test)]
fn drive_with_system_theme(
    ctx: &egui::Context,
    system: Option<egui::Theme>,
    f: impl FnOnce(&egui::Context),
) {
    let raw = egui::RawInput {
        system_theme: system,
        ..Default::default()
    };
    ctx.begin_pass(raw);
    f(ctx);
    let _ = ctx.end_pass();
}

/// F3 (SCR1B3 parity): while `follow_os_theme` is on, a MANUAL theme pick STICKS
/// — `follow_os_theme_tick` re-applies the OS-derived theme ONLY when the OS
/// appearance actually CHANGES, never every frame. Regression guard for the
/// prior "reassert every frame" behaviour that reverted a manual pick on the
/// very next frame.
#[test]
fn follow_os_theme_only_reapplies_on_os_change_not_every_frame() {
    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();
    app.config.follow_os_theme = true;

    // Frame 1: the OS appearance (Dark) is observed for the first time and the
    // matching dark default is applied.
    drive_with_system_theme(&ctx, Some(egui::Theme::Dark), |ctx| {
        app.follow_os_theme_tick(ctx);
    });
    assert_eq!(
        app.config.theme, "itasha-corp",
        "dark OS applies the dark default"
    );
    assert_eq!(
        app.last_os_theme,
        Some(egui::Theme::Dark),
        "OS appearance tracked"
    );

    // The user manually picks a DIFFERENT theme while follow-OS stays on.
    app.config.theme = "wired-noir".to_string();

    // Frames 2..N with the SAME OS appearance must NOT revert the manual pick —
    // this is the whole point of the fix. Drive several frames to be sure.
    for _ in 0..5 {
        drive_with_system_theme(&ctx, Some(egui::Theme::Dark), |ctx| {
            app.follow_os_theme_tick(ctx);
        });
        assert_eq!(
            app.config.theme, "wired-noir",
            "manual pick must STICK while the OS appearance is unchanged"
        );
    }

    // When the OS appearance actually FLIPS to Light, follow-OS re-applies the
    // light default — proving the feature still works, it is just edge-triggered.
    drive_with_system_theme(&ctx, Some(egui::Theme::Light), |ctx| {
        app.follow_os_theme_tick(ctx);
    });
    assert_eq!(
        app.config.theme, "ghost-paper",
        "an actual OS dark→light change re-applies the light default"
    );
    assert_eq!(app.last_os_theme, Some(egui::Theme::Light));
}

/// Turning `follow_os_theme` OFF forgets the tracked OS appearance, so a later
/// re-enable re-applies the OS theme on its next observation (not suppressed by
/// a stale match).
#[test]
fn follow_os_theme_toggle_off_forgets_tracked_appearance() {
    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();
    app.config.follow_os_theme = true;

    drive_with_system_theme(&ctx, Some(egui::Theme::Dark), |ctx| {
        app.follow_os_theme_tick(ctx);
    });
    assert_eq!(app.last_os_theme, Some(egui::Theme::Dark));

    // Toggle off: the tracker is cleared even though the tick otherwise no-ops.
    app.config.follow_os_theme = false;
    drive_with_system_theme(&ctx, Some(egui::Theme::Dark), |ctx| {
        app.follow_os_theme_tick(ctx);
    });
    assert_eq!(
        app.last_os_theme, None,
        "toggle off forgets the tracked appearance"
    );
}

// ---------------------------------------------------------------------------
// OSC 9;4 taskbar-progress WIRING (pump -> taskbar seam)
// ---------------------------------------------------------------------------
// These drive the REAL `pump_pane_effects` and observe the taskbar seam through
// `taskbar::test_spy`. A test that called `map_progress_state` / `apply_progress`
// directly would pass forever even if the pump never called them — which is
// exactly the dormant state this feature was in before the wiring landed (the
// parsers surfaced `HostEffects::progress` and the app dropped it on the floor).

/// Feed a pane two `OSC 9 ; 4` reports, pump ONE frame, and assert the taskbar
/// seam received the LAST one, mapped. Fails if the pump stops draining
/// `fx.progress`, stops calling `latest_progress` (it would apply 30% instead of
/// 42%), or stops calling `map_progress_state`/`apply_progress` at all.
#[test]
fn pump_drives_taskbar_progress_from_osc_9_4() {
    let _guard = taskbar::test_spy::serial();
    taskbar::test_spy::reset();

    let mut app = C0pl4ndApp::bootstrap();
    let pane = PaneTerm::spawn(app.theme.clone(), 80, 24);
    let term = expect_live_pty(&pane);
    {
        let mut t = term.lock().unwrap();
        // Two reports in ONE frame: only the LAST is visible on a single button.
        t.advance(b"\x1b]9;4;1;30\x07"); // Normal 30%
        t.advance(b"\x1b]9;4;1;42\x07"); // Normal 42%  <- latest wins
    }
    app.terms.insert(PaneId(0), pane);

    let ctx = egui::Context::default();
    app.pump_pane_effects(&ctx);

    assert_eq!(
        taskbar::test_spy::take(),
        Some((taskbar::TaskbarProgress::Normal, 42)),
        "pump must map the LATEST drained OSC 9;4 report onto the taskbar seam"
    );
}

/// The WARNING state must reach the taskbar as `Paused` (yellow) through the
/// real pump — the arm most likely to be silently folded into `Normal`.
#[test]
fn pump_maps_warning_progress_to_paused() {
    let _guard = taskbar::test_spy::serial();
    taskbar::test_spy::reset();

    let mut app = C0pl4ndApp::bootstrap();
    let pane = PaneTerm::spawn(app.theme.clone(), 80, 24);
    let term = expect_live_pty(&pane);
    term.lock().unwrap().advance(b"\x1b]9;4;4;61\x07"); // state 4 = Warning
    app.terms.insert(PaneId(0), pane);

    app.pump_pane_effects(&egui::Context::default());

    assert_eq!(
        taskbar::test_spy::take(),
        Some((taskbar::TaskbarProgress::Paused, 61)),
        "OSC 9;4 state 4 (warning) must surface as Paused/yellow, not Normal"
    );
}

/// A frame with NO progress reports must leave the taskbar button untouched —
/// the pump must not spam a `None`/clear every frame (which would wipe a
/// progress segment set by a previous frame and cost a COM call per frame).
#[test]
fn pump_leaves_taskbar_untouched_when_no_progress_drained() {
    let _guard = taskbar::test_spy::serial();
    taskbar::test_spy::reset();

    let mut app = C0pl4ndApp::bootstrap();
    let pane = PaneTerm::spawn(app.theme.clone(), 80, 24);
    let term = expect_live_pty(&pane);
    // Plain output + an unrelated OSC: no progress reports at all.
    term.lock().unwrap().advance(b"hello\r\n\x1b]0;title\x07");
    app.terms.insert(PaneId(0), pane);

    app.pump_pane_effects(&egui::Context::default());

    assert_eq!(
        taskbar::test_spy::take(),
        None,
        "an empty progress drain must not touch the taskbar button"
    );
}

/// The last link of the OSC 52 clipboard-READ chain: real frames must push
/// `config.clipboard_read_allow` down onto EVERY live pane's emulator, in BOTH
/// directions. Without this the Settings checkbox would persist a value that
/// changed nothing (a dead setting), and — worse — turning it back OFF would
/// leave the hole open for the rest of the session.
///
/// Drives the REAL `frame_tick` and observes the REAL emulator gate, re-reading
/// the live panes after each frame, so it cannot pass on a stale handle or a
/// test-only mirror of the propagation.
///
/// Note the ordering the assertions pin: a pane spawned DURING a frame is
/// created after the propagation loop has already run, so it picks the setting
/// up on the following frame. That lag is fail-closed by construction — a fresh
/// emulator denies reads on its own — so the opt-IN is what is delayed, never
/// the opt-OUT.
#[test]
fn frame_tick_propagates_the_clipboard_read_gate_to_every_pane() {
    /// Every live pane's gate state, as the emulator itself reports it.
    fn gates(app: &C0pl4ndApp) -> Vec<bool> {
        app.terms
            .values()
            .filter_map(|p| p.terminal_for_test())
            .map(|t| t.lock().unwrap().clipboard_read_enabled())
            .collect()
    }
    fn frame(ctx: &egui::Context, app: &mut C0pl4ndApp) {
        ctx.begin_pass(egui::RawInput::default());
        app.frame_tick(ctx);
        let _ = ctx.end_pass();
    }

    let ctx = egui::Context::default();
    let mut app = C0pl4ndApp::bootstrap();
    assert!(
        !app.config.clipboard_read_allow,
        "precondition: the shipping default denies clipboard reads"
    );

    // Opt in. The first frame spawns the pane; the pane must NOT come up already
    // opened — it starts denied and is opened by the propagation, never before.
    app.config.clipboard_read_allow = true;
    frame(&ctx, &mut app);
    let born = gates(&app);
    assert!(
        !born.is_empty(),
        "the app must have at least one live pane to assert on"
    );
    assert!(
        born.iter().all(|&g| !g),
        "a newly spawned pane must start DENIED regardless of config, got {born:?}"
    );

    frame(&ctx, &mut app);
    let on = gates(&app);
    assert_eq!(on.len(), born.len(), "same panes still live");
    assert!(
        on.iter().all(|&g| g),
        "a frame must push the opt-in down to every live pane's emulator, got {on:?}"
    );

    // Opt back out: the hole must close on the very next frame.
    app.config.clipboard_read_allow = false;
    frame(&ctx, &mut app);
    let off = gates(&app);
    assert_eq!(off.len(), on.len(), "same panes still live");
    assert!(
        off.iter().all(|&g| !g),
        "turning the setting OFF must close the gate again, not leave it open, got {off:?}"
    );
}

/// The Settings checkbox must be bound to the real field: clicking
/// "Allow programs to read the clipboard (OSC 52)" has to flip
/// `config.clipboard_read_allow` — and nothing else in the Clipboard group.
///
/// Drives the REAL widget by its accessible label through the REAL frame loop
/// (open the gear → pick Terminal → click the row), so a checkbox wired to the
/// wrong field, or rendered but inert, fails here rather than shipping as a
/// security setting that does nothing.
#[test]
fn clicking_the_clipboard_read_checkbox_flips_the_live_config() {
    use egui_kittest::kittest::Queryable;

    let app = std::cell::RefCell::new(C0pl4ndApp::bootstrap());
    assert!(
        !app.borrow().config.clipboard_read_allow,
        "precondition: clipboard reads default to DENIED"
    );
    // Neighbours in the same Clipboard group, to prove the click is targeted.
    let copy_on_select_before = app.borrow().config.copy_on_select;
    let paste_warn_before = app.borrow().config.paste_warn_multiline;

    #[allow(deprecated)]
    let mut h = egui_kittest::Harness::new(|ctx| app.borrow_mut().frame_tick(ctx));
    h.set_size(egui::vec2(1200.0, 800.0));
    h.run();

    h.get_by_label("settings").click();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Terminal")
        .click();
    h.run();

    h.get_by_label("Allow programs to read the clipboard (OSC 52)")
        .click();
    h.run();

    assert!(
        app.borrow().config.clipboard_read_allow,
        "clicking the OSC 52 row must opt the live config IN"
    );
    assert_eq!(
        app.borrow().config.copy_on_select,
        copy_on_select_before,
        "the click must not disturb its neighbour rows"
    );
    assert_eq!(
        app.borrow().config.paste_warn_multiline,
        paste_warn_before,
        "the click must not disturb its neighbour rows"
    );

    // And it must toggle back OFF — a one-way security switch would be a trap.
    h.get_by_label("Allow programs to read the clipboard (OSC 52)")
        .click();
    h.run();
    assert!(
        !app.borrow().config.clipboard_read_allow,
        "clicking again must opt back OUT"
    );
}
