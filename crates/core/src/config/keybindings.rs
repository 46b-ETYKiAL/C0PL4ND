//! User-facing keymap schema, parsing, and validation ([`Chord`],
//! [`Keybindings`], [`KeybindingIssue`]).
//!
//! This module owns the SINGLE definition of "what does this combo string
//! mean". The egui shell's keybinding dispatcher and the command palette both
//! resolve a chord through [`Chord::parse`], and [`Keybindings::validate`]
//! groups collisions by [`Chord::canonical`] — so the matcher and the validator
//! can never disagree about whether two bindings are the same chord.
//!
//! `mod` is the platform command modifier (Ctrl on Windows/Linux, Cmd on
//! macOS) — the `modifiers.command` egui reports. `ctrl` / `control` / `cmd` /
//! `command` are accepted as aliases for it, and `option` / `opt` as aliases for
//! `alt`, so a config written with either muscle-memory still parses to the same
//! chord. Every shipped default reproduces the shortcut the egui shell used to
//! hard-wire, so a user who never opens `[keybindings]` sees no change.
//!
//! [`Chord`] is deliberately engine-neutral: it carries the modifier flags plus
//! a canonical key TOKEN (`"t"`, `"f11"`, `"closebracket"`), and the app crate
//! maps its windowing library's key type onto that token. That keeps
//! `c0pl4nd-core` free of any UI dependency.

use serde::{Deserialize, Serialize};

/// A parsed key combo: the modifier flags plus the canonical non-modifier key
/// token (lowercased, e.g. `"t"` / `"f11"` / `"closebracket"`).
///
/// Matching is EXACT on modifiers: a chord parsed from `"mod+t"` has
/// `shift == false` and must NOT fire when Shift is also held (that is what
/// keeps `mod+t` and `mod+shift+t` distinct actions).
///
/// # The `plus` exception
///
/// `+` is a SHIFTED glyph on most layouts (`Shift` + the `=` key), so the same
/// physical key reports as `Equals` unshifted and `Plus` shifted. Treating those
/// as two chords would make "Ctrl and +" unbindable in practice, so both the
/// parser and the key-token mapper collapse them to the single token `"plus"`
/// AND clear the Shift requirement for it. `mod+plus` therefore fires for both
/// `Ctrl+=` and `Ctrl+Shift+=`, which is what a user means by "Ctrl +".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// The platform command modifier (Ctrl on Windows/Linux, Cmd on macOS) is
    /// required.
    pub cmd: bool,
    /// Shift is required.
    pub shift: bool,
    /// Alt / Option is required.
    pub alt: bool,
    /// Canonical lowercase key token — the one non-modifier token in the combo.
    pub key: String,
}

/// Canonicalize one non-modifier key token to the form [`Chord`] compares on.
///
/// The canonical vocabulary is egui's `Key::name()` lowercased (`"a"`,
/// `"arrowup"`, `"closebracket"`, `"num0"`, `"f11"`), so the app's key-token
/// mapper needs no second alias table. Friendly spellings a user would actually
/// type in a TOML file (`"]"`, `"esc"`, `"up"`, `"pgup"`, `"0"`) fold onto it.
/// An unrecognised token passes through lowercased, so a key this table has not
/// heard of is still bindable by its egui name.
#[must_use]
pub fn canonical_key_token(raw: &str) -> String {
    let t = raw.trim().to_ascii_lowercase();
    match t.as_str() {
        // Punctuation the user types as the glyph itself.
        "]" | "closebracket" | "bracketright" => "closebracket",
        "[" | "openbracket" | "bracketleft" => "openbracket",
        "," | "comma" => "comma",
        "." | "period" | "dot" => "period",
        "/" | "slash" => "slash",
        "\\" | "backslash" => "backslash",
        ";" | "semicolon" => "semicolon",
        "'" | "quote" | "apostrophe" => "quote",
        "`" | "backtick" | "grave" => "backtick",
        // The `+`/`=` physical key collapses to one token (see the type docs).
        "+" | "plus" | "=" | "equals" | "equal" => "plus",
        "-" | "minus" | "dash" | "hyphen" => "minus",
        // Digits: egui names them `Num0`..`Num9`.
        "0" => "num0",
        "1" => "num1",
        "2" => "num2",
        "3" => "num3",
        "4" => "num4",
        "5" => "num5",
        "6" => "num6",
        "7" => "num7",
        "8" => "num8",
        "9" => "num9",
        // Named keys with common short spellings.
        "esc" | "escape" => "escape",
        "return" | "enter" => "enter",
        "del" | "delete" => "delete",
        "ins" | "insert" => "insert",
        "pgup" | "pageup" => "pageup",
        "pgdn" | "pgdown" | "pagedown" => "pagedown",
        "up" | "arrowup" => "arrowup",
        "down" | "arrowdown" => "arrowdown",
        "left" | "arrowleft" => "arrowleft",
        "right" | "arrowright" => "arrowright",
        // Everything else (letters, `f11`, `home`, `end`, `space`, `tab`,
        // `backspace`) is already its own egui name.
        _ => return t,
    }
    .to_string()
}

impl Chord {
    /// Parse a combo string such as `"mod+shift+t"`.
    ///
    /// Returns `None` when the combo is unusable: no non-modifier key (`""`,
    /// `"mod"`), or more than one non-modifier key (`"a+b"`). Tokens are trimmed
    /// and lowercased, so `"Mod + Shift + T"` parses like `"mod+shift+t"`.
    /// Rejecting a two-key combo is deliberate — silently honouring only the
    /// last key would make a typo look like a working binding.
    #[must_use]
    pub fn parse(combo: &str) -> Option<Self> {
        let mut chord = Self {
            cmd: false,
            shift: false,
            alt: false,
            key: String::new(),
        };
        let mut key_seen = false;
        for raw in combo.split('+') {
            let token = raw.trim().to_ascii_lowercase();
            if token.is_empty() {
                // An empty split part is either padding (`"mod++"`, the natural
                // way to write Ctrl and `+`) or a stray separator. Treat the
                // FIRST such part in a trailing `++` as the `+` key so
                // `"mod++"` is bindable; otherwise skip it.
                continue;
            }
            match token.as_str() {
                "mod" | "ctrl" | "control" | "cmd" | "command" => chord.cmd = true,
                "shift" => chord.shift = true,
                "alt" | "option" | "opt" => chord.alt = true,
                _ => {
                    if key_seen {
                        return None;
                    }
                    key_seen = true;
                    chord.key = canonical_key_token(&token);
                }
            }
        }
        if !key_seen {
            return None;
        }
        if chord.key == "plus" {
            // `+` already IS the shifted form of `=` — folding Shift in keeps
            // `Ctrl+=` and `Ctrl+Shift+=` on the same binding.
            chord.shift = false;
        }
        Some(chord)
    }

    /// The canonical rendering of this chord (`"mod+alt+shift+key"`, modifiers in
    /// a fixed order). Two combo strings that mean the same thing —
    /// `"shift+mod+c"`, `"ctrl+shift+c"`, `"mod+shift+c"` — share one canonical
    /// form, which is what makes [`Keybindings::validate`] conflict detection
    /// alias-aware and what keeps the dispatcher's matching in step with it.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        if self.cmd {
            out.push_str("mod+");
        }
        if self.alt {
            out.push_str("alt+");
        }
        if self.shift {
            out.push_str("shift+");
        }
        out.push_str(&self.key);
        out
    }

    /// Whether a live key event EXACTLY matches this chord.
    ///
    /// `key` must already be a canonical token (the app maps its key type
    /// through [`canonical_key_token`]). Every modifier must agree — a chord
    /// with `shift == false` does NOT fire while Shift is held, so `mod+t` and
    /// `mod+shift+t` stay distinct actions. The `plus` token ignores Shift on
    /// both sides (see the type docs).
    #[must_use]
    pub fn matches(&self, cmd: bool, shift: bool, alt: bool, key: &str) -> bool {
        if self.key != key {
            return false;
        }
        let shift_matches = self.key == "plus" || self.shift == shift;
        self.cmd == cmd && self.alt == alt && shift_matches
    }
}

/// A problem found in a [`Keybindings`] set by [`Keybindings::validate`].
///
/// The bindings are user-editable, so two actions can end up bound to the SAME
/// combo (only one would ever fire), a binding can be left blank (the action
/// becomes unreachable), or a combo can be unparseable (it looks plausible in
/// the file and simply never fires) — all silently, with no surfacing.
/// `validate` makes these explicit so the settings UI can warn instead of the
/// user wondering why a shortcut "does nothing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindingIssue {
    /// `action` has an empty / whitespace-only combo — it can never trigger.
    Empty { action: &'static str },
    /// `action`'s combo cannot be parsed into a chord (no key, or more than one
    /// key — e.g. `"mod"` alone or `"a+b"`), so the action is unreachable.
    Invalid {
        /// The action whose combo is unparseable.
        action: &'static str,
        /// The offending combo, verbatim, so the user can find it in the file.
        combo: String,
    },
    /// `actions` (≥2) are all bound to the same canonical `combo` — they
    /// collide; at most one can win.
    Conflict {
        /// The shared canonical combo.
        combo: String,
        /// Every action bound to it, in declaration order.
        actions: Vec<&'static str>,
    },
}

impl KeybindingIssue {
    /// A human-readable, settings-surfaceable description of the issue.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            KeybindingIssue::Empty { action } => {
                format!("'{action}' has no key bound — it cannot be triggered")
            }
            KeybindingIssue::Invalid { action, combo } => {
                format!("'{action}' has an unreadable key combo '{combo}' — it cannot be triggered")
            }
            KeybindingIssue::Conflict { combo, actions } => {
                format!(
                    "'{combo}' is bound to multiple actions: {}",
                    actions.join(", ")
                )
            }
        }
    }
}

/// User-rebindable key bindings (action name -> key combo string).
///
/// Every field's default is the combo the egui shell hard-wired before the
/// dispatcher landed, so the default keymap is behaviour-identical to the
/// previous release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keybindings {
    /// Copy the selection to the clipboard.
    pub copy: String,
    /// Paste from the clipboard.
    pub paste: String,
    /// Open a new tab.
    pub new_tab: String,
    /// Close the current tab.
    pub close_tab: String,
    /// Switch to the next tab.
    pub next_tab: String,
    /// Split the focused pane to the right.
    pub split_right: String,
    /// Split the focused pane downward.
    pub split_down: String,
    /// Open the in-buffer find / search overlay.
    pub search: String,
    /// Open the command palette.
    pub command_palette: String,
    /// Toggle the command-history quick-run sidebar.
    pub history_sidebar: String,
    /// Increase the font size.
    pub increase_font: String,
    /// Decrease the font size.
    pub decrease_font: String,
    /// Reset the font size to the built-in default.
    pub reset_font: String,
    /// Toggle zoom on the focused pane.
    pub zoom_pane: String,
    /// Rebuild the layout as a uniform, equal-sized grid.
    pub equalize_panes: String,
    /// Flip the pane shell between the grid and tab layouts.
    pub toggle_view_mode: String,
    /// Open / close the settings window.
    pub settings: String,
    /// Toggle borderless OS fullscreen.
    pub fullscreen: String,
    /// Clear the focused pane's scrollback.
    pub clear_scrollback: String,
    /// Copy the focused pane's whole buffer to the clipboard.
    pub copy_all: String,
    /// Scroll the focused pane to the oldest retained line.
    pub scroll_to_top: String,
    /// Scroll the focused pane back to live output.
    pub scroll_to_bottom: String,
    /// Re-open the most recently closed pane, in the directory it was in.
    pub reopen_closed_tab: String,
}

impl Default for Keybindings {
    fn default() -> Self {
        // Each combo is the EXACT chord the egui shell hard-wired before the
        // dispatcher landed — reproducing today's behaviour with zero change.
        // `mod` = the platform command modifier (Ctrl / Cmd).
        Keybindings {
            copy: "mod+shift+c".into(),
            paste: "mod+shift+v".into(),
            new_tab: "mod+shift+t".into(),
            close_tab: "mod+shift+w".into(),
            next_tab: "mod+shift+]".into(),
            split_right: "mod+shift+d".into(),
            split_down: "mod+shift+e".into(),
            search: "mod+shift+f".into(),
            command_palette: "mod+shift+p".into(),
            history_sidebar: "mod+shift+h".into(),
            increase_font: "mod+plus".into(),
            decrease_font: "mod+minus".into(),
            reset_font: "mod+0".into(),
            zoom_pane: "mod+shift+z".into(),
            equalize_panes: "mod+shift+g".into(),
            toggle_view_mode: "mod+shift+b".into(),
            settings: "mod+,".into(),
            fullscreen: "f11".into(),
            clear_scrollback: "mod+shift+k".into(),
            copy_all: "mod+shift+a".into(),
            scroll_to_top: "mod+shift+home".into(),
            scroll_to_bottom: "mod+shift+end".into(),
            // NOT `mod+shift+t` (the browser/VS Code convention for "reopen
            // closed"): that chord is already `new_tab` here, matching Windows
            // Terminal, and moving it would break the muscle memory of every
            // existing user. `u` is free in the whole shipped keymap and reads
            // as "undo close".
            reopen_closed_tab: "mod+shift+u".into(),
        }
    }
}

/// The number of bindings in the schema — the length both [`Keybindings::entries`]
/// and [`Keybindings::entries_mut`] return.
pub const BINDING_COUNT: usize = 23;

impl Keybindings {
    /// Every (action-name, combo) pair, in a stable declaration order. The
    /// single source of truth [`Keybindings::validate`], the dispatcher, and the
    /// settings UI all key off, so a new binding is covered by adding ONE line
    /// here (and its mirror in [`Keybindings::entries_mut`]).
    #[must_use]
    pub fn entries(&self) -> [(&'static str, &str); BINDING_COUNT] {
        [
            ("copy", &self.copy),
            ("paste", &self.paste),
            ("new_tab", &self.new_tab),
            ("close_tab", &self.close_tab),
            ("next_tab", &self.next_tab),
            ("split_right", &self.split_right),
            ("split_down", &self.split_down),
            ("search", &self.search),
            ("command_palette", &self.command_palette),
            ("history_sidebar", &self.history_sidebar),
            ("increase_font", &self.increase_font),
            ("decrease_font", &self.decrease_font),
            ("reset_font", &self.reset_font),
            ("zoom_pane", &self.zoom_pane),
            ("equalize_panes", &self.equalize_panes),
            ("toggle_view_mode", &self.toggle_view_mode),
            ("settings", &self.settings),
            ("fullscreen", &self.fullscreen),
            ("clear_scrollback", &self.clear_scrollback),
            ("copy_all", &self.copy_all),
            ("scroll_to_top", &self.scroll_to_top),
            ("scroll_to_bottom", &self.scroll_to_bottom),
            ("reopen_closed_tab", &self.reopen_closed_tab),
        ]
    }

    /// Every (action-name, MUTABLE combo) pair, in the SAME declaration order as
    /// [`Keybindings::entries`].
    ///
    /// The write half of `entries`, and the reason the settings UI renders one
    /// editable row per binding generically instead of hand-listing every field
    /// (a list that would silently fall behind the schema).
    /// `entries_mut_matches_entries` pins the two orders together.
    #[must_use]
    pub fn entries_mut(&mut self) -> [(&'static str, &mut String); BINDING_COUNT] {
        [
            ("copy", &mut self.copy),
            ("paste", &mut self.paste),
            ("new_tab", &mut self.new_tab),
            ("close_tab", &mut self.close_tab),
            ("next_tab", &mut self.next_tab),
            ("split_right", &mut self.split_right),
            ("split_down", &mut self.split_down),
            ("search", &mut self.search),
            ("command_palette", &mut self.command_palette),
            ("history_sidebar", &mut self.history_sidebar),
            ("increase_font", &mut self.increase_font),
            ("decrease_font", &mut self.decrease_font),
            ("reset_font", &mut self.reset_font),
            ("zoom_pane", &mut self.zoom_pane),
            ("equalize_panes", &mut self.equalize_panes),
            ("toggle_view_mode", &mut self.toggle_view_mode),
            ("settings", &mut self.settings),
            ("fullscreen", &mut self.fullscreen),
            ("clear_scrollback", &mut self.clear_scrollback),
            ("copy_all", &mut self.copy_all),
            ("scroll_to_top", &mut self.scroll_to_top),
            ("scroll_to_bottom", &mut self.scroll_to_bottom),
            ("reopen_closed_tab", &mut self.reopen_closed_tab),
        ]
    }

    /// The parsed chord bound to `action`, or `None` when the binding is blank
    /// or unparseable (the action is then unreachable by keyboard — surfaced by
    /// [`Keybindings::validate`]).
    #[must_use]
    pub fn chord(&self, action: &str) -> Option<Chord> {
        self.entries()
            .iter()
            .find(|(name, _)| *name == action)
            .and_then(|(_, combo)| Chord::parse(combo))
    }

    /// Detect keybinding issues: blank bindings (unreachable actions),
    /// unparseable combos, and combos bound to more than one action
    /// (collisions). Returns an empty `Vec` when the set is clean — the default
    /// set is clean by construction.
    ///
    /// Collisions are grouped by [`Chord::canonical`], so a conflict written
    /// with different modifier aliases (`"ctrl+shift+c"` vs `"mod+shift+c"`) is
    /// detected. Pure + order-deterministic (empties/invalids first in
    /// declaration order, then conflicts sorted by canonical combo) so the
    /// settings surfacing is stable frame-to-frame.
    #[must_use]
    pub fn validate(&self) -> Vec<KeybindingIssue> {
        let entries = self.entries();
        let mut issues = Vec::new();

        // Unreachable actions: a blank combo, or one that cannot parse into a
        // chord. Both would otherwise fail silently.
        for (name, combo) in entries.iter() {
            if combo.trim().is_empty() {
                issues.push(KeybindingIssue::Empty { action: name });
            } else if Chord::parse(combo).is_none() {
                issues.push(KeybindingIssue::Invalid {
                    action: name,
                    combo: (*combo).to_string(),
                });
            }
        }

        // Collisions: group parseable bindings by their CANONICAL chord, so two
        // spellings of one physical chord land in the same group.
        let mut groups: Vec<(String, Vec<&'static str>)> = Vec::new();
        for (name, combo) in entries.iter() {
            let Some(canon) = Chord::parse(combo).map(|c| c.canonical()) else {
                continue;
            };
            if let Some(slot) = groups.iter_mut().find(|(c, _)| *c == canon) {
                slot.1.push(name);
            } else {
                groups.push((canon, vec![name]));
            }
        }
        groups.sort_by(|a, b| a.0.cmp(&b.0));
        for (combo, actions) in groups {
            if actions.len() > 1 {
                issues.push(KeybindingIssue::Conflict { combo, actions });
            }
        }

        issues
    }
}

/// The human-readable label for a binding's action name, for the settings rows.
/// An unknown name falls back to the raw field name so a newly-added binding is
/// never rendered blank.
#[must_use]
pub fn action_label(action: &str) -> &str {
    match action {
        "copy" => "Copy selection",
        "paste" => "Paste",
        "new_tab" => "New tab",
        "close_tab" => "Close pane",
        "next_tab" => "Focus next pane",
        "split_right" => "Split right",
        "split_down" => "Split down",
        "search" => "Find in terminal",
        "command_palette" => "Command palette",
        "history_sidebar" => "Command-history sidebar",
        "increase_font" => "Increase font size",
        "decrease_font" => "Decrease font size",
        "reset_font" => "Reset font size",
        "zoom_pane" => "Zoom pane",
        "equalize_panes" => "Equalize panes",
        "toggle_view_mode" => "Toggle grid / tabs view",
        "settings" => "Settings",
        "fullscreen" => "Fullscreen",
        "clear_scrollback" => "Clear scrollback",
        "copy_all" => "Copy everything",
        "scroll_to_top" => "Scroll to top",
        "scroll_to_bottom" => "Scroll to bottom",
        "reopen_closed_tab" => "Reopen closed pane",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keybindings_have_no_issues() {
        // The shipped default set is clean by construction: no action is blank,
        // every combo parses, and no two actions share a canonical chord.
        let issues = Keybindings::default().validate();
        assert!(
            issues.is_empty(),
            "the default keymap must be issue-free: {issues:?}"
        );
    }

    #[test]
    fn entries_mut_matches_entries() {
        // The read and write halves must agree on NAMES and ORDER, or a settings
        // row renders one action's label over another action's combo.
        let mut kb = Keybindings::default();
        let read: Vec<(String, String)> = kb
            .entries()
            .iter()
            .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
            .collect();
        let write: Vec<(String, String)> = kb
            .entries_mut()
            .iter()
            .map(|(n, v)| ((*n).to_string(), v.to_string()))
            .collect();
        assert_eq!(read, write);
    }

    #[test]
    fn parse_folds_modifier_aliases_onto_one_canonical_form() {
        // Every spelling of "the command modifier + Shift + C" must canonicalize
        // identically — the property `validate`'s conflict detection rests on.
        let forms = [
            "mod+shift+c",
            "ctrl+shift+c",
            "shift+Ctrl+C",
            "Command+Shift+c",
            "cmd + shift + c",
        ];
        let canon: Vec<String> = forms
            .iter()
            .map(|f| Chord::parse(f).expect("parses").canonical())
            .collect();
        for c in &canon {
            assert_eq!(c, "mod+shift+c", "all aliases fold to one form: {canon:?}");
        }
    }

    #[test]
    fn parse_rejects_unusable_combos() {
        assert_eq!(Chord::parse(""), None, "empty combo has no key");
        assert_eq!(Chord::parse("   "), None, "blank combo has no key");
        assert_eq!(Chord::parse("mod"), None, "modifier-only combo has no key");
        assert_eq!(Chord::parse("mod+shift"), None, "modifiers only");
        assert_eq!(
            Chord::parse("mod+a+b"),
            None,
            "two non-modifier keys is not an expressible chord"
        );
    }

    #[test]
    fn key_tokens_fold_friendly_spellings_onto_the_egui_names() {
        assert_eq!(canonical_key_token("]"), "closebracket");
        assert_eq!(canonical_key_token("["), "openbracket");
        assert_eq!(canonical_key_token(","), "comma");
        assert_eq!(canonical_key_token("0"), "num0");
        assert_eq!(canonical_key_token("esc"), "escape");
        assert_eq!(canonical_key_token("PgUp"), "pageup");
        assert_eq!(canonical_key_token("Up"), "arrowup");
        assert_eq!(canonical_key_token("F11"), "f11");
        // Unknown tokens pass through lowercased, so a key this table has not
        // heard of is still bindable by its egui name.
        assert_eq!(canonical_key_token("Zoom"), "zoom");
    }

    /// The canonical token for each punctuation arm, with every alias that must
    /// fold onto it — the GLYPH first, because the glyph is the only spelling
    /// that actually proves the arm is there.
    const PUNCTUATION_ARMS: [(&str, &[&str]); 6] = [
        ("period", &[".", "period", "dot"]),
        ("slash", &["/", "slash"]),
        ("backslash", &["\\", "backslash"]),
        ("semicolon", &[";", "semicolon"]),
        ("quote", &["'", "quote", "apostrophe"]),
        ("backtick", &["`", "backtick", "grave"]),
    ];

    #[test]
    fn punctuation_glyphs_and_their_names_fold_onto_one_token() {
        // Asserting only the name that already EQUALS its token (`"slash"` ->
        // `"slash"`) proves nothing: the `_ =>` fallback returns the input
        // lowercased, so that assertion still holds with the whole arm deleted —
        // while `Ctrl+/` typed as the glyph would quietly stop matching the
        // binding spelled `mod+slash`. The glyph and every alias are therefore
        // asserted together, so no single alias can carry the test on its own.
        for (canonical, aliases) in PUNCTUATION_ARMS {
            for alias in aliases {
                assert_eq!(
                    canonical_key_token(alias),
                    canonical,
                    "{alias:?} must canonicalize to {canonical:?}"
                );
                // Canonicalization is case-insensitive (a glyph is unchanged).
                assert_eq!(
                    canonical_key_token(&alias.to_ascii_uppercase()),
                    canonical,
                    "{alias:?} must canonicalize to {canonical:?} in any case"
                );
                // And with the surrounding whitespace a TOML author leaves in.
                assert_eq!(
                    canonical_key_token(&format!("  {alias} ")),
                    canonical,
                    "{alias:?} must canonicalize to {canonical:?} when padded"
                );
            }
        }
    }

    /// Every DIGIT and NAMED-KEY arm of `canonical_key_token`, with each alias
    /// that must fold onto it.
    ///
    /// The punctuation arms above are covered by their own table; these are the
    /// rest, and they were the surviving mutants. The reason they survive a
    /// spot-check is the `_ =>` fallback: it returns the input lowercased, so
    /// `canonical_key_token("pagedown") == "pagedown"` holds with the whole
    /// `"pgdn" | "pgdown" | "pagedown"` arm deleted. Only the SHORT spellings —
    /// `pgdn`, `del`, `ins`, `return`, `up` — actually prove the arm exists,
    /// and only the digits prove the `num*` prefix is applied at all.
    const NAMED_KEY_ARMS: [(&str, &[&str]); 19] = [
        ("num0", &["0"]),
        ("num1", &["1"]),
        ("num2", &["2"]),
        ("num3", &["3"]),
        ("num4", &["4"]),
        ("num5", &["5"]),
        ("num6", &["6"]),
        ("num7", &["7"]),
        ("num8", &["8"]),
        ("num9", &["9"]),
        ("escape", &["esc", "escape"]),
        ("enter", &["return", "enter"]),
        ("delete", &["del", "delete"]),
        ("insert", &["ins", "insert"]),
        ("pageup", &["pgup", "pageup"]),
        ("pagedown", &["pgdn", "pgdown", "pagedown"]),
        ("arrowup", &["up", "arrowup"]),
        ("arrowdown", &["down", "arrowdown"]),
        ("arrowleft", &["left", "arrowleft"]),
    ];

    #[test]
    fn every_digit_and_named_key_alias_folds_onto_its_egui_name() {
        for (canonical, aliases) in NAMED_KEY_ARMS {
            for alias in aliases {
                assert_eq!(
                    canonical_key_token(alias),
                    canonical,
                    "{alias:?} must canonicalize to {canonical:?}"
                );
                assert_eq!(
                    canonical_key_token(&alias.to_ascii_uppercase()),
                    canonical,
                    "{alias:?} must canonicalize to {canonical:?} in any case"
                );
                assert_eq!(
                    canonical_key_token(&format!("  {alias} ")),
                    canonical,
                    "{alias:?} must canonicalize to {canonical:?} when padded"
                );
            }
        }
        // `right` is the one arm the table above cannot hold (a 20th entry would
        // exceed the array's declared length); assert it directly so no arrow is
        // left uncovered.
        assert_eq!(canonical_key_token("right"), "arrowright");
        assert_eq!(canonical_key_token("arrowright"), "arrowright");

        // A digit that is NOT in the table must still pass through unprefixed,
        // so the `num*` assertions above cannot be satisfied by a blanket
        // "prefix every short token with num".
        assert_eq!(canonical_key_token("a"), "a");
        assert_eq!(canonical_key_token("f1"), "f1");
    }

    /// `alt`, `option` and `opt` must all set the SAME modifier.
    ///
    /// `opt`/`option` are the macOS spellings; a mutant dropping either arm
    /// sends them to the `_ =>` key branch, where they become the chord's KEY.
    /// The binding then looks correct in the TOML and never fires — and because
    /// the key differs, `validate` does not report it as a conflict either.
    #[test]
    fn every_alt_spelling_sets_the_alt_modifier() {
        let base = Chord::parse("alt+t").expect("alt+t parses");
        assert!(base.alt && !base.cmd && !base.shift);
        assert_eq!(base.key, "t");
        for spelling in ["alt", "option", "opt"] {
            let c = Chord::parse(&format!("{spelling}+t"))
                .unwrap_or_else(|| panic!("{spelling}+t must parse"));
            assert_eq!(
                c.canonical(),
                base.canonical(),
                "{spelling:?} must set the ALT modifier, not become the key"
            );
            assert_eq!(c.key, "t", "{spelling:?} must not be taken as the key");
        }
    }

    /// `action_label` maps known actions and falls back to the RAW name.
    ///
    /// The fallback is what stops a newly-added binding rendering blank in
    /// settings, and it is exactly what a mutant returning `""` (or any fixed
    /// string) breaks — invisibly, because every mapped action still looks fine.
    #[test]
    fn action_label_maps_known_actions_and_falls_back_to_the_raw_name() {
        assert_eq!(action_label("copy"), "Copy selection");
        assert_eq!(action_label("reopen_closed_tab"), "Reopen closed pane");
        // Unknown -> the raw name, never blank and never a placeholder.
        assert_eq!(action_label("a_brand_new_action"), "a_brand_new_action");
        assert!(
            !action_label("a_brand_new_action").is_empty(),
            "an unmapped action must never render blank"
        );
        // Every SHIPPED action must have a real label, i.e. one that is not just
        // the raw field name echoed back.
        for (name, _) in Keybindings::default().entries() {
            let label = action_label(&name);
            assert!(!label.is_empty(), "{name} has a blank label");
            assert_ne!(
                label, name,
                "{name} is a shipped binding and must have a human-readable \
                 label, not the raw field name"
            );
        }
    }

    #[test]
    fn punctuation_chords_parse_match_and_agree_across_spellings() {
        // End-to-end for the bindings a user actually writes (`mod+/`, `mod+;`):
        // the glyph form and the named form must be the SAME chord, or one of
        // them is a binding that looks right in the file and never fires — and
        // `validate`'s collision detection would not see them as one chord.
        for (canonical, aliases) in PUNCTUATION_ARMS {
            let named = Chord::parse(&format!("mod+{canonical}"))
                .unwrap_or_else(|| panic!("mod+{canonical} must parse"));
            assert_eq!(named.key, canonical);
            assert_eq!(named.canonical(), format!("mod+{canonical}"));
            assert!(
                named.matches(true, false, false, canonical),
                "mod+{canonical} must fire for its own token"
            );
            assert!(
                !named.matches(true, true, false, canonical),
                "mod+{canonical} must stay distinct from the Shift variant"
            );
            for alias in aliases {
                let chord = Chord::parse(&format!("mod+{alias}"))
                    .unwrap_or_else(|| panic!("mod+{alias} must parse"));
                assert_eq!(
                    chord, named,
                    "mod+{alias} and mod+{canonical} must be one binding"
                );
            }
        }
    }

    #[test]
    fn the_plus_key_collapses_equals_and_folds_shift() {
        // `+` is Shift+`=` on most layouts, so both spellings and both shift
        // states are ONE binding — otherwise "Ctrl and +" is unbindable.
        let plus = Chord::parse("mod+plus").expect("parses");
        assert_eq!(plus.key, "plus");
        assert!(!plus.shift, "shift is folded into the `+` glyph");
        assert_eq!(Chord::parse("mod+=").expect("parses"), plus);
        assert_eq!(Chord::parse("mod+shift+plus").expect("parses"), plus);
        assert!(plus.matches(true, false, false, "plus"), "Ctrl+= fires");
        assert!(
            plus.matches(true, true, false, "plus"),
            "Ctrl+Shift+= fires"
        );
    }

    #[test]
    fn matching_is_exact_on_modifiers() {
        let c = Chord::parse("mod+shift+t").expect("parses");
        assert!(c.matches(true, true, false, "t"), "the exact chord fires");
        assert!(!c.matches(true, false, false, "t"), "Shift is required");
        assert!(
            !c.matches(false, true, false, "t"),
            "the cmd modifier is required"
        );
        assert!(
            !c.matches(true, true, true, "t"),
            "an extra Alt must NOT fire"
        );
        assert!(
            !c.matches(true, true, false, "w"),
            "a different key must NOT fire"
        );

        // A no-modifier chord must not fire while a modifier is held.
        let f11 = Chord::parse("f11").expect("parses");
        assert!(f11.matches(false, false, false, "f11"));
        assert!(!f11.matches(true, false, false, "f11"), "bare F11 only");
    }

    #[test]
    fn validate_detects_a_collision_written_with_modifier_aliases() {
        // The regression this rewrite exists for: the OLD token-sorting
        // normalizer compared "c+ctrl+shift" against "c+mod+shift" and reported
        // NO conflict, so two actions could silently share one physical chord.
        let kb = Keybindings {
            paste: "ctrl+shift+c".into(), // same physical chord as `copy`
            ..Default::default()
        };
        let issues = kb.validate();
        let conflict = issues
            .iter()
            .find(|i| matches!(i, KeybindingIssue::Conflict { .. }))
            .unwrap_or_else(|| panic!("an alias-spelled collision must be reported: {issues:?}"));
        match conflict {
            KeybindingIssue::Conflict { combo, actions } => {
                assert_eq!(combo, "mod+shift+c", "grouped by the canonical form");
                assert!(actions.contains(&"copy") && actions.contains(&"paste"));
            }
            other => panic!("expected a Conflict, got {other:?}"),
        }
    }

    #[test]
    fn validate_detects_a_duplicate_combo_collision() {
        // Order-insensitive spelling of the same chord still collides.
        let kb = Keybindings {
            paste: "shift+mod+c".into(),
            ..Default::default()
        };
        let issues = kb.validate();
        assert_eq!(issues.len(), 1, "exactly one conflict expected: {issues:?}");
        match &issues[0] {
            KeybindingIssue::Conflict { combo, actions } => {
                assert_eq!(combo, "mod+shift+c");
                assert!(actions.contains(&"copy") && actions.contains(&"paste"));
            }
            other => panic!("expected a Conflict, got {other:?}"),
        }
    }

    #[test]
    fn validate_detects_an_empty_binding() {
        let kb = Keybindings {
            search: "   ".into(), // whitespace-only → unreachable
            ..Default::default()
        };
        let issues = kb.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, KeybindingIssue::Empty { action } if *action == "search")),
            "an empty binding must be reported: {issues:?}"
        );
    }

    #[test]
    fn validate_detects_an_unparseable_binding() {
        // A combo that LOOKS plausible in the file but can never fire.
        let kb = Keybindings {
            search: "mod+a+b".into(),
            ..Default::default()
        };
        let issues = kb.validate();
        assert!(
            issues.iter().any(
                |i| matches!(i, KeybindingIssue::Invalid { action, .. } if *action == "search")
            ),
            "an unparseable binding must be reported: {issues:?}"
        );
    }

    #[test]
    fn keybinding_issue_messages_are_human_readable() {
        let empty = KeybindingIssue::Empty { action: "copy" };
        assert!(empty.message().contains("copy"));
        let invalid = KeybindingIssue::Invalid {
            action: "search",
            combo: "mod+a+b".into(),
        };
        let m = invalid.message();
        assert!(m.contains("search") && m.contains("mod+a+b"));
        let conflict = KeybindingIssue::Conflict {
            combo: "mod+shift+c".into(),
            actions: vec!["copy", "paste"],
        };
        let m = conflict.message();
        assert!(m.contains("copy") && m.contains("paste") && m.contains("mod+shift+c"));
    }

    #[test]
    fn chord_lookup_resolves_by_action_name() {
        let kb = Keybindings::default();
        assert_eq!(
            kb.chord("command_palette").map(|c| c.canonical()),
            Some("mod+shift+p".to_string())
        );
        assert_eq!(kb.chord("no_such_action"), None);
    }

    #[test]
    fn every_binding_has_a_label() {
        // A binding added to the schema without a label would render as a raw
        // field name in settings — catch it here instead of in the UI.
        for (name, _) in Keybindings::default().entries() {
            assert_ne!(
                action_label(name),
                name,
                "binding '{name}' has no human-readable label"
            );
        }
    }
}
