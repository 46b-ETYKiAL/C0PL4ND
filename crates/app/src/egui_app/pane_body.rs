//! The pane-body RENDER PATH: painting one terminal pane for a frame.
//!
//! Lifted verbatim out of `egui_app/mod.rs`. [`C0pl4ndApp::render_pane_body`] is
//! the single largest item that module held, and it is deliberately a FREE
//! associated function rather than a `&mut self` method: the `grid_ui` closure
//! has to borrow `terms`/`theme` disjointly from `self.grid_tree`, which
//! `tree.ui` already borrows mutably. That is the classic egui_tiles borrow
//! split, and it is why this one lifts cleanly while the `&mut self` frame
//! driver around it does not.
//!
//! It stays an inherent method (an `impl` block here rather than a bare `fn`) so
//! the one call site in `grid_ui` keeps its original `Self::render_pane_body`
//! spelling and a reader following the call chain lands where they expect.
//!
//! Its interface type [`PaneBodyOutcome`] deliberately does NOT move with it:
//! that struct is the contract between this render and the `frame_tick` caller
//! that applies the queued actions, so it stays beside the caller. A child
//! module can reach its parent's private items, so nothing needed widening for
//! that.

use super::*;

impl C0pl4ndApp {
    /// Paint one terminal pane's body and wire its per-frame interaction:
    ///
    /// 1. Allocate the pane rect and paint the theme background quad + focus ring
    ///    behind the glyphs (so text never blends directly against the acrylic).
    /// 2. Compute the physical-pixel size and DEBOUNCED-resize the PTY to fit.
    /// 3. DISPLAY the visible grid with egui's native coloured-text painter via
    ///    [`paint_grid_native`].
    /// 4. Report click (refocus) + drag-start (egui_tiles).
    ///
    /// A failed-spawn pane paints an error label instead of a grid — never a
    /// panic. This is a FREE function (not `&mut self`) so the `grid_ui` closure
    /// can borrow `terms`/`theme` disjointly from `self.grid_tree` (which
    /// `tree.ui` borrows mutably) — the classic egui_tiles borrow split.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_pane_body(
        ui: &mut egui::Ui,
        pane_id: PaneId,
        focused: bool,
        terms: &mut HashMap<PaneId, PaneTerm>,
        pending_spawn: &mut HashSet<PaneId>,
        // Per-pane working dirs from a restored layout snapshot; consumed here so
        // a deferred first-spawn opens in its saved cwd (disjoint borrow, like
        // `pending_spawn`).
        restored_cwds: &mut HashMap<PaneId, String>,
        galley_cache: &mut GalleyCache,
        image_textures: &mut ImageTextureCache,
        theme: &c0pl4nd_core::Theme,
        // The configured `TERM` advertised to a deferred-first-spawn pane, so the
        // initial pane's child PTY sees the same `TERM` as every later pane.
        term: &str,
        // The ACTIVE shell profile, so a deferred first-spawn runs the same shell
        // the immediate `spawn_term_in` path would (it used to always spawn the
        // platform default, ignoring the profile entirely).
        spawn_profile: SpawnProfile<'_>,
        font_size: f32,
        line_height_px: f32,
        cursor_cfg: c0pl4nd_core::config::CursorConfig,
        // Deterministic cursor-blink phase override for visual-QA capture; `None`
        // leaves the caret's phase free-running off the frame clock.
        cursor_blink_phase: Option<CursorBlinkPhase>,
        effects: c0pl4nd_core::config::EffectsConfig,
        padding: f32,
        bg_alpha: u8,
        search: Option<SearchHighlight<'_>>,
        links: &[(CellSpan, String)],
        // True while Ctrl/Cmd is held: enables the whole-pane link underline +
        // click-to-open (the hover underline shows regardless). Gating click on
        // this — not on `links` being non-empty — is what lets links be detected
        // every frame for the hover affordance without a plain click opening one.
        link_modifier: bool,
        // The focused pane's in-progress IME pre-edit (composition) string, for
        // display at the cursor (F3-1). `None` for non-focused panes and when no
        // composition is active. Never sent to the PTY — display only.
        ime_preedit: Option<&str>,
        // The app-wide mouse text selection state, updated here on drag and read
        // by the selection painter below. Threaded as `&mut` (a separate field
        // from `terms`/`theme`) so the egui_tiles disjoint-borrow split holds.
        selection: &mut Option<Selection>,
        // While the atlas-warmup gate is open, skip painting the grid glyphs (the
        // pane still lays out, spawns, sizes, and paints its background) so no
        // glyph is sampled before the warmed atlas is uploaded + resident.
        warming: bool,
    ) -> PaneBodyOutcome {
        let (rect, resp) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());

        // Right-click context menu (table-stakes terminal gesture). Copy +
        // Clear-scrollback run INLINE here (they only touch `terms`); split /
        // new / close need `&mut self`, so they are QUEUED into the outcome and
        // applied by the caller after the egui_tiles closure releases its
        // borrows. Paste is intentionally disabled: egui exposes no clipboard
        // READ, so paste can only arrive via the OS Ctrl/Cmd+Shift+V event.
        let mut context_menu_action: Option<ContextMenuAction> = None;
        resp.context_menu(|ui| {
            let has_selection = selection
                .as_ref()
                .is_some_and(|s| s.pane == pane_id && s.anchor != s.head);
            if ui
                .add_enabled(has_selection, egui::Button::new("Copy"))
                .clicked()
            {
                if let Some(sel) = *selection {
                    if sel.pane == pane_id {
                        if let Some(term) = terms.get(&pane_id) {
                            // Selection anchors are ABSOLUTE scrollback lines; map
                            // them to the CURRENT display rows before extracting
                            // (the view may be scrolled back), exactly like the
                            // drag-release and Ctrl/Cmd+Shift+C copy paths. Passing
                            // the absolute coords straight to `selection_text`
                            // (which takes DISPLAY coords) would copy the wrong
                            // rows whenever the pane is scrolled up.
                            let rows = term.size().1 as usize;
                            let ws = term.window_start().unwrap_or(0);
                            if let Some((a, b)) =
                                selection_visible_rows(sel.anchor, sel.head, ws, rows)
                            {
                                let block = sel.mode == SelectionMode::Block;
                                if let Some(text) = term.selection_text(a, b, block) {
                                    ui.ctx().copy_text(text);
                                }
                            }
                        }
                    }
                }
                ui.close_kind(egui::UiKind::Menu);
            }
            // Copy the WHOLE buffer (scrollback + screen) — the no-selection
            // companion to Copy, always available. The mouse-selection Copy above
            // is display-window-bound; this copies the entire retained buffer.
            if ui.button("Copy all").clicked() {
                if let Some(t) = terms.get(&pane_id) {
                    if let Some(text) = t.buffer_text() {
                        ui.ctx().copy_text(text);
                    }
                }
                ui.close_kind(egui::UiKind::Menu);
            }
            ui.add_enabled(false, egui::Button::new("Paste"))
                .on_hover_text("Paste with the keyboard shortcut (Ctrl/Cmd+Shift+V)");
            ui.separator();
            if ui.button("Clear scrollback").clicked() {
                if let Some(t) = terms.get_mut(&pane_id) {
                    t.clear_scrollback();
                }
                ui.close_kind(egui::UiKind::Menu);
            }
            ui.separator();
            if ui.button("Split right").clicked() {
                context_menu_action = Some(ContextMenuAction::SplitRight);
                ui.close_kind(egui::UiKind::Menu);
            }
            if ui.button("Split down").clicked() {
                context_menu_action = Some(ContextMenuAction::SplitDown);
                ui.close_kind(egui::UiKind::Menu);
            }
            if ui.button("New tab").clicked() {
                context_menu_action = Some(ContextMenuAction::NewTerminal);
                ui.close_kind(egui::UiKind::Menu);
            }
            ui.separator();
            if ui.button("Close pane").clicked() {
                context_menu_action = Some(ContextMenuAction::ClosePane(pane_id));
                ui.close_kind(egui::UiKind::Menu);
            }
        });

        let ppp = ui.ctx().pixels_per_point();
        let painter = ui.painter_at(rect);
        // Cell metrics from the SAME monospace font the grid is drawn with, so
        // the PTY's `(cols, rows)` match the rendered glyph size. Measured via a
        // probe galley (`Painter::layout_job`); ppp scales points → physical px
        // to match the `rect * ppp` resize math below. The configured
        // Line-height folds into the row pitch here so the PTY reflows to the
        // SAME pitch the painter draws at.
        let cell_metrics = monospace_cell_metrics(&painter, font_size, ppp, line_height_px);

        // --- deferred first-spawn at the MEASURED size (bug #40) ---
        // The configurable inner padding (points) insets the text on every edge,
        // so the grid area is the rect minus 2×padding per axis. We need it both
        // for the deferred spawn (below) and the debounced resize (further down),
        // so compute it ONCE here.
        let pad = padding.max(0.0);
        let px_w = (rect.width() - 2.0 * pad).max(0.0) * ppp;
        let px_h = (rect.height() - 2.0 * pad).max(0.0) * ppp;
        // A pane whose PTY was deferred (the initial pane) is spawned HERE, at the
        // real `(cols, rows)` derived from its measured rect — exactly the size
        // `resize_to_px` would otherwise reflow it to a frame later. Spawning at
        // the correct size up front means the subsequent debounced `resize_to_px`
        // is a no-op, so cmd's banner/prompt cursor never snaps home to (0,0).
        if pending_spawn.remove(&pane_id) {
            let (cols, rows) = cell_metrics.cols_rows(px_w, px_h);
            // A restored pane opens in its saved cwd; otherwise the one-shot
            // `--cwd` / `-d` startup directory (the "Open C0PL4ND here" shell
            // verb) applies to the FIRST pane spawned; a fresh pane with neither
            // opens in the default dir. Both `remove` and `take_startup_cwd`
            // CONSUME their entry, so a later re-use of the id can never inherit
            // a stale cwd and later tabs/splits never inherit the CLI flag.
            //
            // Routed through the SHARED `spawn_pane_term` funnel so this arm
            // honours the active shell profile exactly like the immediate path.
            // It used to call the default-shell spawns directly, so a deferred
            // pane under a named profile came back running the WRONG shell.
            let cwd = restored_cwds
                .remove(&pane_id)
                .or_else(crate::cli_cwd::take_startup_cwd);
            let pane_term = spawn_pane_term(
                theme.clone(),
                spawn_profile.program,
                spawn_profile.args,
                cols,
                rows,
                Some(term),
                cwd.as_deref(),
            );
            terms.insert(pane_id, pane_term);
        }

        // --- background quad (theme bg) + focus ring ---
        // SINGLE-BACKDROP RULE (opacity linearity): the terminal background is
        // painted EXACTLY ONCE — by the `CentralPanel` `central_fill` behind the
        // whole tiling grid (see `ui`), which already carries the opacity-folded
        // `pane_bg_alpha` AND backs the gaps between panes (so an opaque window
        // stays solid, no desktop leak in the 4px seams). This per-pane body used
        // to ALSO fill the pane rect at the same `bg_alpha`, so the two identical
        // theme-bg layers COMPOUNDED (`opacity` over `opacity` ≈ `opacity²` — at
        // 0.7 → ~0.91 effective), which read as a heavy haze that never went clear
        // like SCR1B3 (whose editor paints its background once). Dropping this
        // second fill makes the opacity slider LINEAR — one alpha over the desktop.
        // The tint (background layer, behind `central_fill`) still reaches the pane
        // through the single translucent backing, in exactly one pass. `bg_alpha`
        // stays in use below to fold the bezel/focus-ring stroke.
        //
        // Focus ring + bezel follow the active theme (accent on focus, bezel
        // otherwise) so the grid chrome matches the rest of the themed UI. Both
        // fold the window-transparency alpha (`bg_alpha`) so the border is as
        // translucent as the pane it frames — a full-alpha border over a
        // see-through window read as a hard opaque line "unaffected by tint or
        // transparency" (the reported divider bug). At an opaque window
        // (`bg_alpha == 255`) `fold_alpha` returns the colour unchanged.
        let pane_colors = theme::ChromeColors::from_theme(theme);
        let stroke = if focused {
            // Focus is SEMANTIC (which pane has keyboard focus), so floor its
            // folded alpha: it still tints/fades with the window, but never drops
            // below a legible strength even at very low opacity.
            const FOCUS_RING_ALPHA_FLOOR: u8 = 150;
            let a = bg_alpha.max(FOCUS_RING_ALPHA_FLOOR);
            egui::Stroke::new(2.0f32, window_effects::fold_alpha(pane_colors.accent, a))
        } else {
            // The unfocused bezel is pure definition — let it fully fade into
            // negative space as the window goes see-through.
            egui::Stroke::new(
                1.0f32,
                window_effects::fold_alpha(pane_colors.bezel, bg_alpha),
            )
        };
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(4),
            stroke,
            egui::StrokeKind::Inside,
        );

        // --- resize the PTY to fit this rect (debounced) ---
        // The configurable inner padding (points) insets the text on every edge,
        // so the area available for the terminal grid is the rect minus 2×padding
        // on each axis. Subtract it BEFORE the px conversion so the computed
        // (cols, rows) match what `paint_grid_native` actually draws inside the
        // padded origin — otherwise a large padding would size the PTY for the
        // full rect and clip the last row/column.
        let pad = padding.max(0.0);
        let px_w = (rect.width() - 2.0 * pad).max(0.0) * ppp;
        let px_h = (rect.height() - 2.0 * pad).max(0.0) * ppp;
        if let Some(term) = terms.get_mut(&pane_id) {
            term.resize_to_px(px_w, px_h, cell_metrics);
        }

        // --- display the grid ---
        match terms.get(&pane_id) {
            Some(term) if term.error().is_none() => {
                // Single native render path for BOTH the live window and headless
                // snapshots (see `paint_grid_native`). egui's own glyph painter
                // draws the coloured grid reliably on the real swapchain — the
                // glyphon GPU paths (callback + offscreen texture) composited
                // black inside `egui_tiles` panes live while passing the wgpu
                // test harness.
                if !warming {
                    paint_grid_native(
                        &painter,
                        rect,
                        term,
                        galley_cache,
                        font_size,
                        line_height_px,
                        theme,
                        focused,
                        cursor_cfg,
                        cursor_blink_phase,
                        effects,
                        pad,
                    );
                }
                // Find-overlay highlight: tint every match span (and outline the
                // active one) over the rendered grid. Only the focused pane while
                // the overlay is open carries a `SearchHighlight`.
                if let Some(hl) = search {
                    paint_search_highlight(
                        &painter,
                        rect,
                        font_size,
                        line_height_px,
                        pad,
                        &pane_colors,
                        // The current match takes the theme's CURSOR colour — the
                        // one palette entry that already means "where you are" —
                        // so it is hue-distinct from the accent tint the other
                        // matches share, in every theme.
                        c0pl4nd_core::theme::parse_hex(&theme.cursor)
                            .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
                            .unwrap_or(pane_colors.accent),
                        hl,
                    );
                }
            }
            Some(term) => {
                // Failed spawn: show the error, never panic.
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    term.error().unwrap_or(
                        "This pane couldn't open a terminal. Close it and open a new pane.",
                    ),
                    egui::FontId::monospace(14.0),
                    pane_colors.fg,
                );
            }
            None => {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "This pane is empty. Open a new pane to start a terminal.",
                    egui::FontId::monospace(14.0),
                    pane_colors.fg,
                );
            }
        }

        // Hyperlinks. `links` holds the detected URL spans for the FOCUSED pane
        // (empty for the others), computed EVERY frame. `link_modifier` is true
        // while Ctrl/Cmd is held. Affordances:
        //   - HOVER (always, no modifier): underline the URL under the pointer +
        //     show the hand cursor, so a link is discoverable; the hand signals
        //     "Ctrl/Cmd+click to open".
        //   - Ctrl/Cmd HELD: underline EVERY URL (the whole-pane click affordance)
        //     and OPEN the one a click lands on.
        // The pixel→cell mapping ([`cell_at_pos`]) and the span hit test are pure
        // + unit-tested; only this thin wiring + the OS-opener side effect live
        // here.
        let mut opened_url = None;
        if !links.is_empty() {
            let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
            let origin = grid_text_origin(rect, pad);
            if link_modifier {
                paint_link_underlines(&painter, origin, cw, ch, &pane_colors, links);
            }
            if let Some(hover) = resp.hover_pos() {
                if let Some((r, c)) = cell_at_pos(hover, origin, cw, ch) {
                    if let Some(span) = link_span_at_cell(links, r, c) {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        // Without the modifier, underline JUST the hovered link
                        // (with it, every link is already underlined above).
                        if !link_modifier {
                            paint_one_link_underline(&painter, origin, cw, ch, &pane_colors, span);
                        }
                    }
                }
            }
            if link_modifier && resp.clicked() {
                if let Some(click) = resp.interact_pointer_pos() {
                    if let Some((r, c)) = cell_at_pos(click, origin, cw, ch) {
                        if let Some(url) = link_url_at_cell(links, r, c) {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                            opened_url = Some(url.to_string());
                        }
                    }
                }
            }
        }

        // --- mouse reporting (E6) + local wheel scrollback ---
        // When the program in this pane has grabbed the mouse (?1000/?1002/?1003)
        // translate pointer gestures into `encode_mouse` reports written to its
        // PTY (mouse in vim/tmux/htop/less). Otherwise the wheel scrolls this
        // pane's local scrollback. Two conventional overrides force LOCAL
        // handling even while a program grabs the mouse: holding Shift (the
        // standard "let me select/scroll" escape) and Ctrl-with-a-link-under-the-
        // pointer (`links` is non-empty only then — that click opens the URL).
        // Without this the canonical egui binary reported NO mouse at all and
        // could not scroll back through history; the legacy winit shell did both.
        let mut mouse_captured = false;
        let mut copy_selection: Option<String> = None;
        {
            use c0pl4nd_core::term::{MouseButton, MouseEventKind, MouseMode, MouseModifiers};
            let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
            let origin = grid_text_origin(rect, pad);
            // The pane's grid size (cols, rows), so a reported mouse cell can be
            // clamped to the grid — `cell_at_pos` only guards the LOW edge.
            let pane_size = terms.get(&pane_id).map(PaneTerm::size);
            // The absolute line at the top of the visible window THIS frame, so a
            // mouse selection is anchored to absolute scrollback lines (not the
            // display row, which changes as the view scrolls).
            let window_start = terms
                .get(&pane_id)
                .and_then(PaneTerm::window_start)
                .unwrap_or(0);
            // 1-based (col, row) of a screen-space point over the grid, if any.
            // Clamped to the grid's high edge too: a point over the trailing
            // padding or a fractional last cell must not encode an out-of-range
            // cell into the SGR/X10 mouse report (a conformant terminal clamps
            // reported cells to the grid bounds; oversized values make TUIs like
            // vim/tmux mis-parse the report).
            let cell_of = |pos: egui::Pos2| -> Option<(usize, usize)> {
                let (r, c) = cell_at_pos(pos, origin, cw, ch)?;
                let (cols, rows) = pane_size?;
                if cols == 0 || rows == 0 {
                    return None;
                }
                Some(((c + 1).min(cols as usize), (r + 1).min(rows as usize)))
            };
            let m = ui.input(|i| i.modifiers);
            let mods = MouseModifiers {
                shift: m.shift,
                alt: m.alt,
                control: m.ctrl,
            };
            // BOTH axes. egui folds a wheel notch into a SINGLE axis before the
            // app sees it: with the horizontal-scroll modifier (Shift by default)
            // held it rewrites the delta as `vec2(x + y, 0.0)`, leaving `.y` at
            // ZERO. The mouse-REPORT branch below can keep reading `.y` alone
            // (Shift forces LOCAL handling, so a reported wheel is never folded),
            // but the local scrollback branch must consume whichever axis the
            // notch landed on — see `wheel_scroll_lines`.
            let scroll = ui.input(|i| i.smooth_scroll_delta);
            let scroll_y = scroll.y;
            // egui's own points-per-wheel-line, so a notch count can be recovered
            // from the points it hands us.
            let points_per_notch = ui.ctx().options(|o| o.input_options.line_scroll_speed);
            let mode = terms
                .get(&pane_id)
                .map(PaneTerm::mouse_mode)
                .unwrap_or(MouseMode::Off);
            // Report to the program only when it grabbed the mouse, Shift is not
            // forcing local selection, and the link modifier is not held (a
            // Ctrl/Cmd+click opens a hovered link instead of being reported).
            let report = mode != MouseMode::Off && !m.shift && !link_modifier;
            if report {
                // Button press/release at the interacted cell.
                let buttons = [
                    (egui::PointerButton::Primary, MouseButton::Left),
                    (egui::PointerButton::Middle, MouseButton::Middle),
                    (egui::PointerButton::Secondary, MouseButton::Right),
                ];
                let pos = resp
                    .interact_pointer_pos()
                    .or(resp.hover_pos())
                    .or_else(|| ui.input(|i| i.pointer.latest_pos()));
                if let Some(pos) = pos {
                    if let Some((col, row)) = cell_of(pos) {
                        if let Some(term) = terms.get_mut(&pane_id) {
                            for (egui_btn, term_btn) in buttons {
                                if ui.input(|i| i.pointer.button_pressed(egui_btn)) {
                                    mouse_captured |= term.report_mouse(
                                        term_btn,
                                        mods,
                                        col,
                                        row,
                                        MouseEventKind::Press,
                                    );
                                }
                                if ui.input(|i| i.pointer.button_released(egui_btn)) {
                                    term.report_mouse(
                                        term_btn,
                                        mods,
                                        col,
                                        row,
                                        MouseEventKind::Release,
                                    );
                                }
                            }
                            // Motion: ?1002 reports drag (button held), ?1003 any
                            // motion. encode_mouse gates by mode, so a bare hover
                            // under ?1002 yields nothing.
                            if resp.dragged() || resp.hovered() {
                                let held = if ui.input(|i| i.pointer.primary_down()) {
                                    MouseButton::Left
                                } else if ui.input(|i| i.pointer.secondary_down()) {
                                    MouseButton::Right
                                } else if ui.input(|i| i.pointer.middle_down()) {
                                    MouseButton::Middle
                                } else {
                                    MouseButton::None
                                };
                                if term.report_mouse(held, mods, col, row, MouseEventKind::Motion) {
                                    mouse_captured = true;
                                }
                            }
                        }
                    }
                }
                // Wheel → buttons 64/65 (one report per ~cell of travel, capped).
                if scroll_y.abs() > f32::EPSILON {
                    let pos = resp
                        .hover_pos()
                        .or_else(|| ui.input(|i| i.pointer.latest_pos()));
                    if let (Some(pos), Some(term)) = (pos, terms.get_mut(&pane_id)) {
                        if let Some((col, row)) = cell_of(pos) {
                            let btn = if scroll_y > 0.0 {
                                MouseButton::WheelUp
                            } else {
                                MouseButton::WheelDown
                            };
                            let ticks = ((scroll_y.abs() / ch.max(1.0)).round() as i32).clamp(1, 8);
                            for _ in 0..ticks {
                                term.report_mouse(btn, mods, col, row, MouseEventKind::Press);
                            }
                            mouse_captured = true;
                        }
                    }
                }
            } else {
                // Local gesture (the program has NOT grabbed the mouse, or Shift
                // forces local): a primary-drag selects grid text and copies it on
                // release; a plain click clears any selection; the wheel scrolls
                // this pane's scrollback. This is the mouse text-selection the egui
                // shell lacked entirely (the legacy shell had it).
                // NOTE (selection lifetime): this path deliberately does NOT fall
                // back to the global `pointer.latest_pos()` the mouse-REPORTING
                // branch above uses. That fallback made a primary press ANYWHERE —
                // including inside the floating right-click context menu, which
                // always drops down-RIGHT over the pane and so maps to a real grid
                // cell — reset `selection` to an empty `anchor == head`. Since the
                // menu's "Copy" item is gated on `anchor != head` and `clicked()`
                // fires on RELEASE, the press that reached for Copy disabled Copy.
                // `interact_pointer_pos()` still tracks a drag that leaves the pane,
                // so nothing is lost.
                let pos = resp.interact_pointer_pos().or(resp.hover_pos());
                // Hit cell as an ABSOLUTE (line, col): display row + window_start.
                let cell0 = pos
                    .and_then(|p| cell_at_pos(p, origin, cw, ch))
                    .map(|(r, c)| (window_start + r, c));
                // ...and the press itself is gated on the pointer being over THIS
                // pane's body with nothing floating above it. `contains_pointer`
                // resolves through the layer stack, so an open context menu / popup
                // / modal over the pane makes it false — a click on a menu item can
                // no longer clobber the selection that item is about to copy.
                let press_over_body = resp.contains_pointer();
                if press_over_body
                    && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
                {
                    if let Some((line, c)) = cell0 {
                        // Alt-drag selects a rectangular BLOCK; a plain drag is
                        // line-wise. The mode is fixed at press and carried for the
                        // whole drag.
                        let mode = if ui.input(|i| i.modifiers.alt) {
                            SelectionMode::Block
                        } else {
                            SelectionMode::Linewise
                        };
                        *selection = Some(Selection {
                            pane: pane_id,
                            anchor: (line, c),
                            head: (line, c),
                            mode,
                        });
                        mouse_captured = true;
                    }
                }
                // Double-click selects the WORD under the cursor; triple-click
                // selects the whole LINE. Both set an absolute-coords selection
                // AND copy it immediately (so even a single-char word copies),
                // the table-stakes terminal gesture the egui shell lacked. The
                // release-clear below is suppressed on these frames so it cannot
                // wipe the fresh selection.
                let multi_click = resp.double_clicked() || resp.triple_clicked();
                if multi_click {
                    if let Some((line, c)) = cell0 {
                        let r = line.saturating_sub(window_start);
                        let (start, end) = if resp.triple_clicked() {
                            let cols = pane_size.map(|(cc, _)| cc as usize).unwrap_or(0);
                            (0, cols.saturating_sub(1))
                        } else {
                            let row = terms
                                .get(&pane_id)
                                .map(|t| t.display_row_chars(r))
                                .unwrap_or_default();
                            word_bounds(&row, c)
                        };
                        *selection = Some(Selection {
                            pane: pane_id,
                            anchor: (line, start),
                            head: (line, end),
                            mode: SelectionMode::Linewise,
                        });
                        if let Some(term) = terms.get(&pane_id) {
                            copy_selection = term.selection_text((r, start), (r, end), false);
                        }
                        mouse_captured = true;
                    }
                }
                if resp.dragged() {
                    if let Some(sel) = selection.as_mut() {
                        if sel.pane == pane_id {
                            // AUTOSCROLL: dragging past the top/bottom edge scrolls
                            // this pane's view AND keeps extending the selection over
                            // the lines that scroll into view — without it a selection
                            // could never exceed one screenful (the pointer simply left
                            // the grid, `cell_at_pos` returned `None` above the top, and
                            // the head froze). Both edges, both directions, and the rate
                            // scales with how far past the edge the pointer is;
                            // `scroll_view` clamps at the scrollback ends so neither
                            // direction can run past the history.
                            let mut extended = false;
                            if let (Some(p), Some((cols, prows))) = (pos, pane_size) {
                                let rows = prows as usize;
                                let grid_bottom = origin.y + rows as f32 * ch;
                                let lines = autoscroll_lines(p.y, origin.y, grid_bottom, ch);
                                if lines != 0 {
                                    if let Some(term) = terms.get_mut(&pane_id) {
                                        term.scroll_view(lines);
                                    }
                                    // The pointer can sit STILL outside the grid while the
                                    // view keeps scrolling, and a held-still pointer emits
                                    // no input event — so ask for the next frame explicitly
                                    // or the autoscroll would stall after one step.
                                    ui.ctx().request_repaint();
                                }
                                // Re-read the window top AFTER the scroll (`scroll_view`
                                // clamps, so the move may be shorter than asked) and map
                                // the pointer — clamped to the grid's edges — into an
                                // ABSOLUTE line. Clamping is what lets the head follow a
                                // pointer that is outside the pane instead of freezing,
                                // and it keeps an off-grid pointer from naming a row that
                                // does not exist.
                                let ws = terms
                                    .get(&pane_id)
                                    .and_then(PaneTerm::window_start)
                                    .unwrap_or(window_start);
                                if let Some((r, c)) =
                                    clamp_pos_to_grid_cell(p, origin, cw, ch, cols as usize, rows)
                                {
                                    sel.head = (ws + r, c);
                                    extended = true;
                                }
                            }
                            // Degenerate pane (no size / no pointer): fall back to the
                            // plain unclamped hit test, so a pane whose size is not yet
                            // known still drags exactly as it did before.
                            if !extended {
                                if let Some((line, c)) = cell0 {
                                    sel.head = (line, c);
                                }
                            }
                            mouse_captured = true;
                        }
                    }
                }
                if !multi_click
                    && ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary))
                {
                    if let Some(sel) = *selection {
                        if sel.pane == pane_id {
                            if sel.anchor == sel.head {
                                // A plain click (no drag) clears any selection.
                                *selection = None;
                            } else if let Some(term) = terms.get(&pane_id) {
                                // Map the absolute selection to current display
                                // rows (it may have scrolled since press); copy
                                // the visible portion.
                                let rows = pane_size.map(|(_, r)| r as usize).unwrap_or(0);
                                if let Some((a, b)) =
                                    selection_visible_rows(sel.anchor, sel.head, window_start, rows)
                                {
                                    copy_selection =
                                        term.selection_text(a, b, sel.mode == SelectionMode::Block);
                                }
                            }
                        }
                    }
                }
                // Local scrollback: wheel up (positive) goes BACK into history.
                // A Ctrl/Cmd-held wheel is reserved for font zoom (frame_tick):
                // egui reroutes it into `zoom_delta` and zeroes `smooth_scroll_delta`
                // (so `scroll` is already zero here during a zoom), and this
                // `command` guard is a belt-and-suspenders skip regardless.
                //
                // SHIFT is handled here too, and it MUST be: egui moves a
                // Shift-held notch onto the x-axis and zeroes y, so the old
                // `scroll_y`-only read made Shift+wheel a complete no-op — which
                // silently broke the documented "hold Shift to force LOCAL
                // scrolling" escape (the ONLY route into the scrollback while
                // vim/tmux/htop has grabbed the mouse). The magnitude comes from
                // the OS wheel setting rather than a font-size-derived constant.
                if scroll != egui::Vec2::ZERO && resp.hovered() && !m.command {
                    let rows = terms
                        .get(&pane_id)
                        .map(|t| t.size().1 as usize)
                        .unwrap_or(0);
                    let lines = wheel_scroll_lines(
                        scroll,
                        m.shift,
                        points_per_notch,
                        os_wheel_scroll_lines(),
                        rows,
                    );
                    if lines != 0 {
                        if let Some(term) = terms.get_mut(&pane_id) {
                            term.scroll_view(lines);
                        }
                    }
                }
            }
        }

        // --- inline images (Sixel / Kitty graphics), paint AFTER the grid text
        // so the image covers the placeholder cells. Each visible image is drawn
        // at native pixel size (ppp-corrected to physical pixels), anchored at
        // its grid cell; the GPU texture is cached + pruned per frame. Core
        // decodes + exposes these via Terminal::images(); the egui shell
        // previously dropped them silently (the legacy winit shell rendered
        // them). Clipped to the pane by `painter` (a painter_at(rect)).
        {
            let metas = terms
                .get(&pane_id)
                .map(PaneTerm::visible_image_metas)
                .unwrap_or_default();
            if !metas.is_empty() {
                let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
                let origin = grid_text_origin(rect, pad);
                let ppp = ui.ctx().pixels_per_point().max(0.01);
                for m in metas {
                    // `display_row` may be NEGATIVE when a tall image's top has
                    // scrolled above the window top; the image's visible remainder
                    // must still draw (the painter clips the off-top portion).
                    let min = origin + egui::vec2(m.col as f32 * cw, m.display_row as f32 * ch);
                    // Native pixel size in points (physical px / ppp).
                    let size = egui::vec2(m.width as f32 / ppp, m.height as f32 / ppp);
                    // Skip an image whose BOTTOM edge is at/above the grid top —
                    // it's fully scrolled off, so don't even upload its texture
                    // (a partial top is kept and clipped by `painter_at(rect)`).
                    if min.y + size.y <= origin.y {
                        continue;
                    }
                    let key: ImageKey = (pane_id, m.line, m.col, m.width, m.height);
                    // Pixels are fetched+cloned ONLY on a cache miss (the closure
                    // runs only then); an already-uploaded texture just returns
                    // its id.
                    let tex_id = image_textures.get_or_upload(ui.ctx(), key, || {
                        terms
                            .get(&pane_id)
                            .and_then(|t| t.image_rgba(m.line, m.col))
                    });
                    if let Some(tex_id) = tex_id {
                        painter.image(
                            tex_id,
                            egui::Rect::from_min_size(min, size),
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                }
            }
        }

        // --- selection wash (paint AFTER the grid so the translucent highlight
        // sits ON TOP of the text, the standard selection look) ---
        if let Some(sel) = *selection {
            if sel.pane == pane_id && sel.anchor != sel.head {
                let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
                let origin = grid_text_origin(rect, pad);
                let (cols, rows) = terms
                    .get(&pane_id)
                    .map(|t| {
                        let (c, r) = t.size();
                        (c as usize, r as usize)
                    })
                    .unwrap_or((0, 0));
                // Map the absolute selection to display rows for THIS frame's view
                // — the wash tracks the selected content as the view scrolls.
                let ws = terms
                    .get(&pane_id)
                    .and_then(PaneTerm::window_start)
                    .unwrap_or(0);
                if let Some((start, end)) = selection_visible_rows(sel.anchor, sel.head, ws, rows) {
                    // The selection wash comes from the ACTIVE THEME
                    // (`selection_background`), not a hard-coded steel blue: a
                    // fixed `#6080c0` ignored every theme the user picked and
                    // clashed with any palette that was not blue-ish. The theme's
                    // own colour is opaque, so it is applied at the wash alpha
                    // that keeps the glyphs beneath legible; the builtin fallback
                    // preserves the previous look for a theme with no selection
                    // colour set.
                    let sel_bg = c0pl4nd_core::theme::parse_hex(&theme.selection_background)
                        .unwrap_or((0x60, 0x80, 0xc0));
                    let wash = egui::Color32::from_rgba_unmultiplied(
                        sel_bg.0,
                        sel_bg.1,
                        sel_bg.2,
                        SELECTION_WASH_ALPHA,
                    );
                    let block = sel.mode == SelectionMode::Block;
                    // Block mode: every row shares the same column range; the wash
                    // must paint the SAME rectangle each row so it matches the
                    // block-mode copy (anchored to the endpoint columns).
                    let (block_lo, block_hi) = (
                        sel.anchor.1.min(sel.head.1),
                        sel.anchor.1.max(sel.head.1).min(cols.saturating_sub(1)),
                    );
                    for r in start.0..=end.0 {
                        let lo = if block {
                            block_lo
                        } else if r == start.0 {
                            start.1
                        } else {
                            0
                        };
                        // `end.1` may be usize::MAX (selection end scrolled below
                        // the bottom → to line end); clamp to the last column.
                        let hi_raw = if block {
                            block_hi
                        } else if r == end.0 {
                            end.1
                        } else {
                            cols.saturating_sub(1)
                        };
                        let hi = hi_raw.min(cols.saturating_sub(1));
                        if cols == 0 || hi < lo {
                            continue;
                        }
                        let x0 = origin.x + lo as f32 * cw;
                        let x1 = origin.x + (hi as f32 + 1.0) * cw;
                        let y0 = origin.y + r as f32 * ch;
                        let sel_rect =
                            egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y0 + ch));
                        painter.rect_filled(sel_rect, 0.0, wash);
                        // Honour `selection_foreground` too: re-draw the selected
                        // glyphs in the theme's selection text colour ON TOP of
                        // the wash. Without this the wash alone tints whatever
                        // colour the text already had, so a dark-on-dark or
                        // low-contrast pairing stayed unreadable while selected —
                        // the theme declares a selection foreground precisely to
                        // guarantee contrast, and it was being ignored.
                        // `parse_hex` returns Result, not Option — a malformed
                        // selection_foreground simply leaves the wash to tint the
                        // existing glyphs rather than failing the frame.
                        if let Ok(sel_fg) =
                            c0pl4nd_core::theme::parse_hex(&theme.selection_foreground)
                        {
                            let fg32 = egui::Color32::from_rgb(sel_fg.0, sel_fg.1, sel_fg.2);
                            let font = egui::FontId::monospace(font_size);
                            if let Some(rows) = terms.get(&pane_id).and_then(PaneTerm::grid_rows) {
                                if let Some(runs) = rows.get(r) {
                                    for (c, _, col_cells) in pane_term::row_glyph_cells(runs) {
                                        if col_cells < lo || col_cells > hi {
                                            continue;
                                        }
                                        painter.text(
                                            egui::pos2(
                                                origin.x + col_cells as f32 * cw,
                                                origin.y + r as f32 * ch,
                                            ),
                                            egui::Align2::LEFT_TOP,
                                            c,
                                            font.clone(),
                                            fg32,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // --- accessibility (F2-1): expose the grid text to screen readers ---
        // The terminal grid is custom-painted, so without an explicit AccessKit
        // node a screen reader perceives only an empty interactive region — the
        // terminal's actual content is invisible to assistive tech. Attach the
        // visible grid text as the pane's accessible value, marking the focused
        // pane active. egui invokes this closure LAZILY and ONLY while building an
        // AccessKit tree (i.e. when a screen reader / `egui_kittest` is attached),
        // so the full-grid `grid_text()` snapshot costs nothing in the common
        // no-assistive-tech case.
        resp.widget_info(|| {
            let text = terms
                .get(&pane_id)
                .and_then(PaneTerm::grid_text)
                .unwrap_or_default();
            egui::WidgetInfo::labeled(egui::WidgetType::Label, focused, text)
        });

        // --- IME composition (F3-1): cursor rect + pre-edit display ---
        // Compute the focused pane's terminal-cursor cell rect in screen space
        // using the SAME geometry the glyph painter, cursor, and link hit-test
        // share (`origin + (col*cw, row*ch)`). The caller hands this rect to
        // `ctx.output_mut(|o| o.ime = Some(IMEOutput {..}))` so winit's
        // `set_ime_cursor_area` places the OS candidate window AT the caret.
        // Only the focused pane reports a rect (the OS tracks a single caret).
        let mut ime_cursor_rect = None;
        if focused {
            if let Some((row, col)) = terms.get(&pane_id).and_then(PaneTerm::cursor_cell) {
                let (cw, ch) = monospace_cell_points(&painter, font_size, line_height_px);
                let origin = grid_text_origin(rect, pad);
                let cell_min = origin + egui::vec2(col as f32 * cw, row as f32 * ch);
                ime_cursor_rect = Some(egui::Rect::from_min_size(cell_min, egui::vec2(cw, ch)));

                // Paint the in-progress pre-edit string at the cursor, underlined
                // and in the theme fg, so the user sees what they are composing
                // before commit. The candidate window (positioned via the rect
                // above) shows the IME's own suggestion list; this is the inline
                // composition echo at the caret. The pre-edit is DISPLAY-ONLY —
                // it is never forwarded to the PTY (only `ImeEvent::Commit` is).
                if let Some(pre) = ime_preedit.filter(|s| !s.is_empty()) {
                    let fg = theme::ChromeColors::from_theme(theme).fg;
                    let font = egui::FontId::monospace(font_size);
                    let galley = painter.layout_no_wrap(pre.to_string(), font, fg);
                    let text_pos = origin + egui::vec2(col as f32 * cw, row as f32 * ch);
                    let galley_w = galley.size().x;
                    painter.galley(text_pos, galley, fg);
                    // Underline the composition span (the conventional pre-edit
                    // affordance), one device-px line at the cell baseline.
                    let underline = egui::Rect::from_min_size(
                        text_pos + egui::vec2(0.0, ch - 1.0),
                        egui::vec2(galley_w, 1.0),
                    );
                    painter.rect_filled(underline, 0.0, fg);
                }
            }
        }

        // --- right-side scrollbar (overlay; auto-hides when everything fits) ---
        // The terminal grid is custom-painted (no `egui::ScrollArea`), so the bar
        // owns its rect + hit-testing. It reflects the scrollback position and
        // viewport size, is draggable to scrub, click-in-trough pages, and marks
        // the focused pane's search hits. Painted LAST so it sits over the grid.
        let mut scrollbar_grabbed = false;
        {
            let (scrollback_len, view_offset, rows) = terms
                .get(&pane_id)
                .map(|t| (t.scrollback_len(), t.view_offset(), t.size().1 as usize))
                .unwrap_or((0, 0, 0));
            let metrics = scrollbar::ScrollMetrics {
                scrollback_len,
                view_offset,
                rows,
            };
            if metrics.scrollable() {
                let track = scrollbar::track_rect(rect);
                let sb_resp = ui.interact(
                    track,
                    egui::Id::new(("c0pl4nd_scrollbar", pane_id.raw())),
                    egui::Sense::click_and_drag(),
                );
                // Grabbing the bar must NOT also start an egui_tiles pane-rearrange.
                scrollbar_grabbed = sb_resp.dragged() || sb_resp.drag_started();
                let thumb = scrollbar::thumb_rect(&metrics, track);
                // A drag scrubs (thumb centres on the pointer); a trough click
                // above/below the thumb pages by a viewport.
                let mut target: Option<usize> = None;
                if sb_resp.dragged() {
                    if let Some(p) = sb_resp.interact_pointer_pos() {
                        target = Some(scrollbar::view_offset_for_pointer_y(&metrics, track, p.y));
                    }
                } else if sb_resp.clicked() {
                    if let Some(p) = sb_resp.interact_pointer_pos() {
                        if p.y < thumb.top() {
                            target = Some((view_offset + rows).min(scrollback_len));
                        } else if p.y > thumb.bottom() {
                            target = Some(view_offset.saturating_sub(rows));
                        }
                    }
                }
                if let Some(off) = target {
                    if let Some(t) = terms.get_mut(&pane_id) {
                        // `scroll_view(+n)` goes BACK into history (more offset).
                        let delta = off as i32 - view_offset as i32;
                        if delta != 0 {
                            t.scroll_view(delta);
                        }
                    }
                    // The mutated view repaints next frame — request it so a click
                    // (which does not hold the pointer) still redraws immediately.
                    ui.ctx().request_repaint();
                }
                // Marks: three SEMANTIC kinds, each already tracked by core and
                // each drawn in its own colour + its own slice of the track (see
                // `scrollbar::mark_rect`) so they stay distinguishable:
                //
                // - PROMPTS — the OSC 133 `;A`/`;B` marks core already captures and
                //   the Ctrl+Shift+Up/Down jump-to-prompt chord already walks. They
                //   turn the bar into a map of "where did each command start",
                //   which is the whole point of shell prompt-integration.
                // - FAILURES — the OSC 133 `;D` command-end marks whose reported
                //   exit code was non-zero (the same marks the status bar's
                //   exit-code indicator reads), so a failure deep in history is
                //   findable without scrolling for it.
                // - SEARCH HITS — as before: the focused pane's find matches,
                //   mapped from their visible display row to an absolute content
                //   line via `window_start`.
                //
                // Prompt/failure marks are ALREADY absolute content lines (core
                // anchors them to `history.len() + row`, the same space
                // `window_start` lives in), so they need no display-row mapping.
                let mut marks: Vec<scrollbar::ScrollMark> = Vec::new();
                let last = metrics.total().saturating_sub(1);
                if let Some(t) = terms.get(&pane_id) {
                    // Newest-first + capped: a hostile program may hold thousands of
                    // marks (core caps prompts at 4096, commands at 8192) and painting
                    // them all would be both slow and visual mush on a ~700pt track.
                    // The most RECENT marks are the ones a user is looking for.
                    let mut push_capped = |lines: Vec<usize>, kind: scrollbar::ScrollMarkKind| {
                        for abs in lines.into_iter().rev().take(MAX_SEMANTIC_SCROLL_MARKS) {
                            marks.push(scrollbar::ScrollMark {
                                abs_line: abs.min(last),
                                kind,
                                selected: false,
                            });
                        }
                    };
                    push_capped(t.prompt_mark_lines(), scrollbar::ScrollMarkKind::Prompt);
                    push_capped(t.failed_command_lines(), scrollbar::ScrollMarkKind::Error);
                }
                if let Some(hl) = search {
                    let ws = metrics.window_start();
                    for (i, span) in hl.spans.iter().enumerate() {
                        marks.push(scrollbar::ScrollMark {
                            abs_line: (ws + span.line).min(last),
                            kind: scrollbar::ScrollMarkKind::SearchHit,
                            selected: i == hl.selected,
                        });
                    }
                }
                let theme_color = |hex: &str, fallback: egui::Color32| {
                    c0pl4nd_core::theme::parse_hex(hex)
                        .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
                        .unwrap_or(fallback)
                };
                let mark_colors = scrollbar::MarkColors {
                    search: pane_colors.fg,
                    // The selected hit takes the cursor colour — hue-distinct from
                    // the accent thumb, as before.
                    selected: theme_color(&theme.cursor, pane_colors.accent),
                    // Prompts take the theme's bright blue and failures its bright
                    // red: hue-distinct from each other, from the fg search ticks,
                    // and from the accent thumb. (Brand Akira-red `#ff0040` stays
                    // reserved for alarms — a non-zero exit is routine.)
                    prompt: theme_color(&theme.bright.blue, pane_colors.muted),
                    error: theme_color(&theme.bright.red, pane_colors.fg),
                };
                let active = sb_resp.hovered() || sb_resp.dragged();
                scrollbar::paint(
                    &painter,
                    track,
                    &metrics,
                    &pane_colors,
                    &mark_colors,
                    active,
                    &marks,
                );
            }
        }

        PaneBodyOutcome {
            // A body-drag normally tells egui_tiles to REARRANGE the pane. When a
            // program grabbed the mouse and we reported the drag to its PTY, the
            // gesture belongs to the program — never rearrange panes underneath it.
            // A scrollbar grab is likewise NOT a pane-rearrange.
            drag_started: resp.drag_started() && !mouse_captured && !scrollbar_grabbed,
            clicked: resp.clicked(),
            size: rect.size(),
            opened_url,
            ime_cursor_rect,
            copy_selection,
            context_menu_action,
            body_rect: rect,
        }
    }
}
