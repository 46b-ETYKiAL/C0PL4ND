//! Engine-agnostic key → PTY-byte encoding.
//!
//! The terminal must translate a key press into the byte sequence a shell or
//! TUI expects on its PTY. The exact sequences (CSI `ESC [ x`, SS3 `ESC O x`,
//! the `ESC [ n ~` editing-key tilde form, Alt-as-Meta `ESC`-prefixing) are the
//! single source of truth and live HERE, in the UI-free core, so every front
//! end (the legacy winit shell, the modern egui shell, and the headless tests)
//! shares ONE encoding rather than re-deriving it.
//!
//! Each front end maps its native key type onto [`LogicalKey`] and calls
//! [`encode_key`]; the escape sequences are never duplicated. The winit shell's
//! `key_to_bytes` and the egui shell's input-forwarding both delegate here.

/// A platform-neutral key press, abstracted away from any windowing toolkit.
///
/// A front end translates its native key event into one of these (named keys
/// for the special keys it recognises; [`LogicalKey::Text`] for everything that
/// produces composed Unicode, including ordinary characters and IME output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogicalKey {
    /// Composed text (a normal character, an IME commit, a dead-key result).
    /// Carries the already-composed UTF-8 string the front end received.
    Text(String),
    /// Return / Enter — encodes as CR (`\r`).
    Enter,
    /// Backspace — encodes as DEL (`0x7f`), the xterm default.
    Backspace,
    /// Tab — encodes as `\t`.
    Tab,
    /// Escape — encodes as `0x1b`.
    Escape,
    /// Space — encodes as `' '`.
    Space,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Right arrow.
    ArrowRight,
    /// Left arrow.
    ArrowLeft,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Insert key.
    Insert,
    /// Delete (forward-delete) key.
    Delete,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Function key F1–F12 (`n` is 1..=12). Out-of-range `n` encodes nothing.
    Function(u8),
}

/// Active modifier keys at the time of the press.
///
/// All four participate in the encoding: `alt` drives Alt-as-Meta
/// `ESC`-prefixing, and every modifier feeds the xterm modifier parameter
/// (`1 + shift + alt*2 + ctrl*4 + super*8`) used by the `CSI 1 ; <p> <letter>`
/// cursor-key form and the `CSI <n> ; <p> ~` editing-key form.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    /// Control key held.
    pub ctrl: bool,
    /// Alt / Option key held (acts as Meta — `ESC`-prefixes a text key).
    pub alt: bool,
    /// Shift key held.
    pub shift: bool,
    /// Logo / Super / Command key held.
    pub logo: bool,
}

impl KeyModifiers {
    /// No modifiers.
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        logo: false,
    };
}

/// How `Ctrl+Backspace` is encoded on the legacy path.
///
/// xterm's spec-correct answer is `0x08` (`^H`), but `^H` is indistinguishable
/// from `Ctrl+H`, and neither readline nor PSReadLine binds `^H` to
/// backward-kill-word out of the box — so the spec-correct byte often does
/// nothing useful. `0x17` (`^W`) IS bound to backward-kill-word by default in
/// both, so it is offered as an opt-in ergonomic alternative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CtrlBackspace {
    /// `0x08` (`^H`) — the xterm/spec-correct encoding. The default.
    #[default]
    ControlH,
    /// `0x17` (`^W`) — the readline / PSReadLine backward-kill-word binding.
    ControlW,
}

impl CtrlBackspace {
    /// The single byte this encoding emits for `Ctrl+Backspace`.
    fn byte(self) -> u8 {
        match self {
            CtrlBackspace::ControlH => 0x08,
            CtrlBackspace::ControlW => 0x17,
        }
    }
}

/// Tunable knobs for the legacy [`encode_key`] path.
///
/// [`encode_key`] uses [`KeyEncodeOptions::default`]; a front end that surfaces
/// a user preference calls [`encode_key_with`] instead. Adding a field here is
/// source-compatible for every caller that constructs it with `..Default`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyEncodeOptions {
    /// Which byte `Ctrl+Backspace` emits. Defaults to [`CtrlBackspace::ControlH`].
    pub ctrl_backspace: CtrlBackspace,
}

/// The xterm modifier parameter: `1 + shift + alt*2 + ctrl*4 + super*8`.
///
/// Identical to the kitty protocol's modifier value — the two protocols share
/// this formula, so one function serves both (see [`kitty_mod_value`]).
/// A value of `1` means "no modifiers held".
fn xterm_mod_param(mods: KeyModifiers) -> u8 {
    kitty_mod_value(mods)
}

/// Whether any modifier is held (i.e. the xterm modifier parameter is not `1`).
fn any_modifier(mods: KeyModifiers) -> bool {
    xterm_mod_param(mods) != 1
}

/// Encode a logical key into the bytes to write to the PTY.
///
/// Equivalent to [`encode_key_with`] using [`KeyEncodeOptions::default`].
///
/// Honours DECCKM (`app_cursor`): in application-cursor mode the *unmodified*
/// arrow and Home/End keys use SS3 (`ESC O x`) instead of CSI (`ESC [ x`), which
/// is what readline/vim expect. Encodes the function keys (F1–F12) and editing
/// keys (Home/End/Insert/Delete/PageUp/PageDown). Alt acts as Meta: an
/// `Alt`-modified text key is prefixed with `ESC` (the xterm convention behind
/// Alt+B / Alt+F word motion in bash). Returns `None` when the key produces no
/// PTY bytes (e.g. an empty text string, or an out-of-range function key).
///
/// Modified cursor and editing keys carry the xterm modifier parameter, so
/// `Ctrl+Left` is `ESC [ 1 ; 5 D` (the sequence a shell's line editor needs for
/// word-jump) rather than a bare `ESC [ D`.
pub fn encode_key(key: &LogicalKey, app_cursor: bool, mods: KeyModifiers) -> Option<Vec<u8>> {
    encode_key_with(key, app_cursor, mods, KeyEncodeOptions::default())
}

/// [`encode_key`] with explicit [`KeyEncodeOptions`].
///
/// # Modifier encoding
///
/// The modifier parameter is `1 + shift + alt*2 + ctrl*4 + super*8` — so Shift
/// is `2`, Alt `3`, Ctrl `5`, Ctrl+Shift `6`, Ctrl+Alt `7`. When any modifier is
/// held:
///
/// - arrows and Home/End use `CSI 1 ; <param> <letter>` — **even in DECCKM
///   application-cursor mode**, because the SS3 form has no modifier field at
///   all. Unmodified keys keep the exact SS3/CSI behaviour they always had.
/// - Insert/Delete/PageUp/PageDown use `CSI <n> ; <param> ~` (e.g. `Ctrl+Delete`
///   → `ESC [ 3 ; 5 ~`). Unmodified output is byte-identical to before.
/// - `Ctrl+Backspace` emits [`KeyEncodeOptions::ctrl_backspace`]'s byte;
///   `Alt+Backspace` is the Meta form `ESC DEL`; plain Backspace stays `DEL`.
pub fn encode_key_with(
    key: &LogicalKey,
    app_cursor: bool,
    mods: KeyModifiers,
    opts: KeyEncodeOptions,
) -> Option<Vec<u8>> {
    let param = xterm_mod_param(mods);
    let modified = any_modifier(mods);
    // Arrow / Home / End. Modified => the CSI-with-parameter form (the SS3 form
    // cannot carry modifiers, so it is bypassed even under DECCKM). Unmodified
    // => SS3 in application-cursor mode, else CSI.
    let cursor = |c: u8| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{param}{}", c as char).into_bytes()
        } else if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    // `ESC [ <n> ~` editing-key form, gaining a `; <param>` field when modified.
    let tilde = |n: &str| -> Vec<u8> {
        if modified {
            format!("\x1b[{n};{param}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    // The tilde helper is also used for F5–F12, whose modified forms this legacy
    // path deliberately leaves alone (see the `Function` arm below).
    let plain_tilde = |n: &str| -> Vec<u8> { format!("\x1b[{n}~").into_bytes() };
    let base: Option<Vec<u8>> = match key {
        LogicalKey::Enter => Some(vec![b'\r']),
        // Ctrl+Backspace is a distinct byte; Alt+Backspace falls through to the
        // Meta ESC-prefix below (DEL is not ESC, so it gets prefixed).
        LogicalKey::Backspace => Some(if mods.ctrl {
            vec![opts.ctrl_backspace.byte()]
        } else {
            vec![0x7f]
        }),
        LogicalKey::Tab => Some(vec![b'\t']),
        LogicalKey::Escape => Some(vec![0x1b]),
        LogicalKey::Space => Some(vec![b' ']),
        LogicalKey::ArrowUp => Some(cursor(b'A')),
        LogicalKey::ArrowDown => Some(cursor(b'B')),
        LogicalKey::ArrowRight => Some(cursor(b'C')),
        LogicalKey::ArrowLeft => Some(cursor(b'D')),
        LogicalKey::Home => Some(cursor(b'H')),
        LogicalKey::End => Some(cursor(b'F')),
        LogicalKey::Insert => Some(tilde("2")),
        LogicalKey::Delete => Some(tilde("3")),
        LogicalKey::PageUp => Some(tilde("5")),
        LogicalKey::PageDown => Some(tilde("6")),
        // F1–F4 use SS3 (the VT100 PF-key form); F5–F12 use CSI tilde. The
        // function keys keep their UNMODIFIED encoding regardless of modifiers
        // (the kitty path carries modified function keys) — hence `plain_tilde`.
        LogicalKey::Function(n) => match n {
            1 => Some(vec![0x1b, b'O', b'P']),
            2 => Some(vec![0x1b, b'O', b'Q']),
            3 => Some(vec![0x1b, b'O', b'R']),
            4 => Some(vec![0x1b, b'O', b'S']),
            5 => Some(plain_tilde("15")),
            6 => Some(plain_tilde("17")),
            7 => Some(plain_tilde("18")),
            8 => Some(plain_tilde("19")),
            9 => Some(plain_tilde("20")),
            10 => Some(plain_tilde("21")),
            11 => Some(plain_tilde("23")),
            12 => Some(plain_tilde("24")),
            _ => None,
        },
        LogicalKey::Text(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.as_bytes().to_vec())
            }
        }
    };
    // Alt = Meta: prefix ESC for a text-producing key (never double-prefix a
    // sequence that already starts with ESC, e.g. the arrows above).
    match base {
        Some(bytes) if mods.alt && bytes.first() != Some(&0x1b) => {
            let mut v = Vec::with_capacity(bytes.len() + 1);
            v.push(0x1b);
            v.extend_from_slice(&bytes);
            Some(v)
        }
        other => other,
    }
}

/// A key event's transition kind, for the kitty keyboard protocol's
/// REPORT-EVENT-TYPES (bit 2) progressive enhancement. Legacy encoding only
/// ever produces presses; the kitty encoder additionally reports repeats and
/// releases when the running program negotiated bit 2.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyEventKind {
    /// A key was pressed (the only event legacy encoding emits).
    #[default]
    Press,
    /// A key auto-repeated while held.
    Repeat,
    /// A key was released.
    Release,
}

/// Compute the kitty modifier value: `1 + shift + alt*2 + ctrl*4 + super*8`.
/// (super = the logo/command key). A value of `1` means "no modifiers".
fn kitty_mod_value(mods: KeyModifiers) -> u8 {
    1 + (mods.shift as u8) + (mods.alt as u8) * 2 + (mods.ctrl as u8) * 4 + (mods.logo as u8) * 8
}

/// Encode the `<event>` sub-parameter for REPORT-EVENT-TYPES: `1` press
/// (omitted), `2` repeat, `3` release.
fn kitty_event_subparam(kind: KeyEventKind) -> u8 {
    match kind {
        KeyEventKind::Press => 1,
        KeyEventKind::Repeat => 2,
        KeyEventKind::Release => 3,
    }
}

/// Assemble the kitty `CSI <number> ; <mods>[:<event>] u` form. When the
/// modifier value is `1` (no modifiers) and the event is a plain press, the
/// trailing `; <mods>` is omitted to match kitty's canonical minimal output
/// (`CSI 27 u`, not `CSI 27 ; 1 u`). An event sub-param forces the `; <mods>`
/// field to be present (kitty requires a modifier field of at least `1`
/// alongside an event field).
fn kitty_csi_u(
    number: u32,
    mods: KeyModifiers,
    kind: KeyEventKind,
    report_events: bool,
) -> Vec<u8> {
    let modv = kitty_mod_value(mods);
    let emit_event = report_events && kind != KeyEventKind::Press;
    let mut s = format!("\x1b[{number}");
    if modv != 1 || emit_event {
        s.push_str(&format!(";{modv}"));
        if emit_event {
            s.push_str(&format!(":{}", kitty_event_subparam(kind)));
        }
    }
    s.push('u');
    s.into_bytes()
}

/// Assemble the kitty cursor-key `CSI 1 ; <mods>[:<event>] <letter>` form for
/// arrows / Home / End (the CSI-with-modifiers letter form, NOT the `u` form).
fn kitty_csi_letter(
    letter: u8,
    mods: KeyModifiers,
    kind: KeyEventKind,
    report_events: bool,
) -> Vec<u8> {
    let modv = kitty_mod_value(mods);
    let emit_event = report_events && kind != KeyEventKind::Press;
    let mut s = format!("\x1b[1;{modv}");
    if emit_event {
        s.push_str(&format!(":{}", kitty_event_subparam(kind)));
    }
    s.push(letter as char);
    s.into_bytes()
}

/// Assemble the kitty function-key tilde form `CSI <n> ; <mods>[:<event>] ~`
/// (F5–F12 and the editing keys Insert/Delete/PageUp/PageDown).
fn kitty_tilde(n: u32, mods: KeyModifiers, kind: KeyEventKind, report_events: bool) -> Vec<u8> {
    let modv = kitty_mod_value(mods);
    let emit_event = report_events && kind != KeyEventKind::Press;
    let mut s = format!("\x1b[{n};{modv}");
    if emit_event {
        s.push_str(&format!(":{}", kitty_event_subparam(kind)));
    }
    s.push('~');
    s.into_bytes()
}

/// Encode a logical key into kitty-keyboard-protocol (CSI u) bytes.
///
/// This is the progressive-enhancement path: it is consulted ONLY when a
/// running program has negotiated the protocol (the terminal's flags are
/// non-zero). The caller falls back to the legacy [`encode_key`] whenever this
/// returns `None`.
///
/// `flags` is the negotiated bitset (top of the terminal's kitty flag stack):
/// bit1(=1) DISAMBIGUATE, bit2(=2) REPORT-EVENT-TYPES, bit4(=4)
/// report-alternate-keys, bit8(=8) report-all-keys-as-escape-codes, bit16(=16)
/// report-associated-text.
///
/// # Encoder boundary (DEFINED behavior, not a TODO)
///
/// This encoder FULLY encodes the bits that carry the protocol's load-bearing
/// value and falls through (returns `None` → legacy encoding) for the rest:
///
/// - **bit1 DISAMBIGUATE (fully encoded).** Enter / Tab / Backspace / Escape
///   emit their unambiguous CSI-u numbers (`CSI 13 u`, `CSI 9 u`, `CSI 127 u`,
///   `CSI 27 u`) so an app can tell Shift+Enter from Enter, Ctrl+I from Tab,
///   and a lone Esc from the start of an escape sequence. Modified character
///   keys emit `CSI <codepoint> ; <mods> u`.
/// - **bit2 REPORT-EVENT-TYPES (fully encoded).** When set, releases and
///   repeats are encoded with the `:<event>` sub-param; when UNSET, a `Release`
///   returns `None` (silent, matching legacy/disambiguate-only behavior) and a
///   `Repeat` is encoded identically to a `Press`.
/// - **bit16 REPORT-ASSOCIATED-TEXT (encoded for single-codepoint keys).** When
///   set, the associated text codepoint(s) are appended as `; <text>` before
///   the `u`.
/// - **bit4 report-alternate-keys / bit8 report-all-keys-as-escape-codes
///   (fall-through).** These request the shifted/base-layout alternate
///   codepoints (bit4) and that EVERY key — including plain unmodified
///   printable text — be reported as an escape code (bit8). Encoding them
///   correctly requires keyboard-layout data this UI-free core does not carry,
///   so for keys where they would change the output this encoder returns `None`
///   and the legacy text/encoding path is used. The flags are still stored and
///   reported faithfully by the terminal so the negotiation handshake stays
///   honest — the program learns the terminal accepted the flags even though
///   this build encodes the conservative subset.
///
/// Returns `None` (→ legacy fallback) when:
/// - `flags == 0` (protocol not negotiated);
/// - the key is an unmodified cursor / function / editing key with no event to
///   report (the legacy form is already unambiguous — no CSI-u needed);
/// - a `Release` arrives while bit2 is unset;
/// - the key is multi-codepoint text (an IME commit) under a non-all-keys mode
///   (it passes through as text);
/// - an out-of-range function key.
pub fn encode_key_kitty(
    key: &LogicalKey,
    mods: KeyModifiers,
    flags: u8,
    kind: KeyEventKind,
) -> Option<Vec<u8>> {
    if flags == 0 {
        return None;
    }
    let disambiguate = flags & 1 != 0;
    let report_events = flags & 2 != 0;
    let report_text = flags & 16 != 0;

    // Releases are silent unless the program asked for event types.
    if kind == KeyEventKind::Release && !report_events {
        return None;
    }

    let modv = kitty_mod_value(mods);
    let modified = modv != 1;
    let need_event = report_events && kind != KeyEventKind::Press;

    match key {
        // Cursor keys + Home/End: CSI-with-mods letter form. Unmodified with no
        // event → legacy form is unambiguous, so fall through to legacy.
        LogicalKey::ArrowUp => {
            csi_letter_or_fallback(b'A', mods, kind, report_events, modified, need_event)
        }
        LogicalKey::ArrowDown => {
            csi_letter_or_fallback(b'B', mods, kind, report_events, modified, need_event)
        }
        LogicalKey::ArrowRight => {
            csi_letter_or_fallback(b'C', mods, kind, report_events, modified, need_event)
        }
        LogicalKey::ArrowLeft => {
            csi_letter_or_fallback(b'D', mods, kind, report_events, modified, need_event)
        }
        LogicalKey::Home => {
            csi_letter_or_fallback(b'H', mods, kind, report_events, modified, need_event)
        }
        LogicalKey::End => {
            csi_letter_or_fallback(b'F', mods, kind, report_events, modified, need_event)
        }

        // The disambiguation core: Enter / Tab / Backspace / Escape as CSI-u
        // numbers when bit1 is set OR a modifier/event is present. Without
        // disambiguate and unmodified with no event, fall through to legacy.
        LogicalKey::Escape => csi_u_or_fallback(
            27,
            mods,
            kind,
            report_events,
            disambiguate || modified || need_event,
        ),
        LogicalKey::Enter => csi_u_or_fallback(
            13,
            mods,
            kind,
            report_events,
            disambiguate || modified || need_event,
        ),
        LogicalKey::Tab => csi_u_or_fallback(
            9,
            mods,
            kind,
            report_events,
            disambiguate || modified || need_event,
        ),
        LogicalKey::Backspace => csi_u_or_fallback(
            127,
            mods,
            kind,
            report_events,
            disambiguate || modified || need_event,
        ),

        // Editing keys: tilde form when modified or an event must be reported;
        // otherwise the legacy tilde form already disambiguates → fall through.
        LogicalKey::Insert => tilde_or_fallback(2, mods, kind, report_events, modified, need_event),
        LogicalKey::Delete => tilde_or_fallback(3, mods, kind, report_events, modified, need_event),
        LogicalKey::PageUp => tilde_or_fallback(5, mods, kind, report_events, modified, need_event),
        LogicalKey::PageDown => {
            tilde_or_fallback(6, mods, kind, report_events, modified, need_event)
        }

        // Function keys: F1–F4 use the CSI-1-letter P/Q/R/S form; F5–F12 use the
        // tilde form. Unmodified with no event → legacy form is fine, fall
        // through to legacy.
        LogicalKey::Function(n) => {
            if !modified && !need_event {
                return None; // legacy form is unambiguous
            }
            match n {
                1 => Some(kitty_csi_letter(b'P', mods, kind, report_events)),
                2 => Some(kitty_csi_letter(b'Q', mods, kind, report_events)),
                3 => Some(kitty_csi_letter(b'R', mods, kind, report_events)),
                4 => Some(kitty_csi_letter(b'S', mods, kind, report_events)),
                5 => Some(kitty_tilde(15, mods, kind, report_events)),
                6 => Some(kitty_tilde(17, mods, kind, report_events)),
                7 => Some(kitty_tilde(18, mods, kind, report_events)),
                8 => Some(kitty_tilde(19, mods, kind, report_events)),
                9 => Some(kitty_tilde(20, mods, kind, report_events)),
                10 => Some(kitty_tilde(21, mods, kind, report_events)),
                11 => Some(kitty_tilde(23, mods, kind, report_events)),
                12 => Some(kitty_tilde(24, mods, kind, report_events)),
                _ => None,
            }
        }

        // Space is a plain character key (codepoint 32). Only encode CSI-u when
        // modified or an event is reported; otherwise let legacy emit ' '.
        LogicalKey::Space => {
            if !modified && !need_event {
                return None;
            }
            Some(encode_char_key(' ', mods, kind, report_events, report_text))
        }

        // Character / text keys.
        LogicalKey::Text(s) => {
            let mut chars = s.chars();
            let (Some(c), None) = (chars.next(), chars.clone().next()) else {
                // Empty or multi-codepoint (IME commit): pass through as text.
                return None;
            };
            // Single codepoint. Encode CSI-u only when it carries kitty value:
            // a modifier is held, or an event must be reported. Plain unmodified
            // text passes through legacy (bit8 all-keys-as-escape is a documented
            // fall-through — see the boundary note above).
            if !modified && !need_event {
                return None;
            }
            Some(encode_char_key(c, mods, kind, report_events, report_text))
        }
    }
}

/// Encode a single character key in the CSI-u form. The CSI-u *number* is the
/// UNSHIFTED base codepoint (ASCII letters are lowercased so Ctrl+Shift+A and
/// Ctrl+A share base 97, per the kitty spec); when REPORT-ASSOCIATED-TEXT is
/// active the original character's codepoint is appended as the text field.
fn encode_char_key(
    c: char,
    mods: KeyModifiers,
    kind: KeyEventKind,
    report_events: bool,
    report_text: bool,
) -> Vec<u8> {
    let base = if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else {
        c
    };
    let number = base as u32;
    let modv = kitty_mod_value(mods);
    let emit_event = report_events && kind != KeyEventKind::Press;
    let mut s = format!("\x1b[{number}");
    // A text field forces the modifier field to be present.
    if modv != 1 || emit_event || report_text {
        s.push_str(&format!(";{modv}"));
        if emit_event {
            s.push_str(&format!(":{}", kitty_event_subparam(kind)));
        }
    }
    if report_text {
        s.push_str(&format!(";{}", c as u32));
    }
    s.push('u');
    s.into_bytes()
}

/// CSI-u letter (cursor-key) form, or `None` to fall through to legacy when the
/// key is unmodified with no event to report.
fn csi_letter_or_fallback(
    letter: u8,
    mods: KeyModifiers,
    kind: KeyEventKind,
    report_events: bool,
    modified: bool,
    need_event: bool,
) -> Option<Vec<u8>> {
    if !modified && !need_event {
        None
    } else {
        Some(kitty_csi_letter(letter, mods, kind, report_events))
    }
}

/// CSI-u number form, or `None` to fall through to legacy when `emit` is false.
fn csi_u_or_fallback(
    number: u32,
    mods: KeyModifiers,
    kind: KeyEventKind,
    report_events: bool,
    emit: bool,
) -> Option<Vec<u8>> {
    if emit {
        Some(kitty_csi_u(number, mods, kind, report_events))
    } else {
        None
    }
}

/// CSI-u tilde (editing/function-key) form, or `None` to fall through to legacy
/// when the key is unmodified with no event to report.
fn tilde_or_fallback(
    n: u32,
    mods: KeyModifiers,
    kind: KeyEventKind,
    report_events: bool,
    modified: bool,
    need_event: bool,
) -> Option<Vec<u8>> {
    if !modified && !need_event {
        None
    } else {
        Some(kitty_tilde(n, mods, kind, report_events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_encodes_carriage_return() {
        assert_eq!(
            encode_key(&LogicalKey::Enter, false, KeyModifiers::NONE),
            Some(vec![b'\r'])
        );
    }

    #[test]
    fn text_passes_through_as_utf8() {
        assert_eq!(
            encode_key(&LogicalKey::Text("hi".into()), false, KeyModifiers::NONE),
            Some(b"hi".to_vec())
        );
    }

    #[test]
    fn empty_text_encodes_nothing() {
        assert_eq!(
            encode_key(&LogicalKey::Text(String::new()), false, KeyModifiers::NONE),
            None
        );
    }

    #[test]
    fn arrow_up_csi_in_normal_mode_ss3_in_app_mode() {
        assert_eq!(
            encode_key(&LogicalKey::ArrowUp, false, KeyModifiers::NONE),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(&LogicalKey::ArrowUp, true, KeyModifiers::NONE),
            Some(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn home_end_track_app_cursor_mode() {
        assert_eq!(
            encode_key(&LogicalKey::Home, false, KeyModifiers::NONE),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            encode_key(&LogicalKey::End, true, KeyModifiers::NONE),
            Some(b"\x1bOF".to_vec())
        );
    }

    #[test]
    fn editing_keys_use_tilde_form() {
        assert_eq!(
            encode_key(&LogicalKey::Delete, false, KeyModifiers::NONE),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode_key(&LogicalKey::PageUp, false, KeyModifiers::NONE),
            Some(b"\x1b[5~".to_vec())
        );
    }

    #[test]
    fn function_keys_f1_f4_use_ss3_f5_plus_use_tilde() {
        assert_eq!(
            encode_key(&LogicalKey::Function(1), false, KeyModifiers::NONE),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(&LogicalKey::Function(5), false, KeyModifiers::NONE),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_key(&LogicalKey::Function(12), false, KeyModifiers::NONE),
            Some(b"\x1b[24~".to_vec())
        );
        // Out-of-range function key encodes nothing.
        assert_eq!(
            encode_key(&LogicalKey::Function(99), false, KeyModifiers::NONE),
            None
        );
    }

    #[test]
    fn alt_prefixes_esc_for_text_but_not_for_escape_sequences() {
        // Alt+b → ESC b (Meta-b word-back in bash).
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::NONE
        };
        assert_eq!(
            encode_key(&LogicalKey::Text("b".into()), false, alt),
            Some(vec![0x1b, b'b'])
        );
        // Alt+ArrowUp must NOT double-prefix ESC (it already starts with ESC).
        // It carries the Alt modifier parameter (3) instead.
        let out = encode_key(&LogicalKey::ArrowUp, false, alt).expect("arrow encodes");
        assert_eq!(out, b"\x1b[1;3A".to_vec());
        assert_ne!(out.first(), out.get(1), "must not double-prefix ESC");
        // Same for an editing key that already begins with ESC.
        assert_eq!(
            encode_key(&LogicalKey::Delete, false, alt),
            Some(b"\x1b[3;3~".to_vec())
        );
        // …and for a function key, whose legacy form is modifier-free.
        assert_eq!(
            encode_key(&LogicalKey::Function(5), false, alt),
            Some(b"\x1b[15~".to_vec())
        );
    }

    // ---- kitty keyboard protocol (CSI u) encoder ----

    /// Disambiguate-only flags (bit1).
    const DISAMB: u8 = 1;
    /// Disambiguate + report-event-types (bit1 | bit2).
    const DISAMB_EVENTS: u8 = 3;

    fn ctrl() -> KeyModifiers {
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::NONE
        }
    }
    fn shift() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            ..KeyModifiers::NONE
        }
    }
    fn ctrl_shift() -> KeyModifiers {
        KeyModifiers {
            ctrl: true,
            shift: true,
            ..KeyModifiers::NONE
        }
    }

    #[test]
    fn kitty_flags_zero_returns_none() {
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Escape,
                KeyModifiers::NONE,
                0,
                KeyEventKind::Press
            ),
            None
        );
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text("a".into()),
                ctrl(),
                0,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_modifier_value() {
        assert_eq!(kitty_mod_value(KeyModifiers::NONE), 1);
        assert_eq!(kitty_mod_value(shift()), 2);
        assert_eq!(kitty_mod_value(ctrl()), 5);
        assert_eq!(kitty_mod_value(ctrl_shift()), 6);
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::NONE
        };
        assert_eq!(kitty_mod_value(alt), 3);
        let logo = KeyModifiers {
            logo: true,
            ..KeyModifiers::NONE
        };
        assert_eq!(kitty_mod_value(logo), 9);
    }

    #[test]
    fn kitty_escape_disambiguates() {
        // Esc under disambiguate → CSI 27 u (no trailing ; 1).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Escape,
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[27u".to_vec())
        );
    }

    #[test]
    fn kitty_tab_vs_ctrl_i() {
        // Plain Tab disambiguates to CSI 9 u; Ctrl+I (encoded as Tab + ctrl)
        // becomes CSI 9 ; 5 u — the two are now distinguishable.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Tab,
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[9u".to_vec())
        );
        assert_eq!(
            encode_key_kitty(&LogicalKey::Tab, ctrl(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[9;5u".to_vec())
        );
    }

    #[test]
    fn kitty_shift_enter() {
        // Shift+Enter → CSI 13 ; 2 u (the canonical "can't tell from Enter" fix).
        assert_eq!(
            encode_key_kitty(&LogicalKey::Enter, shift(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[13;2u".to_vec())
        );
    }

    #[test]
    fn kitty_backspace_disambiguates() {
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Backspace,
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[127u".to_vec())
        );
    }

    #[test]
    fn kitty_modified_letter() {
        // Ctrl+Shift+a → CSI 97 ; 6 u (base codepoint is lowercase 'a' = 97).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text("a".into()),
                ctrl_shift(),
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[97;6u".to_vec())
        );
        // Uppercase commit text is lowercased to the base codepoint too.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text("A".into()),
                ctrl_shift(),
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[97;6u".to_vec())
        );
    }

    #[test]
    fn kitty_unmodified_text_falls_through() {
        // Plain unmodified printable text is NOT escape-encoded (bit8 fall-through);
        // returns None so the caller sends it as legacy text.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text("a".into()),
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_multi_codepoint_text_falls_through() {
        // An IME commit (multi-char) passes through as text.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text("漢字".into()),
                ctrl(),
                DISAMB,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_release_silent_without_event_bit() {
        // Release with bit2 UNSET → None (legacy/disambiguate-only is press-only).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Escape,
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Release
            ),
            None
        );
    }

    #[test]
    fn kitty_release_encoded_with_event_bit() {
        // Release with bit2 SET → CSI 27 ; 1 : 3 u (mod field forced to 1).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Escape,
                KeyModifiers::NONE,
                DISAMB_EVENTS,
                KeyEventKind::Release
            ),
            Some(b"\x1b[27;1:3u".to_vec())
        );
        // Modified release carries the real modifier.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Enter,
                shift(),
                DISAMB_EVENTS,
                KeyEventKind::Release
            ),
            Some(b"\x1b[13;2:3u".to_vec())
        );
    }

    #[test]
    fn kitty_repeat_event() {
        // Repeat with bit2 SET → :2 event sub-param.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Tab,
                KeyModifiers::NONE,
                DISAMB_EVENTS,
                KeyEventKind::Repeat
            ),
            Some(b"\x1b[9;1:2u".to_vec())
        );
        // Repeat WITHOUT bit2 encodes identically to a press (no event field).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Tab,
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Repeat
            ),
            Some(b"\x1b[9u".to_vec())
        );
    }

    #[test]
    fn kitty_unmodified_cursor_key_falls_through() {
        // Unmodified arrow with no event → legacy form is unambiguous → None.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::ArrowUp,
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_modified_cursor_key() {
        // Shift+ArrowUp → CSI 1 ; 2 A (CSI-with-mods letter form).
        assert_eq!(
            encode_key_kitty(&LogicalKey::ArrowUp, shift(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[1;2A".to_vec())
        );
        // Ctrl+End → CSI 1 ; 5 F.
        assert_eq!(
            encode_key_kitty(&LogicalKey::End, ctrl(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[1;5F".to_vec())
        );
    }

    #[test]
    fn kitty_function_keys() {
        // Unmodified F1 with no event → legacy form → None.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Function(1),
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Press
            ),
            None
        );
        // Ctrl+F1 → CSI 1 ; 5 P.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Function(1),
                ctrl(),
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[1;5P".to_vec())
        );
        // Shift+F5 → CSI 15 ; 2 ~.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Function(5),
                shift(),
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[15;2~".to_vec())
        );
        // Out-of-range function key → None.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Function(99),
                ctrl(),
                DISAMB,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_modified_editing_key() {
        // Ctrl+Delete → CSI 3 ; 5 ~ (tilde form).
        assert_eq!(
            encode_key_kitty(&LogicalKey::Delete, ctrl(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[3;5~".to_vec())
        );
        // Unmodified Delete falls through (legacy CSI 3 ~ is unambiguous).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Delete,
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_report_associated_text() {
        // bit1 | bit16: a Ctrl+'a' carries the text codepoint appended.
        let flags = 1 | 16;
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text("a".into()),
                ctrl(),
                flags,
                KeyEventKind::Press
            ),
            Some(b"\x1b[97;5;97u".to_vec())
        );
    }

    #[test]
    fn kitty_default_event_kind_is_press() {
        assert_eq!(KeyEventKind::default(), KeyEventKind::Press);
    }

    // ---- legacy encode_key: every named-key arm + Alt-as-Meta ----

    #[test]
    fn legacy_backspace_is_del() {
        assert_eq!(
            encode_key(&LogicalKey::Backspace, false, KeyModifiers::NONE),
            Some(vec![0x7f])
        );
    }

    #[test]
    fn legacy_tab_is_horizontal_tab() {
        assert_eq!(
            encode_key(&LogicalKey::Tab, false, KeyModifiers::NONE),
            Some(vec![b'\t'])
        );
    }

    #[test]
    fn legacy_escape_is_esc_byte() {
        assert_eq!(
            encode_key(&LogicalKey::Escape, false, KeyModifiers::NONE),
            Some(vec![0x1b])
        );
    }

    #[test]
    fn legacy_space_is_space_byte() {
        assert_eq!(
            encode_key(&LogicalKey::Space, false, KeyModifiers::NONE),
            Some(vec![b' '])
        );
    }

    #[test]
    fn legacy_all_arrows_csi_and_ss3() {
        // Exact bytes for every arrow in BOTH cursor modes (kills any
        // letter-swap mutant on A/B/C/D).
        for (key, letter) in [
            (LogicalKey::ArrowUp, b'A'),
            (LogicalKey::ArrowDown, b'B'),
            (LogicalKey::ArrowRight, b'C'),
            (LogicalKey::ArrowLeft, b'D'),
        ] {
            assert_eq!(
                encode_key(&key, false, KeyModifiers::NONE),
                Some(vec![0x1b, b'[', letter])
            );
            assert_eq!(
                encode_key(&key, true, KeyModifiers::NONE),
                Some(vec![0x1b, b'O', letter])
            );
        }
    }

    #[test]
    fn legacy_insert_and_pagedown_tilde_forms() {
        assert_eq!(
            encode_key(&LogicalKey::Insert, false, KeyModifiers::NONE),
            Some(b"\x1b[2~".to_vec())
        );
        assert_eq!(
            encode_key(&LogicalKey::PageDown, false, KeyModifiers::NONE),
            Some(b"\x1b[6~".to_vec())
        );
    }

    #[test]
    fn legacy_function_keys_exact_bytes_f1_through_f12() {
        // Every F-key's exact sequence — kills any tilde-number / SS3-letter
        // swap mutant across the whole F1..=F12 table.
        let expected: &[(u8, &[u8])] = &[
            (1, b"\x1bOP"),
            (2, b"\x1bOQ"),
            (3, b"\x1bOR"),
            (4, b"\x1bOS"),
            (5, b"\x1b[15~"),
            (6, b"\x1b[17~"),
            (7, b"\x1b[18~"),
            (8, b"\x1b[19~"),
            (9, b"\x1b[20~"),
            (10, b"\x1b[21~"),
            (11, b"\x1b[23~"),
            (12, b"\x1b[24~"),
        ];
        for (n, seq) in expected {
            assert_eq!(
                encode_key(&LogicalKey::Function(*n), false, KeyModifiers::NONE),
                Some(seq.to_vec()),
                "F{n} legacy sequence"
            );
        }
        // n == 0 is out of range and encodes nothing (lower-bound guard).
        assert_eq!(
            encode_key(&LogicalKey::Function(0), false, KeyModifiers::NONE),
            None
        );
    }

    #[test]
    fn legacy_alt_does_not_double_prefix_an_arrow() {
        // Alt+Enter prefixes ESC (Enter is a non-ESC byte).
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::NONE
        };
        assert_eq!(
            encode_key(&LogicalKey::Enter, false, alt),
            Some(vec![0x1b, b'\r'])
        );
        // Alt+Function(5) (tilde form already starts with ESC) is NOT prefixed.
        assert_eq!(
            encode_key(&LogicalKey::Function(5), false, alt),
            Some(b"\x1b[15~".to_vec())
        );
    }

    // ---- kitty encoder: remaining branches ----

    #[test]
    fn kitty_modified_space_encodes_csi_u_codepoint_32() {
        // Ctrl+Space → CSI 32 ; 5 u (space is codepoint 32).
        assert_eq!(
            encode_key_kitty(&LogicalKey::Space, ctrl(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[32;5u".to_vec())
        );
        // Unmodified Space with no event → fall through to legacy (None).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Space,
                KeyModifiers::NONE,
                DISAMB,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_modified_pageup_and_pagedown_tilde() {
        // Ctrl+PageUp → CSI 5 ; 5 ~ ; Ctrl+PageDown → CSI 6 ; 5 ~.
        assert_eq!(
            encode_key_kitty(&LogicalKey::PageUp, ctrl(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[5;5~".to_vec())
        );
        assert_eq!(
            encode_key_kitty(&LogicalKey::PageDown, ctrl(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[6;5~".to_vec())
        );
        // Ctrl+Insert → CSI 2 ; 5 ~.
        assert_eq!(
            encode_key_kitty(&LogicalKey::Insert, ctrl(), DISAMB, KeyEventKind::Press),
            Some(b"\x1b[2;5~".to_vec())
        );
    }

    #[test]
    fn kitty_function_keys_f2_f3_f4_csi_letter() {
        // Ctrl+F2/F3/F4 use the CSI-1-letter Q/R/S form (kills letter-swap).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Function(2),
                ctrl(),
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[1;5Q".to_vec())
        );
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Function(3),
                ctrl(),
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[1;5R".to_vec())
        );
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Function(4),
                ctrl(),
                DISAMB,
                KeyEventKind::Press
            ),
            Some(b"\x1b[1;5S".to_vec())
        );
    }

    #[test]
    fn kitty_function_keys_f6_through_f12_tilde() {
        // Each higher F-key under Ctrl emits its tilde number (kills number swap).
        let expected: &[(u8, &[u8])] = &[
            (6, b"\x1b[17;5~"),
            (7, b"\x1b[18;5~"),
            (8, b"\x1b[19;5~"),
            (9, b"\x1b[20;5~"),
            (10, b"\x1b[21;5~"),
            (11, b"\x1b[23;5~"),
            (12, b"\x1b[24;5~"),
        ];
        for (n, seq) in expected {
            assert_eq!(
                encode_key_kitty(
                    &LogicalKey::Function(*n),
                    ctrl(),
                    DISAMB,
                    KeyEventKind::Press
                ),
                Some(seq.to_vec()),
                "kitty F{n}"
            );
        }
    }

    #[test]
    fn kitty_empty_text_falls_through() {
        // Empty Text under a modifier → None (the `Some(c), None` guard's empty arm).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text(String::new()),
                ctrl(),
                DISAMB,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_release_on_letter_with_event_bit() {
        // Release of Ctrl+'a' with bit2: CSI 97 ; 5 : 3 u (encode_char_key event arm).
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text("a".into()),
                ctrl(),
                DISAMB_EVENTS,
                KeyEventKind::Release
            ),
            Some(b"\x1b[97;5:3u".to_vec())
        );
    }

    #[test]
    fn kitty_all_arrows_and_home_with_modifier() {
        // Modified cursor keys use the CSI-1-letter form; one assert per letter
        // kills the letter-swap mutant on every arm of csi_letter_or_fallback.
        let cases: &[(LogicalKey, &[u8])] = &[
            (LogicalKey::ArrowDown, b"\x1b[1;2B"),
            (LogicalKey::ArrowRight, b"\x1b[1;2C"),
            (LogicalKey::ArrowLeft, b"\x1b[1;2D"),
            (LogicalKey::Home, b"\x1b[1;2H"),
        ];
        for (key, seq) in cases {
            assert_eq!(
                encode_key_kitty(key, shift(), DISAMB, KeyEventKind::Press),
                Some(seq.to_vec()),
                "modified {key:?}"
            );
        }
    }

    #[test]
    fn kitty_repeat_event_on_letter_and_tilde_forms() {
        // Repeat (event sub-param 2) on the CSI-letter form (arrow) and the
        // tilde form (F5) — exercises kitty_csi_letter / kitty_tilde event arms
        // and the kitty_event_subparam Repeat=2 branch.
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::ArrowUp,
                KeyModifiers::NONE,
                DISAMB_EVENTS,
                KeyEventKind::Repeat
            ),
            Some(b"\x1b[1;1:2A".to_vec())
        );
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Function(5),
                KeyModifiers::NONE,
                DISAMB_EVENTS,
                KeyEventKind::Repeat
            ),
            Some(b"\x1b[15;1:2~".to_vec())
        );
    }

    #[test]
    fn kitty_enter_falls_through_without_disambiguate_or_modifier() {
        // Events-only flag (bit2, no disambiguate): an unmodified Enter PRESS has
        // nothing to disambiguate and no event to report -> csi_u_or_fallback
        // returns None (legacy CR is sent).
        const EVENTS_ONLY: u8 = 2;
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Enter,
                KeyModifiers::NONE,
                EVENTS_ONLY,
                KeyEventKind::Press
            ),
            None
        );
    }

    #[test]
    fn kitty_associated_text_with_event_sub_param() {
        // bit1|bit2|bit16 + a Repeat: encode_char_key emits both the event
        // sub-param AND the trailing text codepoint (covers the report_text arm
        // alongside emit_event).
        let flags = 1 | 2 | 16;
        assert_eq!(
            encode_key_kitty(
                &LogicalKey::Text("a".into()),
                ctrl(),
                flags,
                KeyEventKind::Repeat
            ),
            Some(b"\x1b[97;5:2;97u".to_vec())
        );
    }
}
