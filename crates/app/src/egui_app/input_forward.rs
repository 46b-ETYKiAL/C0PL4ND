//! Forwarding a frame's keyboard and paste input to the FOCUSED pane's PTY.
//!
//! Lifted verbatim out of `egui_app/mod.rs`. This is the "typing reaches the
//! PTY" half of the round-trip whose other half is `pane_body` — it translates
//! this frame's egui events into terminal bytes through the SHARED core key
//! encoder, honouring the kitty keyboard protocol flags the focused program
//! negotiated, and consumes Tab/arrows so egui cannot steal them for widget
//! navigation.
//!
//! It is one concern with one call site (`frame_tick` step 0b) and no state of
//! its own beyond `self`, so it moves as a unit. Kept as an inherent method (an
//! `impl` block here rather than a bare `fn`) so that call site keeps its
//! original `self.forward_input_to_focused(` spelling.
//!
//! Distinct from `actions`, which resolves CHORDS to app actions: by the time
//! input reaches here it is destined for the child process, not for the shell.

use super::*;

impl C0pl4ndApp {
    /// Forward this frame's keyboard + paste events to the FOCUSED pane's PTY,
    /// using the SHARED core key encoder. Consumes Tab/arrows so egui does not
    /// steal them for widget navigation (recon dossier §5.1). Called once per
    /// frame. Returns the bytes forwarded (for tests that drive the real input
    /// path and assert what reached the PTY).
    pub(super) fn forward_input_to_focused(&mut self, ctx: &egui::Context) -> Vec<u8> {
        use c0pl4nd_core::term::{KeyEventKind, KeyModifiers, LogicalKey};

        // When the focused program negotiated the kitty keyboard protocol with
        // REPORT-EVENT-TYPES (bit2), ALSO forward key RELEASE and REPEAT events;
        // otherwise keep the legacy press-only behavior. Read the flag once.
        let report_event_types = self
            .terms
            .get(&self.focused_pane)
            .map(|t| t.kitty_reports_event_types())
            .unwrap_or(false);

        // Collect input events under the immutable input borrow first, THEN
        // mutate the PTY (egui forbids re-entrant input borrows).
        let mut keys: Vec<(LogicalKey, KeyModifiers, KeyEventKind)> = Vec::new();
        let mut pastes: Vec<String> = Vec::new();
        // The pre-edit (composition) string to store on `self` after the input
        // borrow closes. `Some(Some(s))` = set/replace the preedit; `Some(None)`
        // = clear it; `None` = no IME event this frame, leave it as-is (F3-1).
        let mut ime_update: Option<Option<String>> = None;
        // Ctrl/Cmd+Shift+Arrow requests a directional pane-focus move; captured
        // here and applied after the forward loop so the arrow is NOT also sent to
        // the PTY as a cursor sequence.
        let mut dir_focus: Option<Direction> = None;
        ctx.input(|i| {
            let mods = KeyModifiers {
                ctrl: i.modifiers.ctrl,
                alt: i.modifiers.alt,
                shift: i.modifiers.shift,
                logo: i.modifiers.command || i.modifiers.mac_cmd,
            };
            for ev in &i.events {
                match ev {
                    // Composed text (printable chars, IME). Skip when Ctrl/logo
                    // is held so a shortcut chord (Ctrl+C etc.) is handled by the
                    // Key event below, not double-sent as raw text.
                    egui::Event::Text(t) if !mods.ctrl && !mods.logo => {
                        keys.push((LogicalKey::Text(t.clone()), mods, KeyEventKind::Press));
                    }
                    // IME composition (F3-1). When an IME (CJK / complex-script)
                    // is active, egui routes composed text through `Event::Ime`
                    // INSTEAD of `Event::Text`, so without this arm CJK input is
                    // impossible. The OS candidate-window position is set
                    // separately each frame via `ctx.output_mut(|o| o.ime = ...)`
                    // in `render_pane_body` (so the popup tracks the caret).
                    egui::Event::Ime(ime) => match ime {
                        // Final composed result: send it to the PTY exactly as
                        // ordinary `Event::Text` would, and clear the pre-edit.
                        // Commit text is final and MUST reach the shell
                        // regardless of modifier state (an IME commit is not a
                        // shortcut chord), so — unlike `Event::Text` above — it
                        // is forwarded even while Ctrl/logo is held.
                        egui::ImeEvent::Commit(text) => {
                            if !text.is_empty() {
                                keys.push((
                                    LogicalKey::Text(text.clone()),
                                    mods,
                                    KeyEventKind::Press,
                                ));
                            }
                            ime_update = Some(None);
                        }
                        // In-progress candidate text: buffer for DISPLAY only —
                        // never sent to the PTY. An empty pre-edit ends the
                        // current composition without committing.
                        egui::ImeEvent::Preedit(text) => {
                            ime_update = Some(if text.is_empty() {
                                None
                            } else {
                                Some(text.clone())
                            });
                        }
                        // Composition session boundaries: clear any stale
                        // pre-edit so a cancelled composition leaves nothing
                        // painted at the cursor.
                        egui::ImeEvent::Enabled | egui::ImeEvent::Disabled => {
                            ime_update = Some(None);
                        }
                    },
                    egui::Event::Paste(s) => pastes.push(s.clone()),
                    egui::Event::Key {
                        key,
                        pressed,
                        repeat,
                        modifiers,
                        ..
                    } => {
                        // Press-only by default; with REPORT-EVENT-TYPES also
                        // forward releases and distinguish repeats.
                        if !*pressed && !report_event_types {
                            continue;
                        }
                        let kind = if !*pressed {
                            KeyEventKind::Release
                        } else if *repeat {
                            KeyEventKind::Repeat
                        } else {
                            KeyEventKind::Press
                        };
                        // Ctrl/Cmd+Shift+Arrow moves keyboard focus to the
                        // adjacent pane instead of sending a cursor sequence to
                        // the PTY. Capture the direction and skip forwarding the
                        // arrow (the ctrl-OR-command discipline used everywhere).
                        if *pressed
                            && (modifiers.ctrl || modifiers.command)
                            && modifiers.shift
                            && !modifiers.alt
                        {
                            let d = match key {
                                egui::Key::ArrowLeft => Some(Direction::Left),
                                egui::Key::ArrowRight => Some(Direction::Right),
                                egui::Key::ArrowUp => Some(Direction::Up),
                                egui::Key::ArrowDown => Some(Direction::Down),
                                _ => None,
                            };
                            if let Some(d) = d {
                                dir_focus = Some(d);
                                continue;
                            }
                        }
                        let m = KeyModifiers {
                            ctrl: modifiers.ctrl,
                            alt: modifiers.alt,
                            shift: modifiers.shift,
                            logo: modifiers.command || modifiers.mac_cmd,
                        };
                        if let Some(lk) = egui_key_to_logical(*key, m) {
                            keys.push((lk, m, kind));
                        }
                    }
                    _ => {}
                }
            }
        });

        // Apply the buffered IME pre-edit change now the input borrow is closed
        // (F3-1). `None` means no IME event this frame — leave the pre-edit as-is
        // so a composition spanning multiple frames is not dropped.
        if let Some(new_preedit) = ime_update {
            self.ime_preedit = new_preedit;
        }

        // Tab/arrows must reach the PTY, not drive egui focus — consume them so
        // egui's built-in navigation does not also act on them.
        ctx.input_mut(|i| {
            for key in [
                egui::Key::Tab,
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
                egui::Key::ArrowLeft,
                egui::Key::ArrowRight,
            ] {
                while i.consume_key(egui::Modifiers::NONE, key) {}
            }
        });

        let mut forwarded: Vec<u8> = Vec::new();
        if let Some(term) = self.terms.get_mut(&self.focused_pane) {
            for (lk, m, kind) in &keys {
                // The common press path goes through the stable `forward_key`
                // wrapper; repeats/releases (kitty REPORT-EVENT-TYPES) take the
                // full event form.
                forwarded.extend(if *kind == KeyEventKind::Press {
                    term.forward_key(lk, *m)
                } else {
                    term.forward_key_event(lk, *m, *kind)
                });
            }
        }

        // Apply a directional pane-focus move AFTER forwarding this frame's other
        // keys (so they reach the previously-focused pane). Uses the pane rects
        // captured during the last grid render (the layout is stable frame to
        // frame); a no-op before the first render or with no neighbour.
        if let Some(dir) = dir_focus {
            self.focus_directional(dir);
        }

        // Paste handling — SECURITY: every paste goes through the core paste-
        // injection guard (`PaneTerm::write_paste` → `Terminal::frame_paste`),
        // NEVER raw `write_bytes`. Two hazards DEFER a paste to the confirm
        // overlay (`pending_paste`) instead of pasting immediately: a MULTI-LINE
        // paste (it executes the instant its embedded newline lands) and an
        // oversized SINGLE-line paste (`paste_warn_bytes` — a hidden-tail
        // command or an accidental whole-file flood, which the newline gate
        // cannot see). Both are decided by ONE core policy function so the two
        // halves can never drift apart. The config read / `pending_paste` set /
        // `terms` borrow are sequential statements so they never alias `self`.
        for s in &pastes {
            if let Some(reason) = c0pl4nd_core::paste_guard::paste_confirm_reason(&self.config, s) {
                self.pending_paste = Some(s.clone());
                self.pending_paste_reason = Some(reason);
            } else if let Some(term) = self.terms.get_mut(&self.focused_pane) {
                term.write_paste(s);
            }
        }

        // Best-effort capture of the line being typed, for the command-palette
        // history (see `c0pl4nd_core::command_history`). Printable text accrues,
        // Backspace pops one char, and Enter commits the line then clears the
        // accumulator. This models printable input + Backspace, NOT full shell
        // line-editing (cursor motion, kill-line) — exactly the contract the
        // `command_history` module documents. Only runs when typing reaches the
        // PTY (the palette routes its own keys away from here), so the history is
        // a record of what the user actually ran, not what they searched for.
        // Ordinary printable characters (incl. Space) arrive as `LogicalKey::Text`
        // (egui delivers them via `Event::Text`); only the special keys below are
        // `LogicalKey` variants, so this captures the full typed line.
        for (lk, _m, kind) in &keys {
            // Releases never accrue typed-line content (a released Enter must not
            // re-commit the line). Presses and repeats do.
            if *kind == KeyEventKind::Release {
                continue;
            }
            match lk {
                LogicalKey::Text(t) => {
                    // Ctrl-letter chords arrive here as a single C0 control byte
                    // (Ctrl+C = 0x03, Ctrl+U = 0x15, …), NOT printable line
                    // content. Ctrl+C / Ctrl+U abort the current line in a shell,
                    // so mirror that by clearing the accumulator; other control
                    // bytes are ignored. Printable text (incl. Space) accrues.
                    if t.chars().all(|c| !c.is_control()) {
                        self.input_line.push_str(t);
                    } else if t == "\u{3}" || t == "\u{15}" {
                        self.input_line.clear();
                    }
                }
                LogicalKey::Backspace => {
                    self.input_line.pop();
                }
                LogicalKey::Enter => {
                    let line = std::mem::take(&mut self.input_line);
                    if self.should_record_history(&line) {
                        // `record` redacts inline secrets (--password=…, API_KEY=…).
                        self.cmd_history.record(line);
                    }
                }
                _ => {}
            }
        }
        forwarded
    }
}
