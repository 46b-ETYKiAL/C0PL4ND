//! The shell's **shared action vocabulary** and the SINGLE dispatch path every
//! action surface routes through.
//!
//! Before this module the egui shell had a command palette that could only
//! re-run shell history, and a `[keybindings]` config block that controlled
//! nothing at all (the shortcuts were hard-wired inline in `frame_tick`, and the
//! settings rows were rendered disabled with an honest "not yet rebindable"
//! note). Two surfaces, no shared vocabulary, one of them inert.
//!
//! [`Action`] is that vocabulary. [`super::C0pl4ndApp::dispatch_action`] is the
//! ONE place an action becomes an effect, and BOTH callers reach it:
//!
//! - the **command palette** — `run_palette_selection` dispatches the selected
//!   [`Action`] entry (`>` filters the palette to actions only, VS Code style);
//! - the **keybinding dispatcher** — [`super::C0pl4ndApp::dispatch_keybindings`]
//!   matches this frame's key events against the chords parsed from the LIVE
//!   `config.keybindings` and dispatches the same [`Action`].
//!
//! There is deliberately no second dispatch table: rebinding
//! `command_palette = "mod+shift+k"` in `config.toml` moves the palette chord
//! because the dispatcher is the only thing that opens it.
//!
//! Chord parsing, alias folding, and canonicalisation live in
//! [`c0pl4nd_core::config::keybindings`] — the same code
//! [`c0pl4nd_core::config::Keybindings::validate`] uses to detect collisions, so
//! the matcher and the validator can never disagree about whether two combos are
//! the same chord.

use eframe::egui;

use c0pl4nd_core::config::{action_label, canonical_key_token, Chord};

use super::{grid, PaneId};

/// Declare the shell's whole action vocabulary ONCE.
///
/// The [`Action`] variants, [`Action::ALL`], and [`Action::binding`] are all
/// expanded from the single arm list below, so an action that is missing from
/// `ALL` — or from the binding table — cannot be WRITTEN, let alone shipped.
///
/// That is the entire point. `ALL` used to be a hand-maintained
/// `[Action; 20]` guarded by `assert_eq!(Action::ALL.len(), 20)`, which compares
/// the array's declared length with itself: a tautology that can never fail. A
/// 21st variant with arms in `binding()` and `dispatch_action` but omitted from
/// `ALL` compiled and passed the whole suite, while being invisible to the
/// command palette and unreachable from `dispatch_keybindings`. Generating the
/// list from the same arms as the enum makes that omission unrepresentable
/// instead of merely untested.
macro_rules! define_actions {
    ($(
        $(#[$vmeta:meta])*
        $variant:ident => $binding:literal,
    )+) => {
        /// Everything the shell can be asked to DO, independent of who asked.
        ///
        /// Each variant maps to exactly one `[keybindings]` field (its
        /// [`Action::binding`]) and to one command-palette row (its
        /// [`Action::label`], which is the same label the settings rows show —
        /// one source of truth).
        ///
        /// Clipboard copy/paste are deliberately NOT here: `egui-winit` intercepts
        /// those chords in its window-event dispatcher and delivers them as
        /// `Event::Copy` / `Event::Cut` rather than `Event::Key`, so they are
        /// handled on their own path and their settings rows stay honestly
        /// disabled rather than pretending to be rebindable here.
        ///
        /// Declared through [`define_actions!`], which also generates
        /// [`Action::ALL`] and [`Action::binding`] from the same arm list.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action {
            $(
                $(#[$vmeta])*
                $variant,
            )+
        }

        impl Action {
            /// How many actions there are — counted from the macro arms, never
            /// written by hand, so it cannot disagree with [`ALL`](Self::ALL).
            pub const COUNT: usize = [$(Action::$variant,)+].len();

            /// Every action, in command-palette display order.
            ///
            /// Exhaustive BY CONSTRUCTION: generated from the same
            /// [`define_actions!`] arm list as the variants themselves, so there
            /// is no way to declare an action that is absent from this list.
            pub const ALL: [Action; Self::COUNT] = [$(Action::$variant,)+];

            /// The `[keybindings]` config field this action is bound through. The
            /// dispatcher resolves the user's combo from this name, so a rebind in
            /// `config.toml` moves the chord with no code change.
            #[must_use]
            pub fn binding(self) -> &'static str {
                match self {
                    $(Action::$variant => $binding,)+
                }
            }
        }
    };
}

define_actions! {
    /// Open a new terminal pane.
    NewTab => "new_tab",
    /// Close the focused pane (never the last one).
    ClosePane => "close_tab",
    /// Move keyboard focus to the next pane, wrapping.
    FocusNextPane => "next_tab",
    /// Split the focused pane to the right.
    SplitRight => "split_right",
    /// Split the focused pane downward.
    SplitDown => "split_down",
    /// Toggle zoom on the focused pane.
    ZoomPane => "zoom_pane",
    /// Rebuild the layout as a uniform, equal-sized grid.
    EqualizePanes => "equalize_panes",
    /// Flip the pane shell between the grid and tab layouts.
    ToggleViewMode => "toggle_view_mode",
    /// Toggle the in-terminal find overlay.
    ToggleSearch => "search",
    /// Toggle the command palette.
    ToggleCommandPalette => "command_palette",
    /// Toggle the command-history quick-run sidebar.
    ToggleHistorySidebar => "history_sidebar",
    /// Toggle the settings window.
    ToggleSettings => "settings",
    /// Toggle borderless OS fullscreen.
    ToggleFullscreen => "fullscreen",
    /// Increase the terminal font size.
    IncreaseFont => "increase_font",
    /// Decrease the terminal font size.
    DecreaseFont => "decrease_font",
    /// Reset the terminal font size to the built-in default.
    ResetFont => "reset_font",
    /// Clear the focused pane's scrollback.
    ClearScrollback => "clear_scrollback",
    /// Copy the focused pane's whole buffer to the clipboard.
    CopyAll => "copy_all",
    /// Scroll the focused pane to the oldest retained line.
    ScrollToTop => "scroll_to_top",
    /// Scroll the focused pane back to live output.
    ScrollToBottom => "scroll_to_bottom",
}

impl Action {
    /// The human-readable label shown in the command palette — the SAME string
    /// the settings keybinding row shows, resolved from the core label table so
    /// the two surfaces can never drift apart.
    #[must_use]
    pub fn label(self) -> &'static str {
        // `action_label` returns its argument for an unknown name; every
        // `binding()` is a real schema field, so this always resolves to a real
        // label (asserted by `every_action_has_a_distinct_label`).
        action_label(self.binding())
    }
}

/// One row of the command palette: either a shell command from the history
/// (re-run in the focused pane) or one of the shell's own [`Action`]s.
///
/// Modelling both as one row type is what lets the palette's navigation
/// (`↑`/`↓`/Enter/click) stay a single code path while the palette gained the
/// ability to actually DO things instead of only re-running history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteEntry {
    /// A shell action, dispatched through `C0pl4ndApp::dispatch_action`.
    Action(Action),
    /// A previously-run command line, re-run in the focused pane.
    History(String),
}

impl PaletteEntry {
    /// The row's display text: the action label plus its bound chord (so the
    /// palette doubles as the shortcut cheat-sheet), or the command verbatim.
    #[must_use]
    pub fn display(&self, bindings: &c0pl4nd_core::config::Keybindings) -> String {
        match self {
            PaletteEntry::Action(a) => match bindings.chord(a.binding()) {
                Some(chord) => format!("{}   ({})", a.label(), chord.canonical()),
                // An unbound / unparseable binding still shows the row: the
                // action stays reachable from the palette, and the settings UI
                // surfaces WHY the chord does nothing.
                None => a.label().to_string(),
            },
            PaletteEntry::History(cmd) => cmd.clone(),
        }
    }
}

/// The canonical key token for an egui key, in the vocabulary
/// [`Chord`] compares on.
///
/// egui's `Key::name()` (`"A"`, `"CloseBracket"`, `"ArrowUp"`, `"Num0"`,
/// `"F11"`) IS the canonical vocabulary, so this is a lowercase +
/// [`canonical_key_token`] fold — the only real work it does is collapse
/// `Equals` onto `plus`, so the one physical `+`/`=` key is one binding.
#[must_use]
pub fn egui_key_token(key: egui::Key) -> String {
    canonical_key_token(key.name())
}

/// The three chord modifier flags for a live egui modifier snapshot.
///
/// `cmd` accepts `ctrl` OR `command` OR `mac_cmd`: on Windows/Linux `egui-winit`
/// sets `command == ctrl`, on macOS `command`/`mac_cmd` is ⌘ while `ctrl` is
/// real Control, and synthetic test events set only `ctrl`. Accepting all three
/// is exactly the `modifiers.command || modifiers.ctrl` discipline every
/// previously hard-wired chord in `frame_tick` used, so no platform changes
/// behaviour.
#[must_use]
fn chord_modifiers(m: &egui::Modifiers) -> (bool, bool, bool) {
    (m.ctrl || m.command || m.mac_cmd, m.shift, m.alt)
}

impl super::C0pl4ndApp {
    /// The (chord, action) table for the LIVE config, rebuilt each frame so a
    /// settings edit or a config hot-reload takes effect immediately. A blank or
    /// unparseable binding simply yields no entry — the action stays reachable
    /// from the command palette, and `Keybindings::validate` surfaces the
    /// problem in the settings UI rather than failing silently.
    pub(crate) fn keybinding_table(&self) -> Vec<(Chord, Action)> {
        Action::ALL
            .iter()
            .filter_map(|a| self.config.keybindings.chord(a.binding()).map(|c| (c, *a)))
            .collect()
    }

    /// Match this frame's key events against the configured chords, CONSUME every
    /// match (so a bound chord never also reaches the PTY as a control byte), and
    /// dispatch each matched [`Action`]. Returns what fired, in event order.
    ///
    /// This is the shell's ONLY keyboard-shortcut path. Matching is exact on
    /// modifiers ([`Chord::matches`]), so `mod+t` and `mod+shift+t` stay distinct
    /// actions and a stray Alt never triggers a chord.
    pub(crate) fn dispatch_keybindings(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let table = self.keybinding_table();
        let mut fired: Vec<Action> = Vec::new();
        ctx.input_mut(|i| {
            i.events.retain(|ev| {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                else {
                    return true;
                };
                let token = egui_key_token(*key);
                let (cmd, shift, alt) = chord_modifiers(modifiers);
                match table
                    .iter()
                    .find(|(chord, _)| chord.matches(cmd, shift, alt, &token))
                {
                    Some((_, action)) => {
                        fired.push(*action);
                        false // consumed — never leaks to the PTY
                    }
                    None => true,
                }
            });
        });
        for action in &fired {
            self.dispatch_action(*action, ctx);
        }
        fired
    }

    /// Perform one [`Action`]. The single place an action becomes an effect —
    /// the command palette and the keybinding dispatcher both land here, so the
    /// two surfaces cannot drift.
    pub(crate) fn dispatch_action(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::NewTab => self.new_terminal(),
            Action::ClosePane => self.close_pane(self.focused_pane),
            Action::FocusNextPane => self.focus_next_pane(),
            Action::SplitRight => self.split(egui_tiles::LinearDir::Horizontal),
            Action::SplitDown => self.split(egui_tiles::LinearDir::Vertical),
            Action::ZoomPane => self.toggle_zoom_pane(),
            Action::EqualizePanes => self.equalize_panes(ctx),
            Action::ToggleViewMode => self.toggle_view_mode(),
            Action::ToggleSearch => self.toggle_search(),
            Action::ToggleCommandPalette => self.toggle_palette(),
            Action::ToggleHistorySidebar => self.toggle_history_sidebar(),
            Action::ToggleSettings => self.settings_open = !self.settings_open,
            Action::ToggleFullscreen => {
                // Read the OS-reported state so a fullscreen entered by another
                // path (a window-manager shortcut) is honoured by the toggle.
                let now = ctx.input(|i| i.viewport().fullscreen.unwrap_or(self.fullscreen));
                self.set_fullscreen(ctx, !now);
            }
            Action::IncreaseFont => self.nudge_font_size(ctx, 1.0),
            Action::DecreaseFont => self.nudge_font_size(ctx, -1.0),
            Action::ResetFont => {
                let before = self.config.font.size;
                self.config.font.size = c0pl4nd_core::Config::default().font.size;
                self.after_font_size_change(ctx, before);
            }
            Action::ClearScrollback => {
                if let Some(term) = self.terms.get_mut(&self.focused_pane) {
                    term.clear_scrollback();
                }
                // The scrollbar is shorter this frame — repaint so it reflects it.
                ctx.request_repaint();
            }
            Action::CopyAll => {
                // The no-selection companion to the copy chord: an empty buffer
                // copies nothing rather than clearing the clipboard.
                if let Some(text) = self
                    .terms
                    .get(&self.focused_pane)
                    .and_then(super::PaneTerm::buffer_text)
                {
                    ctx.copy_text(text);
                }
            }
            Action::ScrollToTop => {
                if let Some(term) = self.terms.get_mut(&self.focused_pane) {
                    if term.scroll_to_top() {
                        ctx.request_repaint();
                    }
                }
            }
            Action::ScrollToBottom => {
                if let Some(term) = self.terms.get_mut(&self.focused_pane) {
                    let was = term.view_offset();
                    term.scroll_to_bottom();
                    if was != 0 {
                        ctx.request_repaint();
                    }
                }
            }
        }
    }

    /// Move keyboard focus to the next pane in tab order, wrapping. A no-op with
    /// fewer than two panes. Clears the per-pane typed-line accumulator on a real
    /// move so a half-typed line is not attributed to the newly-focused pane.
    fn focus_next_pane(&mut self) {
        let panes: Vec<PaneId> = self.pane_titles().into_iter().map(|(p, _)| p).collect();
        if panes.len() < 2 {
            return;
        }
        let idx = panes
            .iter()
            .position(|p| *p == self.focused_pane)
            .unwrap_or(0);
        let next = panes[(idx + 1) % panes.len()];
        if next != self.focused_pane {
            self.input_line.clear();
            self.focused_pane = next;
        }
    }

    /// Rebuild the layout as a UNIFORM grid so every pane is equal-sized
    /// regardless of the prior (possibly nested / asymmetric) split structure.
    /// Preserves pane order and every attached terminal. A no-op for a 0/1-pane
    /// tree. Shared by the chrome's "symmetrical" button and the action path.
    pub(crate) fn equalize_panes(&mut self, ctx: &egui::Context) {
        if let Some(grid) = grid::rebuild_as_uniform_grid(&self.grid_tree) {
            self.grid_tree = grid;
            ctx.request_repaint();
        }
    }

    /// Flip the pane shell layout (Grid ⇄ Tabs) and persist it. The disk write is
    /// real-window-only: the headless harness observes the in-memory flip, and
    /// persisting there would pollute the user's real `config.toml`. Shared by
    /// the chrome's view-mode button and the action path.
    pub(crate) fn toggle_view_mode(&mut self) {
        self.config.view_mode = self.config.view_mode.toggled();
        if !self.live_window {
            return;
        }
        if let Some(path) = c0pl4nd_core::Config::default_path() {
            // Surface a persist failure (read-only %APPDATA%, full disk,
            // permission error) instead of silently dropping the user's settings
            // change. A GUI user never sees stderr, so a visible toast is the
            // real surface.
            if let Err(e) = self.config.save_to(&path) {
                self.toast = Some(crate::user_error::config_save_failed(
                    e,
                    "The layout change",
                ));
            }
        }
    }

    /// Enter or leave borderless OS fullscreen, keeping the local mirror in step.
    ///
    /// The mirror is the source of truth the panels read THIS frame:
    /// `i.viewport().fullscreen` only updates a frame late (the OS reports back
    /// next frame), so reading it here would flash the titlebar on enter / the
    /// bare grid on exit.
    pub(crate) fn set_fullscreen(&mut self, ctx: &egui::Context, want: bool) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(want));
        self.fullscreen = want;
    }

    /// Step the terminal font size by `dz` points, clamped to `[6, 48]`. Shared
    /// by the font-zoom actions and the Ctrl/Cmd+wheel zoom, so both apply the
    /// same clamp and the same debounced persist.
    pub(crate) fn nudge_font_size(&mut self, ctx: &egui::Context, dz: f32) {
        let before = self.config.font.size;
        self.config.font.size = (before + dz).clamp(6.0, 48.0);
        self.after_font_size_change(ctx, before);
    }

    /// Repaint + schedule the debounced config persist after a font-size change.
    ///
    /// The renderer reads `config.font.size` every frame, so the new size applies
    /// live immediately. The PERSIST is DEBOUNCED: writing the whole config file
    /// (atomic temp-write + rename + perms) on every wheel notch is wasteful
    /// under a fast zoom, so a single save is scheduled for
    /// `FONT_SAVE_DEBOUNCE_SECS` after the LAST change. `frame_tick` flushes it;
    /// a repaint is scheduled for the deadline so an otherwise-idle app still
    /// wakes to write it.
    fn after_font_size_change(&mut self, ctx: &egui::Context, before: f32) {
        if self.config.font.size == before {
            return;
        }
        ctx.request_repaint();
        let now = ctx.input(|i| i.time);
        self.pending_font_save_at = Some(now + super::FONT_SAVE_DEBOUNCE_SECS);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(
            super::FONT_SAVE_DEBOUNCE_SECS,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use c0pl4nd_core::config::Keybindings;

    #[test]
    fn every_action_binds_to_a_real_schema_field() {
        // An action whose `binding()` is not a real `[keybindings]` field would
        // be silently unreachable by keyboard — `chord()` would always return
        // `None` and no test of the action itself would notice.
        let kb = Keybindings::default();
        let names: Vec<&str> = kb.entries().iter().map(|(n, _)| *n).collect();
        for a in Action::ALL {
            assert!(
                names.contains(&a.binding()),
                "{a:?} binds to '{}', which is not a Keybindings field",
                a.binding()
            );
            assert!(
                kb.chord(a.binding()).is_some(),
                "{a:?} ('{}') has no parseable default chord",
                a.binding()
            );
        }
    }

    #[test]
    fn every_action_has_a_distinct_label() {
        // The palette rows and the settings rows are keyed on the label; two
        // actions sharing one would make a palette row ambiguous.
        let mut seen: Vec<&str> = Vec::new();
        for a in Action::ALL {
            let l = a.label();
            assert_ne!(l, a.binding(), "{a:?} has no human-readable label");
            assert!(!seen.contains(&l), "duplicate action label {l:?}");
            seen.push(l);
        }
    }

    #[test]
    fn actions_and_bindings_are_unique() {
        // A duplicated binding name would mean two actions fight over one chord.
        let mut seen: Vec<&str> = Vec::new();
        for a in Action::ALL {
            assert!(!seen.contains(&a.binding()), "duplicate binding {a:?}");
            seen.push(a.binding());
        }
        // A REPEATED action is the other way a generated list can be wrong: two
        // rows for one action in the palette, and a `matching_actions` order that
        // shows it twice.
        //
        // What is NOT asserted here any more is `Action::ALL.len() == 20`. `ALL`
        // is `[Action; Self::COUNT]` with `COUNT` counted from the same
        // `define_actions!` arms, so that comparison was the array's declared
        // length against itself — a tautology that stayed green when a 21st
        // variant was added and left out of `ALL`. Omission is now a compile
        // error instead, which is strictly stronger than any runtime assertion
        // could be.
        let mut seen_actions: Vec<Action> = Vec::new();
        for a in Action::ALL {
            assert!(
                !seen_actions.contains(&a),
                "{a:?} appears twice in Action::ALL"
            );
            seen_actions.push(a);
        }
        // No length assertion here on purpose. `ALL.len() == COUNT` (or any other
        // literal) would be the SAME tautology in a new coat: `ALL` is
        // `[Action; Self::COUNT]`, so its length is `COUNT` by its own type.
        // Cardinality is the compiler's job now, not a test's.
    }

    #[test]
    fn egui_keys_map_onto_the_config_key_vocabulary() {
        // The dispatcher only fires when the token from a live egui key equals
        // the token parsed from the config string — so the two vocabularies must
        // agree for every key a default binding uses.
        assert_eq!(egui_key_token(egui::Key::T), "t");
        assert_eq!(egui_key_token(egui::Key::F11), "f11");
        assert_eq!(egui_key_token(egui::Key::Comma), "comma");
        assert_eq!(egui_key_token(egui::Key::Home), "home");
        assert_eq!(egui_key_token(egui::Key::End), "end");
        assert_eq!(egui_key_token(egui::Key::CloseBracket), "closebracket");
        assert_eq!(egui_key_token(egui::Key::Num0), "num0");
        assert_eq!(egui_key_token(egui::Key::Minus), "minus");
        // The one real fold: `=` and `+` are one physical key, one binding.
        assert_eq!(egui_key_token(egui::Key::Plus), "plus");
        assert_eq!(egui_key_token(egui::Key::Equals), "plus");
    }

    #[test]
    fn default_chords_match_the_egui_keys_they_name() {
        // End-to-end on the pure halves: the DEFAULT combo string for each
        // action must actually match a plausible live event for that key. A
        // vocabulary drift (e.g. renaming a token) breaks this even though both
        // halves still compile.
        let kb = Keybindings::default();
        let cases: [(Action, egui::Key, bool); 6] = [
            (Action::NewTab, egui::Key::T, true),
            (Action::ToggleCommandPalette, egui::Key::P, true),
            (Action::ToggleSettings, egui::Key::Comma, false),
            (Action::ScrollToTop, egui::Key::Home, true),
            (Action::FocusNextPane, egui::Key::CloseBracket, true),
            (Action::ResetFont, egui::Key::Num0, false),
        ];
        for (action, key, shift) in cases {
            let chord = kb.chord(action.binding()).expect("default parses");
            assert!(
                chord.matches(true, shift, false, &egui_key_token(key)),
                "{action:?} default '{}' does not match {key:?} (shift={shift})",
                chord.canonical()
            );
        }
        // Bare F11 has no modifiers at all.
        let fs = kb
            .chord(Action::ToggleFullscreen.binding())
            .expect("parses");
        assert!(fs.matches(false, false, false, &egui_key_token(egui::Key::F11)));
    }

    #[test]
    fn chord_modifiers_accept_ctrl_command_and_mac_cmd() {
        // Real winit on Windows/Linux sets `command == ctrl`; macOS sets
        // `mac_cmd`; synthetic test events set only `ctrl`. All three must read
        // as the command modifier or a chord fires on one platform and not
        // another.
        let ctrl_only = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(chord_modifiers(&ctrl_only), (true, false, false));
        let mac = egui::Modifiers {
            mac_cmd: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(chord_modifiers(&mac), (true, false, false));
        let alt_shift = egui::Modifiers {
            alt: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(chord_modifiers(&alt_shift), (false, true, true));
    }
}
