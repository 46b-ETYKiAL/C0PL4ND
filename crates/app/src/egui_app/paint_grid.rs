//! The terminal-grid PAINTER: the free functions that turn a pane's
//! [`ColorRun`]-tagged rows into pixels.
//!
//! Lifted verbatim out of `egui_app/mod.rs`. Every item here is a FREE function
//! or a constant — none takes `&mut self` — and they form one closed pipeline:
//! [`paint_grid_native`] runs its three passes (backgrounds, glyphs, line
//! decorations) plus the cursor and the CRT scanlines, delegating to
//! [`paint_underline`] for decorations, [`snap_to_physical`] for quad-edge
//! alignment, [`grid_text_origin`] for the text origin, [`term_default_fg`] for
//! the fallback colour, and [`row_style_key`] for the galley-cache key.
//!
//! They sit beside `glyph_cache` and `crt`, whose caches and effects this path
//! consumes, rather than inside the 6.8k-line shell module that merely calls
//! them once from `render_pane_body`.

use super::*;

/// The theme's default foreground as an `(r,g,b)` triple — the glyph colour for
/// runs with no explicit SGR colour, and the egui-painter fallback colour.
fn term_default_fg(theme: &c0pl4nd_core::Theme) -> (u8, u8, u8) {
    c0pl4nd_core::theme::parse_hex(&theme.foreground).unwrap_or((232, 230, 240))
}

/// The top-left point at which a pane's terminal grid text is drawn, given the
/// pane's body `rect` and the configurable inner `padding` (points). Pure +
/// GPU-free so the padding live-apply wiring is unit-testable: the origin must
/// move with the padding (a larger padding insets the grid further from the
/// pane's top-left corner). Negative paddings are clamped to zero so a bad
/// config can never push the origin outside the pane.
pub(super) fn grid_text_origin(rect: egui::Rect, padding: f32) -> egui::Pos2 {
    let p = padding.max(0.0);
    rect.left_top() + egui::vec2(p, p)
}

/// Alpha the theme's opaque `selection_background` is washed over the grid at.
///
/// The theme colour is an opaque RGB; painting it solid would hide the text
/// underneath (the wash is drawn AFTER the glyphs). This alpha is the previous
/// hard-coded wash's alpha, so the selection reads exactly as before while now
/// taking the ACTIVE THEME's hue instead of a fixed steel blue.
pub(super) const SELECTION_WASH_ALPHA: u8 = 0x60;

/// Snap a POINT coordinate to the physical-pixel grid at `ppp`.
///
/// Background quads must tile without seams: two adjacent cells with the same
/// background are painted as separate rectangles whose shared edge lands on a
/// fractional pixel at most DPI scalings. Rounding that edge to a whole physical
/// pixel makes the left quad's right edge and the right quad's left edge the
/// SAME value, so they abut exactly — no bright hairline where the window
/// background shows through, and no double-blended overlap.
fn snap_to_physical(v: f32, ppp: f32) -> f32 {
    if ppp > 0.0 {
        (v * ppp).round() / ppp
    } else {
        v
    }
}

/// Draw one span's underline in `style`, from `x0` to `x1` with its top at `y`.
///
/// Every variant is drawn ANALYTICALLY from the span geometry (no glyph, no
/// texture), so all of them stay crisp and correctly-proportioned at any
/// `pixels_per_point` — the requirement that rules out rendering the curly
/// variant as a repeated `~`-like glyph, which aliases into mush on HiDPI.
// Geometry primitive: endpoints, thickness, colour, style and pixels-per-point
// are all independent painting parameters. A struct would not reduce the count,
// only rename it — the same rationale as `glyph_button`'s existing allow.
#[allow(clippy::too_many_arguments)]
fn paint_underline(
    painter: &egui::Painter,
    x0: f32,
    x1: f32,
    y: f32,
    thickness: f32,
    color: egui::Color32,
    style: c0pl4nd_core::grid::UnderlineStyle,
    ppp: f32,
) {
    use c0pl4nd_core::grid::UnderlineStyle as U;
    if x1 <= x0 {
        return;
    }
    // A solid horizontal bar from `a` to `b`, snapped so it is exactly the
    // requested thickness in physical pixels.
    let bar = |a: f32, b: f32, top: f32| {
        let ty = snap_to_physical(top, ppp);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(a, ty),
                egui::pos2(b, ty + thickness.max(1.0 / ppp.max(0.01))),
            ),
            0.0,
            color,
        );
    };
    match style {
        U::None => {}
        U::Single => bar(x0, x1, y),
        // Two hairlines with a gap of one thickness between them.
        U::Double => {
            bar(x0, x1, y - thickness);
            bar(x0, x1, y + thickness);
        }
        // Dot on / dot off.
        //
        // Each dot MUST be wider than TWO PHYSICAL PIXELS, and that is a hard
        // constraint of the rasteriser, not a taste call. `epaint`'s
        // `Tessellator::tessellate_rect` re-routes any un-stroked rect whose
        // WIDTH is `<= 2.0 * feathering` (and `feathering` is exactly one
        // physical pixel) into `tessellate_line_segment` between the rect's
        // top-centre and bottom-centre — i.e. it approximates a thin rect as a
        // VERTICAL hairline. For an underline dot that vertical segment is one
        // pixel long, so it feathers away to nothing.
        //
        // That is precisely what shipped: this arm drew `bar(x, x + thickness)`
        // every `thickness * 2.0`, and with `thickness = (ch * 0.06).max(1.0 /
        // ppp)` = 1.02pt at ppp 1.0 it emitted 46 rects ~1.02pt wide that
        // rasterised to ZERO pixels — the row was byte-identical to a row with
        // no underline at all, even at a per-channel tolerance of 90/255, while
        // the same run gave 94px solid and 60px dashed. `ESC[4:4m` was
        // indistinguishable from `ESC[24m`. `U::Dashed` only ever escaped it
        // because its dash is `thickness * 4.0` wide.
        //
        // Three physical pixels clears the threshold with margin, so the dot
        // takes the real rect path; its 1px HEIGHT then takes the HORIZONTAL
        // line-segment path, landing on the same crisp single scanline
        // `U::Single` does. A 3-on/3-off period stays visibly finer than
        // `U::Dashed` (4 on, 3 off), so the two styles remain distinct.
        //
        // The x edges are deliberately NOT run through `snap_to_physical` here:
        // `tessellate_rect` already rounds every filled rect to the physical
        // pixel grid (`round_rects_to_pixels`, on by default), so snapping first
        // is a measured no-op — with and without it this run renders the same 48
        // pixels in the same 16 dots. The physical-pixel WIDTH FLOOR is the whole
        // fix; anything else here would be decoration that reads as load-bearing.
        U::Dotted => {
            let px = 1.0 / ppp.max(0.01);
            let dot = (thickness * 2.0).max(3.0 * px);
            let step = (thickness * 4.0).max(6.0 * px);
            let mut x = x0;
            while x < x1 {
                bar(x, (x + dot).min(x1), y);
                x += step;
            }
        }
        // Longer dashes at a 7x period — visually distinct from dotted.
        U::Dashed => {
            let dash = (thickness * 4.0).max(2.0);
            let step = (thickness * 7.0).max(3.0);
            let mut x = x0;
            while x < x1 {
                bar(x, (x + dash).min(x1), y);
                x += step;
            }
        }
        // Undercurl (nvim LSP diagnostics): a sine sampled at ~1 physical pixel
        // so the wave has the same shape and amplitude in PHYSICAL terms on a
        // 1x and a 2x display.
        U::Curly => {
            let amplitude = thickness * 1.5;
            let period = (thickness * 6.0).max(4.0);
            let sample = (1.0 / ppp.max(0.01)).max(0.25);
            let mid = y + thickness * 0.5;
            let mut pts: Vec<egui::Pos2> = Vec::new();
            let mut x = x0;
            while x < x1 {
                let phase = (x - x0) / period * std::f32::consts::TAU;
                pts.push(egui::pos2(x, mid + phase.sin() * amplitude));
                x += sample;
            }
            // Always close on the span's right edge so the curl spans the full
            // run regardless of where the sampling loop happened to stop.
            let phase = (x1 - x0) / period * std::f32::consts::TAU;
            pts.push(egui::pos2(x1, mid + phase.sin() * amplitude));
            if pts.len() >= 2 {
                painter.add(egui::Shape::line(
                    pts,
                    egui::Stroke::new(thickness.max(1.0 / ppp.max(0.01)), color),
                ));
            }
        }
    }
}

/// Paint a pane's visible grid with egui's NATIVE text painter, using the
/// per-row colour runs from [`PaneTerm::grid_rows`]. This is the single,
/// engine-agnostic render path for BOTH the live window and the headless
/// snapshot tests — identical code, so a passing test faithfully proves the
/// live render. It deliberately uses egui's own glyph rasteriser (the same one
/// that draws the chrome, and the same approach SCR1B3 uses for coloured code)
/// rather than a glyphon GPU paint callback: the glyphon paint (in-pass
/// callback AND offscreen texture) composited black inside `egui_tiles` panes
/// on the real eframe/winit swapchain — a class of defect the wgpu test harness
/// could not reproduce — whereas native text renders reliably everywhere.
///
/// Rows are NOT wrapped (`max_width = INFINITY`): each terminal row stays one
/// visual line and is clipped at the pane edge by the caller's `painter_at`
/// clip rect, so row alignment is preserved.
///
/// The argument list is a bundle of per-frame render inputs (font size,
/// line-height, theme, focus, cursor config, effects, padding) threaded from the
/// single call site in [`C0pl4ndApp::render_pane_body`]; the
/// `too_many_arguments` allow matches that sibling free function for the same
/// reason.
///
/// Rows are painted ONE GALLEY PER ROW at the effective row pitch
/// ([`effective_row_pitch`] of the natural galley height and the configured
/// `line_height_px`) rather than as a single multi-row galley — egui's combined
/// galley uses the font's own line spacing, which the Line-height setting could
/// not influence. Per-row positioning makes the row pitch the live, configurable
/// thing the cursor / search / hit-test all share.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_grid_native(
    painter: &egui::Painter,
    rect: egui::Rect,
    term: &PaneTerm,
    galley_cache: &mut GalleyCache,
    font_size: f32,
    line_height_px: f32,
    theme: &c0pl4nd_core::Theme,
    focused: bool,
    cursor_cfg: c0pl4nd_core::config::CursorConfig,
    // Deterministic cursor-blink phase override for visual-QA capture; `None`
    // leaves the caret's phase free-running off the frame clock.
    cursor_blink_phase: Option<CursorBlinkPhase>,
    effects: c0pl4nd_core::config::EffectsConfig,
    padding: f32,
) {
    let default_fg = term_default_fg(theme);
    let font = egui::FontId::monospace(font_size);
    // Inset the grid by the configurable window padding (points), read live from
    // `config.window.padding` each frame (threaded down from `grid_ui`). Pure
    // [`grid_text_origin`] helper so the live-apply wiring is unit-testable.
    let origin = grid_text_origin(rect, padding);
    // Cell size in POINTS: `M` advance for width, the effective row pitch for the
    // vertical advance per grid row. This is the SAME `(cw, ch)` the cursor,
    // search highlight, and hyperlink hit-test use, so every Y stays aligned.
    let (cw, ch) = monospace_cell_points(painter, font_size, line_height_px);

    // Each row becomes one galley, painted at `origin.y + row_idx * ch`.
    // Damage-gated, already grouped per-row by [`PaneTerm::grid_rows`] (an `Rc`
    // clone on the idle/blinking-cursor path — no per-frame grid clone, run
    // rebuild, or newline-split). The fallback (dead session mid-frame) wraps the
    // mono text in the same `Rc` shape so the paint loop below is uniform.
    let rows: std::rc::Rc<Vec<Vec<ColorRun>>> = match term.grid_rows() {
        Some(rows) if !rows.is_empty() => rows,
        _ => {
            // No colour runs (e.g. dead session mid-frame): mono fallback so the
            // pane is never blank. One row per text line, all in the default fg.
            std::rc::Rc::new(
                term.grid_text()
                    .unwrap_or_default()
                    .lines()
                    .map(|line| vec![(line.to_string(), pane_term::RunStyle::plain(default_fg))])
                    .collect(),
            )
        }
    };

    // The effective chromatic intensity (gated by the explicit enable toggle),
    // resolved to a PHYSICAL-px-aware ghost offset so the fringe clears the glyph
    // on HiDPI panels (issue #28). Zero-cost when off (offset == 0 ⇒ skipped).
    let ppp = painter.ctx().pixels_per_point();
    let chroma = effects.effective_chromatic();
    let ghost_offset = chromatic_offset(chroma, ppp);
    let ghost_alpha = chromatic_ghost_alpha(chroma);
    // Style bits shared by every glyph this frame (font size + the fallback fg).
    // Folded into each glyph's cache key so a font-size or theme change relays
    // them (a font FAMILY change clears the whole cache via `clear()`).
    let style_key = row_style_key(font_size, default_fg);
    let default_fg32 = egui::Color32::from_rgb(default_fg.0, default_fg.1, default_fg.2);
    // Paint each grid CELL's glyph at its exact cell origin `origin + (col*cw,
    // row*ch)`. Positions are COMPUTED from the cell column, never accumulated
    // from glyph advances, so the layout is font-advance-independent: a wide
    // (CJK/emoji) or fallback glyph occupies its own cell(s) and can NEVER shift
    // another cell — and there is no proportional-font scatter (the failure mode
    // that reverted the per-run approach). `grid_rows` already split wide glyphs
    // into their own runs and skipped the continuation spacer, so `col_cells`
    // (advanced by each glyph's cell width) is the true grid column. Blank cells
    // are skipped (the background is already painted); this also bounds the glyph
    // count to the non-blank glyphs actually on screen.
    // --- PASS 1: per-cell BACKGROUNDS -------------------------------------
    // Every cell whose resolved background is NOT the window default gets a
    // filled quad, painted BEFORE any glyph so the text sits on top of it. This
    // is what makes `grep --color`, `ls` directory colours, `git diff`, fzf's
    // selected row, starship segments and every TUI's selected row show their
    // coloured block — the runs used to carry only a foreground, so all of that
    // rendered as plain text on the window background. It is also what makes
    // reverse video (SGR `7`) visible at all: with no quad, an inverse cell drew
    // its BACKGROUND colour as text onto an unchanged background.
    //
    // `row_cell_spans` merges neighbouring same-style runs, so a highlighted
    // region is ONE quad rather than one per glyph, and the quads are snapped to
    // the physical pixel grid: adjacent spans share a snapped boundary, so they
    // tile exactly with no hairline seam and no overlap at any DPI.
    let bold_font = egui::FontId::new(font_size, fonts::bold_monospace_family());
    let faux_bold = !fonts::bold_face_available();
    for (row_idx, runs) in rows.iter().enumerate() {
        let row_y = origin.y + row_idx as f32 * ch;
        let y0 = snap_to_physical(row_y, ppp);
        let y1 = snap_to_physical(row_y + ch, ppp);
        for span in pane_term::row_cell_spans(runs) {
            let Some(bg) = span.style.bg else {
                continue; // window default — the common case, no quad needed
            };
            let x0 = snap_to_physical(origin.x + span.col as f32 * cw, ppp);
            let x1 = snap_to_physical(origin.x + (span.col + span.width) as f32 * cw, ppp);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)),
                0.0,
                egui::Color32::from_rgb(bg.0, bg.1, bg.2),
            );
        }
    }

    // --- PASS 2: glyphs ----------------------------------------------------
    for (row_idx, runs) in rows.iter().enumerate() {
        let row_y = origin.y + row_idx as f32 * ch;
        // `row_glyph_cells` is the single source of truth for per-cell X: each
        // painted glyph paired with its grid cell column (wide glyphs advance 2,
        // blanks skipped). Positions are COMPUTED from the cell column, never
        // accumulated from glyph advances — see its doc + unit tests.
        for (c, style, col_cells) in pane_term::row_glyph_cells(runs) {
            let rgb = style.fg;
            let attrs = glyph_cache::GlyphAttrs {
                bold: style.bold,
                italic: style.italic,
            };
            // A bold cell is drawn with the dedicated bold FAMILY (egui's FontId
            // selects a family, not a weight). When the machine has no bold cut
            // that family mirrors the regular stack, so the glyph is additionally
            // double-struck a half physical pixel to the right — faux bold, the
            // same fallback every terminal uses rather than drawing SGR-1 thin.
            let glyph_font = if style.bold { &bold_font } else { &font };
            let cell_origin = egui::pos2(origin.x + col_cells as f32 * cw, row_y);
            // --- chromatic aberration (CRT effect, off by default): pure-
            // channel ghosts at ±offset BEHIND the crisp glyph (red left,
            // blue right), edge-weighted by the row's vertical position.
            if ghost_offset > 0.0 {
                let off =
                    chromatic_edge_weighted_offset(ghost_offset, row_y, rect.top(), rect.bottom());
                let red = egui::Color32::from_rgba_unmultiplied(255, 0, 0, ghost_alpha);
                let red_g = galley_cache.glyph(
                    painter,
                    glyph_cache_key(c, (ghost_alpha, 0, 1), RowPass::GhostRed, style_key, attrs),
                    || build_glyph_job(c, glyph_font, red, attrs),
                );
                painter.galley(cell_origin + egui::vec2(-off, 0.0), red_g, default_fg32);
                let blue = egui::Color32::from_rgba_unmultiplied(0, 0, 255, ghost_alpha);
                let blue_g = galley_cache.glyph(
                    painter,
                    glyph_cache_key(c, (ghost_alpha, 0, 2), RowPass::GhostBlue, style_key, attrs),
                    || build_glyph_job(c, glyph_font, blue, attrs),
                );
                painter.galley(cell_origin + egui::vec2(off, 0.0), blue_g, default_fg32);
            }
            // Crisp main pass in the cell's real colour, on top of ghosts.
            let color = egui::Color32::from_rgb(rgb.0, rgb.1, rgb.2);
            let main_g = galley_cache.glyph(
                painter,
                glyph_cache_key(c, rgb, RowPass::Main, style_key, attrs),
                || build_glyph_job(c, glyph_font, color, attrs),
            );
            // Faux bold: no bold cut is installed, so double-strike the SAME
            // galley half a PHYSICAL pixel right, UNDER the crisp pass. That
            // thickens the stem at any DPI without shifting the cell (the offset
            // is sub-cell), which is how a terminal shows SGR-1 on a font that
            // ships only one weight.
            if style.bold && faux_bold {
                painter.galley(
                    cell_origin + egui::vec2(0.5 / ppp.max(0.01), 0.0),
                    std::sync::Arc::clone(&main_g),
                    default_fg32,
                );
            }
            painter.galley(cell_origin, main_g, default_fg32);
        }
    }

    // --- PASS 3: line decorations -----------------------------------------
    // Underlines (including the `4:0..5` styled variants and the SGR 58/59
    // underline colour) and strikethrough are drawn ANALYTICALLY per contiguous
    // span rather than baked into each glyph's galley. Per-span is what makes an
    // underline continuous across a word instead of one dash per glyph, and
    // analytic is what makes the curly variant survive HiDPI — a sampled sine
    // scales with `pixels_per_point`, a pre-rendered squiggle glyph does not.
    for (row_idx, runs) in rows.iter().enumerate() {
        let row_y = origin.y + row_idx as f32 * ch;
        for span in pane_term::row_cell_spans(runs) {
            if !span.style.has_decoration() {
                continue;
            }
            let x0 = origin.x + span.col as f32 * cw;
            let x1 = origin.x + (span.col + span.width) as f32 * cw;
            // At least one PHYSICAL pixel, so a decoration is never sub-pixel and
            // invisible on a low-DPI display.
            let thickness = (ch * 0.06).max(1.0 / ppp.max(0.01));
            let deco_rgb = span.style.underline_color.unwrap_or(span.style.fg);
            let deco = egui::Color32::from_rgb(deco_rgb.0, deco_rgb.1, deco_rgb.2);
            if span.style.underline != c0pl4nd_core::grid::UnderlineStyle::None {
                paint_underline(
                    painter,
                    x0,
                    x1,
                    row_y + ch - thickness * 2.0,
                    thickness,
                    deco,
                    span.style.underline,
                    ppp,
                );
            }
            if span.style.strikeout {
                // Strikethrough always takes the TEXT colour: SGR 58 scopes the
                // custom colour to the underline only.
                let fg = span.style.fg;
                let y = snap_to_physical(row_y + ch * 0.55, ppp);
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, y),
                        egui::pos2(x1, y + thickness.max(1.0 / ppp.max(0.01))),
                    ),
                    0.0,
                    egui::Color32::from_rgb(fg.0, fg.1, fg.2),
                );
            }
            if span.style.overline {
                // SGR 53: a line along the TOP of the cell, in the TEXT colour
                // (like strikeout, the custom SGR-58 colour is underline-scoped).
                let fg = span.style.fg;
                let y = snap_to_physical(row_y, ppp);
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, y),
                        egui::pos2(x1, y + thickness.max(1.0 / ppp.max(0.01))),
                    ),
                    0.0,
                    egui::Color32::from_rgb(fg.0, fg.1, fg.2),
                );
            }
        }
    }

    // --- terminal cursor ---
    if let Some((row, col)) = term.cursor_cell() {
        let cell_min = origin + egui::vec2(col as f32 * cw, row as f32 * ch);
        let cell = egui::Rect::from_min_size(cell_min, egui::vec2(cw, ch));
        let cur = c0pl4nd_core::theme::parse_hex(&theme.cursor).unwrap_or((0, 255, 144));
        let col32 = egui::Color32::from_rgb(cur.0, cur.1, cur.2);
        // Blink only on the focused pane (and only if configured), with an
        // optional pinned phase for deterministic visual-QA capture.
        let on = cursor_blink_on(
            cursor_blink_phase,
            cursor_cfg.blink,
            focused,
            painter.ctx().input(|i| i.time),
        );
        if on {
            match cursor_cfg.style {
                c0pl4nd_core::config::CursorStyle::Block => {
                    if focused {
                        // Semi-transparent fill so the glyph beneath stays legible.
                        painter.rect_filled(cell, 1.0, col32.gamma_multiply(0.55));
                    } else {
                        painter.rect_stroke(
                            cell,
                            1.0,
                            egui::Stroke::new(1.0f32, col32),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                c0pl4nd_core::config::CursorStyle::Bar => {
                    let bar = egui::Rect::from_min_size(cell_min, egui::vec2(2.0, ch));
                    painter.rect_filled(bar, 0.0, col32);
                }
                c0pl4nd_core::config::CursorStyle::Underline => {
                    let under = egui::Rect::from_min_size(
                        cell_min + egui::vec2(0.0f32, ch - 2.0),
                        egui::vec2(cw, 2.0),
                    );
                    painter.rect_filled(under, 0.0, col32);
                }
            }
        }
    }

    // --- CRT scanlines (research §1): the LAST thing painted over this pane's
    // grid, so the dark bands dim the glyphs + cursor uniformly. Filled dark
    // bands at a physical-px-anchored period + an animated rolling scan band.
    // Drawn only when the setting is on (strictly zero-cost otherwise); the
    // repaint request keeps the roll animating without an explicit timer.
    if effects.crt_scanlines {
        // F2-2: honour the user's reduced-motion preference (env override OR the
        // OS accessibility setting). When reduced motion is requested, FREEZE the
        // rolling scan band (`t = 0` → a static frame; the dark scan-line bands
        // are a texture, not motion, so they remain) and STOP the per-frame
        // animation repaint. This makes the "Auto-disabled under reduced-motion"
        // promise the settings UI already shows actually true.
        let reduce = c0pl4nd_core::reduced_motion::reduced_motion();
        // Freeze the scanline drift (static texture, bands still painted) under
        // reduced-motion OR when the master animation switch is off; otherwise the
        // drift clock is scaled by the dedicated `scanline_speed` multiplier (its
        // own Motion → Scanline-drift-speed slider), so the scan bands roll at
        // their configured rate independently of the other overlays and the
        // UI-transition-speed slider. Default 1.0 reproduces the shipped roll.
        let speed = effects.clamped_scanline_speed();
        let animate = !reduce && effects.animations_enabled;
        let t = if animate {
            painter.ctx().input(|i| i.time) as f32 * speed
        } else {
            0.0
        };
        paint_crt_scanlines(painter, rect, ppp, t, effects.scanline_darkness);
        if animate {
            painter.ctx().request_repaint();
        }
    }
}

/// Fold the per-frame style bits (font size + fallback fg) into a stable seed for
/// a glyph's cache key. Font SIZE is captured here (so a size change relays the
/// glyphs); a font FAMILY/fallback change instead clears the whole cache (the
/// galleys reference the old atlas). Pure → unit-testable.
pub(super) fn row_style_key(font_size: f32, default_fg: (u8, u8, u8)) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    font_size.to_bits().hash(&mut h);
    default_fg.hash(&mut h);
    h.finish()
}
