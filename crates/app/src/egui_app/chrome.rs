//! The C0PL4ND chrome: a frameless titlebar (two-tone wordmark + tab strip +
//! caption buttons), a settings gear, and a bottom status bar. Window controls
//! use `egui::ViewportCommand` (no Win32) per recon dossier §3.2.
//!
//! The titlebar mirrors the sibling SCR1B3 editor's frameless titlebar for a
//! same-product-family read: a `horizontal_centered` row with the left content
//! (wordmark + tabs + split buttons) first, then the caption cluster
//! (settings/minimize/maximize/close) pinned to the window's right edge. The
//! cluster is placed at absolute rects anchored to `ctx().screen_rect()` — the
//! only reliable bounded right edge in this non-justified row (the ui's own
//! `clip_rect()`/`max_rect()` right edge is unbounded in the render frame). The
//! buttons use Phosphor glyphs via `ui.button` so they self-size and never fall
//! back to tofu.

use std::collections::HashSet;

use c0pl4nd_core::term::MouseMode;
use egui::{Color32, RichText, Sense};
use egui_phosphor::thin as icon;

use super::grid::PaneId;
use super::theme::{brand, ChromeColors, CLOSE_RED, CLOSE_RED_PRESSED};
use super::C0pl4ndApp;

// ---------------------------------------------------------------------------
// Flat chrome glyph buttons — ONE shared hover/press/focus treatment
// ---------------------------------------------------------------------------
//
// Every glyph button in the chrome (tab pin, tab ×, the ‹ › tab step-arrows and
// the caption cluster ⚙ — ▢ ✕) goes through [`glyph_button`]. Before this there
// were TWO hand-rolled copies of the pattern (the caption cluster and
// `step_arrow`) and the tab pin/× had NONE — they were `Button::…frame(false)`,
// which egui paints with no fill and no stroke in EVERY widget state
// (`egui-0.34.3/src/widgets/button.rs:353-357`), so the `flatten_chrome_buttons`
// hover veil was discarded and the glyph colour was baked at construction. The
// result was a button with zero hover feedback (the reported bug). Funnelling all
// four sites through one helper is what stops a future change fixing one and
// regressing another.

/// Interaction state of a flat chrome glyph button. Each maps to a VISUALLY
/// DISTINCT paint (see [`chrome_glyph_paint`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GlyphState {
    /// Not interactive (e.g. a step-arrow with no previous/next tab).
    Disabled,
    /// Idle — no pointer, not held.
    Rest,
    /// Pointer over the button.
    Hover,
    /// Primary button held down over the button.
    Press,
}

/// The resolved paint for one flat chrome glyph button: the glyph colour and the
/// optional backplate fill. Pure data so the state→colour mapping is unit-testable
/// without an egui `Ui`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GlyphPaint {
    /// Colour the glyph is drawn in.
    pub glyph: Color32,
    /// Backplate fill, or `None` for a fully frameless (idle) button.
    pub fill: Option<Color32>,
}

/// Duration (seconds) of the hover fade. Short enough to feel immediate, long
/// enough to read as a fade rather than a snap.
pub(super) const HOVER_FADE_SECONDS: f32 = 0.10;

/// The hover-fade duration to pass to `Context::animate_bool_with_time`: **zero**
/// under the reduced-motion preference (WCAG 2.3.3 — the state still changes, it
/// just changes instantly), else [`HOVER_FADE_SECONDS`]. Pure so the
/// reduced-motion branch is unit-testable without an OS query.
pub(super) fn hover_fade_time(reduced_motion: bool) -> f32 {
    if reduced_motion {
        0.0
    } else {
        HOVER_FADE_SECONDS
    }
}

/// The subtle hover/press veil for a chrome button, matching the polarity
/// `flatten_chrome_buttons` uses (white on dark themes, black on light) and the
/// same alphas (20 hover / 32 press) so the animated fill lands on exactly the
/// treatment the rest of the chrome already speaks.
fn veil(colors: ChromeColors, alpha: u8) -> Color32 {
    if super::theme::is_light(colors.bg) {
        Color32::from_black_alpha(alpha)
    } else {
        Color32::from_white_alpha(alpha)
    }
}

/// Resolve the paint for a flat chrome glyph button.
///
/// * `rest` — the idle glyph colour (the muted tone for the pin/×/caption, a
///   brighter mid-tone for the step-arrows, brand violet for a PINNED pin).
/// * `is_close` — the ✕ / × takes the conventional destructive-red treatment.
/// * `t` — the animated hover amount, `0.0` (rest) .. `1.0` (fully hovered); it
///   fades the glyph AND the fill so the transition is continuous. Under reduced
///   motion the caller passes a zero-duration animation, so `t` is only ever
///   exactly `0.0` or `1.0`.
///
/// The four states are guaranteed distinct: disabled is dimmer than rest, hover
/// brightens the glyph over a veil (close-red for the ✕), and press deepens both.
pub(super) fn chrome_glyph_paint(
    state: GlyphState,
    colors: ChromeColors,
    rest: Color32,
    is_close: bool,
    t: f32,
) -> GlyphPaint {
    let t = t.clamp(0.0, 1.0);
    match state {
        // Dimmer than rest, and never a backplate — an inert chevron.
        GlyphState::Disabled => GlyphPaint {
            glyph: rest.lerp_to_gamma(colors.bg, 0.55),
            fill: None,
        },
        // Held: the deepest treatment of the four.
        GlyphState::Press => GlyphPaint {
            glyph: if is_close { Color32::WHITE } else { colors.fg },
            fill: Some(if is_close {
                CLOSE_RED_PRESSED
            } else {
                veil(colors, 32)
            }),
        },
        // Rest and Hover share one continuous ramp parameterised by `t`, so the
        // fade-in and the fade-out are the same code path.
        GlyphState::Rest | GlyphState::Hover => {
            let hot = if is_close { Color32::WHITE } else { colors.fg };
            let glyph = rest.lerp_to_gamma(hot, t);
            let fill = if t <= 0.0 {
                None
            } else if is_close {
                Some(CLOSE_RED.gamma_multiply(t))
            } else {
                Some(veil(colors, (t * 20.0).round() as u8))
            };
            GlyphPaint { glyph, fill }
        }
    }
}

/// Paint one flat chrome glyph button into `rect` and return its `Response`.
///
/// The hover state is pre-checked with `rect_contains_pointer` so the glyph
/// colour and fill are chosen BEFORE the `Button` paints — a plain flow `Button`
/// cannot do this (its text colour is fixed at construction), which is precisely
/// why the tab pin/× had no hover feedback.
///
/// `anim_id` MUST be derived from the button's stable identity (pane id + role),
/// never `ui.next_auto_id()`: auto ids shift when a tab is inserted or removed,
/// which would restart every in-flight animation on the strip.
///
/// A keyboard-focused button gets an explicit focus RING (egui 0.34 has no
/// focus-ring style — see [`super::theme::focus_ring_color`]), drawn outside the
/// rect so it is distinct from both the hover veil and the press fill.
#[allow(clippy::too_many_arguments)]
fn glyph_button(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    text: impl FnOnce(Color32) -> RichText,
    colors: ChromeColors,
    rest: Color32,
    is_close: bool,
    enabled: bool,
    anim_id: egui::Id,
    hover_text: &str,
) -> egui::Response {
    let hovered = enabled && ui.rect_contains_pointer(rect);
    let held = hovered && ui.input(|i| i.pointer.primary_down());
    let t = ui.ctx().animate_bool_with_time(
        anim_id,
        hovered,
        hover_fade_time(c0pl4nd_core::reduced_motion::reduced_motion()),
    );
    let state = if !enabled {
        GlyphState::Disabled
    } else if held {
        GlyphState::Press
    } else if hovered {
        GlyphState::Hover
    } else {
        GlyphState::Rest
    };
    let paint = chrome_glyph_paint(state, colors, rest, is_close, t);
    // `min_size(rect.size())` keeps the HIT TARGET at the full rect (>= 24x24 for
    // the tab controls — Fitts / WCAG 2.5.8) while the glyph itself stays small.
    let mut button = egui::Button::new(text(paint.glyph)).min_size(rect.size());
    match paint.fill {
        Some(fill) => button = button.fill(fill),
        // Nothing to paint: stay frameless so an idle button never floats as an
        // opaque chip over the translucent bar.
        None => button = button.frame(false),
    }
    let resp = ui.put(rect, button).on_hover_text(hover_text);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            egui::CornerRadius::same(4),
            egui::Stroke::new(2.0f32, super::theme::focus_ring_color(colors)),
            egui::StrokeKind::Outside,
        );
    }
    resp
}

/// Minimum hit-target edge (logical px) for the in-tab pin/× controls. The glyph
/// is 13px, but a 13px target is a Fitts-law miss magnet and below the WCAG 2.5.8
/// target-size floor — so the BUTTON is allocated at this size while the glyph is
/// unchanged.
const TAB_CONTROL_HIT: f32 = 24.0;

/// The next in-tab control rect at the flow cursor, vertically centred on the tab
/// row and sized to [`TAB_CONTROL_HIT`].
///
/// Pre-computing the rect is what makes hover feedback POSSIBLE: [`glyph_button`]
/// must know whether the pointer is inside BEFORE it constructs the button, because
/// an `egui::Button`'s text colour is fixed at construction and can never respond to
/// hover afterwards. That is precisely why the pin/× read as dead controls before.
fn tab_control_rect(ui: &egui::Ui) -> egui::Rect {
    let cy = ui.max_rect().center().y;
    let x0 = ui.cursor().min.x;
    egui::Rect::from_min_max(
        egui::pos2(x0, cy - TAB_CONTROL_HIT / 2.0),
        egui::pos2(x0 + TAB_CONTROL_HIT, cy + TAB_CONTROL_HIT / 2.0),
    )
}

/// The egui-memory key the in-flight tab drag is stored under.
fn tab_drag_id() -> egui::Id {
    egui::Id::new("tab_strip_drag")
}

// ---------------------------------------------------------------------------
// Tab middle-click + drag-to-reorder
// ---------------------------------------------------------------------------

/// The drag payload for a tab drag-to-reorder gesture: the pane being moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabDrag(PaneId);

/// The egui-memory key the user's explicit tab order is stored under.
fn tab_order_id() -> egui::Id {
    egui::Id::new("tab_strip_user_order")
}

/// Whether a middle-click on a tab should close it.
///
/// A PINNED tab's × is deliberately hidden (unpin to close, so a pinned pane
/// cannot be shut by accident) — middle-click must not become a back door around
/// that, so it is inert on a pinned tab. Pure → unit-testable.
pub(super) fn middle_click_closes(is_pinned: bool) -> bool {
    !is_pinned
}

/// Re-order `tabs` (already in the derived pinned-first order) by the user's
/// explicit `saved` order.
///
/// Panes present in `saved` sort by their saved rank; panes the user has never
/// moved (a freshly-opened tab) keep their derived position AFTER the ordered
/// ones within their group. The pinned/unpinned GROUPING is re-asserted as the
/// primary key, so a saved order can never interleave a pinned tab with an
/// unpinned one. Pure → unit-testable.
pub(super) fn apply_saved_order<T>(
    mut tabs: Vec<(PaneId, T)>,
    saved: &[PaneId],
    pinned: &HashSet<PaneId>,
) -> Vec<(PaneId, T)> {
    let rank = |p: &PaneId| saved.iter().position(|s| s == p).unwrap_or(usize::MAX);
    // Stable sort: equal keys (e.g. two never-moved tabs) keep derived order.
    tabs.sort_by_key(|(pid, _)| (!pinned.contains(pid), rank(pid)));
    tabs
}

/// Compute the new tab order after dropping `from` onto `to`.
///
/// `before` is true when the pointer was on the LEFT half of the target tab (drop
/// before it), false for the right half (drop after it). Returns `None` — meaning
/// "no change" — for a no-op drop (onto itself), an unknown pane, or a
/// CROSS-GROUP drop (pinned onto unpinned or vice versa), because pinned tabs are
/// a distinct leading group and a drag must never smuggle a tab across that
/// boundary. Pure → unit-testable.
pub(super) fn reorder_tabs(
    order: &[PaneId],
    pinned: &HashSet<PaneId>,
    from: PaneId,
    to: PaneId,
    before: bool,
) -> Option<Vec<PaneId>> {
    if from == to {
        return None;
    }
    if pinned.contains(&from) != pinned.contains(&to) {
        return None;
    }
    if !order.contains(&from) || !order.contains(&to) {
        return None;
    }
    let mut out: Vec<PaneId> = order.iter().copied().filter(|p| *p != from).collect();
    let at = out.iter().position(|p| *p == to)?;
    out.insert(if before { at } else { at + 1 }, from);
    (out != order).then_some(out)
}

/// Outcome of one chrome frame — the actions the user requested via the chrome
/// widgets. The host applies them after the panel closure returns so that the
/// grid/tree mutation does not happen mid-borrow.
#[derive(Debug, Default, Clone)]
pub struct ChromeActions {
    /// User clicked a tab; switch focus to this pane.
    pub focus_tab: Option<super::grid::PaneId>,
    /// User clicked a tab's close (×); close this pane.
    pub close_tab: Option<super::grid::PaneId>,
    /// User clicked a tab's pin; toggle this pane's pinned state.
    pub pin_tab: Option<super::grid::PaneId>,
    /// User clicked the "+" new-terminal button; open a new pane (the host picks
    /// the split direction to keep the grid balanced).
    pub new_terminal: bool,
    /// User picked a shell from the top-bar ▾ switcher; index into
    /// [`super::C0pl4ndApp::shell_profiles`]. The host opens a new terminal with
    /// that shell and makes it the active profile for the plain "+" button.
    pub open_shell: Option<usize>,
    /// User toggled the settings window.
    pub toggle_settings: bool,
    /// User clicked the view-mode button; flip the pane shell layout between the
    /// `egui_tiles` grid and the single-pane tabs view (`#30`). Routed through the
    /// action struct (like [`new_terminal`](Self::new_terminal)) so the host
    /// applies the `config.view_mode` flip after the panel closure returns.
    pub toggle_view_mode: bool,
    /// User clicked the "make panes symmetrical" button; equalise every split so
    /// all panes are the same size (a one-shot, independent of the
    /// `link_pane_dividers` setting). Only offered in grid view with 2+ panes.
    pub equalize_panes: bool,
    /// User clicked a caption button (minimize / maximize / close). Routed
    /// through the action struct (instead of sending the `ViewportCommand`
    /// inline) so `frame_tick` is the single place that issues the real OS
    /// command AND records it for the interaction tests to observe — a click on
    /// the real button thus has an assertable outcome without a window.
    pub window_cmd: Option<super::WindowCmd>,
    /// User clicked the script-menu "Open…" item (#35). The host runs the native
    /// `rfd` file picker AFTER the panel closure returns (the blocking modal
    /// dialog must not fire mid-panel-borrow), then feeds the picked path to the
    /// focused PTY as a command via the existing run path.
    pub open_script_file: bool,
    /// User clicked a history row in the script menu (#35) — re-run this command
    /// in the focused pane via the SAME [`run_command_in_focused`] path the
    /// command palette + history sidebar use. Routed through the action struct
    /// (like [`open_shell`](Self::open_shell)) so the host applies it after the
    /// panel closes.
    pub rerun_command: Option<String>,
    /// User clicked the script-menu "Report an issue…" item (W1TN3SS). The host
    /// opens the manual-issue dialog ([`crate::issue_intake::IssueIntakeState::open_fresh`])
    /// after the panel closure returns. Routed through the action struct so the
    /// `&mut self` dialog state is touched outside the panel borrow.
    pub report_issue: bool,
}

/// Extract the script-file path from a history entry that is a SINGLE quoted
/// path invocation — the exact shapes [`super::quote_path_for_shell`] emits for a
/// file picked via the script-menu "Open…" item:
///   * PowerShell — `& "PATH"` (`"` un-escaped from `` `" ``)
///   * cmd / Windows default — `"PATH"`
///   * POSIX — `'PATH'` (`'` un-escaped from `'\''`)
///
/// Returns `None` for anything else (a plain typed command), so ordinary history
/// rows are never mis-parsed.
fn script_path_of(cmd: &str) -> Option<String> {
    let s = cmd.trim();
    if let Some(rest) = s.strip_prefix("& \"") {
        return rest
            .strip_suffix('"')
            .map(|inner| inner.replace("`\"", "\""));
    }
    if let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        // cmd paths cannot contain `"`, so a lone wrapped token is the whole path.
        if !inner.contains('"') {
            return Some(inner.to_string());
        }
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        return Some(inner.replace("'\\''", "'"));
    }
    None
}

/// The label to show for a script-menu history row: the FILE NAME when the entry
/// is a quoted script-file invocation with a directory component (so the menu is
/// not dominated by long absolute paths — the full command stays in the hover
/// tooltip), else the command verbatim. Only shortens when there is a real parent
/// directory to hide, so a quoted plain-string arg is left untouched.
fn script_menu_label(cmd: &str) -> String {
    if let Some(path) = script_path_of(cmd) {
        // Split on BOTH separators so a Windows-style path (`\`) is shortened even
        // on a POSIX host and a POSIX path (`/`) on Windows — `std::path::Path` is
        // host-specific (it does not treat `\` as a separator on Linux/macOS), and
        // C0PL4ND ships on all three OSes.
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path.as_str());
        // Only shorten when there is a real parent directory to hide (a separator
        // was present, so the basename differs from the whole path); a bare token
        // is left untouched.
        if !name.is_empty() && name != path {
            return name.to_string();
        }
    }
    cmd.to_string()
}

/// The last `n` non-blank lines of `text`, joined with '\n' — the tail of a
/// pane's visible terminal grid for the tab hover preview (#15). Trailing blank
/// rows (the unused bottom of the grid) are dropped first so the preview shows
/// real output, not empty space. Pure → unit-testable.
fn last_nonblank_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let start = end.saturating_sub(n);
    lines[start..end].join("\n")
}

impl C0pl4ndApp {
    /// Paint the titlebar (wordmark + tab strip + caption controls). Returns the
    /// actions the host should apply this frame. `colors` carries the
    /// theme-derived chrome palette so the tab text / caption glyphs / accents
    /// follow the active terminal theme. BOTH tones of the two-tone C0PL4ND
    /// wordmark ("C0PL" + "4ND") are theme-derived ([`ChromeColors::logo_a`] /
    /// [`ChromeColors::logo_b`]) — each guaranteed readable + bright against the
    /// titlebar and hue-distant from the other, so the logo tints with the active
    /// palette while always staying a legible, contrasting two-tone signature.
    pub(super) fn titlebar_and_tabs(
        &self,
        ui: &mut egui::Ui,
        colors: ChromeColors,
    ) -> ChromeActions {
        let mut actions = ChromeActions::default();
        // One left→right row: the LEFT content (wordmark + tabs + split buttons)
        // flows normally; the caption cluster is then placed at absolute rects
        // pinned to the window's right edge (see the cluster block below).
        ui.horizontal_centered(|ui| {
            // two-tone C0PL4ND wordmark + drag/double-click caption region.
            let mut job = egui::text::LayoutJob::default();
            let fmt = |color| egui::text::TextFormat {
                color,
                font_id: egui::FontId::proportional(16.0),
                ..Default::default()
            };
            // BOTH tones are theme-derived and readability-guaranteed: "C0PL"
            // from the theme's bright magenta (echoing brand purple), "4ND" from
            // the theme's bright green (echoing brand green). Each is tone-lifted
            // to stay bright + readable against the titlebar and hue-distant from
            // the other, so the wordmark tints with the theme yet never goes dark,
            // muddy, or low-contrast. (See ChromeColors::logo_a / logo_b.)
            job.append("C0PL", 0.0, fmt(colors.logo_a));
            job.append("4ND", 0.0, fmt(colors.logo_b));
            // `.selectable(false)` is load-bearing: egui labels are text-selectable
            // by default (`style.interaction.selectable_labels`), so a drag on the
            // wordmark began a TEXT SELECTION (the reported "it highlights the app
            // name") instead of moving the window — the selection consumed the drag
            // before `StartDrag` could fire. With selection off, the wordmark is a
            // pure caption drag-handle: click-and-drag moves the window, double-click
            // maximizes, and the name still renders in its two-tone brand colors.
            let title_resp = ui.add(
                egui::Label::new(job)
                    .selectable(false)
                    .sense(Sense::click_and_drag()),
            );
            if title_resp.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if title_resp.double_clicked() {
                actions.window_cmd = Some(super::WindowCmd::ToggleMaximize);
            }

            ui.separator();

            // Tab strip: one tab per pane, SCR1B3-style — each tab is
            // [title] [pin] [×]. Pinned tabs sort first and carry a violet pin;
            // their × is hidden (unpin to close) so they can't be shut by
            // accident. Clicking the title focuses the pane; × closes it.
            //
            // The strip is a horizontally-scrollable region whose width RESERVES
            // space for the right-pinned caption cluster (close/max/min/settings)
            // and the trailing "+"/"▾" controls. This stops many wide (OSC-titled)
            // tabs from growing UNDER the caption — which previously occluded the
            // rightmost tab's pin/× (drawn beneath the caption buttons → those
            // controls became unclickable). `auto_shrink([true, false])` keeps the
            // common case pixel-identical: with few tabs the region shrinks to its
            // content and "+" sits immediately after the last tab; only an
            // overflowing strip scrolls instead of bleeding under the caption.
            // ---- overflow arrow-stepping (#14) ----
            // When the tabs overflow the strip's reserved width, flank it with ‹ ›
            // chevrons that STEP the focused tab to the previous/next one
            // (Ctrl+Tab-like) and scroll it into view — clearer than a thin
            // horizontal scrollbar. Overflow is only known AFTER the strip lays
            // out, so the LEFT chevron (rendered first) reads LAST frame's flag
            // from ctx memory; the RIGHT chevron uses this frame's. A one-shot
            // `follow` flag, set on any arrow click and consumed next frame, drives
            // `scroll_to_me` on the newly-focused tab (the host applies the focus
            // AFTER this closure returns, so the scroll lands one frame later).
            let overflow_id = egui::Id::new("tab_strip_overflow");
            let follow_id = egui::Id::new("tab_strip_follow");
            let show_left_arrow = ui
                .ctx()
                .data(|d| d.get_temp::<bool>(overflow_id).unwrap_or(false));
            let follow = ui.ctx().data_mut(|d| {
                let v = d.get_temp::<bool>(follow_id).unwrap_or(false);
                d.remove::<bool>(follow_id);
                v
            });
            // Sorted (pinned-first) order, hoisted OUT of the strip closure so the
            // arrows can compute the prev/next targets before the strip consumes
            // the list.
            let mut tabs = self.pane_titles();
            // Stable sort: pinned first, original visual order preserved within
            // each group (`sort_by_key` is stable).
            tabs.sort_by_key(|(pid, _)| !self.pinned.contains(pid));
            // Then re-assert the user's explicit drag-to-reorder order on top of
            // that grouping. Persisted in egui memory, so it survives tab open/close
            // within the session; panes the user never moved keep their derived slot.
            let saved_order: Vec<PaneId> = ui.ctx().data(|d| {
                d.get_temp::<Vec<PaneId>>(tab_order_id())
                    .unwrap_or_default()
            });
            let tabs = apply_saved_order(tabs, &saved_order, &self.pinned);
            // The pane order actually on screen this frame — the basis a drop
            // computes the new order from.
            let visible_order: Vec<PaneId> = tabs.iter().map(|(p, _)| *p).collect();
            // An in-flight tab drag, and where it would land. Resolved AFTER the
            // strip closure so the reorder never mutates the list mid-iteration.
            let dragging: Option<TabDrag> = ui.ctx().data(|d| d.get_temp::<TabDrag>(tab_drag_id()));
            let mut drop_target: Option<(PaneId, PaneId, bool)> = None;
            let cur = tabs.iter().position(|(pid, _)| *pid == self.focused_pane);
            let prev_target = cur
                .filter(|&i| i > 0)
                .and_then(|i| tabs.get(i - 1))
                .map(|(p, _)| *p);
            let next_target = cur.and_then(|i| tabs.get(i + 1)).map(|(p, _)| *p);
            // Index of the last tab, so the between-tabs hairline divider is drawn
            // AFTER every tab except the last (a boundary line between adjacent
            // tabs, never a trailing rule before the "+").
            let last_tab = tabs.len().saturating_sub(1);
            // A tab step-arrow ‹ / › that speaks the SAME hover language as the
            // caption cluster (the reference treatment below): the chevron rests at
            // a mid-tone — brighter than the old flat `muted`, which read as
            // barely-there — and brightens to `colors.fg` under the pointer, while
            // the flatten_chrome_buttons veil fills the frame on hover (so it is
            // obviously interactive). A DISABLED arrow (no prev/next tab) stays a
            // dim, frameless, inert chevron. The rect is pre-computed from the flow
            // cursor so `rect_contains_pointer` can recolour the glyph BEFORE the
            // `Button` paints — exactly as the caption loop does — which a plain
            // flow `Button` cannot (its text colour is fixed at construction).
            // Returns whether the (enabled) arrow was clicked this frame.
            let step_arrow =
                move |ui: &mut egui::Ui, glyph: &str, enabled: bool, hover: &str| -> bool {
                    let row = ui.max_rect();
                    let (w, h) = (22.0_f32, 26.0_f32);
                    let x0 = ui.cursor().min.x;
                    let cy = row.center().y;
                    let rect = egui::Rect::from_min_max(
                        egui::pos2(x0, cy - h / 2.0),
                        egui::pos2(x0 + w, cy + h / 2.0),
                    );
                    if !enabled {
                        ui.put(
                            rect,
                            egui::Button::new(RichText::new(glyph).size(15.0).color(colors.muted))
                                .frame(false),
                        );
                        return false;
                    }
                    let hovered = ui.rect_contains_pointer(rect);
                    let glyph_col = if hovered {
                        colors.fg
                    } else {
                        // Resting tone: `muted` is 0.55 toward bg; 0.35 keeps the arrow
                        // clearly present without competing with the focused tab.
                        colors.fg.lerp_to_gamma(colors.bg, 0.35)
                    };
                    ui.put(
                        rect,
                        egui::Button::new(RichText::new(glyph).size(15.0).color(glyph_col)),
                    )
                    .on_hover_text(hover)
                    .clicked()
                };
            // LEFT chevron — only when the strip overflowed last frame; disabled at
            // the first tab.
            if show_left_arrow
                && step_arrow(ui, icon::CARET_LEFT, prev_target.is_some(), "previous tab")
            {
                actions.focus_tab = prev_target;
                ui.ctx().data_mut(|d| d.insert_temp(follow_id, true));
            }
            let tabs_max_w = tab_strip_max_width(ui.available_width());
            let strip_out = egui::ScrollArea::horizontal()
                .id_salt("tab_strip")
                .max_width(tabs_max_w)
                .auto_shrink([true, false])
                // The ‹ › step-arrows (above/below) now own overflow navigation, so
                // the thin horizontal scrollbar is pure visual noise in the top bar.
                // Hide it entirely — `scroll_to_me` (arrow-driven scroll-into-view)
                // works independently of the bar's visibility.
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    for (i, (pane_id, title)) in tabs.into_iter().enumerate() {
                        let selected = pane_id == self.focused_pane;
                        let is_pinned = self.pinned.contains(&pane_id);
                        // Per-tab accessible labels. The pin/× glyph buttons AND the tab
                        // itself would otherwise expose a NON-unique name (the title),
                        // and two shells in the same directory routinely set the SAME OSC
                        // title — ambiguous for screen readers AND for `get_by_label`
                        // tests. Anchor every label on the unique `pane {id}` so each tab
                        // is distinguishable even when titles collide. The VISIBLE tab
                        // text stays the bare title; only the accessible name carries the
                        // id suffix.
                        let a11y = Self::tab_a11y_label(pane_id, &title);
                        let pin_label =
                            format!("{} {a11y}", if is_pinned { "unpin" } else { "pin" });
                        let close_label = format!("close {a11y}");
                        ui.scope(|ui| {
                            // Tight spacing INSIDE a tab so title/pin/× read as one unit.
                            ui.spacing_mut().item_spacing.x = 3.0;
                            // The selected tab is painted with the accent SELECTION
                            // WASH (`visuals.selection.bg_fill` = accent @ ~38%), so
                            // accent-coloured text on it is the same hue → unreadable
                            // (the reported bug). Use the bright theme FOREGROUND for
                            // the active tab (clearly readable on the wash in both
                            // light and dark themes) and a MUTED tone for inactive
                            // tabs — readability + a conventional active/inactive cue.
                            let label = RichText::new(&title).color(if selected {
                                colors.fg
                            } else {
                                colors.muted
                            });
                            let tab = ui.selectable_label(selected, label);
                            // Hover preview (#15): the last few lines of THIS pane's
                            // visible terminal, shown after egui's built-in hover
                            // delay. Attached to the tab label response ONLY — the
                            // pin/× buttons keep their own `.on_hover_text`, so nothing
                            // fights. `grid_text` briefly locks the term mutex; called
                            // once per frame for the single hovered tab → negligible.
                            let tab = match self
                                .terms
                                .get(&pane_id)
                                .and_then(super::pane_term::PaneTerm::grid_text)
                                .map(|text| last_nonblank_lines(&text, 8))
                                .filter(|s| !s.trim().is_empty())
                            {
                                Some(body) => tab.on_hover_ui(|ui| {
                                    ui.set_max_width(520.0);
                                    ui.label(RichText::new(&title).strong());
                                    ui.separator();
                                    ui.label(RichText::new(body).monospace().size(11.0));
                                }),
                                None => tab,
                            };
                            // Bring an arrow-stepped tab into view the frame after the
                            // click (the host has applied the new focus by then).
                            if follow && selected {
                                tab.scroll_to_me(Some(egui::Align::Center));
                            }
                            // Override the accessible name with the UNIQUE label so the
                            // a11y tree never has two same-named tab nodes (the visible
                            // text is unchanged — still just the title).
                            tab.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::SelectableLabel,
                                    true,
                                    &a11y,
                                )
                            });
                            if tab.clicked() {
                                actions.focus_tab = Some(pane_id);
                            }
                            // Middle-click closes — the universal browser/terminal
                            // convention. Inert on a PINNED tab: its × is deliberately
                            // hidden so a pinned pane cannot be shut by accident, and
                            // middle-click must not become a back door around that.
                            if tab.middle_clicked() && middle_click_closes(is_pinned) {
                                actions.close_tab = Some(pane_id);
                            }
                            // Drag-to-reorder. The tab body is the drag handle; the
                            // pin/× keep their own click semantics.
                            let tab = tab.interact(egui::Sense::click_and_drag());
                            if tab.drag_started() {
                                ui.ctx()
                                    .data_mut(|d| d.insert_temp(tab_drag_id(), TabDrag(pane_id)));
                            }
                            // While a drag is live, the tab under the pointer is the drop
                            // target; the pointer's side of centre decides before/after.
                            // An insertion caret is painted so the drop point is visible.
                            if let (Some(TabDrag(from)), Some(pos)) =
                                (dragging, ui.ctx().pointer_interact_pos())
                            {
                                if from != pane_id && tab.rect.contains(pos) {
                                    let before = pos.x < tab.rect.center().x;
                                    drop_target = Some((from, pane_id, before));
                                    let x = if before {
                                        tab.rect.left()
                                    } else {
                                        tab.rect.right()
                                    };
                                    ui.painter().vline(
                                        x,
                                        tab.rect.y_range(),
                                        egui::Stroke::new(2.0f32, colors.accent),
                                    );
                                }
                            }
                            // Pinned → SOLID violet pin (Fill family); unpinned → thin
                            // muted pin. The fill glyph makes "pinned" read at a glance.
                            // Routed through `glyph_button` (pre-computed rect) so the
                            // glyph recolours and the hover veil fills under the
                            // pointer. A plain flow `Button` bakes its text colour at
                            // construction and `.frame(false)` paints no fill in ANY
                            // state — which is exactly why the pin/× read as dead.
                            let pin_rest = if is_pinned {
                                brand::PURPLE
                            } else {
                                colors.muted
                            };
                            let pin = glyph_button(
                                ui,
                                tab_control_rect(ui),
                                |c| {
                                    if is_pinned {
                                        RichText::new(egui_phosphor::fill::PUSH_PIN)
                                            .family(egui::FontFamily::Name("phosphor-fill".into()))
                                            .size(13.0)
                                            .color(c)
                                    } else {
                                        RichText::new(icon::PUSH_PIN).size(13.0).color(c)
                                    }
                                },
                                colors,
                                pin_rest,
                                false,
                                true,
                                egui::Id::new(("tab_pin_hover", pane_id)),
                                &pin_label,
                            );
                            pin.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    &pin_label,
                                )
                            });
                            if pin.clicked() {
                                actions.pin_tab = Some(pane_id);
                            }
                            if !is_pinned {
                                // `is_close = true` selects the CLOSE_RED hover family
                                // (and CLOSE_RED_PRESSED when held) — the Windows
                                // convention for a destructive ✕.
                                let close = glyph_button(
                                    ui,
                                    tab_control_rect(ui),
                                    |c| RichText::new(icon::X).size(13.0).color(c),
                                    colors,
                                    colors.muted,
                                    true,
                                    true,
                                    egui::Id::new(("tab_close_hover", pane_id)),
                                    &close_label,
                                );
                                close.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        true,
                                        &close_label,
                                    )
                                });
                                if close.clicked() {
                                    actions.close_tab = Some(pane_id);
                                }
                            }
                        });
                        // Subtle 1px theme-tinted HAIRLINE between adjacent tabs — a
                        // boundary line, NOT egui's spacing `separator()` (which adds
                        // a wide gap). The theme foreground at ~16% alpha reads as a
                        // faint rule in both light and dark themes, and because it
                        // carries its OWN low alpha (not folded to zero by the window
                        // opacity) it stays visible in transparency mode without ever
                        // reading as a hard opaque bar. Skipped after the last tab.
                        if i != last_tab {
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(5.0, 18.0), egui::Sense::hover());
                            ui.painter().vline(
                                rect.center().x,
                                rect.y_range(),
                                egui::Stroke::new(1.0f32, colors.fg.gamma_multiply(0.16)),
                            );
                        }
                    }
                });
            // Resolve the drag AFTER the strip closure so the order is never mutated
            // mid-iteration. On release: commit the new order if the pointer was over a
            // valid target, then clear the drag either way (a release over empty space
            // is a cancelled drag, not a reorder).
            if dragging.is_some() && ui.ctx().input(|i| i.pointer.any_released()) {
                if let Some((from, to, before)) = drop_target {
                    if let Some(new_order) =
                        reorder_tabs(&visible_order, &self.pinned, from, to, before)
                    {
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(tab_order_id(), new_order));
                    }
                }
                ui.ctx().data_mut(|d| d.remove::<TabDrag>(tab_drag_id()));
            }
            // Record overflow for NEXT frame's left-arrow gate, and render the
            // RIGHT chevron now (it may use THIS frame's overflow). Overflow =
            // content wider than the viewport (egui's own "needs scrolling" signal).
            let overflowing = strip_out.content_size.x > strip_out.inner_rect.width() + 0.5;
            ui.ctx()
                .data_mut(|d| d.insert_temp(overflow_id, overflowing));
            if overflowing && step_arrow(ui, icon::CARET_RIGHT, next_target.is_some(), "next tab") {
                actions.focus_tab = next_target;
                ui.ctx().data_mut(|d| d.insert_temp(follow_id, true));
            }

            // Single "+" new-terminal button: opens a new pane and lets the host
            // expand the grid logically (it splits the focused pane along its
            // longer axis, keeping panes balanced — no manual direction choice).
            // It runs the active shell profile (set via the ▾ switcher below).
            let new_term = ui
                .button(RichText::new(icon::PLUS).size(16.0))
                .on_hover_text(format!("new terminal ({})", self.active_shell_label()));
            new_term.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "new terminal")
            });
            if new_term.clicked() {
                actions.new_terminal = true;
            }

            // LEFT toolbar group (`config.toolbar.left`): the customizable
            // quick-action buttons that live in the titlebar flow, right after the
            // "+". By default view-mode, equalize, and shell-switcher are here; the
            // script launcher defaults to the RIGHT cluster (below). Contents +
            // order come from config; an id not applicable this frame (e.g.
            // equalize outside grid view) is skipped and takes no slot.
            for id in &self.config.toolbar.left {
                if self.toolbar_action_applicable(id) {
                    self.render_toolbar_item(ui, id, &mut actions, colors);
                }
            }

            // ---- right-pinned caption cluster ----
            // Placed at ABSOLUTE rects via `ui.put`. Every layout-flow attempt
            // (`right_to_left`, `Sides`, `allocate_ui_with_layout`, an
            // `available_width()` spacer) AND every right-edge taken from the
            // ui's own rects left the right side EMPTY: in this non-justified
            // `horizontal_centered` the ui's `clip_rect()`/`max_rect()` right edge
            // is UNBOUNDED in the render frame (`rect_filled(clip_rect)` paints
            // the visible width, but `clip_rect().right()` is ~f32::MAX), so a
            // right-anchored x landed off-screen. The window's `screen_rect()` is
            // the only reliable bounded right edge; `min_rect()` (the content laid
            // out so far) gives the true row Y. Reads left→right ⚙ — ▢ ✕.
            let screen = ui.ctx().content_rect();
            let row = ui.min_rect();
            let bw = 42.0_f32;
            let bh = 28.0_f32;
            let cy = row.center().y;
            let right_edge = screen.right() - 8.0; // window edge minus panel inset
                                                   // Maximize button follows the Windows convention: a single square when
                                                   // the window is restored (click → maximize), and a "restore" glyph (two
                                                   // overlapping squares) when the window is maximized (click → restore
                                                   // down). Without this the button showed a static square in both states,
                                                   // giving no visual cue of the current window state. The maximized state
                                                   // is read from the live viewport (the same source the titlebar
                                                   // double-click-to-toggle uses).
            let is_maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            let (max_glyph, max_hover) = if is_maximized {
                (icon::COPY, "restore")
            } else {
                (icon::SQUARE, "maximize")
            };
            // 5th field = `is_close`: only the ✕ takes the Windows close-red hover
            // treatment. It's a distinct flag (not `cmd == Close`) because the gear
            // ALSO carries `WindowCmd::Close` as a placeholder — keying on the cmd
            // would paint the settings button red too.
            let specs: [(&str, &str, super::WindowCmd, bool, bool); 4] = [
                (icon::X, "close", super::WindowCmd::Close, false, true),
                (
                    max_glyph,
                    max_hover,
                    super::WindowCmd::ToggleMaximize,
                    false,
                    false,
                ),
                (
                    icon::MINUS,
                    "minimize",
                    super::WindowCmd::Minimize,
                    false,
                    false,
                ),
                (icon::GEAR, "settings", super::WindowCmd::Close, true, false), // gear → settings
            ];
            let mut right_x = right_edge;
            for (glyph, hover, cmd, is_gear, is_close) in specs {
                let rect = egui::Rect::from_min_max(
                    egui::pos2(right_x - bw, cy - bh / 2.0),
                    egui::pos2(right_x, cy + bh / 2.0),
                );
                // Hover treatment ported from SCR1B3's caption cluster so the two
                // apps share one window-control language: on hover the min / max /
                // restore / settings glyph brightens to the light foreground over
                // the flat veil `flatten_chrome_buttons` already paints, while the
                // CLOSE button fills Windows-standard close-red (#E81123) with a
                // white glyph. Pre-checking `rect_contains_pointer` lets us style the
                // `Button` BEFORE it paints — the fill is set ONLY on the hovered
                // frame, so the resting state stays flat/translucent (no always-on
                // opaque chip is reintroduced through the bar wash). Close-red is
                // theme-independent by design (Windows convention in both themes).
                let hovered = ui.rect_contains_pointer(rect);
                let glyph_col = if hovered {
                    if is_close {
                        egui::Color32::WHITE
                    } else {
                        colors.fg
                    }
                } else {
                    colors.muted
                };
                let mut button =
                    egui::Button::new(RichText::new(glyph).size(16.0).color(glyph_col));
                if hovered && is_close {
                    // Overrides the flat veil `flatten_chrome_buttons` set on
                    // `widgets.hovered` (Button::fill wins over `weak_bg_fill`).
                    button = button.fill(egui::Color32::from_rgb(0xE8, 0x11, 0x23));
                }
                let resp = ui.put(rect, button).on_hover_text(hover);
                // Accessible label (for screen readers AND the `get_by_label`
                // interaction tests) — the visible content is a glyph, so the
                // semantic name must be set explicitly.
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, hover)
                });
                if resp.clicked() {
                    if is_gear {
                        actions.toggle_settings = true;
                    } else {
                        actions.window_cmd = Some(cmd);
                    }
                }
                right_x -= bw + 2.0;
            }

            // Customizable quick-action cluster — pinned to the RIGHT, laid out
            // right-to-left from the settings gear (`right_x` is at the gear's left
            // edge after the caption loop). Contents + order come from
            // `config.toolbar`; the LAST item renders immediately left of the gear.
            self.render_toolbar_cluster(ui, right_x, cy, bh, &mut actions, colors);
        });
        actions
    }

    /// Render the user-configurable quick-action cluster (`config.toolbar.right`)
    /// pinned to the right of the titlebar, laid out RIGHT-TO-LEFT starting at
    /// `right_x` (the settings gear's left edge) so `right.last()` sits nearest the
    /// gear. Every item shares ONE right-anchored child ui (a single `scope_builder`
    /// with a `right_to_left` layout) for the same reason the caption cluster is
    /// absolute-placed — the non-justified row cannot right-align in flow — but a
    /// single uniform `item_spacing.x` now gives every button the SAME inter-button
    /// gap regardless of its content width (previously each item sat in its own
    /// fixed-width slot, which visually pinched the single-glyph toggles). An unknown
    /// id, or an item whose action is not currently applicable (e.g. `equalize_panes`
    /// outside grid view), is skipped and takes no slot. When `menu` is non-empty and
    /// `show_overflow` is on, an overflow "⋯" button is placed at the LEFT end
    /// (farthest from the gear).
    fn render_toolbar_cluster(
        &self,
        ui: &mut egui::Ui,
        right_x: f32,
        cy: f32,
        bh: f32,
        actions: &mut ChromeActions,
        colors: ChromeColors,
    ) {
        // One UNIFORM inter-button gap for the whole cluster. Previously each item
        // sat in its own fixed-width slot (32 px toggles / 52 px menus) with the
        // button right-anchored inside, so the single-glyph toggles (view-toggle +
        // equalize) ended up visually pinched while the wider ▾ menu buttons had
        // roomier gaps — the reported "squished split/layout pair". Laying every
        // item out in ONE right-anchored flow with a single `item_spacing.x` makes
        // the gap identical regardless of a button's content width.
        const RIGHT_CLUSTER_GAP: f32 = 6.0;
        let show_overflow = self.config.toolbar.show_overflow
            && self
                .config
                .toolbar
                .menu
                .iter()
                .any(|id| self.toolbar_action_applicable(id));
        let any_item = show_overflow
            || self
                .config
                .toolbar
                .right
                .iter()
                .any(|id| self.toolbar_action_applicable(id));
        if !any_item {
            return;
        }
        // Every item gets an IDENTICAL fixed slot and its button is centered-and-
        // justified to FILL that slot, so the whole right cluster is a row of
        // uniform-pitch icon buttons with a constant gap between each. There is no
        // per-glyph width measurement to get wrong: the previous measured-slot
        // attempt mis-sized the phosphor menu glyphs, so the shell / script buttons
        // overflowed their slots and collided. A uniform slot + a uniform gap is
        // overlap-proof by construction (each button is exactly `SLOT_W` wide, and
        // the next slot starts a full `SLOT_W + RIGHT_CLUSTER_GAP` to the left).
        const SLOT_W: f32 = 30.0;
        let slot_at = |cx: f32| {
            egui::Rect::from_min_max(
                egui::pos2(cx - SLOT_W, cy - bh / 2.0),
                egui::pos2(cx, cy + bh / 2.0),
            )
        };
        let slot_layout = egui::Layout::centered_and_justified(egui::Direction::LeftToRight);
        let mut cx = right_x;
        // right.last() sits nearest the gear, so iterate `.rev()` and march left.
        for id in self.config.toolbar.right.iter().rev() {
            if !self.toolbar_action_applicable(id) {
                continue;
            }
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(slot_at(cx))
                    .layout(slot_layout),
                |ui| self.render_toolbar_item(ui, id, actions, colors),
            );
            cx -= SLOT_W + RIGHT_CLUSTER_GAP;
        }
        // Overflow "⋯" lands at the LEFT end of the cluster (farthest from the gear).
        if show_overflow {
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(slot_at(cx))
                    .layout(slot_layout),
                |ui| self.render_toolbar_overflow(ui, actions, colors),
            );
        }
    }

    /// Whether a toolbar action id should render THIS frame: a known catalog id
    /// whose action is currently applicable. `equalize_panes` only applies in grid
    /// view with a real split (2+ panes) — outside that it is a dead button, so it
    /// is skipped (takes no slot) exactly as the old inline button was hidden.
    fn toolbar_action_applicable(&self, id: &str) -> bool {
        if !super::chrome_toolbar::is_known_action(id) {
            return false;
        }
        if id == "equalize_panes" {
            let in_tabs = self.config.view_mode == c0pl4nd_core::config::ViewMode::Tabs;
            return !in_tabs && self.pane_count() >= 2;
        }
        true
    }

    /// Render a single quick-action widget (button or menu) for `id` into the
    /// current (already absolute-placed) child ui, wiring its click to `actions`.
    fn render_toolbar_item(
        &self,
        ui: &mut egui::Ui,
        id: &str,
        actions: &mut ChromeActions,
        colors: ChromeColors,
    ) {
        match id {
            "view_mode" => {
                let in_tabs = self.config.view_mode == c0pl4nd_core::config::ViewMode::Tabs;
                let (glyph, hover) = if in_tabs {
                    (icon::CARDS, "switch to grid view (show all panes)")
                } else {
                    (icon::GRID_FOUR, "switch to tabs view (one pane at a time)")
                };
                // SCR1B3-parity toggled-ON treatment: a stateful toolbar button
                // (here "tabs view" is the non-default active state) paints the
                // theme accent at 20% alpha — `accent.gamma_multiply(0.20)`, the
                // exact value SCR1B3's toolbar uses for a selected item — with the
                // glyph in the full accent colour. Idle/hover stay transparent /
                // veil-filled via `flatten_chrome_buttons`, so this is the ONLY
                // opaque-ish fill and only while the mode is on.
                let active = in_tabs;
                let glyph_col = if active { colors.accent } else { colors.muted };
                let mut button =
                    egui::Button::new(RichText::new(glyph).size(16.0).color(glyph_col));
                if active {
                    button = button.fill(colors.accent.gamma_multiply(0.20));
                }
                let btn = ui.add(button).on_hover_text(hover);
                btn.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        "toggle view: grid/tabs",
                    )
                });
                if btn.clicked() {
                    actions.toggle_view_mode = true;
                }
            }
            "equalize_panes" => {
                let sym = ui
                    .button(RichText::new(icon::COLUMNS).size(16.0).color(colors.muted))
                    .on_hover_text("make panes symmetrical (equal sizes)");
                sym.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        "make panes symmetrical",
                    )
                });
                if sym.clicked() {
                    actions.equalize_panes = true;
                }
            }
            "shell_switcher" => {
                // Single-glyph icon (no "▾"), size + colour matched to the toggle
                // buttons, so every right-cluster item is a uniform icon button (the
                // wider "▾" label is what overflowed the slot and collided). The
                // menu still opens on click; the tooltip conveys the affordance.
                let menu = ui.menu_button(
                    RichText::new(icon::TERMINAL_WINDOW)
                        .size(16.0)
                        .color(colors.muted),
                    |ui| self.toolbar_shell_menu(ui, actions),
                );
                menu.response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "shell menu")
                });
                menu.response
                    .on_hover_text("Choose which shell new terminals run");
            }
            "script_launcher" => {
                let scripts = ui.menu_button(
                    RichText::new(icon::SCROLL).size(16.0).color(colors.muted),
                    |ui| self.toolbar_script_menu(ui, actions),
                );
                scripts.response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "script menu")
                });
                scripts
                    .response
                    .on_hover_text("Run a script file or re-run a previous command");
            }
            _ => {}
        }
    }

    /// The overflow "⋯" menu: each parked (`config.toolbar.menu`) action as a row
    /// that performs the SAME action as its toolbar button would.
    fn render_toolbar_overflow(
        &self,
        ui: &mut egui::Ui,
        actions: &mut ChromeActions,
        _colors: ChromeColors,
    ) {
        let menu = ui.menu_button(RichText::new(icon::DOTS_THREE).size(16.0), |ui| {
            ui.label(RichText::new("More actions").weak().small());
            ui.separator();
            for id in self.config.toolbar.menu.clone() {
                if !self.toolbar_action_applicable(&id) {
                    continue;
                }
                let label = super::chrome_toolbar::action_label(&id).unwrap_or("");
                match id.as_str() {
                    "view_mode" => {
                        let clicked = ui.button(label).clicked();
                        if clicked {
                            actions.toggle_view_mode = true;
                            ui.close_kind(egui::UiKind::Menu);
                        }
                    }
                    "equalize_panes" => {
                        let clicked = ui.button(label).clicked();
                        if clicked {
                            actions.equalize_panes = true;
                            ui.close_kind(egui::UiKind::Menu);
                        }
                    }
                    "shell_switcher" => {
                        ui.menu_button(label, |ui| self.toolbar_shell_menu(ui, actions));
                    }
                    "script_launcher" => {
                        ui.menu_button(label, |ui| self.toolbar_script_menu(ui, actions));
                    }
                    _ => {}
                }
            }
        });
        menu.response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "more actions")
        });
        menu.response.on_hover_text("More toolbar actions");
    }

    /// The shell-switcher menu body (shared by the toolbar button and the overflow
    /// menu). Lists detected shells; picking one opens a new terminal with it and
    /// makes it the active "+" profile.
    fn toolbar_shell_menu(&self, ui: &mut egui::Ui, actions: &mut ChromeActions) {
        ui.label(RichText::new("Open a new terminal with…").weak().small());
        ui.separator();
        let active = self.active_shell_label().to_owned();
        for (i, profile) in self.shell_profiles().iter().enumerate() {
            let is_active = profile.label == active;
            // Active shell shown via egui's SELECTABLE-LABEL highlight (not an
            // appended "✓", which renders as tofu in the menu font).
            let item = ui.selectable_label(is_active, &profile.label);
            item.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("open shell {}", profile.label),
                )
            });
            if item.clicked() {
                actions.open_shell = Some(i);
            }
        }
    }

    /// The script-launcher menu body (shared by the toolbar button and the overflow
    /// menu): an "Open…" file-picker item, the newest-first command history
    /// (click to re-run), and a W1TN3SS "Report an issue…" item. Every outcome is
    /// deferred to the host via `actions` (the picker BLOCKS and the run/report
    /// paths are `&mut self`, unsafe mid-panel).
    fn toolbar_script_menu(&self, ui: &mut egui::Ui, actions: &mut ChromeActions) {
        if ui
            .button(format!("{} Open…", icon::FOLDER_OPEN))
            .on_hover_text("Pick a script file to run in the focused terminal")
            .clicked()
        {
            actions.open_script_file = true;
            ui.close_kind(egui::UiKind::Menu);
        }
        ui.separator();
        // `entries()` is already most-recent-first. Collect owned so the borrow of
        // `self` does not outlive the closure's per-row widget building.
        let entries: Vec<String> = self.cmd_history.entries().map(str::to_string).collect();
        if entries.is_empty() {
            ui.weak("No commands run yet.");
        } else {
            egui::ScrollArea::vertical()
                .id_salt("script_menu_history")
                .max_height(320.0)
                .show(ui, |ui| {
                    for cmd in &entries {
                        // Show the script's file NAME (not the long absolute path)
                        // for a picked-file run; the full command stays in the
                        // hover tooltip and is what actually re-runs.
                        let label = script_menu_label(cmd);
                        let item = ui.button(&label).on_hover_text(cmd);
                        item.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                format!("re-run {cmd}"),
                            )
                        });
                        if item.clicked() {
                            actions.rerun_command = Some(cmd.clone());
                            ui.close_kind(egui::UiKind::Menu);
                        }
                    }
                });
        }
        // W1TN3SS manual "Report an issue…" entry (opt-in, user-initiated). Opens
        // the prefilled-GitHub-issue dialog; nothing is sent until the user reviews
        // + submits in their browser. Deferred to the host.
        ui.separator();
        let report = ui
            .button(format!("{} Report an issue…", icon::BUG))
            .on_hover_text("Open a prefilled GitHub issue (review before submitting)");
        report.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "report an issue")
        });
        if report.clicked() {
            actions.report_issue = true;
            ui.close_kind(egui::UiKind::Menu);
        }
    }

    /// Paint the bottom status bar — pane count + a theme-tinted hint. `colors`
    /// carries the theme-derived palette so the bar follows the active theme.
    ///
    /// Every accent-tinted item here uses [`ChromeColors::accent_text`], NOT the
    /// raw `accent`: `accent` is the theme's `selection_background`, a wash
    /// colour meant to sit behind text, and on the default `void` theme it is
    /// `#33106b` against this bar's `#202020` panel — **1.11:1**, so the pane
    /// counter and the toast were painted and unreadable. `accent_text` is the
    /// same hue clamped to the WCAG AA text floor.
    pub(super) fn status_bar(&self, ui: &mut egui::Ui, colors: ChromeColors) {
        ui.horizontal(|ui| {
            let panes = super::grid::count_panes(&self.grid_tree);
            ui.label(
                RichText::new(format!("{panes}/{} panes", super::grid::MAX_PANES))
                    .color(colors.accent_text),
            );
            ui.separator();
            ui.label(
                RichText::new("C0PL4ND — local-first terminal")
                    .color(colors.fg)
                    .weak(),
            );
            ui.separator();
            ui.label(
                RichText::new("Ctrl+Shift+P: commands")
                    .color(colors.fg)
                    .weak(),
            );
            ui.separator();
            // F11 is the only fullscreen affordance (the titlebar double-click-to-
            // maximize is hidden while the bar is hidden), so surface it here for
            // discoverability (#36).
            ui.label(RichText::new("F11: fullscreen").color(colors.fg).weak());
            // Mouse-reporting badge: when the FOCUSED pane's TUI has grabbed the
            // mouse (DEC ?1000/?1002/?1003), show a small badge so the user can
            // see why their clicks/scroll go to the app instead of the terminal.
            // Hidden entirely when reporting is Off (the common case).
            if let Some(term) = self.terms.get(&self.focused_pane) {
                let mode = term.mouse_mode();
                if let Some(label) = mouse_mode_badge_label(mode) {
                    ui.separator();
                    ui.label(
                        RichText::new(format!("{} {label}", icon::MOUSE_SIMPLE))
                            .color(colors.accent_text),
                    )
                    .on_hover_text(
                        "The focused application has enabled mouse reporting \
                         (clicks and scroll are sent to the program).",
                    );
                }
            }
            // Exit-code indicator: the FOCUSED pane's last finished command's
            // OSC 133 `D` exit code. A green check for success (0), an X plus
            // the code for a failure. Hidden entirely when no command has
            // finished (a bare shell with no prompt integration never emits a
            // `D` mark — the common case).
            if let Some(term) = self.terms.get(&self.focused_pane) {
                if let Some(indicator) = exit_code_indicator(term.last_command_exit_code()) {
                    ui.separator();
                    // Success uses the theme/brand green live-accent; a failure
                    // uses the muted foreground (Akira-red #ff0040 is reserved
                    // for alarms, not routine non-zero command exits).
                    let color = if indicator.is_failure {
                        colors.muted
                    } else {
                        brand::GREEN
                    };
                    ui.label(RichText::new(indicator.text).color(color))
                        .on_hover_text(indicator.hover);
                }
            }
            if let Some(toast) = &self.toast {
                ui.separator();
                // TRUNCATE, never run off the window. The welcome toast is longer
                // than the space left after the four hints, and an untruncated
                // label is laid out at its full width and simply clipped by the
                // panel — so it ended mid-word at the window edge with no ellipsis
                // and no way to tell whether anything was missing. `truncate()`
                // fits it to the remaining width and marks the cut with an
                // ellipsis; the full text stays available on hover.
                ui.add(egui::Label::new(RichText::new(toast).color(colors.accent_text)).truncate())
                    .on_hover_text(toast);
            }
        });
    }
}

/// A rendered exit-code status-bar indicator: the glyph+code text, an
/// accessible hover label, and whether it represents a failed command (so the
/// caller picks the colour). Kept as a plain struct returned by a free function
/// so the indicator-selection logic is unit-testable without an egui `Ui`.
struct ExitCodeIndicator {
    /// The status-bar text — a Phosphor glyph plus, for failures, the code.
    text: String,
    /// Accessible hover/description label (mirrors the mouse-mode badge).
    hover: &'static str,
    /// `true` for a non-zero exit code (drives the failure colour).
    is_failure: bool,
}

/// Build the status-bar [`ExitCodeIndicator`] for a pane's
/// [`last_command_exit_code`](super::pane_term::PaneTerm::last_command_exit_code)
/// value, or `None` when no command has finished yet (no indicator shown).
///
/// - outer `None` → `None` (no finished command; the status bar shows nothing);
/// - `Some(Some(0))` → green check, "success" (`is_failure = false`);
/// - `Some(Some(code))` for `code != 0` → X + the code, "failed"
///   (`is_failure = true`);
/// - `Some(None)` → a check glyph with a "finished (no exit code reported)"
///   label, treated as non-failure so it does not alarm.
fn exit_code_indicator(exit: Option<Option<i32>>) -> Option<ExitCodeIndicator> {
    let code = exit?;
    Some(match code {
        Some(0) => ExitCodeIndicator {
            text: icon::CHECK_CIRCLE.to_string(),
            hover: "Last command succeeded (exit code 0).",
            is_failure: false,
        },
        Some(code) => ExitCodeIndicator {
            text: format!("{} {code}", icon::X_CIRCLE),
            hover: "The last command exited with an error.",
            is_failure: true,
        },
        None => ExitCodeIndicator {
            text: icon::CHECK_CIRCLE.to_string(),
            hover: "Last command finished (no exit code reported).",
            is_failure: false,
        },
    })
}

/// The status-bar badge label for a mouse-reporting mode, or `None` when mouse
/// reporting is [`MouseMode::Off`] (no badge shown). Kept as a free function so
/// the badge-visibility logic is unit-testable without an egui `Ui`.
fn mouse_mode_badge_label(mode: MouseMode) -> Option<&'static str> {
    match mode {
        MouseMode::Off => None,
        MouseMode::Normal => Some("MOUSE"),
        MouseMode::ButtonEvent => Some("MOUSE: BTN"),
        MouseMode::AnyEvent => Some("MOUSE: ANY"),
    }
}

/// Maximum width (points) of the scrollable tab strip for a titlebar of
/// `available_width`. Reserves [`TAB_STRIP_RIGHT_RESERVE`] on the right for the
/// absolute-positioned caption cluster (close/max/min/settings) plus the
/// trailing "+"/"▾" controls, so the strip can never grow under them; floored at
/// a small minimum so a very narrow window still shows a sliver of tabs (which
/// then scroll) rather than collapsing to zero. Pure so the reserve invariant is
/// unit-testable without an egui frame.
fn tab_strip_max_width(available_width: f32) -> f32 {
    /// Caption cluster (~176pt: 4 × 42 + inset) + the
    /// "+"/view-toggle/"▾ shell"/"📜 ▾ scripts" flow controls.
    const TAB_STRIP_RIGHT_RESERVE: f32 = 370.0;
    (available_width - TAB_STRIP_RIGHT_RESERVE).max(80.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_mode_badge_hidden_when_off() {
        assert_eq!(mouse_mode_badge_label(MouseMode::Off), None);
    }

    #[test]
    fn script_menu_label_shows_filename_for_each_quoting_shape() {
        // The three shapes `quote_path_for_shell` emits for a picked file — the
        // label is the basename, never the long absolute path.
        assert_eq!(
            script_menu_label("& \"C:\\scripts\\deploy.ps1\""),
            "deploy.ps1",
            "PowerShell `& \"PATH\"` → basename"
        );
        assert_eq!(
            script_menu_label("\"C:\\scripts\\build.bat\""),
            "build.bat",
            "cmd/Windows `\"PATH\"` → basename"
        );
        assert_eq!(
            script_menu_label("'/home/user/run.sh'"),
            "run.sh",
            "POSIX `'PATH'` → basename"
        );
    }

    #[test]
    fn script_menu_label_leaves_plain_commands_untouched() {
        // A typed command is not a quoted single-path invocation → verbatim.
        assert_eq!(script_menu_label("ls -la"), "ls -la");
        assert_eq!(script_menu_label("git status"), "git status");
        // A quoted token with NO directory component has no long path to hide,
        // so it is left as-is (only shortens when there is a real parent dir).
        assert_eq!(script_menu_label("\"hello\""), "\"hello\"");
    }

    #[test]
    fn script_path_of_unescapes_and_rejects_non_paths() {
        // PowerShell un-escapes the doubled `` `" `` back to a literal quote.
        assert_eq!(
            script_path_of("& \"C:\\a`\"b\\x.ps1\"").as_deref(),
            Some("C:\\a\"b\\x.ps1")
        );
        // POSIX un-escapes `'\''` back to a literal apostrophe.
        assert_eq!(
            script_path_of("'/tmp/it'\\''s.sh'").as_deref(),
            Some("/tmp/it's.sh")
        );
        // A plain command is not a single quoted path.
        assert_eq!(script_path_of("echo hi"), None);
    }

    #[test]
    fn tab_strip_reserves_caption_space_and_floors() {
        // A roomy titlebar leaves width for tabs after reserving the caption
        // cluster + "+"/"▾" controls, so the strip never grows under them.
        let wide = tab_strip_max_width(1100.0);
        assert!(
            wide < 1100.0 - 200.0,
            "the strip must reserve a substantial right margin for the caption \
             cluster (got {wide} for a 1100pt bar)"
        );
        assert!(wide > 0.0);
        // A very narrow window floors the strip to a scrollable sliver rather
        // than collapsing to zero (or going negative).
        assert_eq!(
            tab_strip_max_width(100.0),
            80.0,
            "a narrow titlebar floors the tab strip width (it then scrolls)"
        );
        assert_eq!(
            tab_strip_max_width(0.0),
            80.0,
            "a degenerate zero width still floors, never negative"
        );
        // Reserve is monotonic: more titlebar width → more (or equal) tab width.
        assert!(tab_strip_max_width(1600.0) > tab_strip_max_width(900.0));
    }

    #[test]
    fn mouse_mode_badge_shown_when_reporting() {
        assert_eq!(mouse_mode_badge_label(MouseMode::Normal), Some("MOUSE"));
        assert_eq!(
            mouse_mode_badge_label(MouseMode::ButtonEvent),
            Some("MOUSE: BTN")
        );
        assert_eq!(
            mouse_mode_badge_label(MouseMode::AnyEvent),
            Some("MOUSE: ANY")
        );
    }

    #[test]
    fn exit_code_indicator_hidden_when_no_finished_command() {
        assert!(
            exit_code_indicator(None).is_none(),
            "no finished command must show no indicator"
        );
    }

    #[test]
    fn exit_code_indicator_success_is_not_a_failure() {
        let ind = exit_code_indicator(Some(Some(0))).expect("success must show an indicator");
        assert!(!ind.is_failure, "exit code 0 is a success, not a failure");
        assert_eq!(
            ind.text,
            icon::CHECK_CIRCLE.to_string(),
            "success shows a bare check glyph (no code)"
        );
    }

    #[test]
    fn exit_code_indicator_failure_shows_code() {
        let ind = exit_code_indicator(Some(Some(127))).expect("failure must show an indicator");
        assert!(ind.is_failure, "non-zero exit code is a failure");
        assert_eq!(
            ind.text,
            format!("{} 127", icon::X_CIRCLE),
            "failure shows the X glyph plus the exit code"
        );
        // The hover uses plain language, not the "non-zero exit code" jargon
        // (inventory C0-044).
        assert_eq!(ind.hover, "The last command exited with an error.");
    }

    #[test]
    fn exit_code_indicator_missing_code_is_neutral() {
        // A finished command with no shell-reported code (`OSC 133 ; D`) shows
        // a non-alarming indicator rather than being hidden.
        let ind = exit_code_indicator(Some(None)).expect("finished command must show an indicator");
        assert!(
            !ind.is_failure,
            "an absent exit code must not be treated as a failure"
        );
    }
}
