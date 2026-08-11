//! OSC (Operating System Command) helpers for the terminal core.
//!
//! Houses self-contained helpers for OSC sequence handling so the main
//! [`crate::term`] module's `Perform` impl stays focused on dispatch. This
//! includes a dependency-free RFC 4648 base64 codec (for OSC 52 clipboard
//! payloads), X-style color spec parsing/formatting (for OSC 4/10/11/12), and
//! the small value types the public API hands back to the application.
//!
//! Colors are plain `(u8, u8, u8)` RGB triples — the same representation
//! [`crate::grid::Color::Rgb`] carries — so no new color type is introduced.

/// An RGB triple `(r, g, b)`, matching [`crate::grid::Color::Rgb`]'s fields.
pub type Rgb = (u8, u8, u8);

// ============================================================================
// Clipboard (OSC 52)
// ============================================================================

/// Which selection an OSC 52 clipboard operation targets.
///
/// An OSC 52 sequence may list several selection characters (e.g. `c` for the
/// system clipboard, `p` for the primary selection). We model the two we care
/// about; any unknown/empty selection list falls back to
/// [`ClipboardSelection::Clipboard`], matching xterm's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    /// The system clipboard (`c`).
    Clipboard,
    /// The primary selection (`p`).
    Primary,
}

/// A pending OSC 52 clipboard write request drained by the application.
///
/// The decoded clipboard `text` is potentially sensitive (a program inside the
/// terminal can place arbitrary bytes — including secrets — onto the clipboard
/// via OSC 52). To keep that plaintext from lingering in freed allocations
/// recoverable from a crash dump / pagefile (P-V3), the buffer is wiped on drop
/// via [`zeroize`]. Dropping a `ClipboardWrite` — whether the app drains and
/// drops it after copying to the OS clipboard, or the terminal clears the
/// pending queue on reset — zeroes `text`'s backing bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardWrite {
    /// The selection targeted by the request.
    pub selection: ClipboardSelection,
    /// The decoded UTF-8 text to place on the clipboard.
    pub text: String,
}

impl zeroize::Zeroize for ClipboardWrite {
    /// Wipe the sensitive `text` buffer in place. `selection` is a non-sensitive
    /// `Copy` enum and needs no zeroizing.
    fn zeroize(&mut self) {
        self.text.zeroize();
    }
}

impl Drop for ClipboardWrite {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.text.zeroize();
    }
}

// `ZeroizeOnDrop` is the marker promising the `Drop` impl above zeroes all
// sensitive state — it lets the type compose into other zeroizing containers.
impl zeroize::ZeroizeOnDrop for ClipboardWrite {}

/// A pending OSC 52 clipboard READ request (`OSC 52 ; <sel> ; ? ST`) drained by
/// the application, which supplies the clipboard text via
/// [`crate::term::Terminal::respond_clipboard_read`].
///
/// A request is ONLY ever queued when clipboard reads have been explicitly
/// opted into (`clipboard_read_allow`, default OFF). While reads are denied —
/// the default — the terminal answers the query itself with an EMPTY payload
/// (see [`format_clipboard_reply`]) and queues NOTHING here, so a denied read
/// can never reach the host clipboard.
///
/// Unlike [`ClipboardWrite`] this carries no text, so there is nothing to
/// zeroize — it is a plain `Copy` request record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardReadRequest {
    /// The selection the program asked to read.
    pub selection: ClipboardSelection,
}

/// The OSC 52 wire character for a selection (`c` clipboard / `p` primary).
pub fn selection_char(selection: ClipboardSelection) -> char {
    match selection {
        ClipboardSelection::Clipboard => 'c',
        ClipboardSelection::Primary => 'p',
    }
}

/// Builds the `ESC ] 52 ; <sel> ; <base64> BEL` reply to an OSC 52 read query.
///
/// `payload` is the ALREADY-base64-encoded clipboard data. Passing an EMPTY
/// `payload` produces the **deny / no-data** reply — a structurally valid OSC 52
/// response that carries zero clipboard bytes.
///
/// The deny reply matters: a program that issues `OSC 52 ; c ; ?` typically
/// BLOCKS on the answer, so a terminal that stays silent leaves it hung until
/// its own timeout expires (the neovim `getreg('+')` stall, neovim#32699). The
/// empty-data-field response is the convention terminals use to refuse without
/// hanging the caller, and it leaks nothing: base64 of nothing is nothing.
pub fn format_clipboard_reply(selection: ClipboardSelection, payload: &str) -> String {
    format!("\x1b]52;{};{}\x07", selection_char(selection), payload)
}

// ============================================================================
// Dynamic colors / notifications
// ============================================================================

/// Which dynamic color an OSC 10/11/12 (or 110/111/112 reset) targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicColor {
    /// Default foreground (OSC 10).
    Foreground,
    /// Default background (OSC 11).
    Background,
    /// Cursor color (OSC 12).
    Cursor,
}

/// A pending color-set request drained by the application so it can update its
/// live theme. Produced by OSC 4 (indexed) and OSC 10/11/12 (dynamic) sets, and
/// by OSC 104/110/111/112 resets (the `rgb` then carries the default value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSet {
    /// Set indexed palette entry `index` to `rgb`.
    Indexed { index: u8, rgb: Rgb },
    /// Set a dynamic color (fg/bg/cursor) to `rgb`.
    Dynamic { which: DynamicColor, rgb: Rgb },
}

/// A pending desktop notification (OSC 9 / OSC 777) drained by the application.
///
/// OSC 9 carries only a body; its `title` is empty. OSC 777 (`notify`) carries
/// both a title and a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// The state of an `OSC 9 ; 4` taskbar/tab progress report (C26).
///
/// Mirrors the Windows-Terminal/ConEmu progress protocol's four states. The app
/// drives a tab or taskbar indicator from the drained [`Progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    /// State 0 — remove the progress indicator.
    Remove,
    /// State 1 — normal, determinate progress at `percent`.
    Normal,
    /// State 2 — error state (typically shown red) at `percent`.
    Error,
    /// State 3 — indeterminate (spinner); `percent` is ignored.
    Indeterminate,
    /// State 4 — warning/paused (typically shown yellow) at `percent`.
    Warning,
}

/// A pending `OSC 9 ; 4 ; state ; percent` progress report (C26), drained by the
/// application to update a taskbar/tab progress indicator. `percent` is clamped
/// to 0-100; for [`ProgressState::Remove`] / [`ProgressState::Indeterminate`] it
/// is `0` and should be ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub state: ProgressState,
    pub percent: u8,
}

/// Which OSC 133 semantic-prompt zone a [`CommandMark`] records (C28).
///
/// `A`/`B` (prompt-start / prompt-end) are tracked separately as
/// `prompt_marks`; this type captures the command lifecycle: `C` (command
/// output begins) and `D` (command finished, with an optional exit code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMarkKind {
    /// `OSC 133 ; C` — the command's output starts here.
    OutputStart,
    /// `OSC 133 ; D [; exit_code]` — the command finished.
    CommandEnd {
        /// Exit code parsed from the `D` mark, if the shell supplied one.
        exit_code: Option<i32>,
    },
}

/// An OSC 133 command-zone mark (C28), anchored to an absolute grid line.
///
/// Capture-only: like the prompt marks, the terminal NEVER reports these back to
/// the PTY (preserves the iTerm2 CVE-2024-38395/38396-class anti-injection
/// posture). The app reads them to draw success/fail prompt glyphs and command
/// durations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandMark {
    pub kind: CommandMarkKind,
    /// Absolute line index (history length + grid row) where the mark landed.
    pub line: usize,
}

// ============================================================================
// base64 (RFC 4648, standard alphabet)
// ============================================================================

const B64_ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes raw bytes as standard-alphabet RFC 4648 base64 with padding.
///
/// Used to build OSC 52 clipboard READ replies. Dependency-free.
pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for c in &mut chunks {
        let n = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
        out.push(B64_ALPHA[(n >> 18 & 0x3f) as usize] as char);
        out.push(B64_ALPHA[(n >> 12 & 0x3f) as usize] as char);
        out.push(B64_ALPHA[(n >> 6 & 0x3f) as usize] as char);
        out.push(B64_ALPHA[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        0 => {}
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(B64_ALPHA[(n >> 18 & 0x3f) as usize] as char);
            out.push(B64_ALPHA[(n >> 12 & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (rem[0] as u32) << 16 | (rem[1] as u32) << 8;
            out.push(B64_ALPHA[(n >> 18 & 0x3f) as usize] as char);
            out.push(B64_ALPHA[(n >> 12 & 0x3f) as usize] as char);
            out.push(B64_ALPHA[(n >> 6 & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => unreachable!("chunks_exact remainder is < 3"),
    }
    out
}

/// Decodes standard-alphabet RFC 4648 base64 into raw bytes.
///
/// Whitespace is ignored. Padding (`=`) is honoured. Returns `None` on any
/// invalid character or malformed length. Dependency-free.
pub fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    // Strip whitespace.
    let mut symbols: Vec<u8> = Vec::with_capacity(input.len());
    for &b in input {
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
            continue;
        }
        symbols.push(b);
    }

    // Count trailing padding and validate it is only at the end.
    let mut pad = 0usize;
    while symbols.last() == Some(&b'=') {
        symbols.pop();
        pad += 1;
    }
    if pad > 2 || symbols.contains(&b'=') {
        return None;
    }

    // Leftover symbol count must be consistent with the padding.
    match (pad, symbols.len() % 4) {
        (0, 0) | (1, 3) | (2, 2) => {}
        _ => return None,
    }

    let mut out = Vec::with_capacity(symbols.len() * 3 / 4 + 3);
    let mut chunk = symbols.chunks_exact(4);
    for c in &mut chunk {
        let a = val(c[0])?;
        let b = val(c[1])?;
        let d = val(c[2])?;
        let e = val(c[3])?;
        let n = (a as u32) << 18 | (b as u32) << 12 | (d as u32) << 6 | (e as u32);
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push(n as u8);
    }
    let tail = chunk.remainder();
    match tail.len() {
        0 => {}
        2 => {
            let a = val(tail[0])?;
            let b = val(tail[1])?;
            let n = (a as u32) << 18 | (b as u32) << 12;
            out.push((n >> 16) as u8);
        }
        3 => {
            let a = val(tail[0])?;
            let b = val(tail[1])?;
            let d = val(tail[2])?;
            let n = (a as u32) << 18 | (b as u32) << 12 | (d as u32) << 6;
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
        }
        _ => return None,
    }
    Some(out)
}

// ============================================================================
// Color spec parsing / formatting (OSC 4 / 10 / 11 / 12)
// ============================================================================

/// Parses an X11-style color spec into an [`Rgb`] triple.
///
/// Supports the two forms terminals commonly emit:
/// - `rgb:RR/GG/BB`, `rgb:RRRR/GGGG/BBBB` (1-4 hex digits per channel; scaled
///   to 8 bits).
/// - `#RGB`, `#RRGGBB`, `#RRRRGGGGBBBB` (xterm `#` form).
///
/// Returns `None` for unrecognised specs (including the `?` query sentinel,
/// which the caller handles separately).
pub fn parse_color_spec(spec: &str) -> Option<Rgb> {
    let spec = spec.trim();
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut parts = rest.split('/');
        let r = scale_hex_channel(parts.next()?)?;
        let g = scale_hex_channel(parts.next()?)?;
        let b = scale_hex_channel(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        return Some((r, g, b));
    }
    if let Some(rest) = spec.strip_prefix('#') {
        // The `#` form matches on BYTE length and byte-slices per channel, which
        // is only sound when every byte is a single ASCII char. The OSC 4/10/11/12
        // callers decode the spec with `String::from_utf8_lossy`, so an untrusted
        // PTY emitting `#<invalid-utf8>` yields a U+FFFD replacement char (3 bytes)
        // — `rest.len() == 3` would pick `per = 1` and then `&rest[0..1]` would
        // slice into the middle of that codepoint and PANIC. A real hex color spec
        // is ASCII by definition, so reject non-ASCII up front (root cause: the
        // length match + byte-slicing assume ASCII).
        if !rest.is_ascii() {
            return None;
        }
        let per = match rest.len() {
            3 => 1,
            6 => 2,
            12 => 4,
            _ => return None,
        };
        let r = scale_hex_channel(&rest[0..per])?;
        let g = scale_hex_channel(&rest[per..per * 2])?;
        let b = scale_hex_channel(&rest[per * 2..per * 3])?;
        return Some((r, g, b));
    }
    None
}

/// Scales a 1-4 digit hex channel string into an 8-bit value.
///
/// A channel of width `n` hex digits represents a value in `[0, 16^n - 1]`;
/// we rescale to `[0, 255]` with rounding. A single digit `f` therefore maps to
/// `0xff`, matching xterm.
fn scale_hex_channel(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    let max = (1u32 << (4 * s.len() as u32)) - 1;
    let v = u32::from_str_radix(s, 16).ok()?;
    if v > max {
        return None;
    }
    Some(((v * 255 + max / 2) / max) as u8)
}

/// Formats an [`Rgb`] triple as the `rgb:RRRR/GGGG/BBBB` 16-bit-per-channel
/// reply form xterm uses in OSC 4/10/11/12 query responses.
///
/// Each 8-bit channel is replicated into the high byte (`v -> v*0x101`) so that
/// `0xff` becomes `ffff`, matching xterm's reported values.
pub fn format_color_reply(rgb: Rgb) -> String {
    let expand = |v: u8| (v as u16) * 0x101;
    format!(
        "rgb:{:04x}/{:04x}/{:04x}",
        expand(rgb.0),
        expand(rgb.1),
        expand(rgb.2)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_decode_basic() {
        assert_eq!(base64_decode(b"aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode(b"aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
    }

    #[test]
    fn test_base64_decode_no_padding() {
        assert_eq!(base64_decode(b"Zm9v").unwrap(), b"foo");
    }

    #[test]
    fn test_base64_decode_padding_variants() {
        assert_eq!(base64_decode(b"Zg==").unwrap(), b"f");
        assert_eq!(base64_decode(b"Zm8=").unwrap(), b"fo");
    }

    #[test]
    fn test_base64_decode_whitespace_ignored() {
        assert_eq!(base64_decode(b"aGVs\r\nbG8=").unwrap(), b"hello");
    }

    #[test]
    fn test_base64_decode_invalid() {
        assert!(base64_decode(b"!!!!").is_none());
        assert!(base64_decode(b"aGVsbG8==").is_none()); // 3 pad
        assert!(base64_decode(b"aGV").is_none()); // bad length, no pad
    }

    #[test]
    fn test_base64_encode_basic() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn test_base64_roundtrip() {
        let data = b"The quick brown fox jumps over the lazy dog.";
        let enc = base64_encode(data);
        assert_eq!(base64_decode(enc.as_bytes()).unwrap(), data);
    }

    #[test]
    fn test_parse_color_spec_rgb() {
        assert_eq!(parse_color_spec("rgb:ff/00/00"), Some((255, 0, 0)));
        assert_eq!(parse_color_spec("rgb:ffff/0000/8080"), Some((255, 0, 128)));
        assert_eq!(parse_color_spec("rgb:f/0/0"), Some((255, 0, 0)));
    }

    #[test]
    fn test_parse_color_spec_hash() {
        assert_eq!(parse_color_spec("#ff0000"), Some((255, 0, 0)));
        assert_eq!(parse_color_spec("#f00"), Some((255, 0, 0)));
        assert_eq!(parse_color_spec("#ffff00008080"), Some((255, 0, 128)));
    }

    #[test]
    fn test_parse_color_spec_invalid() {
        assert!(parse_color_spec("?").is_none());
        assert!(parse_color_spec("rgb:zz/00/00").is_none());
        assert!(parse_color_spec("#12345").is_none());
    }

    #[test]
    fn test_parse_color_spec_non_ascii_hash_does_not_panic() {
        // Regression: an untrusted PTY emitting `OSC 10 ; #<invalid-utf8> ST`
        // decodes (lossily) to a U+FFFD replacement char after `#`. Its 3-byte
        // length once matched `per = 1`, and `&rest[0..1]` sliced into the middle
        // of the codepoint and panicked, crashing the terminal. Must now reject
        // cleanly (None), never panic — for the 3/6/12-byte-length shapes that
        // U+FFFD repetition can hit.
        assert!(parse_color_spec("#\u{FFFD}").is_none()); // 3 bytes → old per=1
        assert!(parse_color_spec("#\u{FFFD}\u{FFFD}").is_none()); // 6 bytes → old per=2
        assert!(parse_color_spec("#\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}").is_none()); // 12 → per=4
        assert!(parse_color_spec("#ff\u{FFFD}0").is_none()); // mixed ascii+multibyte
    }

    #[test]
    fn test_format_color_reply() {
        assert_eq!(format_color_reply((255, 0, 0)), "rgb:ffff/0000/0000");
        assert_eq!(format_color_reply((26, 27, 38)), "rgb:1a1a/1b1b/2626");
    }

    #[test]
    fn base64_roundtrips_the_two_non_alnum_alphabet_chars() {
        // `+` (62) and `/` (63) are the only non-alphanumeric base64 symbols and
        // have dedicated decode arms. `>>?` encodes to bytes that round-trip
        // through both, and the encoded form `Pj4/` exercises `/` on decode.
        let enc = base64_encode(b">>?");
        assert_eq!(enc, "Pj4/");
        assert_eq!(base64_decode(b"Pj4/").unwrap(), b">>?");
        // `+` appears when encoding 0xFB 0xFF (`+/8=` -> `+`). Verify the decode
        // arm for `+` directly.
        assert_eq!(base64_decode(b"++++").unwrap(), vec![0xfb, 0xef, 0xbe]);
    }

    #[test]
    fn base64_decode_rejects_embedded_equals_mid_string() {
        // A `=` that is NOT trailing padding is invalid (the `symbols.contains('=')`
        // guard). `aG=s` has padding-stripping leave an interior `=`.
        assert!(base64_decode(b"aG=sbG8=").is_none());
    }

    #[test]
    fn base64_decode_rejects_single_trailing_symbol() {
        // A 4n+1 symbol count (tail.len() == 1) is structurally impossible for
        // base64 and must be rejected, not silently produce a byte.
        assert!(base64_decode(b"ZZZZZ").is_none());
    }

    #[test]
    fn parse_color_spec_rejects_rgb_with_four_channels() {
        // `rgb:` with a stray 4th `/`-separated part is malformed -> None
        // (the `parts.next().is_some()` reject arm).
        assert!(parse_color_spec("rgb:ff/00/00/00").is_none());
    }

    #[test]
    fn scale_hex_channel_rejects_empty_and_overlong() {
        // Empty channel and a >4-digit channel are both rejected.
        assert!(parse_color_spec("rgb://00/00").is_none()); // empty first channel
        assert!(parse_color_spec("rgb:fffff/0/0").is_none()); // 5-digit channel
    }

    #[test]
    fn scale_hex_channel_two_digit_midrange_rounds() {
        // A 2-digit channel `80` scales (0x80 * 255 + 127) / 255 = 128, exact.
        assert_eq!(parse_color_spec("rgb:80/80/80"), Some((128, 128, 128)));
        // 4-digit max `ffff` -> 255 (kills any divisor mutant in scale_hex_channel).
        assert_eq!(
            parse_color_spec("rgb:ffff/ffff/ffff"),
            Some((255, 255, 255))
        );
        // 4-digit `0001` rounds to 0.
        assert_eq!(parse_color_spec("rgb:0001/0000/0000"), Some((0, 0, 0)));
    }
}
