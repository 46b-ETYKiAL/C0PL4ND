//! Unit tests for the VT parser / terminal state machine, split out of
//! `term.rs` (F6-1 decomposition) — pure structural move, no test changes.

use super::*;

#[test]
fn prints_plain_text() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"hello");
    assert!(t.grid().to_text().starts_with("hello"));
}

#[test]
fn clear_damage_resets_until_next_write() {
    // The renderer's damage gate (PaneTerm::grid_rows) relies on this:
    // clear_damage() must reset is_damaged(), and a later write must re-mark
    // it so the next frame redraws.
    let mut t = Terminal::new(4, 20);
    t.advance(b"hi");
    assert!(t.grid().is_damaged(), "a write marks the row dirty");
    t.clear_damage();
    assert!(
        !t.grid().is_damaged(),
        "clear_damage resets every per-row damage bit"
    );
    t.advance(b"x");
    assert!(
        t.grid().is_damaged(),
        "a write after clear_damage re-marks the row dirty"
    );
}

#[test]
fn handles_crlf() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"ab\r\ncd");
    let text = t.grid().to_text();
    let mut lines = text.lines();
    assert!(lines.next().unwrap().starts_with("ab"));
    assert!(lines.next().unwrap().starts_with("cd"));
}

#[test]
fn sgr_sets_indexed_color() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[31mR"); // red foreground
    let cell = t.grid().cell(0, 0).unwrap();
    assert_eq!(cell.c, 'R');
    assert_eq!(cell.fg, Color::Indexed(1));
}

#[test]
fn sgr_truecolor() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[38;2;0;229;255mX");
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Rgb(0, 229, 255));
}

#[test]
fn sgr_color_operands_clamp_not_wrap() {
    // Regression (audit EN-1): out-of-range SGR colour operands must CLAMP to
    // 255, never wrap mod 256 (a bare `as u8` turned `38;5;300` into index 44
    // and `38;2;300;..` into channel 44 — silently wrong colours).
    let mut t = Terminal::new(2, 10);
    // Truecolor, semicolon form: every channel saturates to 255.
    t.advance(b"\x1b[38;2;300;511;256mA");
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Rgb(255, 255, 255));
    // 256-colour indexed, semicolon form: index saturates to 255.
    t.advance(b"\x1b[38;5;300mB");
    assert_eq!(t.grid().cell(0, 1).unwrap().fg, Color::Indexed(255));
    // Colon sub-parameter form clamps identically (underline colour, SGR 58).
    t.advance(b"\x1b[58:2::400:0:0mC");
    assert_eq!(
        t.grid().cell(0, 2).unwrap().underline_color,
        Some(Color::Rgb(255, 0, 0))
    );
}

#[test]
fn sgr_truecolor_channels_map_to_distinct_positions_and_advance() {
    // Mutation-hardening for parse_extended_color: DISTINCT in-range channels
    // prove r/g/b read the correct positions (a `len - n` → `len / n` index
    // mutant would swap a channel), and a trailing SGR after the truecolor
    // proves the parser advanced past EXACTLY the colour's codes (a wrong `*i`
    // advance would drop or misread the trailing `1` = bold).
    let mut t = Terminal::new(2, 12);
    t.advance(b"\x1b[38;2;10;20;30;1mX"); // semicolon form + trailing bold
    let cell = t.grid().cell(0, 0).unwrap();
    assert_eq!(
        cell.fg,
        Color::Rgb(10, 20, 30),
        "r/g/b must map to their own positions, not a swapped index"
    );
    assert!(
        cell.flags.bold,
        "the trailing `;1` (bold) after the truecolor must be parsed — proves \
         parse_extended_color advanced the code index correctly"
    );
    // Colon form with three distinct channels (no empty colorspace slot).
    let mut t2 = Terminal::new(2, 12);
    t2.advance(b"\x1b[38:2:40:50:60mY");
    assert_eq!(
        t2.grid().cell(0, 0).unwrap().fg,
        Color::Rgb(40, 50, 60),
        "colon-form r/g/b must map to their own positions"
    );
}

#[test]
fn erase_display_clears() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"junk\x1b[2J");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ');
}

#[test]
fn osc_sets_title() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]0;C0PL4ND\x07");
    assert_eq!(t.title(), "C0PL4ND");
}

#[test]
fn line_wrap_advances_row() {
    let mut t = Terminal::new(3, 3);
    t.advance(b"abcd"); // wraps after 3 cols
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'a');
    assert_eq!(t.grid().cell(1, 0).unwrap().c, 'd');
}

#[test]
fn scrollback_retains_lines_pushed_off_top() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    // 5 lines into a 2-row grid: 3 lines scroll into history.
    t.advance(b"L0\r\nL1\r\nL2\r\nL3\r\nL4");
    assert!(
        t.scrollback_len() >= 3,
        "history should retain scrolled lines"
    );
    let all = t.all_lines();
    assert!(all.iter().any(|l| l.starts_with("L0")));
    assert!(all.iter().any(|l| l.starts_with("L4")));
}

#[test]
fn scroll_view_offset_clamps_and_resets() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    t.advance(b"a\r\nb\r\nc\r\nd\r\ne");
    t.scroll_up_view(1000);
    assert_eq!(
        t.view_offset(),
        t.scrollback_len(),
        "offset clamps to history"
    );
    t.scroll_to_bottom();
    assert_eq!(t.view_offset(), 0);
}

#[test]
fn display_rows_follows_live_at_bottom() {
    let mut t = Terminal::new(3, 5);
    t.advance(b"x");
    let rows = t.display_rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0].c, 'x');
}

#[test]
fn osc8_captures_hyperlink() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b]8;;https://itasha.corp\x07link\x1b]8;;\x07");
    assert_eq!(t.hyperlinks(), &["https://itasha.corp".to_string()]);
}

#[test]
fn title_is_length_capped() {
    // A hostile OSC 2 stuffing a huge title must NOT be stored verbatim
    // (memory-DoS). The stored title is capped to TITLE_MAX_CHARS.
    let mut t = Terminal::new(2, 40);
    let mut seq = b"\x1b]2;".to_vec();
    seq.extend(std::iter::repeat_n(b'A', 100_000));
    seq.push(0x07);
    t.advance(&seq);
    assert!(
        t.title().chars().count() <= Screen::TITLE_MAX_CHARS,
        "title must be length-capped (was {})",
        t.title().chars().count()
    );
}

#[test]
fn osc52_oversized_write_is_dropped() {
    // A multi-megabyte OSC 52 clipboard write must be dropped, not buffered.
    let mut t = Terminal::new(2, 40);
    let big_b64 = "QQ".repeat(1_500_000); // ~3 MB of base64 → >1 MiB decoded
    let mut seq = b"\x1b]52;c;".to_vec();
    seq.extend_from_slice(big_b64.as_bytes());
    seq.push(0x07);
    t.advance(&seq);
    assert!(
        t.take_clipboard_writes().is_empty(),
        "an oversized OSC 52 write must be dropped"
    );

    // A small write still works (cap doesn't break the legit feature).
    t.advance(b"\x1b]52;c;aGVsbG8=\x07"); // base64("hello")
    assert_eq!(
        t.take_clipboard_writes().len(),
        1,
        "a small OSC 52 write is kept"
    );
}

/// SECURITY (device-reply echo-to-stdin, the #1 terminal-RCE class —
/// CVE-2022-45872 etc.): every reply the terminal queues for the PTY must be
/// built ONLY from validated internal state, never reflect attacker-supplied
/// request bytes, and must be 7-bit-clean (no embedded C0 controls other than
/// the `ESC` / `BEL` / `ST` framing). We feed a battery of malformed
/// DECRQSS / XTGETTCAP / OSC-color-query requests carrying hostile bytes and
/// assert the drained reply never smuggles a control byte that could be
/// echoed onto the shell's stdin as if typed.
#[test]
fn device_replies_are_7bit_clean_with_no_smuggled_controls() {
    let inputs: &[&[u8]] = &[
        b"\x1bP$qm\x1b\\",        // DECRQSS: request SGR
        b"\x1bP$q\"q\x1b\\",      // DECRQSS: request DECSCA
        b"\x1bP$q\x07evil\x1b\\", // DECRQSS with an embedded BEL + junk
        b"\x1bP+q686f7374\x1b\\", // XTGETTCAP: hex name "host"
        b"\x1bP+q00ff\x1b\\",     // XTGETTCAP: hex decoding to NUL/0xff
        b"\x1b]10;?\x07",         // OSC 10 foreground color query
        b"\x1b]11;?\x1b\\",       // OSC 11 background color query (ST-terminated)
        b"\x1b]4;1;?\x07",        // OSC 4 indexed color query
    ];
    for inp in inputs {
        let mut t = Terminal::new(4, 20);
        t.advance(inp);
        let reply = t.take_pty_response();
        assert!(
            !reply.is_empty(),
            "no reply was queued for {inp:x?} — an empty reply makes the \
             7-bit-clean scan below vacuous (it iterates nothing), and a device \
             query the terminal silently ignores hangs the requesting app"
        );
        for (i, &b) in reply.iter().enumerate() {
            let is_framing = b == 0x1b || b == 0x5c || b == 0x07; // ESC, '\', BEL
            let is_printable = (0x20..=0x7e).contains(&b);
            assert!(
                is_framing || is_printable,
                "device reply for {inp:x?} smuggled control byte {b:#04x} at {i}: {reply:x?}"
            );
        }
    }
}

#[test]
fn osc133_records_prompt_mark() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;A\x07$ ");
    assert_eq!(t.prompt_marks().len(), 1);
}

#[test]
fn dcs_sixel_captures_image() {
    let mut t = Terminal::new(4, 20);
    // DCS q ... ST  with a red colour def + one full sixel column.
    t.advance(b"\x1bPq#0;2;100;0;0~\x1b\\");
    assert_eq!(t.images().len(), 1);
    assert_eq!(t.images()[0].image.height, 6);
}

#[test]
fn scrollback_cap_is_enforced() {
    let mut t = Terminal::with_scrollback(1, 4, 2);
    for i in 0..10 {
        t.advance(format!("{i}\r\n").as_bytes());
    }
    assert!(t.scrollback_len() <= 2, "history must not exceed the cap");
}

#[test]
fn set_max_scrollback_raises_the_cap_for_new_lines() {
    // The `scrollback_lines` config is applied to live terminals via this setter
    // (it was previously a dead setting — every pane was fixed at the default).
    let mut t = Terminal::with_scrollback(1, 4, 2);
    for i in 0..10 {
        t.advance(format!("{i}\r\n").as_bytes());
    }
    assert!(t.scrollback_len() <= 2, "starts capped at 2");
    t.set_max_scrollback(100);
    for i in 0..50 {
        t.advance(format!("L{i}\r\n").as_bytes());
    }
    assert!(
        t.scrollback_len() > 2,
        "raising the cap retains more history (got {})",
        t.scrollback_len()
    );
    assert!(t.scrollback_len() <= 100, "still bounded by the new cap");
}

#[test]
fn set_max_scrollback_lowers_the_cap_lazily() {
    let mut t = Terminal::with_scrollback(1, 4, 100);
    for i in 0..50 {
        t.advance(format!("{i}\r\n").as_bytes());
    }
    assert!(
        t.scrollback_len() > 5,
        "history accumulated under the high cap"
    );
    // Lowering is enforced as new lines push old ones out of history.
    t.set_max_scrollback(5);
    for i in 0..20 {
        t.advance(format!("N{i}\r\n").as_bytes());
    }
    assert!(
        t.scrollback_len() <= 5,
        "lowered cap enforced on subsequent output (got {})",
        t.scrollback_len()
    );
}

// ---- Audit finding #4: anchored-metadata Vecs are bounded under a flood ----

#[test]
fn hyperlink_vec_is_count_capped_under_flood() {
    let mut t = Terminal::with_scrollback(4, 80, 100_000);
    // Emit far more OSC 8 hyperlinks than the cap. Each carries a distinct
    // URI so none is de-duplicated; without the cap this Vec grows forever.
    let flood = Screen::HYPERLINKS_MAX + 5_000;
    for i in 0..flood {
        t.advance(format!("\x1b]8;;https://h/{i}\x07x\x1b]8;;\x07").as_bytes());
    }
    assert!(
        t.hyperlinks().len() <= Screen::HYPERLINKS_MAX,
        "hyperlinks must stay <= cap ({}), got {}",
        Screen::HYPERLINKS_MAX,
        t.hyperlinks().len()
    );
    // The most-recent URI is retained (ring-buffer keeps the newest).
    let last = format!("https://h/{}", flood - 1);
    assert_eq!(t.hyperlinks().last(), Some(&last));
}

#[test]
fn prompt_marks_vec_is_count_capped_under_flood() {
    // Huge scrollback so a unique `abs` is produced per mark (abs grows
    // monotonically with history.len()), exercising the count cap, not
    // the dedup path.
    let mut t = Terminal::with_scrollback(2, 8, 1_000_000);
    let flood = Screen::PROMPT_MARKS_MAX + 2_000;
    for _ in 0..flood {
        // A newline bumps history.len() so the next mark's abs differs.
        t.advance(b"\x1b]133;A\x07\r\n");
    }
    assert!(
        t.prompt_marks().len() <= Screen::PROMPT_MARKS_MAX,
        "prompt_marks must stay <= cap ({}), got {}",
        Screen::PROMPT_MARKS_MAX,
        t.prompt_marks().len()
    );
}

#[test]
fn command_marks_vec_is_count_capped_under_flood() {
    let mut t = Terminal::with_scrollback(2, 8, 1_000_000);
    let flood = Screen::COMMAND_MARKS_MAX + 2_000;
    for _ in 0..flood {
        // OSC 133 ; C marks output-start; newline keeps abs advancing.
        t.advance(b"\x1b]133;C\x07\r\n");
    }
    assert!(
        t.command_marks().len() <= Screen::COMMAND_MARKS_MAX,
        "command_marks must stay <= cap ({}), got {}",
        Screen::COMMAND_MARKS_MAX,
        t.command_marks().len()
    );
}

#[test]
fn images_vec_is_count_capped_under_flood() {
    let mut t = Terminal::with_scrollback(4, 20, 1_000_000);
    // Each minimal Sixel produces one TerminalImage. Flood past the cap.
    let flood = Screen::IMAGES_MAX + 500;
    for _ in 0..flood {
        t.advance(b"\x1bPq#0;2;100;0;0~\x1b\\");
        // Advance the cursor so successive images anchor at distinct rows.
        t.advance(b"\r\n");
    }
    assert!(
        t.images().len() <= Screen::IMAGES_MAX,
        "images must stay <= cap ({}), got {}",
        Screen::IMAGES_MAX,
        t.images().len()
    );
}

#[test]
fn images_vec_is_byte_capped_under_flood() {
    let mut t = Terminal::with_scrollback(4, 20, 1_000_000);
    // Flood enough small images that, were they all retained, total RGBA
    // bytes would exceed the byte cap. The byte cap evicts oldest first.
    for _ in 0..(Screen::IMAGES_MAX) {
        t.advance(b"\x1bPq#0;2;100;0;0~\x1b\\");
        t.advance(b"\r\n");
    }
    let total: usize = t.images().iter().map(|i| i.image.rgba.len()).sum();
    assert!(
        total <= Screen::IMAGES_MAX_BYTES,
        "retained image bytes ({total}) must stay <= byte cap ({})",
        Screen::IMAGES_MAX_BYTES
    );
}

#[test]
fn erase_scrollback_reanchors_and_drops_stale_marks() {
    // A small grid + scrollback. Build history, place a prompt mark on a
    // scrolled-off line and another on the live grid, then ESC[3J.
    let mut t = Terminal::with_scrollback(2, 8, 100);
    // Scroll some lines into history first.
    t.advance(b"a\r\nb\r\nc\r\nd\r\n");
    // Mark on the current live grid row (will survive the scrollback erase,
    // re-based to the new history length 0).
    t.advance(b"\x1b]133;A\x07");
    let before = t.prompt_marks().len();
    assert!(before >= 1, "expected at least one prompt mark");
    // Erase scrollback. The live-grid mark survives but re-bases so it is
    // still consistent with the now-zero history length.
    t.advance(b"\x1b[3J");
    assert_eq!(t.scrollback_len(), 0, "scrollback cleared");
    // Surviving marks must now sit within the live coordinate space
    // (history.len() + grid rows) — never dangling above it.
    let rows = t.grid().rows();
    for &m in t.prompt_marks() {
        assert!(
            m <= t.scrollback_len() + rows,
            "re-anchored prompt mark {m} out of live range"
        );
    }
}

#[test]
fn scrollback_eviction_rebases_anchored_marks() {
    // audit EN-2: routine scrollback eviction (history.pop_front over the cap)
    // must rebase the absolute `history.len()+row` anchors, exactly like a
    // scrollback CLEAR does — otherwise old prompt/image/command anchors dangle
    // (point at the wrong physical line) once enough plain scrolling evicts
    // scrollback. Previously only ESC[3J re-anchored.
    let mut t = Terminal::with_scrollback(2, 8, 3); // tiny: 3 history rows kept
                                                    // Mark the very first line as a prompt, then scroll far past the cap.
    t.advance(b"\x1b]133;A\x07first\r\n");
    assert_eq!(t.prompt_marks().len(), 1, "prompt mark recorded");
    for _ in 0..20 {
        t.advance(b"x\r\n");
    }
    // No surviving mark may dangle above the live coordinate space …
    let live = t.scrollback_len() + t.grid().rows();
    for &m in t.prompt_marks() {
        assert!(m < live, "anchor {m} dangles above live space {live}");
    }
    // … and the first-line mark scrolled off a 3-row scrollback, so it must be
    // DROPPED, not left pointing at a now-wrong absolute line (the bug kept it).
    assert!(
        t.prompt_marks().is_empty(),
        "the evicted first-line prompt mark must be dropped: {:?}",
        t.prompt_marks()
    );
}

#[test]
fn hard_reset_clears_anchored_metadata() {
    let mut t = Terminal::with_scrollback(2, 8, 100);
    t.advance(b"\x1b]8;;https://x\x07L\x1b]8;;\x07");
    t.advance(b"\x1b]133;A\x07");
    t.advance(b"\x1bPq#0;2;100;0;0~\x1b\\");
    assert!(!t.hyperlinks().is_empty());
    // RIS hard reset must drop all anchored metadata (grid + scrollback gone).
    t.advance(b"\x1bc");
    assert!(
        t.hyperlinks().is_empty(),
        "hyperlinks cleared on hard reset"
    );
    assert!(
        t.prompt_marks().is_empty(),
        "prompt_marks cleared on hard reset"
    );
    assert!(t.images().is_empty(), "images cleared on hard reset");
    assert!(
        t.command_marks().is_empty(),
        "command_marks cleared on hard reset"
    );
}

/// Deterministic robustness regression mirroring the `vt_parser` fuzz
/// target (see `fuzz/fuzz_targets/vt_parser.rs`). A terminal parser
/// consumes fully untrusted bytes; hostile or malformed escape sequences
/// must never panic, hang, or produce an inconsistent grid. These seeds
/// double as the fuzzer's regression corpus and run in the normal stable
/// test suite on every platform (the fuzz harness itself needs nightly).
#[test]
fn parser_survives_adversarial_escape_sequences() {
    let seeds: &[&[u8]] = &[
        b"\x1b[",                                  // bare CSI, no final byte
        b"\x1b[999999999999999999999999999m",      // CSI param overflow
        b"\x1b[;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;m", // many empty params
        b"\x1b[38;2;",                             // truncated truecolor SGR
        b"\x1b]0;",                                // OSC with no terminator
        b"\x1b]8;;",                               // OSC 8 hyperlink, truncated
        b"\x1b]52;c;",                             // OSC 52 clipboard, truncated
        b"\x1b]1337;File=",                        // iTerm2 image, truncated
        b"\x1bPq",                                 // DCS / Sixel introducer
        b"\x1b#8",                                 // DECALN screen-align
        b"\x08\x08\x08\x08",                       // backspaces past col 0
        b"\x1b[999999;999999H",                    // cursor move far OOB
        b"\x1b[2J\x1b[3J\x1b[1J\x1b[0J",           // erase-display variants
        b"\xff\xfe\xfd\xfc\x00\x01\x02",           // invalid UTF-8 / control bytes
        b"\xe2\x82",                               // truncated UTF-8 multibyte
        b"\x1b[6n\x1b[5n",                         // device status report queries
    ];

    for seed in seeds {
        let mut t = Terminal::with_scrollback(24, 80, 1000);
        // Feed in 1-byte chunks so sequences straddle advance() calls —
        // the realistic split-across-PTY-reads case.
        for b in seed.iter() {
            t.advance(&[*b]);
        }
        // Touch the derived read surface to catch read-side inconsistency.
        let _ = t.title();
        let _ = t.cwd();
        let _ = t.hyperlinks();
        let _ = t.images();
        let _ = t.display_rows();
        let _ = t.all_lines();
        let _ = t.scrollback_len();
        // The new mode-state read surface must also stay consistent.
        let _ = t.dec_modes();
        let _ = t.is_cursor_visible();
        let _ = t.alt_screen_active();
        let _ = t.mouse_mode();
        let _ = t.mouse_encoding();
        let _ = t.cursor_shape();
        let _ = t.cursor_blink();
    }
}

// ---- DEC private mode framework (item 1) ----

#[test]
fn dec_modes_default_state() {
    let t = Terminal::new(4, 20);
    let m = t.dec_modes();
    assert!(m.cursor_visible, "cursor visible by default");
    assert!(m.autowrap, "autowrap on by default");
    assert!(!m.bracketed_paste);
    assert!(!m.application_cursor_keys);
    assert!(!m.focus_reporting);
    assert!(!m.sync_output);
    assert_eq!(m.mouse_mode, MouseMode::Off);
    assert_eq!(m.mouse_encoding, MouseEncoding::X10);
}

#[test]
fn dec_mode_set_and_reset_cursor_visibility() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?25l");
    assert!(!t.is_cursor_visible(), "?25l hides the cursor");
    t.advance(b"\x1b[?25h");
    assert!(t.is_cursor_visible(), "?25h shows it again");
}

#[test]
fn dec_mode_multiple_params_in_one_sequence() {
    let mut t = Terminal::new(4, 20);
    // Enter alt screen AND select SGR mouse encoding in one CSI.
    t.advance(b"\x1b[?1049;1006h");
    assert!(t.alt_screen_active(), "1049 applied");
    assert_eq!(
        t.mouse_encoding(),
        MouseEncoding::Sgr,
        "1006 applied from the same sequence"
    );
}

#[test]
fn dec_mode_bracketed_paste_focus_sync() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?2004h\x1b[?1004h\x1b[?2026h");
    assert!(t.bracketed_paste());
    assert!(t.focus_reporting());
    assert!(t.sync_output());
    t.advance(b"\x1b[?2004l\x1b[?1004l\x1b[?2026l");
    assert!(!t.bracketed_paste());
    assert!(!t.focus_reporting());
    assert!(!t.sync_output());
}

#[test]
fn dec_mode_mouse_tracking_modes() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1000h");
    assert_eq!(t.mouse_mode(), MouseMode::Normal);
    t.advance(b"\x1b[?1002h");
    assert_eq!(t.mouse_mode(), MouseMode::ButtonEvent);
    t.advance(b"\x1b[?1003h");
    assert_eq!(t.mouse_mode(), MouseMode::AnyEvent);
    t.advance(b"\x1b[?1003l");
    assert_eq!(t.mouse_mode(), MouseMode::Off);
}

#[test]
fn dec_mode_application_cursor_keys_and_autowrap() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1h");
    assert!(t.application_cursor_keys());
    t.advance(b"\x1b[?7l");
    assert!(!t.autowrap());
    t.advance(b"\x1b[?7h");
    assert!(t.autowrap());
}

#[test]
fn dec_mode_unknown_number_is_ignored() {
    let mut t = Terminal::new(4, 20);
    // 9999 is not a mode we model; must not panic or disturb defaults.
    t.advance(b"\x1b[?9999h");
    assert!(t.is_cursor_visible());
    assert_eq!(t.mouse_mode(), MouseMode::Off);
}

// ---- Alternate screen (item 2) ----

#[test]
fn alt_screen_preserves_primary_content_and_cursor() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"primary"); // cursor now at col 7
    t.advance(b"\x1b[?1049h"); // enter alt
    assert!(t.alt_screen_active());
    // Alt screen starts blank.
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ');
    t.advance(b"ALTBUF");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'A');
    t.advance(b"\x1b[?1049l"); // leave alt
    assert!(!t.alt_screen_active());
    // Primary content is intact and the cursor was restored.
    assert!(t.grid().to_text().starts_with("primary"));
}

#[test]
fn alt_screen_does_not_pollute_scrollback() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    t.advance(b"\x1b[?1049h");
    // Scroll the alt screen well past its height.
    t.advance(b"a\r\nb\r\nc\r\nd\r\ne\r\nf");
    assert_eq!(
        t.scrollback_len(),
        0,
        "alt-screen scrolling must not feed scrollback"
    );
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn alt_screen_47_variant_switches_without_cursor_save() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"hi");
    t.advance(b"\x1b[?47h");
    assert!(t.alt_screen_active());
    t.advance(b"\x1b[?47l");
    assert!(!t.alt_screen_active());
    assert!(t.grid().to_text().starts_with("hi"));
}

#[test]
fn alt_screen_duplicate_enter_is_noop() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"base");
    t.advance(b"\x1b[?1049h");
    t.advance(b"XX");
    t.advance(b"\x1b[?1049h"); // second enter must not clobber the saved primary
    t.advance(b"\x1b[?1049l");
    assert!(t.grid().to_text().starts_with("base"));
}

// ---- Bracketed paste (item 3) ----

#[test]
fn bracketed_paste_flag_tracks_mode() {
    let mut t = Terminal::new(4, 20);
    assert!(!t.bracketed_paste());
    t.advance(b"\x1b[?2004h");
    assert!(t.bracketed_paste());
    t.advance(b"\x1b[?2004l");
    assert!(!t.bracketed_paste());
}

// ---- Cursor visibility (item 4) covered by dec_mode_set_and_reset_cursor_visibility ----

// ---- DECSCUSR cursor shape (item 5) ----

#[test]
fn decscusr_sets_shapes() {
    let cases: &[(&[u8], CursorShape, bool)] = &[
        (b"\x1b[0 q", CursorShape::Block, true),
        (b"\x1b[1 q", CursorShape::Block, true),
        (b"\x1b[2 q", CursorShape::Block, false),
        (b"\x1b[3 q", CursorShape::Underline, true),
        (b"\x1b[4 q", CursorShape::Underline, false),
        (b"\x1b[5 q", CursorShape::Bar, true),
        (b"\x1b[6 q", CursorShape::Bar, false),
    ];
    for (seq, shape, blink) in cases {
        let mut t = Terminal::new(2, 10);
        t.advance(seq);
        assert_eq!(t.cursor_shape(), *shape, "shape for {seq:?}");
        assert_eq!(t.cursor_blink(), *blink, "blink for {seq:?}");
    }
}

#[test]
fn decscusr_default_is_block() {
    let t = Terminal::new(2, 10);
    assert_eq!(t.cursor_shape(), CursorShape::Block);
}

#[test]
fn cursor_position_tracks_display_space() {
    let mut t = Terminal::new(4, 20);
    assert_eq!(t.cursor_position(), Some((0, 0)), "home at start");
    t.advance(b"hello");
    assert_eq!(t.cursor_position(), Some((0, 5)), "advanced 5 cols");
    t.advance(b"\r\nx");
    assert_eq!(t.cursor_position(), Some((1, 1)), "next row, 1 col");
    // CSI H homes the cursor.
    t.advance(b"\x1b[H");
    assert_eq!(t.cursor_position(), Some((0, 0)), "CUP home");
}

#[test]
fn dec_mode_12_drives_cursor_blink() {
    let mut t = Terminal::new(2, 10);
    // Steady block via DECSCUSR (blink=false), then ?12h enables blink.
    t.advance(b"\x1b[2 q");
    assert!(!t.cursor_blink());
    t.advance(b"\x1b[?12h");
    assert!(t.cursor_blink(), "?12h enables blink independently");
}

// ---- DECAWM autowrap behaviour ----

#[test]
fn autowrap_off_clamps_to_last_column() {
    let mut t = Terminal::new(3, 3);
    t.advance(b"\x1b[?7l"); // disable autowrap
    t.advance(b"abcd"); // 'd' overwrites the last cell instead of wrapping
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'a');
    assert_eq!(t.grid().cell(0, 2).unwrap().c, 'd', "last col overwritten");
    assert_eq!(t.grid().cell(1, 0).unwrap().c, ' ', "no wrap to next line");
}

// ---- Mouse encoding helper (item 6) ----

#[test]
fn encode_mouse_off_returns_none() {
    let t = Terminal::new(4, 20);
    let out = t.encode_mouse(
        MouseButton::Left,
        MouseModifiers::default(),
        5,
        7,
        MouseEventKind::Press,
    );
    assert!(out.is_none(), "no report when mouse mode is off");
}

#[test]
fn encode_mouse_sgr_left_press_and_release() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1000h\x1b[?1006h");
    let press = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            5,
            7,
            MouseEventKind::Press,
        )
        .unwrap();
    assert_eq!(press, b"\x1b[<0;5;7M");
    let release = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            5,
            7,
            MouseEventKind::Release,
        )
        .unwrap();
    assert_eq!(release, b"\x1b[<0;5;7m", "release uses lowercase final m");
}

#[test]
fn encode_mouse_sgr_modifiers_and_buttons() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1000h\x1b[?1006h");
    // Right button (2) + control (16) = 18.
    let out = t
        .encode_mouse(
            MouseButton::Right,
            MouseModifiers {
                control: true,
                ..Default::default()
            },
            1,
            1,
            MouseEventKind::Press,
        )
        .unwrap();
    assert_eq!(out, b"\x1b[<18;1;1M");
}

#[test]
fn encode_mouse_x10_press_offsets_by_32() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1000h"); // X10 encoding by default
    let out = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            1,
            1,
            MouseEventKind::Press,
        )
        .unwrap();
    // CSI M  Cb(0+32=32=' ')  Cx(1+32=33='!')  Cy(1+32=33='!')
    assert_eq!(out, b"\x1b[M !!");
}

#[test]
fn encode_mouse_x10_clamps_large_coords() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1000h");
    let out = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            1000,
            1000,
            MouseEventKind::Press,
        )
        .unwrap();
    // Coords clamp to 223; 223 + 32 = 255.
    assert_eq!(out[0], 0x1b);
    assert_eq!(&out[1..3], b"[M");
    assert_eq!(out[4], 255, "x clamps to 255");
    assert_eq!(out[5], 255, "y clamps to 255");
}

#[test]
fn encode_mouse_normal_mode_drops_motion() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1000h\x1b[?1006h");
    let out = t.encode_mouse(
        MouseButton::Left,
        MouseModifiers::default(),
        5,
        5,
        MouseEventKind::Motion,
    );
    assert!(out.is_none(), "?1000 reports buttons only, not motion");
}

#[test]
fn encode_mouse_button_event_motion_requires_button() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1002h\x1b[?1006h");
    // Motion with no button held → no report.
    assert!(t
        .encode_mouse(
            MouseButton::None,
            MouseModifiers::default(),
            3,
            3,
            MouseEventKind::Motion,
        )
        .is_none());
    // Motion while a button is held → reported (drag, +32 motion bit).
    let drag = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            3,
            3,
            MouseEventKind::Motion,
        )
        .unwrap();
    assert_eq!(drag, b"\x1b[<32;3;3M", "drag sets the motion bit (0+32)");
}

#[test]
fn encode_mouse_any_event_reports_bare_motion() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1003h\x1b[?1006h");
    let out = t
        .encode_mouse(
            MouseButton::None,
            MouseModifiers::default(),
            2,
            2,
            MouseEventKind::Motion,
        )
        .unwrap();
    // No button base = 3, + motion 32 = 35.
    assert_eq!(out, b"\x1b[<35;2;2M");
}

#[test]
fn encode_mouse_urxvt_encoding() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1000h\x1b[?1015h");
    let out = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            5,
            7,
            MouseEventKind::Press,
        )
        .unwrap();
    // urxvt: button offset by 32 → 32; decimal coords; final M.
    assert_eq!(out, b"\x1b[32;5;7M");
}

#[test]
fn encode_mouse_wheel_up_is_button_64() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1000h\x1b[?1006h");
    let out = t
        .encode_mouse(
            MouseButton::WheelUp,
            MouseModifiers::default(),
            1,
            1,
            MouseEventKind::Press,
        )
        .unwrap();
    assert_eq!(out, b"\x1b[<64;1;1M");
}

// ---- OSC 52 clipboard ----

#[test]
fn osc52_clipboard_write_decodes_base64() {
    let mut t = Terminal::new(4, 20);
    // "hello" base64 = aGVsbG8=
    t.advance(b"\x1b]52;c;aGVsbG8=\x07");
    let w = t.take_clipboard_write().expect("clipboard write");
    assert_eq!(w.selection, ClipboardSelection::Clipboard);
    assert_eq!(w.text, "hello");
    assert!(t.take_clipboard_write().is_none(), "drained once");
}

#[test]
fn osc52_clipboard_write_text_zeroized_on_demand() {
    // P-V3: a drained ClipboardWrite's sensitive `text` is wiped by
    // `zeroize()` — the buffer becomes empty AND its previously-occupied
    // backing bytes are scrubbed (verified via the raw heap pointer, the
    // canonical zeroize test). This is the same buffer the app drops after
    // copying to the OS clipboard, so the Drop impl scrubs it identically.
    let mut t = Terminal::new(4, 20);
    // "s3cr3t-token" base64.
    t.advance(b"\x1b]52;c;czNjcjN0LXRva2Vu\x07");
    let mut w = t.take_clipboard_write().expect("clipboard write");
    assert_eq!(w.text, "s3cr3t-token");

    let ptr = w.text.as_ptr();
    let len = w.text.len();
    assert!(len > 0);

    w.zeroize();

    // After zeroize the logical string is empty…
    assert!(w.text.is_empty(), "text must be emptied by zeroize");
    // …and the bytes that backed the secret are wiped to zero. The buffer
    // capacity is retained by zeroize::Zeroize for String, so the original
    // allocation is still valid to read here.
    // SAFETY: `ptr`/`len` describe the still-allocated backing buffer of
    // `w.text`; zeroize keeps the allocation (only sets len=0), so reading
    // `len` bytes from `ptr` is in-bounds. No aliasing: `w` is not borrowed.
    let wiped = unsafe { std::slice::from_raw_parts(ptr, len) };
    assert!(
        wiped.iter().all(|&b| b == 0),
        "secret bytes must be zeroed after zeroize(), got {wiped:?}"
    );
}

#[test]
fn hard_reset_clears_pending_clipboard_writes() {
    // P-V3: a hard reset (RIS, `ESC c`) scrubs any still-pending OSC 52
    // clipboard payloads the app had not yet drained.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]52;c;aGVsbG8=\x07"); // "hello" queued
                                          // RIS — full reset. Must wipe the pending queue.
    t.advance(b"\x1bc");
    assert!(
        t.take_clipboard_write().is_none(),
        "pending clipboard writes must be cleared by a hard reset"
    );
}

#[test]
fn osc52_primary_selection() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]52;p;aGVsbG8=\x07");
    let w = t.take_clipboard_write().unwrap();
    assert_eq!(w.selection, ClipboardSelection::Primary);
}

#[test]
fn osc52_read_default_off_replies_empty_and_leaks_nothing() {
    let mut t = Terminal::new(4, 20);
    // Read request: payload is '?'. Default-off -> the refusal is an OSC 52
    // reply with an EMPTY payload: no clipboard data crosses to the PTY, and
    // the asking program is not left blocking on a reply that never arrives.
    t.advance(b"\x1b]52;c;?\x07");
    assert!(t.take_clipboard_write().is_none());
    assert_eq!(
        t.take_pty_response(),
        b"\x1b]52;c;\x07".to_vec(),
        "must refuse with an empty payload, never host clipboard contents"
    );
    assert!(
        t.take_clipboard_reads().is_empty(),
        "a denied read must not park a request the host could later answer"
    );
}

#[test]
fn osc52_read_opt_in_uses_app_provided_text() {
    let mut t = Terminal::new(4, 20);
    // Even opted in, the core never reads the host clipboard from the OSC
    // sequence; the host must supply the text explicitly.
    t.set_clipboard_read_enabled(true);
    t.advance(b"\x1b]52;c;?\x07");
    assert!(
        t.take_pty_response().is_empty(),
        "the read request alone emits nothing"
    );
    t.respond_clipboard_read(ClipboardSelection::Clipboard, "hi");
    // "hi" base64 = aGk=
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b]52;c;aGk=\x07");
}

/// THE negative test. With the shipping default (reads denied), a program that
/// asks for the clipboard must never receive its contents — not from the parser,
/// and not even if the host layer erroneously tries to answer. Asserted on the
/// bytes that actually reach the PTY.
#[test]
fn osc52_read_denied_by_default_never_leaks_clipboard_bytes_to_pty() {
    const SECRET: &str = "hunter2-api-token";
    // base64("hunter2-api-token"); the exact substring that must never appear.
    let secret_b64 = super::osc::base64_encode(SECRET.as_bytes());

    let mut t = Terminal::new(4, 20);
    assert!(
        !t.clipboard_read_enabled(),
        "the shipping default must deny clipboard reads"
    );

    // A hostile program asks for the clipboard, on both selections and via the
    // empty-selection form (which defaults to the system clipboard).
    t.advance(b"\x1b]52;c;?\x07");
    t.advance(b"\x1b]52;p;?\x07");
    t.advance(b"\x1b]52;;?\x07");

    // The host has the secret on its clipboard and — simulating a wiring bug —
    // tries to answer anyway. The core must refuse: `respond_clipboard_read` is
    // gated on the same flag, so no amount of host-side eagerness can leak.
    assert!(
        t.take_clipboard_reads().is_empty(),
        "a denied read must never be surfaced to the host as a servable request"
    );
    t.respond_clipboard_read(ClipboardSelection::Clipboard, SECRET);
    t.respond_clipboard_read(ClipboardSelection::Primary, SECRET);

    let wire = t.take_pty_response();
    let wire_str = String::from_utf8(wire.clone()).expect("replies are ASCII");

    // Only the three empty-payload refusals reached the PTY.
    assert_eq!(
        wire_str, "\x1b]52;c;\x07\x1b]52;p;\x07\x1b]52;c;\x07",
        "only empty-payload refusals may reach the PTY"
    );
    // …and the secret is absent in every representation.
    assert!(
        !wire_str.contains(&secret_b64),
        "base64 of the clipboard secret must never reach the PTY"
    );
    assert!(
        !wire_str.contains(SECRET),
        "the clipboard secret must never reach the PTY in plaintext either"
    );
}

/// The refusal must not be silence: a program blocking on the reply has to be
/// released. Distinguishes "denied" from "ignored" on the wire.
#[test]
fn osc52_denied_read_answers_rather_than_hanging_the_caller() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]52;c;?\x07");
    let wire = t.take_pty_response();
    assert!(
        !wire.is_empty(),
        "silence would leave the requesting program blocked until its own timeout"
    );
    // A well-formed OSC 52 reply the caller can parse: OSC, 52, selection,
    // empty data field, terminator.
    assert!(wire.starts_with(b"\x1b]52;"), "must be an OSC 52 reply");
    assert!(wire.ends_with(b"\x07"), "must be terminated");
    assert_eq!(wire, b"\x1b]52;c;\x07".to_vec());
}

/// The refusal echoes the selection the program asked about, so a caller that
/// queried the primary selection matches the reply to its own request.
#[test]
fn osc52_denied_read_echoes_the_requested_selection() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]52;p;?\x07");
    assert_eq!(t.take_pty_response(), b"\x1b]52;p;\x07".to_vec());
}

/// When opted in, the request is surfaced to the host with the selection the
/// program asked for — that is what lets the host read the RIGHT selection.
#[test]
fn osc52_opt_in_surfaces_request_with_selection() {
    let mut t = Terminal::new(4, 20);
    t.set_clipboard_read_enabled(true);
    t.advance(b"\x1b]52;p;?\x07");
    t.advance(b"\x1b]52;c;?\x07");
    let reqs = t.take_clipboard_reads();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].selection, ClipboardSelection::Primary);
    assert_eq!(reqs[1].selection, ClipboardSelection::Clipboard);
    assert!(
        t.take_clipboard_reads().is_empty(),
        "drained once, like the write queue"
    );
}

/// A program spamming read queries must not grow the pending queue without
/// bound (same discipline as every other PTY-driven buffer).
#[test]
fn osc52_pending_reads_are_bounded() {
    let mut t = Terminal::new(4, 20);
    t.set_clipboard_read_enabled(true);
    for _ in 0..500 {
        t.advance(b"\x1b]52;c;?\x07");
    }
    let n = t.take_clipboard_reads().len();
    assert!(
        n <= 16,
        "pending read queue must stay bounded, got {n} entries"
    );
    assert!(n > 0, "…while still retaining the most recent requests");
}

/// Turning the setting back OFF while a request is in flight must refuse it,
/// not silently forget it — otherwise the blocked program never gets an answer.
#[test]
fn osc52_disabling_reads_refuses_requests_still_in_flight() {
    let mut t = Terminal::new(4, 20);
    t.set_clipboard_read_enabled(true);
    t.advance(b"\x1b]52;c;?\x07");
    let _ = t.take_pty_response(); // nothing yet; the request is parked

    t.set_clipboard_read_enabled(false);
    assert_eq!(
        t.take_pty_response(),
        b"\x1b]52;c;\x07".to_vec(),
        "the in-flight request must be answered with an empty payload on disable"
    );
    assert!(
        t.take_clipboard_reads().is_empty(),
        "and must no longer be servable"
    );
}

/// A hard reset (RIS) must likewise refuse rather than strand a parked request.
#[test]
fn hard_reset_refuses_pending_clipboard_reads() {
    let mut t = Terminal::new(4, 20);
    t.set_clipboard_read_enabled(true);
    t.advance(b"\x1b]52;c;?\x07");
    let _ = t.take_pty_response();
    t.advance(b"\x1bc"); // RIS
    assert_eq!(
        t.take_pty_response(),
        b"\x1b]52;c;\x07".to_vec(),
        "RIS must answer, not strand, a pending read"
    );
    assert!(t.take_clipboard_reads().is_empty());
}

// ---- OSC 4 / 10 / 11 / 12 colors ----

#[test]
fn osc4_set_indexed_color() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]4;1;rgb:ff/00/00\x07");
    assert_eq!(t.palette_color(1), (255, 0, 0));
    assert_eq!(
        t.take_color_sets(),
        vec![ColorSet::Indexed {
            index: 1,
            rgb: (255, 0, 0)
        }]
    );
}

#[test]
fn osc4_query_emits_reply() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]4;1;rgb:ff/00/00\x07");
    let _ = t.take_color_sets();
    t.advance(b"\x1b]4;1;?\x07");
    assert_eq!(
        t.take_pty_response().as_slice(),
        b"\x1b]4;1;rgb:ffff/0000/0000\x07"
    );
}

#[test]
fn osc11_background_query_emits_reply() {
    let mut t = Terminal::new(4, 20);
    // Default background is xterm black -> rgb:0000/0000/0000
    t.advance(b"\x1b]11;?\x07");
    assert_eq!(
        t.take_pty_response().as_slice(),
        b"\x1b]11;rgb:0000/0000/0000\x07"
    );
}

#[test]
fn osc10_foreground_set() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]10;rgb:12/34/56\x07");
    assert_eq!(
        t.dynamic_color(DynamicColor::Foreground),
        (0x12, 0x34, 0x56)
    );
    assert_eq!(
        t.take_color_sets(),
        vec![ColorSet::Dynamic {
            which: DynamicColor::Foreground,
            rgb: (0x12, 0x34, 0x56)
        }]
    );
}

#[test]
fn osc12_cursor_set() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]12;rgb:00/ff/00\x07");
    assert_eq!(t.dynamic_color(DynamicColor::Cursor), (0, 255, 0));
}

// ---- OSC 104 / 110-112 reset ----

#[test]
fn osc104_reset_single_index() {
    let mut t = Terminal::new(4, 20);
    let original = t.palette_color(2);
    t.advance(b"\x1b]4;2;rgb:ff/00/00\x07");
    let _ = t.take_color_sets();
    assert_eq!(t.palette_color(2), (255, 0, 0));
    t.advance(b"\x1b]104;2\x07");
    assert_eq!(t.palette_color(2), original);
}

#[test]
fn osc104_reset_all() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]4;5;rgb:ff/00/00\x07");
    let _ = t.take_color_sets();
    t.advance(b"\x1b]104\x07");
    assert_ne!(t.palette_color(5), (255, 0, 0));
    assert_eq!(
        t.take_color_sets().len(),
        256,
        "every entry reset is surfaced"
    );
}

#[test]
fn osc110_reset_foreground() {
    let mut t = Terminal::new(4, 20);
    let original = t.dynamic_color(DynamicColor::Foreground);
    t.advance(b"\x1b]10;rgb:ff/ff/ff\x07");
    let _ = t.take_color_sets();
    t.advance(b"\x1b]110\x07");
    assert_eq!(t.dynamic_color(DynamicColor::Foreground), original);
}

// ---- OSC 9 / 777 notifications ----

#[test]
fn osc9_notification() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]9;Build complete\x07");
    let n = t.take_notification().expect("notification");
    assert_eq!(n.title, "");
    assert_eq!(n.body, "Build complete");
}

#[test]
fn osc777_notification() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]777;notify;Title;Body text\x07");
    let n = t.take_notification().unwrap();
    assert_eq!(n.title, "Title");
    assert_eq!(n.body, "Body text");
}

// ---- Title stack (XTWINOPS 22/23) ----

#[test]
fn title_stack_push_pop() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]2;First\x07");
    assert_eq!(t.title(), "First");
    t.advance(b"\x1b[22;0t"); // push
    assert_eq!(t.title_stack_depth(), 1);
    t.advance(b"\x1b]2;Second\x07");
    assert_eq!(t.title(), "Second");
    t.advance(b"\x1b[23;0t"); // pop
    assert_eq!(t.title(), "First");
    assert_eq!(t.title_stack_depth(), 0);
}

#[test]
fn title_stack_pop_empty_is_noop() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]2;Only\x07");
    t.advance(b"\x1b[23;0t"); // pop with empty stack
    assert_eq!(t.title(), "Only");
}

// ---- OSC 133 still never replies (regression guard) ----

#[test]
fn osc133_never_writes_pty_response() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;A\x07");
    assert!(
        t.take_pty_response().is_empty(),
        "OSC 133 marks must remain capture-only (anti-CVE)"
    );
}

// ---- Kitty graphics protocol (APC extraction + decode + placement) ----

#[test]
fn kitty_apc_displays_image_and_passes_text_through() {
    let mut t = Terminal::new(4, 20);
    // "hi" then a Kitty APC (a defaults to T) for a 1x1 RGBA image
    // (payload [1,2,3,4] = base64 "AQIDBA=="), ST-terminated, then "ok".
    t.advance(b"hi\x1b_Gf=32,s=1,v=1;AQIDBA==\x1b\\ok");
    // Exactly one image at the cursor.
    assert_eq!(t.images().len(), 1, "one image produced");
    let img = &t.images()[0].image;
    assert_eq!(img.width, 1);
    assert_eq!(img.height, 1);
    assert_eq!(&img.rgba, &[1, 2, 3, 4]);
    // The non-APC text reaches the grid intact ("hiok").
    assert!(
        t.grid().to_text().starts_with("hiok"),
        "non-APC bytes must reach the grid: got {:?}",
        t.grid().to_text()
    );
}

#[test]
fn kitty_apc_split_across_two_advances() {
    let mut t = Terminal::new(4, 20);
    // Cut the APC mid-payload across two advance() calls.
    t.advance(b"hi\x1b_Gf=32,s=1,v=1;AQID");
    // Nothing finalised yet; "hi" is on the grid, no image.
    assert_eq!(t.images().len(), 0, "APC not yet terminated");
    t.advance(b"BA==\x1b\\ok");
    assert_eq!(t.images().len(), 1, "one image after the second chunk");
    assert_eq!(&t.images()[0].image.rgba, &[1, 2, 3, 4]);
    assert!(
        t.grid().to_text().starts_with("hiok"),
        "no stray APC bytes leak to the grid: got {:?}",
        t.grid().to_text()
    );
}

#[test]
fn kitty_apc_bel_terminated() {
    let mut t = Terminal::new(4, 20);
    // BEL (0x07) terminates the APC instead of ST.
    t.advance(b"x\x1b_Gf=32,s=1,v=1;AQIDBA==\x07y");
    assert_eq!(t.images().len(), 1, "BEL-terminated APC produces an image");
    assert_eq!(&t.images()[0].image.rgba, &[1, 2, 3, 4]);
    assert!(t.grid().to_text().starts_with("xy"));
}

#[test]
fn kitty_chunked_transmission_m_flag() {
    let mut t = Terminal::new(4, 20);
    // 1x1 RGBA split into two base64 chunks via m=1 / m=0, same id.
    // "AQID" then "BA==" together decode to [1,2,3,4].
    t.advance(b"\x1b_Gf=32,s=1,v=1,i=9,m=1;AQID\x1b\\");
    assert_eq!(t.images().len(), 0, "more=1 chunk does not finalise");
    t.advance(b"\x1b_Ga=T,i=9,m=0;BA==\x1b\\");
    assert_eq!(t.images().len(), 1, "m=0 finalises the chunked image");
    assert_eq!(&t.images()[0].image.rgba, &[1, 2, 3, 4]);
}

#[test]
fn kitty_transmit_only_then_display() {
    let mut t = Terminal::new(4, 20);
    // a=t stores the image without displaying it.
    t.advance(b"\x1b_Ga=t,f=32,s=1,v=1,i=5;AQIDBA==\x1b\\");
    assert_eq!(t.images().len(), 0, "a=t must not display");
    // a=p displays the previously-stored id.
    t.advance(b"\x1b_Ga=p,i=5\x1b\\");
    assert_eq!(t.images().len(), 1, "a=p displays the stored image");
    assert_eq!(&t.images()[0].image.rgba, &[1, 2, 3, 4]);
}

#[test]
fn kitty_delete_clears_storage() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b_Ga=t,f=32,s=1,v=1,i=3;AQIDBA==\x1b\\");
    // a=d clears the store; a later a=p finds nothing.
    t.advance(b"\x1b_Ga=d\x1b\\");
    t.advance(b"\x1b_Ga=p,i=3\x1b\\");
    assert_eq!(t.images().len(), 0, "a=d cleared the stored image");
}

#[test]
fn kitty_f24_rgb_displayed_as_rgba() {
    let mut t = Terminal::new(4, 20);
    // 1x1 RGB (3 bytes [10,20,30] = base64 "ChQe"); expands to RGBA.
    t.advance(b"\x1b_Gf=24,s=1,v=1;ChQe\x1b\\");
    assert_eq!(t.images().len(), 1);
    assert_eq!(&t.images()[0].image.rgba, &[10, 20, 30, 255]);
}

#[test]
fn non_kitty_apc_is_swallowed_and_text_survives() {
    let mut t = Terminal::new(4, 20);
    // A non-graphics APC (no leading G) is swallowed (matching vte); the
    // surrounding text still reaches the grid and no image is produced.
    t.advance(b"a\x1b_Xsome-other-apc\x1b\\b");
    assert_eq!(t.images().len(), 0);
    assert!(
        t.grid().to_text().starts_with("ab"),
        "text around a non-kitty APC survives: got {:?}",
        t.grid().to_text()
    );
}

#[test]
fn esc_not_introducing_apc_passes_through_intact() {
    let mut t = Terminal::new(4, 20);
    // A plain SGR escape (ESC not followed by '_') must reach vte intact.
    t.advance(b"\x1b[31mR");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'R');
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Indexed(1));
    assert_eq!(t.images().len(), 0);
}

// ============================================================
// VT correctness P0 batch (C1-C8)
// ============================================================

// ---- C1: DA1 / DA2 device attributes ----

#[test]
fn da1_primary_device_attributes_reply() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[c");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[?62;1;6;22c");
}

#[test]
fn da1_with_explicit_zero_param_replies() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[0c");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[?62;1;6;22c");
}

#[test]
fn da2_secondary_device_attributes_reply() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[>c");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[>0;0;0c");
}

// ---- C2: DSR / CPR ----

#[test]
fn dsr_status_report_ok() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[5n");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[0n");
}

#[test]
fn cpr_reports_one_based_cursor_position() {
    let mut t = Terminal::new(10, 40);
    // Move cursor to row 3, col 7 (0-based 2,6) then request CPR.
    t.advance(b"\x1b[3;7H\x1b[6n");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[3;7R");
}

#[test]
fn cpr_after_printing_text() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"hello\x1b[6n"); // cursor at row1 col6 (1-based)
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[1;6R");
}

// ---- C3: IL / DL / ICH / DCH / ECH ----

#[test]
fn ich_inserts_blanks_shifting_right() {
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcdef\x1b[H"); // fill row 0, home
    t.advance(b"\x1b[3G"); // move to col 3 (1-based) = 0-based 2 ('c')
    t.advance(b"\x1b[2@"); // insert 2 blanks
    let line: String = (0..6).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(line, "ab  cd");
}

#[test]
fn dch_deletes_chars_shifting_left() {
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcdef\x1b[H");
    t.advance(b"\x1b[3G\x1b[2P"); // at col 3, delete 2 chars
    let line: String = (0..6).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(line, "abef  ");
}

#[test]
fn ech_erases_chars_without_shift() {
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcdef\x1b[H");
    t.advance(b"\x1b[3G\x1b[2X"); // at col 3, erase 2
    let line: String = (0..6).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(line, "ab  ef");
}

#[test]
fn il_inserts_lines_within_scroll_region() {
    let mut t = Terminal::new(4, 3);
    t.advance(b"aaa\r\nbbb\r\nccc\r\nddd");
    // Cursor to row 2 (1-based), insert 1 line.
    t.advance(b"\x1b[2;1H\x1b[L");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'a');
    assert_eq!(
        t.grid().cell(1, 0).unwrap().c,
        ' ',
        "blank inserted at row 2"
    );
    assert_eq!(t.grid().cell(2, 0).unwrap().c, 'b', "bbb shifted down");
    assert_eq!(
        t.grid().cell(3, 0).unwrap().c,
        'c',
        "ccc shifted down; ddd lost"
    );
}

#[test]
fn dl_deletes_lines_within_scroll_region() {
    let mut t = Terminal::new(4, 3);
    t.advance(b"aaa\r\nbbb\r\nccc\r\nddd");
    // Cursor to row 2, delete 1 line.
    t.advance(b"\x1b[2;1H\x1b[M");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'a');
    assert_eq!(t.grid().cell(1, 0).unwrap().c, 'c', "ccc shifted up");
    assert_eq!(t.grid().cell(2, 0).unwrap().c, 'd', "ddd shifted up");
    assert_eq!(t.grid().cell(3, 0).unwrap().c, ' ', "blank at bottom");
}

#[test]
fn il_dl_respect_custom_scroll_region() {
    let mut t = Terminal::new(5, 3);
    t.advance(b"r0\r\nr1\r\nr2\r\nr3\r\nr4");
    // Region rows 2..4 (1-based), i.e. 0-based 1..3.
    t.advance(b"\x1b[2;4r");
    // After DECSTBM the cursor is homed to the top margin (row 1, 0-based).
    // Delete 1 line at the top of the region.
    t.advance(b"\x1b[M");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'r', "row 0 untouched");
    assert_eq!(t.grid().cell(0, 1).unwrap().c, '0');
    assert_eq!(
        t.grid().cell(1, 1).unwrap().c,
        '2',
        "r2 shifted up into region top"
    );
    assert_eq!(
        t.grid().cell(3, 1).unwrap().c,
        ' ',
        "blank at region bottom"
    );
    assert_eq!(
        t.grid().cell(4, 1).unwrap().c,
        '4',
        "row 4 below region untouched"
    );
}

// ---- C4: DECSC / DECRC + SCOSC / SCORC ----

#[test]
fn decsc_decrc_round_trips_cursor() {
    let mut t = Terminal::new(6, 20);
    t.advance(b"\x1b[3;5H"); // row 3 col 5 (1-based)
    t.advance(b"\x1b7"); // DECSC save
    t.advance(b"\x1b[1;1H"); // home
    assert_eq!(t.cursor_position(), Some((0, 0)));
    t.advance(b"\x1b8"); // DECRC restore
    assert_eq!(t.cursor_position(), Some((2, 4)), "restored to row3,col5");
}

#[test]
fn scosc_scorc_aliases_save_restore() {
    let mut t = Terminal::new(6, 20);
    t.advance(b"\x1b[4;3H\x1b[s"); // CSI s save
    t.advance(b"\x1b[1;1H\x1b[u"); // home then CSI u restore
    assert_eq!(t.cursor_position(), Some((3, 2)));
}

#[test]
fn decsc_saves_pen() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[31m\x1b7"); // red pen, save
    t.advance(b"\x1b[0m"); // reset pen
    t.advance(b"\x1b8X"); // restore -> prints red X
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Indexed(1));
}

// ---- C5: DECSTBM scroll region ----

#[test]
fn decstbm_constrains_scrolling() {
    let mut t = Terminal::new(4, 3);
    // Region = rows 1..2 (1-based) = 0-based 0..1.
    t.advance(b"\x1b[1;2r");
    // Fill the region and force a scroll: rows below the region stay put.
    t.advance(b"x3\r\n"); // row3 marker first
                          // Reset region to write a fixed bottom line, then re-set region.
    t.advance(b"\x1b[1;4r\x1b[4;1Hbot\x1b[1;2r\x1b[1;1H");
    // Now scroll within region 0..1 by printing 3 lines.
    t.advance(b"AA\r\nBB\r\nCC");
    // Region top should now hold BB (AA scrolled out of the 2-row region).
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'B');
    assert_eq!(t.grid().cell(1, 0).unwrap().c, 'C');
    // The fixed bottom line outside the region is preserved.
    let bottom: String = (0..3).map(|c| t.grid().cell(3, c).unwrap().c).collect();
    assert_eq!(bottom, "bot");
}

#[test]
fn decstbm_no_params_resets_full_screen() {
    let mut t = Terminal::new(3, 3);
    t.advance(b"\x1b[1;2r"); // custom region
    t.advance(b"\x1b[r"); // reset
                          // Full-screen scroll feeds scrollback again.
    let mut t2 = Terminal::with_scrollback(3, 3, 100);
    t2.advance(b"\x1b[1;2r\x1b[r");
    t2.advance(b"a\r\nb\r\nc\r\nd");
    assert!(
        t2.scrollback_len() >= 1,
        "full region feeds scrollback after reset"
    );
    let _ = t;
}

// ---- C6: DEC line-drawing charset ----

#[test]
fn dec_line_drawing_maps_box_chars() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b(0"); // select DEC special graphics into G0
    t.advance(b"lqk"); // upper-left, horiz, upper-right
    assert_eq!(t.grid().cell(0, 0).unwrap().c, '\u{250c}'); // ┌
    assert_eq!(t.grid().cell(0, 1).unwrap().c, '\u{2500}'); // ─
    assert_eq!(t.grid().cell(0, 2).unwrap().c, '\u{2510}'); // ┐
}

#[test]
fn esc_paren_b_returns_to_ascii() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b(0q\x1b(Bq"); // graphics q (─) then ASCII q
    assert_eq!(t.grid().cell(0, 0).unwrap().c, '\u{2500}');
    assert_eq!(t.grid().cell(0, 1).unwrap().c, 'q', "ASCII restored");
}

#[test]
fn si_so_switch_g0_g1() {
    let mut t = Terminal::new(2, 10);
    // G0 = ASCII (default), G1 = line-drawing.
    t.advance(b"\x1b)0"); // designate G1 = graphics
    t.advance(b"q"); // GL=G0=ASCII -> 'q'
    t.advance(b"\x0eq"); // SO -> GL=G1=graphics -> ─
    t.advance(b"\x0fq"); // SI -> back to G0 ASCII -> 'q'
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'q');
    assert_eq!(t.grid().cell(0, 1).unwrap().c, '\u{2500}');
    assert_eq!(t.grid().cell(0, 2).unwrap().c, 'q');
}

// ---- C7: wide-cell width ----

#[test]
fn wide_char_advances_two_columns() {
    let mut t = Terminal::new(2, 10);
    t.advance("世".as_bytes()); // East-Asian wide
                                // Occupies cols 0 + 1 (continuation spacer); cursor now at col 2.
    assert_eq!(t.grid().cell(0, 0).unwrap().c, '世');
    assert_eq!(t.grid().cell(0, 1).unwrap().c, ' ', "continuation spacer");
    assert_eq!(t.cursor_position(), Some((0, 2)));
}

#[test]
fn wide_char_then_narrow() {
    let mut t = Terminal::new(2, 10);
    t.advance("世a".as_bytes());
    assert_eq!(t.grid().cell(0, 0).unwrap().c, '世');
    assert_eq!(t.grid().cell(0, 2).unwrap().c, 'a', "narrow lands at col 2");
}

#[test]
fn wide_char_wraps_at_last_column() {
    let mut t = Terminal::new(2, 3); // 3 cols
    t.advance(b"ab"); // cols 0,1 filled; cursor at col 2 (last)
    t.advance("世".as_bytes()); // can't fit width-2 at col 2 -> wraps to row 1
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'a');
    assert_eq!(t.grid().cell(0, 1).unwrap().c, 'b');
    assert_eq!(
        t.grid().cell(1, 0).unwrap().c,
        '世',
        "wide char wrapped to next row"
    );
    assert_eq!(t.grid().cell(1, 1).unwrap().c, ' ');
}

// ---- C8: ED / EL sub-modes ----

#[test]
fn ed_mode0_erases_cursor_to_end() {
    let mut t = Terminal::new(2, 4);
    t.advance(b"abcd\r\nefgh");
    t.advance(b"\x1b[1;3H\x1b[0J"); // row1 col3, erase to end
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'a');
    assert_eq!(t.grid().cell(0, 1).unwrap().c, 'b');
    assert_eq!(t.grid().cell(0, 2).unwrap().c, ' ', "from cursor erased");
    assert_eq!(t.grid().cell(1, 0).unwrap().c, ' ', "rows below erased");
}

#[test]
fn ed_mode1_erases_start_to_cursor() {
    let mut t = Terminal::new(2, 4);
    t.advance(b"abcd\r\nefgh");
    t.advance(b"\x1b[2;2H\x1b[1J"); // row2 col2, erase start->cursor
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ', "row above erased");
    assert_eq!(t.grid().cell(1, 0).unwrap().c, ' ');
    assert_eq!(t.grid().cell(1, 1).unwrap().c, ' ', "cursor cell inclusive");
    assert_eq!(t.grid().cell(1, 2).unwrap().c, 'g', "after cursor kept");
}

#[test]
fn ed_mode3_clears_scrollback() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    t.advance(b"L0\r\nL1\r\nL2\r\nL3");
    assert!(t.scrollback_len() > 0);
    t.advance(b"\x1b[3J");
    assert_eq!(t.scrollback_len(), 0, "ESC[3J clears scrollback");
}

#[test]
fn clear_scrollback_matches_ed3_and_keeps_live_grid() {
    // The `clear_scrollback()` API must be byte-for-byte equivalent to feeding
    // `ESC [ 3 J` (ED-3): scrollback dropped, view snapped to live output, and
    // the live grid content preserved. Drive two identical terminals down each
    // path and assert they agree.
    let mut a = Terminal::with_scrollback(2, 4, 100);
    let mut b = Terminal::with_scrollback(2, 4, 100);
    a.advance(b"L0\r\nL1\r\nL2\r\nL3");
    b.advance(b"L0\r\nL1\r\nL2\r\nL3");
    assert!(a.scrollback_len() > 0 && b.scrollback_len() > 0);

    // Scroll `a` up into the scrollback so we also prove the view re-snaps.
    a.set_view_offset(a.scrollback_len());
    assert!(a.view_offset() > 0, "scrolled up before clearing");

    a.clear_scrollback(); // the new API
    b.advance(b"\x1b[3J"); // the pre-existing ED-3 path

    assert_eq!(a.scrollback_len(), 0, "clear_scrollback drops scrollback");
    assert_eq!(a.view_offset(), 0, "view snaps back to live output");
    assert_eq!(
        a.scrollback_len(),
        b.scrollback_len(),
        "clear_scrollback matches ED-3 scrollback state"
    );
    // The live grid survives (both keep the last two lines).
    assert_eq!(
        a.grid().to_text(),
        b.grid().to_text(),
        "live grid preserved"
    );
    assert_eq!(a.grid().cell(1, 0).unwrap().c, 'L', "L3 still on screen");
    assert_eq!(a.grid().cell(1, 1).unwrap().c, '3');
}

#[test]
fn buffer_text_returns_whole_buffer_trimmed() {
    // Copy-all payload: the ENTIRE buffer (scrollback history + live grid), rows
    // joined with `\n`, trailing whitespace + trailing blank rows dropped.
    let mut t = Terminal::with_scrollback(2, 8, 100);
    t.advance(b"L0\r\nL1\r\nL2\r\nL3");
    // L0/L1 are in scrollback (2-row grid), L2/L3 on screen: all four must show.
    let text = t.buffer_text().expect("non-empty buffer yields text");
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        vec!["L0", "L1", "L2", "L3"],
        "buffer_text spans scrollback + live grid in order, no blank tail"
    );
    assert!(
        !text.ends_with('\n'),
        "no trailing newline (rows are joined, not terminated)"
    );
}

#[test]
fn buffer_text_is_none_for_a_blank_screen() {
    // An untouched terminal holds only blank cells → no copy-all payload.
    let t = Terminal::new(4, 10);
    assert!(t.buffer_text().is_none(), "blank buffer copies nothing");
}

#[test]
fn buffer_text_skips_the_wide_glyph_continuation_spacer() {
    // A width-2 glyph (東亜 wide) occupies two cells: the glyph then a blank
    // continuation spacer. Copy-all must emit the glyph exactly ONCE with no
    // stray spacer, so the payload matches drawn text (mirrors copy_row_text).
    let mut t = Terminal::new(2, 8);
    t.advance("世X".as_bytes()); // 世 = cols 0-1 (glyph + spacer), X = col 2
    let text = t.buffer_text().expect("non-empty buffer yields text");
    assert_eq!(
        text.lines().next(),
        Some("世X"),
        "the continuation spacer after a wide glyph is skipped (no `世 X`)"
    );
    // Belt-and-suspenders: exactly one glyph, no interior spacer.
    assert!(
        !text.contains("世 "),
        "no stray spacer emitted after the wide glyph"
    );
    assert_eq!(text.matches('世').count(), 1, "the wide glyph appears once");
}

#[test]
fn el_mode1_erases_bol_to_cursor() {
    let mut t = Terminal::new(2, 5);
    t.advance(b"abcde");
    t.advance(b"\x1b[1;3H\x1b[1K"); // col3, erase BOL->cursor
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ');
    assert_eq!(t.grid().cell(0, 1).unwrap().c, ' ');
    assert_eq!(t.grid().cell(0, 2).unwrap().c, ' ', "cursor inclusive");
    assert_eq!(t.grid().cell(0, 3).unwrap().c, 'd', "after cursor kept");
}

#[test]
fn el_mode2_erases_whole_line() {
    let mut t = Terminal::new(2, 5);
    t.advance(b"abcde\x1b[1;3H\x1b[2K");
    for c in 0..5 {
        assert_eq!(t.grid().cell(0, c).unwrap().c, ' ');
    }
}

// ---- C9/C10 bonus: ESC M reverse index, RIS, DECSTR ----

#[test]
fn reverse_index_scrolls_region_down_at_top() {
    let mut t = Terminal::new(3, 3);
    t.advance(b"aaa\r\nbbb\r\nccc");
    t.advance(b"\x1b[1;1H\x1bM"); // home then RI -> scroll down
    assert_eq!(
        t.grid().cell(0, 0).unwrap().c,
        ' ',
        "blank scrolled in at top"
    );
    assert_eq!(t.grid().cell(1, 0).unwrap().c, 'a', "aaa pushed down");
}

#[test]
fn ris_resets_terminal() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    t.advance(b"junk\r\nmore\r\noverflow\x1b[31m");
    t.advance(b"\x1bc"); // RIS
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ', "screen cleared");
    assert_eq!(t.scrollback_len(), 0, "scrollback cleared");
    assert_eq!(t.cursor_position(), Some((0, 0)));
    t.advance(b"x"); // pen reset -> default fg
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Default);
}

#[test]
fn decstr_soft_reset_keeps_scrollback() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    t.advance(b"L0\r\nL1\r\nL2");
    let hist = t.scrollback_len();
    t.advance(b"\x1b[!p"); // DECSTR soft reset
    assert_eq!(t.scrollback_len(), hist, "soft reset preserves scrollback");
    assert_eq!(t.cursor_position(), Some((0, 0)));
}

// ---- Bonus: REP, CHA/VPA absolute moves ----

#[test]
fn rep_repeats_last_char() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"x\x1b[3b"); // print x, repeat 3 more
    let line: String = (0..4).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(line, "xxxx");
}

#[test]
fn rep_count_is_clamped_no_dos() {
    // An attacker-controlled huge REP count must NOT spin billions of times
    // (the iTerm2 REP DoS). The clamp bounds it to <= MAX_REP (~1M) so this
    // returns promptly; we just assert it completes and the grid is sane.
    let mut t = Terminal::with_scrollback(24, 80, 1000);
    let start = std::time::Instant::now();
    t.advance(b"z\x1b[2000000000b"); // REP 2 billion
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "REP with a 2-billion count must be clamped, not loop unbounded"
    );
    // The visible cursor row is full of 'z' (sanity: parsing still works).
    assert_eq!(
        t.grid().cell(t.cursor_position().unwrap().0, 0).unwrap().c,
        'z'
    );
}

// ---- Paste-injection guard (frame_paste) ----

#[test]
fn frame_paste_unbracketed_strips_end_sentinel() {
    let t = Terminal::new(4, 20);
    assert!(!t.bracketed_paste());
    // A hostile clipboard payload carrying an embedded ESC[201~ must have it
    // stripped even on the un-bracketed path; no 200~/201~ framing is added.
    let out = t.frame_paste("a\x1b[201~rm -rf ~");
    assert_eq!(out, b"arm -rf ~");
    assert!(!contains_subslice(&out, b"\x1b[201~"));
}

#[test]
fn frame_paste_bracketed_wraps_and_neutralizes_injection() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?2004h"); // enable bracketed paste
    assert!(t.bracketed_paste());
    // The classic pastejacking payload: an embedded ESC[201~ tries to close
    // the bracket early so `rm -rf ~\n` runs as typed. frame_paste must strip
    // the embedded sentinel and wrap the whole (now-safe) payload exactly
    // once, so nothing escapes the bracket.
    let out = t.frame_paste("a\x1b[201~rm -rf ~\n");
    assert!(out.starts_with(b"\x1b[200~"), "must open the bracket");
    assert!(out.ends_with(b"\x1b[201~"), "must close the bracket");
    // Exactly ONE 201~ (the closing frame) — the embedded one was stripped.
    let closes = out.windows(6).filter(|w| *w == b"\x1b[201~").count();
    assert_eq!(closes, 1, "embedded end-sentinel must be stripped");
}

fn contains_subslice(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn cha_vpa_absolute_moves() {
    let mut t = Terminal::new(5, 10);
    t.advance(b"\x1b[5G"); // column 5 (1-based) -> col 4
    assert_eq!(t.cursor_position(), Some((0, 4)));
    t.advance(b"\x1b[3d"); // row 3 (1-based) -> row 2
    assert_eq!(t.cursor_position(), Some((2, 4)));
}

// ============================================================
// VT correctness P1 batch (C14 / C16 / C19)
// ============================================================

// ---- C19: settable tab stops (HTS / TBC / CHT / CBT) ----

#[test]
fn tab_default_stops_every_eight() {
    let mut t = Terminal::new(2, 30);
    t.advance(b"\t"); // col 0 -> 8
    assert_eq!(t.cursor_position(), Some((0, 8)));
    t.advance(b"\t"); // 8 -> 16
    assert_eq!(t.cursor_position(), Some((0, 16)));
}

#[test]
fn tab_from_mid_default_stop_advances_to_next_multiple() {
    let mut t = Terminal::new(2, 30);
    t.advance(b"abc\t"); // col 3 -> next stop at 8
    assert_eq!(t.cursor_position(), Some((0, 8)));
}

#[test]
fn hts_sets_custom_tab_stop() {
    let mut t = Terminal::new(2, 30);
    // Move to col 3 (1-based 4) and set a stop there via HTS (ESC H).
    t.advance(b"\x1b[4G"); // col 4 (1-based) = col 3 (0-based)
    t.advance(b"\x1bH"); // HTS at col 3
                         // Home, then tab: should stop at the new custom stop (col 3), not col 8.
    t.advance(b"\x1b[1G\t");
    assert_eq!(
        t.cursor_position(),
        Some((0, 3)),
        "tab honours custom HTS stop"
    );
}

#[test]
fn tbc_clear_all_then_tab_goes_to_last_col() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[3g"); // TBC 3 — clear every stop
    t.advance(b"\x1b[1G\t"); // home, tab with no stops -> last column (9)
    assert_eq!(t.cursor_position(), Some((0, 9)), "no stops -> last col");
}

#[test]
fn tbc_clear_current_stop() {
    let mut t = Terminal::new(2, 30);
    // Clear the default stop at col 8, then tab from home jumps to col 16.
    t.advance(b"\x1b[9G"); // col 9 (1-based) = col 8 (0-based), a default stop
    t.advance(b"\x1b[0g"); // TBC 0 — clear stop at cursor (col 8)
    t.advance(b"\x1b[1G\t");
    assert_eq!(
        t.cursor_position(),
        Some((0, 16)),
        "cleared col-8 stop skipped"
    );
}

#[test]
fn cht_forward_tabs_n() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[3I"); // CHT 3 — forward 3 tab stops from col 0 -> 8,16,24
    assert_eq!(t.cursor_position(), Some((0, 24)));
}

#[test]
fn cbt_back_tabs_n() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[30G"); // col 30 (1-based) = col 29
    t.advance(b"\x1b[2Z"); // CBT 2 — back 2 stops: 24 then 16
    assert_eq!(t.cursor_position(), Some((0, 16)));
}

#[test]
fn cbt_stops_at_column_zero() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[5G"); // col 4
    t.advance(b"\x1b[9Z"); // back far more stops than exist
    assert_eq!(t.cursor_position(), Some((0, 0)), "CBT clamps at col 0");
}

#[test]
fn tab_stops_reset_on_ris() {
    let mut t = Terminal::new(2, 30);
    t.advance(b"\x1b[3g"); // clear all stops
    t.advance(b"\x1bc"); // RIS — restores default stops
    t.advance(b"\t");
    assert_eq!(
        t.cursor_position(),
        Some((0, 8)),
        "RIS restores default stops"
    );
}

// ---- C14: focus reporting emit (core half) ----

#[test]
fn focus_report_silent_when_mode_off() {
    let mut t = Terminal::new(4, 20);
    t.focus_report(true);
    t.focus_report(false);
    assert!(
        t.take_pty_response().is_empty(),
        "no focus reports unless ?1004 is enabled"
    );
}

#[test]
fn focus_report_emits_when_mode_on() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1004h"); // enable focus reporting
    assert!(t.focus_reporting());
    t.focus_report(true);
    assert_eq!(
        t.take_pty_response().as_slice(),
        b"\x1b[I",
        "focus-in emits CSI I"
    );
    t.focus_report(false);
    assert_eq!(
        t.take_pty_response().as_slice(),
        b"\x1b[O",
        "focus-out emits CSI O"
    );
}

#[test]
fn focus_report_stops_after_mode_reset() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1004h");
    t.focus_report(true);
    let _ = t.take_pty_response();
    t.advance(b"\x1b[?1004l"); // disable again
    t.focus_report(true);
    assert!(
        t.take_pty_response().is_empty(),
        "disabling ?1004 silences reports"
    );
}

// ---- C16: reflow / rewrap on resize ----

#[test]
fn reflow_narrowing_rewraps_without_losing_chars() {
    // A 12-char logical line on a 20-col grid (one physical row) re-wraps
    // onto a 10-col grid (two physical rows) without losing characters.
    let mut t = Terminal::new(4, 20);
    t.advance(b"abcdefghijkl"); // 12 chars, no wrap at 20 cols
    t.resize(4, 10);
    // Row 0 holds "abcdefghij", row 1 holds "kl".
    let row0: String = (0..10).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    let row1: String = (0..2).map(|c| t.grid().cell(1, c).unwrap().c).collect();
    assert_eq!(row0, "abcdefghij");
    assert_eq!(row1, "kl");
}

#[test]
fn reflow_widening_rejoins_a_wrapped_line() {
    // Print 12 chars into a 5-col grid: it soft-wraps across rows. Widening
    // to 20 cols must rejoin the whole logical line onto one row.
    let mut t = Terminal::new(6, 5);
    t.advance(b"abcdefghijkl"); // wraps: abcde/fghij/kl
    t.resize(6, 20);
    let joined: String = (0..12).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(joined, "abcdefghijkl", "wrapped line rejoined on widen");
}

#[test]
fn reflow_never_merges_across_hard_newline() {
    // Two separate hard lines must stay separate across a reflow, even when
    // each is short enough that a naive join would merge them.
    let mut t = Terminal::new(6, 20);
    t.advance(b"foo\r\nbar");
    t.resize(6, 8);
    let row0: String = (0..3).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    let row1: String = (0..3).map(|c| t.grid().cell(1, c).unwrap().c).collect();
    assert_eq!(row0, "foo");
    assert_eq!(row1, "bar", "hard newline preserved — not merged into foo");
}

#[test]
fn reflow_roundtrip_preserves_text() {
    // Narrow then widen back: the text content must survive intact.
    let mut t = Terminal::new(6, 20);
    t.advance(b"the quick brown fox"); // 19 chars, fits one row at 20
    t.resize(6, 7); // narrow — forces wrap
    t.resize(6, 20); // widen back
    let joined: String = (0..19).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(joined, "the quick brown fox", "narrow→widen round-trips");
}

#[test]
fn reflow_preserves_scrollback_lines() {
    // Lines pushed to scrollback survive a reflow (non-lossy preservation).
    let mut t = Terminal::with_scrollback(2, 8, 100);
    t.advance(b"L0\r\nL1\r\nL2\r\nL3"); // L0/L1 scroll into history
    let before = t.all_lines();
    assert!(before.iter().any(|l| l.starts_with("L0")));
    t.resize(2, 12);
    let after = t.all_lines();
    assert!(
        after.iter().any(|l| l.starts_with("L0")),
        "scrollback line L0 survives reflow"
    );
    assert!(after.iter().any(|l| l.starts_with("L3")));
}

#[test]
fn reflow_alt_screen_uses_plain_resize() {
    // On the alt screen, resize must NOT reflow (the TUI redraws itself);
    // it must remain a no-panic plain resize.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?1049h");
    t.advance(b"ALT");
    t.resize(6, 10);
    assert!(t.alt_screen_active());
    assert_eq!(t.grid().rows(), 6);
    assert_eq!(t.grid().cols(), 10);
}

#[test]
fn growing_grid_expands_full_screen_scroll_region() {
    // ROOT CAUSE of the blank-pane-on-split bug. A terminal spawned at 24
    // rows has a full-screen scroll region of `0..=23`. Growing the grid to
    // 38 rows must EXPAND the region to `0..=37` — a full-screen region stays
    // full-screen across a grow-resize. The pre-fix code tested
    // `scroll_bottom + 1 >= grid.rows()` AFTER the grid had already grown
    // (24 < 38 → mis-classified as a CUSTOM region → frozen at 23), which let
    // a shell's multi-line resize-redraw scroll all content out of the
    // restricted 0..=23 region, blanking the pane.
    let mut t = Terminal::new(24, 80);
    assert_eq!(
        t.scroll_region(),
        (0, 23),
        "fresh 24-row spawn is full-screen"
    );
    t.resize(38, 80);
    assert_eq!(
        t.scroll_region(),
        (0, 37),
        "growing the grid must expand the full-screen scroll region to the new height"
    );
    // And it must do so for a grow-AND-narrow in one step (the real split path).
    let mut t2 = Terminal::new(24, 80);
    t2.resize(38, 57);
    assert_eq!(t2.scroll_region(), (0, 37));
}

#[test]
fn growing_grid_then_full_redraw_keeps_content_visible() {
    // The end-to-end shape of the blank-pane-on-split bug, deterministic.
    // Spawn at 24 rows (default), grow to 38 rows + narrow, then apply a
    // shell-style full-screen redraw (cursor-home + one line per row, the
    // trailing rows blank `ESC[K\r\n`). With the scroll-region-grow fix the
    // content stays on screen; without it the 38-line redraw scrolls it all
    // out of the frozen 0..=23 region and the grid goes blank.
    let mut t = Terminal::new(24, 80);
    t.advance(b"line-one\r\nline-two\r\nC:\\Users\\x>");
    t.resize(38, 57);
    // A 38-row redraw: 3 content rows then blank ESC[K rows, each ESC[K\r\n
    // except the last (exactly as a Windows shell emits on resize).
    let mut redraw = String::from("\x1b[?25l\x1b[H");
    let content = ["line-one", "line-two", "C:\\Users\\x>"];
    for r in 0..38 {
        if r < content.len() {
            redraw.push_str(content[r]);
        }
        redraw.push_str("\x1b[K");
        if r < 37 {
            redraw.push_str("\r\n");
        }
    }
    redraw.push_str("\x1b[?25h");
    t.advance(redraw.as_bytes());
    let nonblank = t
        .grid()
        .to_text()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert!(
        nonblank >= 3,
        "after a grow-resize + full redraw the grid must keep its content \
             visible (blank-pane-on-split regression), got {nonblank} nonblank rows:\n{}",
        t.grid().to_text()
    );
}

// ============================================================
// VT correctness P2/P3 batch (C20/C22/C25/C26/C27/C28/C30/C33/C34)
// ============================================================

use crate::grid::UnderlineStyle;

// ---- C20: styled underlines + underline color ----

#[test]
fn sgr_plain_underline_is_single() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[4mX");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().flags.underline_style,
        UnderlineStyle::Single
    );
    assert!(t.grid().cell(0, 0).unwrap().flags.underline());
}

#[test]
fn sgr_colon_styled_underlines() {
    let cases: &[(&[u8], UnderlineStyle)] = &[
        (b"\x1b[4:0mX", UnderlineStyle::None),
        (b"\x1b[4:1mX", UnderlineStyle::Single),
        (b"\x1b[4:2mX", UnderlineStyle::Double),
        (b"\x1b[4:3mX", UnderlineStyle::Curly),
        (b"\x1b[4:4mX", UnderlineStyle::Dotted),
        (b"\x1b[4:5mX", UnderlineStyle::Dashed),
    ];
    for (seq, style) in cases {
        let mut t = Terminal::new(2, 10);
        t.advance(seq);
        assert_eq!(
            t.grid().cell(0, 0).unwrap().flags.underline_style,
            *style,
            "style for {seq:?}"
        );
    }
}

#[test]
fn sgr_double_underline_via_21() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[21mX");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().flags.underline_style,
        UnderlineStyle::Double
    );
}

#[test]
fn sgr_24_resets_underline() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[4:3m\x1b[24mX");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().flags.underline_style,
        UnderlineStyle::None
    );
}

#[test]
fn sgr_58_underline_color_indexed() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[4:3;58:5:9mX"); // curly + indexed underline color 9
    let cell = t.grid().cell(0, 0).unwrap();
    assert_eq!(cell.flags.underline_style, UnderlineStyle::Curly);
    assert_eq!(cell.underline_color, Some(Color::Indexed(9)));
}

#[test]
fn sgr_58_underline_color_rgb_colon_empty_colorspace() {
    let mut t = Terminal::new(2, 10);
    // `58:2::255:0:0` — note the empty colorspace slot between 2 and r.
    t.advance(b"\x1b[58:2::255:0:0mX");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().underline_color,
        Some(Color::Rgb(255, 0, 0))
    );
}

#[test]
fn sgr_58_underline_color_rgb_semicolon_form() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[58;2;10;20;30mX");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().underline_color,
        Some(Color::Rgb(10, 20, 30))
    );
}

#[test]
fn sgr_59_resets_underline_color() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[58:5:9m\x1b[59mX");
    assert_eq!(t.grid().cell(0, 0).unwrap().underline_color, None);
}

#[test]
fn sgr_extended_fg_color_still_works_after_refactor() {
    // Regression: the sgr() rewrite must not break 38;2 / 38;5.
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[38;5;200mA\x1b[38;2;1;2;3mB");
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Indexed(200));
    assert_eq!(t.grid().cell(0, 1).unwrap().fg, Color::Rgb(1, 2, 3));
}

// ---- C22: REP (verify still green after refactor) ----

#[test]
fn rep_after_p2_changes() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"q\x1b[2b"); // print q, repeat twice more
    let line: String = (0..3).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(line, "qqq");
}

// ---- C25: DECSCNM / IRM / DECOM ----

#[test]
fn decscnm_reverse_screen_flag() {
    let mut t = Terminal::new(2, 10);
    assert!(!t.reverse_screen());
    t.advance(b"\x1b[?5h");
    assert!(t.reverse_screen(), "?5h sets reverse-video screen");
    t.advance(b"\x1b[?5l");
    assert!(!t.reverse_screen());
}

#[test]
fn irm_insert_mode_shifts_line_right() {
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcd\x1b[H"); // fill, home
    t.advance(b"\x1b[4h"); // enable IRM
    t.advance(b"XY"); // insert at col 0: XYabcd -> XYabcd (d pushed off)
    assert!(t.insert_mode());
    let line: String = (0..6).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(line, "XYabcd");
}

#[test]
fn irm_reset_returns_to_overwrite() {
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcd\x1b[H\x1b[4h\x1b[4l"); // set then reset IRM
    assert!(!t.insert_mode());
    t.advance(b"X"); // overwrite, not insert
    let line: String = (0..4).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(line, "Xbcd");
}

#[test]
fn decom_origin_mode_relative_addressing() {
    let mut t = Terminal::new(6, 10);
    t.advance(b"\x1b[2;4r"); // scroll region rows 2..4 (0-based 1..3)
    t.advance(b"\x1b[?6h"); // enable origin mode (homes to top margin)
    assert!(t.origin_mode());
    // CUP row 1 with origin mode -> absolute row = scroll_top (1).
    t.advance(b"\x1b[1;1H");
    assert_eq!(
        t.cursor_position(),
        Some((1, 0)),
        "row 1 maps to top margin"
    );
    // CUP row 2 -> scroll_top + 1 = row 2.
    t.advance(b"\x1b[2;1H");
    assert_eq!(t.cursor_position(), Some((2, 0)));
    // Past the bottom margin clamps to scroll_bottom (3).
    t.advance(b"\x1b[99;1H");
    assert_eq!(
        t.cursor_position(),
        Some((3, 0)),
        "clamped to bottom margin"
    );
}

#[test]
fn decom_off_uses_absolute_addressing() {
    let mut t = Terminal::new(6, 10);
    t.advance(b"\x1b[2;4r"); // region set
    t.advance(b"\x1b[1;1H"); // origin mode OFF -> absolute row 0
    assert_eq!(t.cursor_position(), Some((0, 0)));
}

// ---- C26: OSC 9;4 progress ----

#[test]
fn osc9_4_progress_normal() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]9;4;1;42\x07");
    let p = t.take_progress();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].state, ProgressState::Normal);
    assert_eq!(p[0].percent, 42);
    assert!(t.take_progress().is_empty(), "drained once");
}

#[test]
fn osc9_4_progress_states() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]9;4;0;0\x07"); // remove
    t.advance(b"\x1b]9;4;2;99\x07"); // error
    t.advance(b"\x1b]9;4;3;50\x07"); // indeterminate (percent ignored)
    t.advance(b"\x1b]9;4;4;75\x07"); // warning
    let p = t.take_progress();
    assert_eq!(p[0].state, ProgressState::Remove);
    assert_eq!(p[1].state, ProgressState::Error);
    assert_eq!(p[1].percent, 99);
    assert_eq!(p[2].state, ProgressState::Indeterminate);
    assert_eq!(p[2].percent, 0, "indeterminate ignores percent");
    assert_eq!(p[3].state, ProgressState::Warning);
    assert_eq!(p[3].percent, 75);
}

#[test]
fn osc9_4_clamps_percent() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]9;4;1;250\x07");
    assert_eq!(t.take_progress()[0].percent, 100, "percent clamps to 100");
}

#[test]
fn osc9_plain_notification_not_progress() {
    // OSC 9 without the ;4 sub-code is still a notification.
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]9;Hello\x07");
    assert!(t.take_progress().is_empty());
    assert_eq!(t.take_notification().unwrap().body, "Hello");
}

// ---- C27 / C34: combining marks + variation selectors ----

#[test]
fn combining_mark_attaches_to_previous_cell() {
    let mut t = Terminal::new(2, 10);
    t.advance("e\u{0301}".as_bytes()); // e + combining acute
    let cell = t.grid().cell(0, 0).unwrap();
    assert_eq!(cell.c, 'e');
    assert_eq!(t.grid().grapheme_at(0, 0), "e\u{0301}");
    // The combining mark did NOT advance the cursor into col 1.
    assert_eq!(t.cursor_position(), Some((0, 1)));
    assert_eq!(t.grid().cell(0, 1).unwrap().c, ' ', "no own cell for mark");
}

#[test]
fn multiple_combining_marks_stack() {
    let mut t = Terminal::new(2, 10);
    t.advance("a\u{0301}\u{0302}".as_bytes());
    assert_eq!(t.grid().grapheme_at(0, 0), "a\u{0301}\u{0302}");
}

#[test]
fn variation_selector_attaches_zero_width() {
    let mut t = Terminal::new(2, 10);
    // heart + VS16 (emoji presentation) — VS16 is zero-width, attaches.
    t.advance("\u{2764}\u{FE0F}".as_bytes());
    let cell = t.grid().cell(0, 0).unwrap();
    assert_eq!(cell.c, '\u{2764}');
    assert_eq!(t.grid().grapheme_at(0, 0), "\u{2764}\u{FE0F}");
    assert_eq!(
        t.cursor_position(),
        Some((0, 1)),
        "VS16 did not advance cursor"
    );
}

#[test]
fn combining_mark_attaches_to_wide_glyph_base() {
    let mut t = Terminal::new(2, 10);
    // Wide CJK glyph occupies cols 0+1; a following combining mark must
    // attach to the BASE (col 0), not the continuation spacer (col 1).
    t.advance("世\u{0301}".as_bytes());
    assert_eq!(t.grid().grapheme_at(0, 0), "世\u{0301}");
    assert_eq!(t.cursor_position(), Some((0, 2)));
}

#[test]
fn leading_combining_mark_lands_in_cell() {
    // A combining mark with no preceding cell (col 0) is given a cell so it
    // is not silently lost.
    let mut t = Terminal::new(2, 10);
    t.advance("\u{0301}".as_bytes());
    assert_eq!(
        t.cursor_position(),
        Some((0, 1)),
        "leading mark occupies a cell"
    );
}

// ---- C28: OSC 133 C/D command marks ----

#[test]
fn osc133_command_output_start_mark() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;C\x07");
    let marks = t.command_marks();
    assert_eq!(marks.len(), 1);
    assert!(matches!(marks[0].kind, CommandMarkKind::OutputStart));
}

#[test]
fn osc133_command_end_with_exit_code() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;D;0\x07"); // success
    t.advance(b"\x1b]133;D;127\x07"); // failure
    let marks = t.command_marks();
    assert_eq!(marks.len(), 2);
    assert!(matches!(
        marks[0].kind,
        CommandMarkKind::CommandEnd { exit_code: Some(0) }
    ));
    assert!(matches!(
        marks[1].kind,
        CommandMarkKind::CommandEnd {
            exit_code: Some(127)
        }
    ));
}

#[test]
fn osc133_command_end_without_exit_code() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;D\x07");
    assert!(matches!(
        t.command_marks()[0].kind,
        CommandMarkKind::CommandEnd { exit_code: None }
    ));
}

#[test]
fn osc133_cd_never_writes_pty_response() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;C\x07\x1b]133;D;0\x07");
    assert!(
        t.take_pty_response().is_empty(),
        "OSC 133 C/D stay capture-only (anti-CVE)"
    );
}

#[test]
fn osc133_a_still_records_prompt_mark_not_command_mark() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;A\x07");
    assert_eq!(t.prompt_marks().len(), 1);
    assert_eq!(
        t.command_marks().len(),
        0,
        "A is a prompt mark, not command"
    );
}

#[test]
fn last_command_exit_code_none_when_no_command_finished() {
    // A fresh terminal (or one that has only seen a `C` output-start mark)
    // has no finished command — the accessor reports None so the status
    // bar shows no indicator.
    let mut t = Terminal::new(4, 20);
    assert_eq!(t.last_command_exit_code(), None);
    t.advance(b"\x1b]133;C\x07"); // output start only, no `D`
    assert_eq!(
        t.last_command_exit_code(),
        None,
        "a C (output-start) mark is not a finished command"
    );
}

#[test]
fn last_command_exit_code_reports_latest_success_then_failure() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;D;0\x07"); // first command: success
    assert_eq!(t.last_command_exit_code(), Some(Some(0)));
    t.advance(b"\x1b]133;D;127\x07"); // second command: failure 127
    assert_eq!(
        t.last_command_exit_code(),
        Some(Some(127)),
        "the accessor must report the MOST RECENT command-end mark"
    );
}

#[test]
fn last_command_exit_code_some_none_when_code_absent() {
    // `OSC 133 ; D` with no third field: the command finished but the shell
    // did not report a code — Some(None), distinct from the no-command None.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;D\x07");
    assert_eq!(t.last_command_exit_code(), Some(None));
}

#[test]
fn last_command_exit_code_ignores_trailing_output_start() {
    // After a finished command (`D`), a new command's output begins (`C`)
    // before it ends. The accessor must still report the LAST FINISHED
    // command's code, ignoring the dangling `C`.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;D;0\x07"); // command 1 finished, success
    t.advance(b"\x1b]133;C\x07"); // command 2 output started, not finished
    assert_eq!(
        t.last_command_exit_code(),
        Some(Some(0)),
        "a dangling C must not mask the previous finished command's code"
    );
}

// ---- C30: XTGETTCAP ----

#[test]
fn xtgettcap_replies_to_colors_capability() {
    let mut t = Terminal::new(4, 20);
    // "Co" hex = 436F. Query DCS + q 436F ST.
    t.advance(b"\x1bP+q436F\x1b\\");
    let resp = t.take_pty_response();
    // Valid reply form: DCS 1 + r 436F = <hex of "256"> ST.
    // "256" hex = 323536.
    assert_eq!(resp.as_slice(), b"\x1bP1+r436F=323536\x1b\\");
}

#[test]
fn xtgettcap_unknown_capability_invalid_reply() {
    let mut t = Terminal::new(4, 20);
    // "ZZ" hex = 5A5A — not a capability we report.
    t.advance(b"\x1bP+q5A5A\x1b\\");
    let resp = t.take_pty_response();
    // Invalid form: DCS 0 + r 5A5A ST.
    assert_eq!(resp.as_slice(), b"\x1bP0+r5A5A\x1b\\");
}

#[test]
fn xtgettcap_does_not_disturb_sixel() {
    // A plain DCS q (Sixel, no '+') must still go to the image path, not
    // XTGETTCAP — regression guard for the hook disambiguation.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bPq#0;2;100;0;0~\x1b\\");
    assert_eq!(t.images().len(), 1);
    assert!(
        t.take_pty_response().is_empty(),
        "sixel emits no XTGETTCAP reply"
    );
}

#[test]
fn xtgettcap_answers_a_non_utf8_capability_name() {
    // "00ff" is well-formed hex but decodes to bytes that are not valid UTF-8,
    // so it can never name a capability. It must still be ANSWERED with the
    // unknown-capability form: a query the terminal drops silently hangs an app
    // that blocks on the reply.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bP+q00ff\x1b\\");
    // The quoted name is re-encoded from the DECODED bytes (normalised upper
    // hex), never a byte-for-byte reflection of the request.
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP0+r00FF\x1b\\");
}

#[test]
fn xtgettcap_answers_a_malformed_hex_name() {
    // Odd-length / non-hex tokens name nothing at all, so there is no name to
    // quote back — but the caller still gets the bare unknown form, not silence.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bP+qZZZ\x1b\\");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP0+r\x1b\\");
}

#[test]
fn xtgettcap_advertises_smulx_styled_underline() {
    let mut t = Terminal::new(4, 20);
    // "Smulx" hex = 536D756C78.
    t.advance(b"\x1bP+q536D756C78\x1b\\");
    let expected = format!(
        "\x1bP1+r536D756C78={}\x1b\\",
        hex_encode(SMULX_CAPABILITY.as_bytes())
    );
    assert_eq!(t.take_pty_response(), expected.as_bytes());
}

#[test]
fn xtgettcap_advertises_setulc_underline_colour() {
    let mut t = Terminal::new(4, 20);
    // "Setulc" hex = 536574756C63.
    t.advance(b"\x1bP+q536574756C63\x1b\\");
    let expected = format!(
        "\x1bP1+r536574756C63={}\x1b\\",
        hex_encode(SETULC_CAPABILITY.as_bytes())
    );
    assert_eq!(t.take_pty_response(), expected.as_bytes());
}

/// The terminal NAME capability, under both spellings that share its arm.
///
/// The block above queries `Co`, an unknown name, a non-UTF-8 name, a malformed
/// hex name, `Smulx` and `Setulc` — but never `TN`, `name` or `RGB`, so those
/// two arms could be deleted with the whole suite green. That failure is
/// invisible from INSIDE the terminal: a terminfo-probing app simply reads the
/// unknown form and downgrades, and nothing anywhere reports an error.
///
/// Both spellings are asserted because they SHARE one arm — asserting only
/// `TN` would leave `name` unpinned if the arm were ever split.
#[test]
fn xtgettcap_reports_the_terminal_name_under_both_spellings() {
    // "TN" hex = 544E.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bP+q544E\x1b\\");
    let expected = format!("\x1bP1+r544E={}\x1b\\", hex_encode(b"xterm-256color"));
    assert_eq!(t.take_pty_response(), expected.as_bytes());

    // "name" hex = 6E616D65 — the long spelling of the same capability.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bP+q6E616D65\x1b\\");
    let expected = format!("\x1bP1+r6E616D65={}\x1b\\", hex_encode(b"xterm-256color"));
    assert_eq!(t.take_pty_response(), expected.as_bytes());
}

/// `RGB` is how an application decides truecolor is available.
///
/// It is a BOOLEAN capability, so the reply is the VALID form with NO
/// `=<value>` — `DCS 1 + r <name> ST`. Dropping the arm yields the UNKNOWN form
/// (`0+r`), a single-byte difference that silently makes every probing app fall
/// back to 256 colours, so the reply is asserted byte-for-byte.
#[test]
fn xtgettcap_advertises_rgb_truecolor_as_a_boolean_capability() {
    // "RGB" hex = 524742.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bP+q524742\x1b\\");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP1+r524742\x1b\\");
}

#[test]
fn smulx_advertisement_is_backed_by_real_support() {
    // TRUTHFULNESS: advertising `Smulx` promises that the sequence its template
    // expands to actually selects an underline style. Instantiate the template
    // by hand for each `%p1` and assert the parser really honours it — an
    // advertisement no implementation backs is the lie this test exists to stop.
    assert!(
        SMULX_CAPABILITY.starts_with("\x1b[4:") && SMULX_CAPABILITY.ends_with('m'),
        "Smulx must expand to an SGR 4 colon sub-parameter: {SMULX_CAPABILITY:?}"
    );
    let cases = [
        (b"\x1b[4:0mX".as_slice(), UnderlineStyle::None),
        (b"\x1b[4:1mX".as_slice(), UnderlineStyle::Single),
        (b"\x1b[4:2mX".as_slice(), UnderlineStyle::Double),
        (b"\x1b[4:3mX".as_slice(), UnderlineStyle::Curly),
        (b"\x1b[4:4mX".as_slice(), UnderlineStyle::Dotted),
        (b"\x1b[4:5mX".as_slice(), UnderlineStyle::Dashed),
    ];
    for (seq, want) in cases {
        let mut t = Terminal::new(2, 10);
        t.advance(seq);
        assert_eq!(
            t.grid().cell(0, 0).unwrap().flags.underline_style,
            want,
            "Smulx claims support for {seq:x?}"
        );
    }
}

#[test]
fn setulc_advertisement_is_backed_by_real_support() {
    // TRUTHFULNESS: `Setulc` promises the colon form WITH the empty colorspace
    // slot (`58:2::r:g:b`) is understood. Assert the template really emits that
    // shape, and that the shape really sets the underline colour.
    assert!(
        SETULC_CAPABILITY.starts_with("\x1b[58:2::") && SETULC_CAPABILITY.ends_with('m'),
        "Setulc must expand to the colon RGB form: {SETULC_CAPABILITY:?}"
    );
    let mut t = Terminal::new(2, 10);
    // The hand-expansion of the template for %p1 = 0x0A141E (10,20,30).
    t.advance(b"\x1b[58:2::10:20:30mX");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().underline_color,
        Some(Color::Rgb(10, 20, 30)),
        "Setulc claims support for the empty-colorspace colon form"
    );
}

// ---- XTVERSION (`CSI > 0 q`) ----

#[test]
fn xtversion_reports_the_crate_version() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[>0q");
    let expected = format!("\x1bP>|c0pl4nd({})\x1b\\", env!("CARGO_PKG_VERSION"));
    assert_eq!(t.take_pty_response(), expected.as_bytes());
    // Guard the DERIVATION, not just the shape: a hard-coded literal passes
    // today and breaks on the next version bump, so pin that the reported
    // version is the crate's own and is a real dotted version, never a stub.
    let ver = env!("CARGO_PKG_VERSION");
    assert!(
        ver.contains('.') && ver.chars().next().is_some_and(|c| c.is_ascii_digit()),
        "the crate version must be a real dotted version, got {ver:?}"
    );
}

#[test]
fn xtversion_omitted_parameter_is_treated_as_zero() {
    // `CSI > q` with no parameter is the same request as `CSI > 0 q`.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[>q");
    let expected = format!("\x1bP>|c0pl4nd({})\x1b\\", env!("CARGO_PKG_VERSION"));
    assert_eq!(t.take_pty_response(), expected.as_bytes());
}

#[test]
fn xtversion_ignores_a_non_zero_parameter() {
    // Deliberate narrow exception to "always answer a query": XTVERSION defines
    // no invalid/negative reply form, so there is nothing truthful to send for
    // a Ps the protocol assigns no meaning to.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[>1q");
    assert!(
        t.take_pty_response().is_empty(),
        "only Ps 0 requests the version"
    );
}

#[test]
fn xtversion_does_not_disturb_decscusr_or_secondary_da() {
    // `CSI > Ps q` (XTVERSION), `CSI Ps SP q` (DECSCUSR) and `CSI > Ps c`
    // (secondary DA) are near neighbours. Pin that adding XTVERSION moved
    // neither of the other two.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[4 q"); // DECSCUSR: steady underline
    assert!(
        t.take_pty_response().is_empty(),
        "DECSCUSR sets state and emits no reply"
    );
    t.advance(b"\x1bP$q q\x1b\\"); // DECRQSS confirms the shape really changed
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP1$r4 q\x1b\\");

    t.advance(b"\x1b[>c"); // secondary DA still answers its own reply
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[>0;0;0c");
}

// ---- DECRQSS (`DCS $ q <setting> ST` — report the current setting) ----

#[test]
fn decrqss_reports_current_sgr() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[1;31m"); // bold + red foreground
    t.advance(b"\x1bP$qm\x1b\\");
    // Valid form: DCS 1 $ r <sgr params> m ST, led by 0 so the client can
    // replay it verbatim.
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP1$r0;1;31m\x1b\\");
}

#[test]
fn decrqss_reports_default_sgr_when_the_pen_is_clean() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bP$qm\x1b\\");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP1$r0m\x1b\\");
}

#[test]
fn decrqss_reports_extended_sgr_colors_and_underline_style() {
    let mut t = Terminal::new(4, 20);
    // Curly underline + 24-bit foreground + bright-index background.
    t.advance(b"\x1b[4:3;38;2;10;20;30;101m");
    t.advance(b"\x1bP$qm\x1b\\");
    assert_eq!(
        t.take_pty_response().as_slice(),
        b"\x1bP1$r0;4:3;38;2;10;20;30;101m\x1b\\"
    );
}

/// The 8 / 16 colour-index BOUNDARIES of the DECRQSS SGR report.
///
/// `push_sgr_color` has three arms — `0..=7` (base 30/40), `8..=15` (aixterm
/// bright 90/100, offset by 8) and `16..` (extended `38;5;n`). The extended-SGR
/// test above is the only test in the workspace that drives an INDEXED colour
/// through it, and it picks index 9 — the MIDDLE of the bright arm. Index 9 is
/// invariant under every boundary mutation: `n < 8` widened to `n <= 8` still
/// misses it, `n < 16` widened to `n <= 16` still catches it, and replacing the
/// `n < 16` guard with `true` changes nothing because it already matched. So
/// both guards went unasserted while looking covered.
///
/// The reply is documented as a self-contained sequence a client can replay
/// verbatim, which is why index 8 matters most: `30 + 8` is `38`, the
/// EXTENDED-COLOUR INTRODUCER, so a client replaying a mis-reported index 8
/// mis-parses every parameter after it rather than merely painting one cell in
/// the wrong colour.
#[test]
fn decrqss_sgr_report_pins_the_8_and_16_colour_index_boundaries() {
    // Index 8 is the FIRST bright colour: aixterm `90`, never `30 + 8 == 38`.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[38;5;8m");
    t.advance(b"\x1bP$qm\x1b\\");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP1$r0;90m\x1b\\");

    // Index 16 is the FIRST extended colour: `38;5;16`, never `90 + 16 - 8`.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[38;5;16m");
    t.advance(b"\x1bP$qm\x1b\\");
    assert_eq!(
        t.take_pty_response().as_slice(),
        b"\x1bP1$r0;38;5;16m\x1b\\"
    );

    // And a high index proves the bright arm is not swallowing the whole range:
    // with the `n < 16` guard replaced by `true`, 255 reports as `;347`.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[48;5;255m");
    t.advance(b"\x1bP$qm\x1b\\");
    assert_eq!(
        t.take_pty_response().as_slice(),
        b"\x1bP1$r0;48;5;255m\x1b\\"
    );
}

#[test]
fn decrqss_reports_scroll_region() {
    let mut t = Terminal::new(10, 20);
    t.advance(b"\x1b[3;7r"); // DECSTBM rows 3..7 (1-based)
    t.advance(b"\x1bP$qr\x1b\\");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP1$r3;7r\x1b\\");
}

#[test]
fn decrqss_reports_cursor_style() {
    let mut t = Terminal::new(4, 20);
    // Default shape is a steady block → DECSCUSR Ps 2.
    t.advance(b"\x1bP$q q\x1b\\");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP1$r2 q\x1b\\");

    // A steady underline (CSI 4 SP q) round-trips as Ps 4.
    t.advance(b"\x1b[4 q");
    t.advance(b"\x1bP$q q\x1b\\");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP1$r4 q\x1b\\");
}

#[test]
fn decrqss_unsupported_setting_gets_the_invalid_form() {
    let mut t = Terminal::new(4, 20);
    // DECSCA (`" q`) is not a setting this terminal tracks. The DEC answer is
    // the invalid form — an explicit "no", never silence.
    t.advance(b"\x1bP$q\"q\x1b\\");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1bP0$r\x1b\\");
}

#[test]
fn decrqss_never_echoes_request_bytes() {
    // SECURITY (device-reply echo-to-stdin): the reply is built from internal
    // state only. A hostile selector must come back as the fixed invalid form
    // with none of the request's bytes smuggled into it.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bP$q\x07evil\x1b\\");
    let resp = t.take_pty_response();
    assert_eq!(resp.as_slice(), b"\x1bP0$r\x1b\\");
    assert!(
        !resp.windows(4).any(|w| w == b"evil"),
        "the reply must not carry request bytes: {resp:x?}"
    );
}

#[test]
fn decrqss_does_not_disturb_sixel() {
    // `DCS $ q` (DECRQSS) and `DCS q` (Sixel) share the final byte and are
    // told apart ONLY by the `$` intermediate. Regression guard for the hook
    // disambiguation in BOTH directions.
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1bP$qm\x1b\\");
    assert!(
        t.images().is_empty(),
        "a DECRQSS request must never be decoded as an image"
    );
    assert!(
        !t.take_pty_response().is_empty(),
        "a DECRQSS request must be answered"
    );

    // A plain DCS q is still a Sixel image, and still emits no reply.
    t.advance(b"\x1bPq#0;2;100;0;0~\x1b\\");
    assert_eq!(t.images().len(), 1, "sixel still decodes");
    assert_eq!(t.images()[0].image.height, 6);
    assert!(
        t.take_pty_response().is_empty(),
        "sixel emits no device reply"
    );
}

// ---- C33: DECRQM ----

#[test]
fn decrqm_reports_set_private_mode() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?2004h"); // enable bracketed paste
    t.advance(b"\x1b[?2004$p"); // DECRQM query
                                // Reply: CSI ? 2004 ; 1 $ y  (1 = set).
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[?2004;1$y");
}

#[test]
fn decrqm_reports_reset_private_mode() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?2004$p"); // never enabled -> reset (2)
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[?2004;2$y");
}

#[test]
fn decrqm_reports_unrecognised_mode_zero() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?9999$p");
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[?9999;0$y");
}

#[test]
fn decrqm_reports_ansi_irm() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[4h"); // IRM on
    t.advance(b"\x1b[4$p"); // ANSI DECRQM (no '?')
    assert_eq!(t.take_pty_response().as_slice(), b"\x1b[4;1$y");
}

#[test]
fn p2p3_modes_reset_on_ris() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[?5h\x1b[4h\x1b[?6h"); // reverse + insert + origin
    t.advance(b"\x1bc"); // RIS
    assert!(!t.reverse_screen());
    assert!(!t.insert_mode());
    assert!(!t.origin_mode());
}

// ---- VT parser memchr fast-path equivalence ----

/// The `memchr` ESC-scan fast path (and the bulk run-skip in the APC
/// pre-filter) must be byte-for-byte behaviour-identical to feeding the same
/// stream one byte at a time. We build a stream with LONG runs of plain
/// printable ASCII interleaved with real escape sequences (SGR colour, cursor
/// moves, an OSC title, and a Kitty APC) and assert the resulting grid text,
/// cursor position, and PTY response are identical whether the bytes arrive
/// in one `advance()` call (fast path + bulk skip) or one byte per call
/// (scalar path, ESC never bulk-skipped).
#[test]
fn memchr_fast_path_matches_byte_at_a_time() {
    // A long plain run, an SGR colour change, more plain text, a CUP, an OSC
    // title set, a Kitty graphics APC (filtered), and a trailing plain run.
    let plain_a = "abcdefghijklmnopqrstuvwxyz0123456789".repeat(4);
    let plain_b = "the quick brown fox jumps over the lazy dog".repeat(3);
    let mut stream: Vec<u8> = Vec::new();
    stream.extend_from_slice(plain_a.as_bytes());
    stream.extend_from_slice(b"\x1b[31m"); // SGR red
    stream.extend_from_slice(plain_b.as_bytes());
    stream.extend_from_slice(b"\x1b[2;3H"); // CUP row 2 col 3
    stream.extend_from_slice(b"\x1b]0;a title\x07"); // OSC 0 title
    stream.extend_from_slice(b"\x1b_Gf=24,s=1,v=1;AAA\x1b\\"); // Kitty APC
    stream.extend_from_slice(b"tail-run-plain-text"); // trailing plain run

    // (a) whole chunk: exercises the memchr fast-path gate + bulk run-skip.
    let mut whole = Terminal::new(8, 40);
    whole.advance(&stream);

    // (b) one byte per advance(): ESC can never be bulk-skipped; each byte
    // walks the state machine individually.
    let mut split = Terminal::new(8, 40);
    for &b in &stream {
        split.advance(&[b]);
    }

    assert_eq!(
        whole.grid().to_text(),
        split.grid().to_text(),
        "grid text must match between whole-chunk and byte-at-a-time parsing"
    );
    assert_eq!(
        whole.cursor_position(),
        split.cursor_position(),
        "cursor position must match"
    );
    assert_eq!(whole.title(), split.title(), "OSC title must match");
    assert_eq!(
        whole.images().len(),
        split.images().len(),
        "Kitty APC image count must match"
    );
}

/// A pure plain-ASCII chunk with NO escape byte must take the fast path and
/// land verbatim on the grid (the overwhelmingly common bulk-output case).
#[test]
fn memchr_fast_path_pure_plain_run() {
    let mut t = Terminal::new(4, 80);
    let run = "plain text with no escapes whatsoever 1234567890";
    t.advance(run.as_bytes());
    assert!(
        t.grid().to_text().contains(run),
        "a pure plain run must reach the grid unchanged"
    );
}

// ---- kitty keyboard protocol (CSI u progressive enhancement) ----

#[test]
fn kitty_flags_default_to_zero() {
    let t = Terminal::new(4, 20);
    assert_eq!(t.kitty_keyboard_flags(), 0);
}

#[test]
fn kitty_push_sets_current_flags() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[>5u"); // push flags=5 (disambiguate | alternate-keys)
    assert_eq!(t.kitty_keyboard_flags(), 5);
}

#[test]
fn kitty_push_defaults_to_zero_flags() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[>u"); // push with no param → flags 0
    assert_eq!(t.kitty_keyboard_flags(), 0);
    // But it DID push an entry — popping reveals the empty stack.
    t.advance(b"\x1b[>9u");
    assert_eq!(t.kitty_keyboard_flags(), 9);
    t.advance(b"\x1b[<u"); // pop 1 → back to the flags-0 entry
    assert_eq!(t.kitty_keyboard_flags(), 0);
}

#[test]
fn kitty_pop_restores_previous() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[>1u");
    t.advance(b"\x1b[>3u");
    assert_eq!(t.kitty_keyboard_flags(), 3);
    t.advance(b"\x1b[<u"); // pop 1 (default)
    assert_eq!(t.kitty_keyboard_flags(), 1);
    t.advance(b"\x1b[<5u"); // pop more than remaining → saturates to empty
    assert_eq!(t.kitty_keyboard_flags(), 0);
}

#[test]
fn kitty_set_mode_replace_or_clear() {
    let mut t = Terminal::new(4, 20);
    // Set on an empty stack pushes the result (mode 1 replace, flags=5).
    t.advance(b"\x1b[=5;1u");
    assert_eq!(t.kitty_keyboard_flags(), 5);
    // Mode 2 = OR in bit2.
    t.advance(b"\x1b[=2;2u");
    assert_eq!(t.kitty_keyboard_flags(), 7);
    // Mode 3 = clear bit1.
    t.advance(b"\x1b[=1;3u");
    assert_eq!(t.kitty_keyboard_flags(), 6);
    // Mode defaults to 1 (replace) when omitted.
    t.advance(b"\x1b[=8u");
    assert_eq!(t.kitty_keyboard_flags(), 8);
}

#[test]
fn kitty_query_replies_with_current_flags() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b[>13u");
    t.advance(b"\x1b[?u"); // query
    assert_eq!(t.take_pty_response(), b"\x1b[?13u".to_vec());
}

#[test]
fn kitty_stack_depth_is_capped() {
    let mut t = Terminal::new(4, 20);
    // Push the cap + extra; the oldest is dropped, never grows unbounded.
    for i in 0..(KITTY_KBD_STACK_MAX + 5) {
        let seq = format!("\x1b[>{}u", (i % 7) + 1);
        t.advance(seq.as_bytes());
    }
    assert_eq!(t.screen.kitty_kbd_stack.len(), KITTY_KBD_STACK_MAX);
}

#[test]
fn bare_csi_u_is_still_scorc_cursor_restore() {
    // The bare `CSI u` (no intermediate) MUST remain the ANSI.SYS SCORC
    // cursor-restore alias and NOT be hijacked by the kitty handler.
    let mut t = Terminal::new(8, 40);
    t.advance(b"\x1b[3;5H"); // move cursor to row 3, col 5 (1-based)
    t.advance(b"\x1b[s"); // SCOSC — save cursor
    t.advance(b"\x1b[1;1H"); // move to home
    assert_eq!(t.cursor_position(), Some((0, 0)));
    t.advance(b"\x1b[u"); // bare CSI u → SCORC restore
    assert_eq!(t.cursor_position(), Some((2, 4)));
    // And it left the kitty stack untouched + emitted NO query reply.
    assert_eq!(t.kitty_keyboard_flags(), 0);
    assert!(t.take_pty_response().is_empty());
}

#[cfg(test)]
#[test]
fn cursor_up_down_respect_scroll_margins() {
    // Regression: CUU/CUD clamped to the physical grid, ignoring DECSTBM
    // margins, so relative motion walked into a reserved status-line region.
    let mut t = Terminal::new(6, 10);
    // DECSTBM rows 3..=5 (1-based) → 0-based scroll_top=2, scroll_bottom=4;
    // DECSTBM homes the cursor to the top margin (row 2).
    t.advance(b"\x1b[3;5r");
    assert_eq!(
        t.cursor_position(),
        Some((2, 0)),
        "DECSTBM homes to top margin"
    );
    // CUU by 5 from the top margin must STOP at the top margin (row 2), not run
    // up to the physical top (row 0).
    t.advance(b"\x1b[5A");
    assert_eq!(
        t.cursor_position(),
        Some((2, 0)),
        "CUU stops at the top scroll margin"
    );
    // CUD by 10 must STOP at the bottom margin (row 4), not the physical bottom
    // (row 5) — otherwise it walks into the reserved region.
    t.advance(b"\x1b[10B");
    assert_eq!(
        t.cursor_position(),
        Some((4, 0)),
        "CUD stops at the bottom scroll margin"
    );
}

#[test]
fn su_on_full_screen_feeds_scrollback() {
    // Regression: SU (CSI S) discarded scrolled-off lines even on a full-screen
    // region, where an LF-driven scroll preserves them in scrollback.
    let mut t = Terminal::new(3, 5);
    t.advance(b"a\r\nb\r\nc"); // fill 3 rows, no scroll yet
    let before = t.scrollback_len();
    t.advance(b"\x1b[2S"); // SU 2 — the 'a' and 'b' rows feed scrollback
    assert_eq!(
        t.scrollback_len(),
        before + 2,
        "full-screen SU feeds scrolled-off lines to scrollback"
    );
}

#[test]
fn su_within_margins_does_not_feed_scrollback() {
    // A MARGINED region must NOT feed scrollback (only the full screen does).
    let mut t = Terminal::new(5, 5);
    t.advance(b"\x1b[2;4r"); // DECSTBM region rows 2..=4 (not full)
    let before = t.scrollback_len();
    t.advance(b"\x1b[2S");
    assert_eq!(
        t.scrollback_len(),
        before,
        "margined SU discards scrolled-off lines (no scrollback)"
    );
}

// --- DoS / memory-amplification caps (every PTY-driven queue must be bounded) ---

#[test]
fn dsr_flood_is_bounded_by_pty_response_cap() {
    let mut t = Terminal::new(4, 10);
    // A hostile program streams cursor-position queries faster than the UI
    // drains; each reply (~6 bytes) was appended WITHOUT the PTY_RESPONSE_MAX
    // cap (DSR used extend_from_slice, not push_pty_response).
    for _ in 0..20_000 {
        t.advance(b"\x1b[6n");
    }
    assert!(
        t.take_pty_response().len() <= 64 * 1024,
        "DSR replies must honour the PTY_RESPONSE_MAX (64 KiB) cap"
    );
}

#[test]
fn title_stack_push_is_bounded() {
    let mut t = Terminal::new(4, 10);
    t.advance(b"\x1b]0;hi\x07"); // set a title to push
    for _ in 0..1000 {
        t.advance(b"\x1b[22t"); // XTWINOPS push-title, never popped
    }
    assert!(
        t.title_stack_depth() <= 64,
        "title stack must be bounded (TITLE_STACK_MAX)"
    );
}

#[test]
fn notification_queue_is_bounded() {
    let mut t = Terminal::new(4, 10);
    for _ in 0..1000 {
        t.advance(b"\x1b]9;x\x07"); // OSC 9 desktop notification
    }
    assert!(
        t.take_notifications().len() <= 256,
        "notification queue must be bounded (NOTIFICATIONS_MAX)"
    );
}

#[test]
fn clipboard_write_queue_is_bounded() {
    let mut t = Terminal::new(4, 10);
    for _ in 0..500 {
        t.advance(b"\x1b]52;c;aGk=\x07"); // OSC 52 write "hi"
    }
    assert!(
        t.take_clipboard_writes().len() <= 64,
        "clipboard-write queue must be bounded (CLIPBOARD_WRITES_MAX)"
    );
}

#[test]
fn progress_queue_is_bounded() {
    let mut t = Terminal::new(4, 10);
    for _ in 0..1000 {
        t.advance(b"\x1b]9;4;1;50\x07"); // OSC 9;4 progress
    }
    assert!(
        t.take_progress().len() <= 256,
        "progress queue must be bounded (PROGRESS_MAX)"
    );
}

#[test]
fn frame_paste_strips_controls_but_keeps_tab_and_newline() {
    let t = Terminal::new(4, 10);
    // Default (unbracketed): ESC + BEL + C1 stripped; tab + newline kept.
    let out = t.frame_paste("a\x1b[31mb\x07c\td\ne");
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "a[31mbc\td\ne",
        "ESC and BEL are stripped; tab and newline survive"
    );
    // A C1 control (U+009B, raw CSI) is stripped too.
    assert_eq!(
        String::from_utf8(t.frame_paste("x\u{009b}y")).unwrap(),
        "xy",
        "C1 control characters are stripped"
    );
    // The bracketed-paste end-sentinel is still removed (replaced before the
    // control filter runs, so the whole sentinel match is consumed).
    assert_eq!(
        String::from_utf8(t.frame_paste("a\x1b[201~b")).unwrap(),
        "ab",
        "embedded ESC[201~ end-sentinel is stripped"
    );
}

// ============================================================================
// Coverage-completion batch: OSC color set/query/reset, scroll-region edges,
// charset save/restore, tab-clear, reverse-index, focus reporting, the full
// CSI cursor/edit/scroll arm set, DECSCUSR, mouse encodings, and reflow edges.
// All assertions are mutation-grade (exact cell/cursor/mode state, never just
// "did not panic").
// ============================================================================

// ---- OSC 4 indexed palette: set, query reply, and OSC 104 reset ----

#[test]
fn osc4_set_indexed_color_updates_palette_and_queues_set() {
    let mut t = Terminal::new(2, 10);
    // Set palette index 1 to pure red via the `#` spec form.
    t.advance(b"\x1b]4;1;#ff0000\x07");
    assert_eq!(t.palette_color(1), (255, 0, 0), "palette index 1 updated");
    // A ColorSet is queued for the host to mirror.
    let sets = t.take_color_sets();
    assert!(
        sets.iter().any(|s| matches!(
            s,
            ColorSet::Indexed {
                index: 1,
                rgb: (255, 0, 0)
            }
        )),
        "an Indexed color-set is queued, got {sets:?}"
    );
}

#[test]
fn osc4_query_replies_with_current_palette_value() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]4;2;#00ff00\x07"); // set index 2 green
    let _ = t.take_pty_response(); // drain the set (no reply expected)
    t.advance(b"\x1b]4;2;?\x07"); // query index 2
    let reply = String::from_utf8(t.take_pty_response()).unwrap();
    assert_eq!(
        reply, "\x1b]4;2;rgb:0000/ffff/0000\x07",
        "OSC 4 query reflects the live palette entry"
    );
}

#[test]
fn osc104_resets_single_index_to_default() {
    let mut t = Terminal::new(2, 10);
    let original = t.palette_color(3);
    t.advance(b"\x1b]4;3;#abcdef\x07");
    assert_ne!(t.palette_color(3), original, "palette changed");
    let _ = t.take_color_sets();
    t.advance(b"\x1b]104;3\x07"); // reset only index 3
    assert_eq!(t.palette_color(3), original, "index 3 back to default");
    // The reset queues a ColorSet carrying the default value.
    let sets = t.take_color_sets();
    assert!(
        sets.iter()
            .any(|s| matches!(s, ColorSet::Indexed { index: 3, .. })),
        "reset queues a color-set for index 3"
    );
}

#[test]
fn osc104_no_args_resets_entire_palette() {
    let mut t = Terminal::new(2, 10);
    let original_5 = t.palette_color(5);
    let original_200 = t.palette_color(200);
    t.advance(b"\x1b]4;5;#111111\x07");
    t.advance(b"\x1b]4;200;#222222\x07");
    let _ = t.take_color_sets();
    t.advance(b"\x1b]104\x07"); // reset all
    assert_eq!(t.palette_color(5), original_5);
    assert_eq!(t.palette_color(200), original_200);
    // The full reset queues one color-set per palette entry (256).
    assert_eq!(t.take_color_sets().len(), 256);
}

// ---- OSC 10/11/12 dynamic colors: set, query, and OSC 110/111/112 reset ----

#[test]
fn osc10_11_12_set_dynamic_colors() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]10;#010203\x07"); // foreground
    t.advance(b"\x1b]11;#040506\x07"); // background
    t.advance(b"\x1b]12;#070809\x07"); // cursor
    assert_eq!(t.dynamic_color(DynamicColor::Foreground), (1, 2, 3));
    assert_eq!(t.dynamic_color(DynamicColor::Background), (4, 5, 6));
    assert_eq!(t.dynamic_color(DynamicColor::Cursor), (7, 8, 9));
    let sets = t.take_color_sets();
    assert!(sets.iter().any(|s| matches!(
        s,
        ColorSet::Dynamic {
            which: DynamicColor::Cursor,
            rgb: (7, 8, 9)
        }
    )));
}

#[test]
fn osc11_query_replies_with_background() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]11;#101112\x07");
    let _ = t.take_pty_response();
    t.advance(b"\x1b]11;?\x07");
    let reply = String::from_utf8(t.take_pty_response()).unwrap();
    assert_eq!(reply, "\x1b]11;rgb:1010/1111/1212\x07");
}

#[test]
fn osc110_111_112_reset_dynamic_colors_to_default() {
    let mut t = Terminal::new(2, 10);
    let def_fg = t.dynamic_color(DynamicColor::Foreground);
    let def_bg = t.dynamic_color(DynamicColor::Background);
    let def_cur = t.dynamic_color(DynamicColor::Cursor);
    t.advance(b"\x1b]10;#aabbcc\x07\x1b]11;#ddeeff\x07\x1b]12;#123456\x07");
    let _ = t.take_color_sets();
    t.advance(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07");
    assert_eq!(t.dynamic_color(DynamicColor::Foreground), def_fg);
    assert_eq!(t.dynamic_color(DynamicColor::Background), def_bg);
    assert_eq!(t.dynamic_color(DynamicColor::Cursor), def_cur);
    // Each reset queues a Dynamic color-set carrying the default.
    let sets = t.take_color_sets();
    assert_eq!(sets.len(), 3, "three dynamic resets queued");
}

// ---- OSC 9 / OSC 777 notifications ----

#[test]
fn osc9_notification_body_only() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]9;build done\x07");
    let n = t.take_notification().expect("a notification");
    assert_eq!(n.title, "");
    assert_eq!(n.body, "build done");
    assert!(t.take_notification().is_none(), "queue drained");
}

#[test]
fn osc777_notify_title_and_body() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]777;notify;Heads up;the thing happened\x07");
    let all = t.take_notifications();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].title, "Heads up");
    assert_eq!(all[0].body, "the thing happened");
}

#[test]
fn osc9_empty_body_produces_no_notification() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]9;\x07");
    assert!(t.take_notification().is_none());
}

// ---- OSC 9 ; 4 progress (taskbar) ----

#[test]
fn osc9_4_progress_states_and_percent() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]9;4;1;42\x07"); // normal, 42%
    t.advance(b"\x1b]9;4;3;99\x07"); // indeterminate -> percent forced to 0
    t.advance(b"\x1b]9;4;0;50\x07"); // remove -> percent forced to 0
    let p = t.take_progress();
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].state, ProgressState::Normal);
    assert_eq!(p[0].percent, 42);
    assert_eq!(p[1].state, ProgressState::Indeterminate);
    assert_eq!(p[1].percent, 0, "indeterminate zeroes percent");
    assert_eq!(p[2].state, ProgressState::Remove);
    assert_eq!(p[2].percent, 0, "remove zeroes percent");
}

#[test]
fn osc9_4_error_and_warning_states() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]9;4;2;10\x07"); // error
    t.advance(b"\x1b]9;4;4;200\x07"); // warning, percent clamps to 100
    let p = t.take_progress();
    assert_eq!(p[0].state, ProgressState::Error);
    assert_eq!(p[0].percent, 10);
    assert_eq!(p[1].state, ProgressState::Warning);
    assert_eq!(p[1].percent, 100, "percent clamps to 100");
}

// ---- OSC 52 clipboard write + read (default-off) ----

#[test]
fn osc52_write_decodes_and_queues() {
    let mut t = Terminal::new(2, 10);
    // base64 of "hi" is "aGk=".
    t.advance(b"\x1b]52;c;aGk=\x07");
    let w = t.take_clipboard_write().expect("a clipboard write");
    assert_eq!(w.selection, ClipboardSelection::Clipboard);
    assert_eq!(w.text, "hi");
    assert!(t.take_clipboard_write().is_none(), "queue now empty");
}

#[test]
fn osc52_primary_selection_recognised() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]52;p;aGk=\x07");
    let w = t.take_clipboard_write().unwrap();
    assert_eq!(w.selection, ClipboardSelection::Primary);
}

#[test]
fn osc52_read_request_refused_when_disabled() {
    let mut t = Terminal::new(2, 10);
    assert!(!t.clipboard_read_enabled(), "reads off by default");
    t.advance(b"\x1b]52;c;?\x07");
    // No write produced; the reply carries an empty payload (refusal).
    assert!(t.take_clipboard_write().is_none());
    assert_eq!(t.take_pty_response(), b"\x1b]52;c;\x07".to_vec());
}

#[test]
fn respond_clipboard_read_noop_until_enabled_then_emits() {
    let mut t = Terminal::new(2, 10);
    // Disabled: respond is a no-op.
    t.respond_clipboard_read(ClipboardSelection::Clipboard, "secret");
    assert!(t.take_pty_response().is_empty(), "no reply while disabled");
    // Enabled: the host-supplied text is base64-encoded into an OSC 52 reply.
    t.set_clipboard_read_enabled(true);
    assert!(t.clipboard_read_enabled());
    t.respond_clipboard_read(ClipboardSelection::Primary, "hi");
    let reply = String::from_utf8(t.take_pty_response()).unwrap();
    assert_eq!(reply, "\x1b]52;p;aGk=\x07");
}

// ---- DECSCUSR cursor shapes (CSI Ps SP q) ----

#[test]
fn decscusr_selects_every_shape_and_blink() {
    let cases: &[(&[u8], CursorShape, bool)] = &[
        (b"\x1b[0 q", CursorShape::Block, true),
        (b"\x1b[1 q", CursorShape::Block, true),
        (b"\x1b[2 q", CursorShape::Block, false),
        (b"\x1b[3 q", CursorShape::Underline, true),
        (b"\x1b[4 q", CursorShape::Underline, false),
        (b"\x1b[5 q", CursorShape::Bar, true),
        (b"\x1b[6 q", CursorShape::Bar, false),
    ];
    for (seq, shape, blink) in cases {
        let mut t = Terminal::new(2, 10);
        t.advance(seq);
        assert_eq!(t.cursor_shape(), *shape, "shape for {seq:x?}");
        assert_eq!(
            t.cursor_blink(),
            *blink,
            "blink for {seq:x?} (DECSCUSR half)"
        );
    }
    // Out-of-range Ps is ignored (shape stays default Block).
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[9 q");
    assert_eq!(t.cursor_shape(), CursorShape::Block);
}

#[test]
fn cursor_blink_also_tracks_dec_mode_12() {
    let mut t = Terminal::new(2, 10);
    assert!(!t.cursor_blink(), "no blink by default");
    t.advance(b"\x1b[?12h"); // att610 blink on
    assert!(t.cursor_blink(), "?12h enables blink");
    t.advance(b"\x1b[?12l");
    assert!(!t.cursor_blink());
}

// ---- DECSCNM reverse screen + origin-mode getter ----

#[test]
fn reverse_screen_getter_reflects_mode_5() {
    let mut t = Terminal::new(2, 10);
    assert!(!t.reverse_screen());
    t.advance(b"\x1b[?5h");
    assert!(t.reverse_screen(), "?5h sets DECSCNM");
    t.advance(b"\x1b[?5l");
    assert!(!t.reverse_screen());
}

// ---- Charset save/restore round-trip via DECSC/DECRC ----

#[test]
fn decsc_decrc_restores_g0_charset_selection() {
    let mut t = Terminal::new(2, 10);
    // Select DEC line-drawing on G0, save, switch G0 back to ASCII, restore.
    t.advance(b"\x1b(0"); // G0 = line drawing
    t.advance(b"\x1b7"); // DECSC saves position + charset
    t.advance(b"\x1b(B"); // G0 = ASCII
                          // `q` in line-drawing maps to the horizontal-line glyph; in ASCII it is 'q'.
    t.advance(b"q");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().c,
        'q',
        "ASCII active: 'q' prints literally"
    );
    t.advance(b"\x1b8"); // DECRC restores -> G0 line-drawing again
    t.advance(b"\r");
    t.advance(b"q");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().c,
        '─',
        "restored line-drawing charset maps 'q' to the horizontal bar"
    );
}

#[test]
fn decrc_without_save_homes_cursor() {
    let mut t = Terminal::new(4, 10);
    t.advance(b"\x1b[3;5H"); // move to row 3 col 5
    t.advance(b"\x1b8"); // DECRC with nothing saved -> home
    assert_eq!(
        t.cursor_position(),
        Some((0, 0)),
        "bare restore homes cursor"
    );
}

// ---- Tab stops: HTS / TBC / CHT / CBT ----

#[test]
fn hts_sets_stop_and_tab_lands_on_it() {
    let mut t = Terminal::new(2, 20);
    t.advance(b"\x1b[1G"); // col 0
    t.advance(b"   "); // advance to col 3
    t.advance(b"\x1bH"); // HTS: set a tab stop at col 3
    t.advance(b"\r"); // back to col 0
    t.advance(b"\t"); // tab forward -> should land on col 3 (the new stop)
    assert_eq!(
        t.cursor_position(),
        Some((0, 3)),
        "tab lands on the HTS stop"
    );
}

#[test]
fn tbc_clears_stop_at_cursor() {
    let mut t = Terminal::new(2, 40);
    // Default stop at col 8. Move there, clear it (CSI 0 g), then a tab from
    // col 0 must skip past 8 to the next default stop (16).
    t.advance(b"\x1b[9G"); // col 8 (1-based 9)
    t.advance(b"\x1b[0g"); // TBC clear at cursor
    t.advance(b"\r\t");
    assert_eq!(
        t.cursor_position(),
        Some((0, 16)),
        "with col-8 stop cleared, tab skips to col 16"
    );
}

#[test]
fn tbc_clear_all_then_tab_goes_to_last_column() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[3g"); // clear ALL tab stops
    t.advance(b"\r\t"); // no stop ahead -> last column (9)
    assert_eq!(
        t.cursor_position(),
        Some((0, 9)),
        "tab to last col when no stop"
    );
}

#[test]
fn cht_advances_multiple_tab_stops() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\r");
    t.advance(b"\x1b[2I"); // CHT: forward 2 tab stops -> col 16
    assert_eq!(t.cursor_position(), Some((0, 16)));
}

#[test]
fn cbt_moves_back_tab_stops() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[30G"); // col 29
    t.advance(b"\x1b[2Z"); // CBT: back 2 stops from 29 -> 24 -> 16
    assert_eq!(t.cursor_position(), Some((0, 16)));
}

// ---- Reverse index (ESC M) and IND (ESC D) / NEL (ESC E) ----

#[test]
fn reverse_index_moves_up_when_not_at_top() {
    let mut t = Terminal::new(3, 5);
    t.advance(b"\x1b[2;1H"); // row 1 (0-based)
    t.advance(b"\x1bM"); // RI -> up one row, no scroll
    assert_eq!(t.cursor_position(), Some((0, 0)));
}

#[test]
fn nel_does_cr_then_lf() {
    let mut t = Terminal::new(3, 10);
    t.advance(b"abc"); // cursor at col 3, row 0
    t.advance(b"\x1bE"); // NEL: CR + LF -> col 0, row 1
    assert_eq!(t.cursor_position(), Some((1, 0)));
}

#[test]
fn ind_indexes_down() {
    let mut t = Terminal::new(3, 10);
    t.advance(b"abc"); // col 3 row 0
    t.advance(b"\x1bD"); // IND: down one row, column unchanged
    assert_eq!(t.cursor_position(), Some((1, 3)));
}

// ---- RIS hard reset (ESC c) ----

#[test]
fn ris_clears_screen_scrollback_and_homes() {
    let mut t = Terminal::with_scrollback(2, 8, 100);
    t.advance(b"x\r\ny\r\nz\r\nw"); // build scrollback + content
    t.advance(b"\x1b[31m"); // set a non-default pen
    assert!(t.scrollback_len() > 0);
    t.advance(b"\x1bc"); // RIS
    assert_eq!(t.scrollback_len(), 0, "scrollback dropped");
    assert_eq!(t.cursor_position(), Some((0, 0)), "cursor homed");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ', "screen cleared");
    // Pen reset: a freshly printed glyph is default fg.
    t.advance(b"Q");
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Default);
}

// ---- Focus reporting (CSI I / CSI O) ----

#[test]
fn focus_report_emits_only_when_enabled() {
    let mut t = Terminal::new(2, 10);
    // Disabled by default: focus_report is a no-op.
    t.focus_report(true);
    assert!(
        t.take_pty_response().is_empty(),
        "no report while ?1004 off"
    );
    t.advance(b"\x1b[?1004h"); // enable focus reporting
    t.focus_report(true);
    assert_eq!(t.take_pty_response(), b"\x1b[I", "focus-in report");
    t.focus_report(false);
    assert_eq!(t.take_pty_response(), b"\x1b[O", "focus-out report");
}

// ---- CSI cursor motion: CUD ceiling, CNL, CPL, CHA, VPA ----

#[test]
fn cud_stops_at_bottom_margin() {
    let mut t = Terminal::new(5, 10);
    t.advance(b"\x1b[2;4r"); // scroll region rows 1..=3 (1-based 2..=4)
                             // Inside the region: CUD stops at the bottom margin (row 3), not the
                             // physical bottom (row 4).
    t.advance(b"\x1b[2;1H"); // row 1, inside region
    t.advance(b"\x1b[99B");
    assert_eq!(
        t.cursor_position(),
        Some((3, 0)),
        "in-region CUD stops at bottom margin"
    );
}

#[test]
fn cud_below_region_bounded_by_physical_bottom() {
    let mut t = Terminal::new(6, 10);
    t.advance(b"\x1b[2;3r"); // region rows 1..=2; this homes cursor to row 1
                             // Place the cursor BELOW the bottom margin (row 4, where row > scroll_bottom).
                             // DECTCEM-independent absolute move ignores the region in non-origin mode.
    t.advance(b"\x1b[5;1H"); // row 4 (below region bottom = row 2)
    assert_eq!(t.cursor_position(), Some((4, 0)));
    // CUD from below the region: ceiling is the physical bottom (row 5).
    t.advance(b"\x1b[99B");
    assert_eq!(
        t.cursor_position(),
        Some((5, 0)),
        "below-region CUD hits physical bottom"
    );
}

#[test]
fn cnl_and_cpl_move_to_column_zero() {
    let mut t = Terminal::new(5, 10);
    t.advance(b"\x1b[1;5H"); // row 0 col 4
    t.advance(b"\x1b[2E"); // CNL: down 2, col 0
    assert_eq!(t.cursor_position(), Some((2, 0)));
    t.advance(b"\x1b[3;5H"); // row 2 col 4
    t.advance(b"\x1b[1F"); // CPL: up 1, col 0
    assert_eq!(t.cursor_position(), Some((1, 0)));
}

#[test]
fn cha_sets_absolute_column() {
    let mut t = Terminal::new(3, 20);
    t.advance(b"\x1b[10G"); // CHA: col 9 (1-based 10)
    assert_eq!(t.cursor_position(), Some((0, 9)));
}

#[test]
fn vpa_sets_absolute_row() {
    let mut t = Terminal::new(5, 10);
    t.advance(b"abc"); // col 3
    t.advance(b"\x1b[3d"); // VPA: row 2 (1-based 3), col preserved
    assert_eq!(t.cursor_position(), Some((2, 3)));
}

// ---- CSI editing: IL, DL, ICH, DCH, ECH ----

#[test]
fn il_inserts_blank_lines_within_region() {
    let mut t = Terminal::new(4, 5);
    t.advance(b"AAAA\r\nBBBB\r\nCCCC"); // rows 0,1,2
    t.advance(b"\x1b[2;1H"); // row 1
    t.advance(b"\x1b[1L"); // IL: insert one blank line at row 1
    assert_eq!(t.grid().cell(1, 0).unwrap().c, ' ', "row 1 now blank");
    assert_eq!(
        t.grid().cell(2, 0).unwrap().c,
        'B',
        "B shifted down to row 2"
    );
    assert_eq!(t.cursor_position(), Some((1, 0)), "IL homes column");
}

#[test]
fn dl_deletes_lines_within_region() {
    let mut t = Terminal::new(4, 5);
    t.advance(b"AAAA\r\nBBBB\r\nCCCC");
    t.advance(b"\x1b[1;1H"); // row 0
    t.advance(b"\x1b[1M"); // DL: delete one line at row 0
    assert_eq!(
        t.grid().cell(0, 0).unwrap().c,
        'B',
        "B shifted up into row 0"
    );
    assert_eq!(t.cursor_position(), Some((0, 0)));
}

#[test]
fn ech_erases_chars_in_place() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"abcdef");
    t.advance(b"\x1b[1;2H"); // col 1
    t.advance(b"\x1b[3X"); // ECH: erase 3 chars (no shift)
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'a');
    assert_eq!(t.grid().cell(0, 1).unwrap().c, ' ');
    assert_eq!(t.grid().cell(0, 2).unwrap().c, ' ');
    assert_eq!(t.grid().cell(0, 3).unwrap().c, ' ');
    assert_eq!(
        t.grid().cell(0, 4).unwrap().c,
        'e',
        "e past the erased run intact"
    );
}

// ---- SD (scroll down, CSI T) ----

#[test]
fn sd_scrolls_region_down() {
    let mut t = Terminal::new(3, 5);
    t.advance(b"AAAAA\r\nBBBBB\r\nCCCCC");
    t.advance(b"\x1b[2T"); // SD: scroll whole region down 2
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ', "top now blank");
    assert_eq!(
        t.grid().cell(2, 0).unwrap().c,
        'A',
        "A pushed down two rows"
    );
}

// ---- REP (CSI b) repeats last grapheme ----

#[test]
fn rep_repeats_last_printed_glyph() {
    let mut t = Terminal::new(2, 20);
    t.advance(b"x");
    t.advance(b"\x1b[4b"); // REP: repeat 'x' 4 more times
    let line: String = (0..5).map(|c| t.grid().cell(0, c).unwrap().c).collect();
    assert_eq!(line, "xxxxx", "1 original + 4 repeats");
}

#[test]
fn rep_with_no_prior_print_is_noop() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b[3b"); // REP with nothing printed yet
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ', "no glyph to repeat");
}

// ---- SGR attribute combinations + reset arms ----

/// SGR 2/5/6/8/53 previously fell into the parser's catch-all `_ => {}` arm and
/// were silently dropped. Each must now land on its own flag, and each must have
/// its own reset arm (22 dim, 25 blink, 28 conceal, 55 overline).
#[test]
fn sgr_dim_blink_conceal_overline_are_parsed_and_reset() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[2;5;6;8;53mX");
    let c = t.grid().cell(0, 0).unwrap();
    assert!(c.flags.dim, "SGR 2 -> dim");
    assert!(c.flags.blink, "SGR 5 -> blink");
    assert!(c.flags.rapid_blink, "SGR 6 -> rapid blink");
    assert!(c.flags.conceal, "SGR 8 -> conceal");
    assert!(c.flags.overline, "SGR 53 -> overline");

    // 25 cancels BOTH blink rates; 28 conceal-off; 55 overline-off.
    t.advance(b"\x1b[25;28;55mY");
    let c = t.grid().cell(0, 1).unwrap();
    assert!(!c.flags.blink, "SGR 25 clears slow blink");
    assert!(!c.flags.rapid_blink, "SGR 25 also clears rapid blink");
    assert!(!c.flags.conceal, "SGR 28 clears conceal");
    assert!(!c.flags.overline, "SGR 55 clears overline");
    assert!(c.flags.dim, "SGR 25/28/55 must not touch dim");
}

/// ECMA-48 SGR 22 is "normal intensity": it cancels BOTH bold (1) and faint (2).
/// There is no separate dim-off code, so a 22 that only cleared bold would leave
/// text permanently dim.
#[test]
fn sgr_22_cancels_both_bold_and_dim() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[1;2mA");
    let c = t.grid().cell(0, 0).unwrap();
    assert!(c.flags.bold && c.flags.dim);
    t.advance(b"\x1b[22mB");
    let c = t.grid().cell(0, 1).unwrap();
    assert!(!c.flags.bold, "SGR 22 clears bold");
    assert!(!c.flags.dim, "SGR 22 also clears dim");
}

/// SGR 0 must clear the newly-parsed attributes too — a reset that only knew
/// about the old flag set would leak dim/blink/conceal/overline forever.
#[test]
fn sgr_0_clears_the_extended_attributes() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[2;5;6;8;53mA\x1b[0mB");
    let c = t.grid().cell(0, 1).unwrap();
    assert!(!c.flags.dim && !c.flags.blink && !c.flags.rapid_blink);
    assert!(!c.flags.conceal && !c.flags.overline);
}

/// SGR 39/49 must restore the TAGGED `Color::Default` — not palette slot 7/0.
/// Storing a resolved index here would break live theme switching and would
/// paint "default" text as ANSI white.
#[test]
fn sgr_39_and_49_restore_tagged_default_not_palette_slots() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[37;40mA"); // explicitly white-on-black from the palette
    let c = t.grid().cell(0, 0).unwrap();
    assert_eq!(c.fg, Color::Indexed(7));
    assert_eq!(c.bg, Color::Indexed(0));
    t.advance(b"\x1b[39;49mB");
    let c = t.grid().cell(0, 1).unwrap();
    assert_eq!(c.fg, Color::Default, "39 must be Default, never Indexed(7)");
    assert_eq!(c.bg, Color::Default, "49 must be Default, never Indexed(0)");
}

/// The 256-colour operand must survive the parser as a TAGGED index all the way
/// into the cell, so the theme (not the parser) owns resolution.
#[test]
fn sgr_extended_index_reaches_the_cell_untagged_by_the_parser() {
    let mut t = Terminal::new(2, 40);
    t.advance(b"\x1b[38;5;208mO");
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Indexed(208));
    // And it resolves to the canonical xterm orange through the theme.
    let theme = crate::theme::Theme::builtin_void();
    assert_eq!(theme.ansi(208), (255, 135, 0));
}

#[test]
fn sgr_all_attributes_and_individual_resets() {
    let mut t = Terminal::new(2, 30);
    // bold, italic, underline, inverse, strikeout all on at once.
    t.advance(b"\x1b[1;3;4;7;9mX");
    let c = t.grid().cell(0, 0).unwrap();
    assert!(c.flags.bold && c.flags.italic && c.flags.inverse && c.flags.strikeout);
    assert!(c.flags.underline());
    // Individual resets: 22 bold-off, 23 italic-off, 24 underline-off,
    // 27 inverse-off, 29 strikeout-off.
    t.advance(b"\x1b[22;23;24;27;29mY");
    let c = t.grid().cell(0, 1).unwrap();
    assert!(!c.flags.bold && !c.flags.italic && !c.flags.inverse && !c.flags.strikeout);
    assert!(!c.flags.underline(), "underline reset by SGR 24");
}

#[test]
fn sgr_double_underline_21_and_bright_colors() {
    let mut t = Terminal::new(2, 30);
    t.advance(b"\x1b[21mA"); // SGR 21 -> double underline
    assert_eq!(
        t.grid().cell(0, 0).unwrap().flags.underline_style,
        UnderlineStyle::Double
    );
    // Bright foreground 90..=97 -> indexed 8..=15.
    t.advance(b"\x1b[92mB"); // bright green -> index 10
    assert_eq!(t.grid().cell(0, 1).unwrap().fg, Color::Indexed(10));
    // Bright background 100..=107 -> indexed 8..=15.
    t.advance(b"\x1b[105mC"); // bright magenta bg -> index 13
    assert_eq!(t.grid().cell(0, 2).unwrap().bg, Color::Indexed(13));
}

#[test]
fn sgr_default_fg_bg_and_bg_indexed() {
    let mut t = Terminal::new(2, 30);
    t.advance(b"\x1b[44mB"); // bg indexed 4 (blue)
    assert_eq!(t.grid().cell(0, 0).unwrap().bg, Color::Indexed(4));
    t.advance(b"\x1b[39;49mD"); // reset fg + bg to default
    let c = t.grid().cell(0, 1).unwrap();
    assert_eq!(c.fg, Color::Default);
    assert_eq!(c.bg, Color::Default);
}

#[test]
fn sgr_underline_color_set_and_reset() {
    let mut t = Terminal::new(2, 30);
    // SGR 58;2;r;g;b sets underline color; 4 turns underline on.
    t.advance(b"\x1b[4;58;2;10;20;30mU");
    let c = t.grid().cell(0, 0).unwrap();
    assert_eq!(c.underline_color, Some(Color::Rgb(10, 20, 30)));
    // SGR 59 clears it.
    t.advance(b"\x1b[59mV");
    assert_eq!(t.grid().cell(0, 1).unwrap().underline_color, None);
}

#[test]
fn sgr_underline_color_indexed_colon_form() {
    let mut t = Terminal::new(2, 30);
    // Colon form `58:5:n` selects indexed underline color.
    t.advance(b"\x1b[4;58:5:200mU");
    assert_eq!(
        t.grid().cell(0, 0).unwrap().underline_color,
        Some(Color::Indexed(200))
    );
}

#[test]
fn sgr_extended_bg_indexed_semicolon_form() {
    let mut t = Terminal::new(2, 30);
    t.advance(b"\x1b[48;5;123mZ"); // bg indexed 123
    assert_eq!(t.grid().cell(0, 0).unwrap().bg, Color::Indexed(123));
}

// ---- Mouse encodings: urxvt, wheel, motion gating, X10 release ----

#[test]
fn encode_mouse_urxvt_offsets_button_by_32() {
    let mut t = Terminal::new(4, 80);
    t.advance(b"\x1b[?1000h\x1b[?1015h"); // normal tracking, urxvt encoding
    let out = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            5,
            7,
            MouseEventKind::Press,
        )
        .unwrap();
    // Left=0, +32 = 32; decimal `CSI 32 ; 5 ; 7 M`.
    assert_eq!(out, b"\x1b[32;5;7M");
}

#[test]
fn encode_mouse_sgr_wheel_up_is_button_64_press() {
    let mut t = Terminal::new(4, 80);
    t.advance(b"\x1b[?1000h\x1b[?1006h"); // SGR encoding
    let out = t
        .encode_mouse(
            MouseButton::WheelUp,
            MouseModifiers::default(),
            3,
            4,
            MouseEventKind::Press,
        )
        .unwrap();
    // Wheel up = 64; SGR always reports wheel as press (`M`).
    assert_eq!(out, b"\x1b[<64;3;4M");
    // Wheel "release" still encodes with `M` (not `m`).
    let out = t
        .encode_mouse(
            MouseButton::WheelDown,
            MouseModifiers::default(),
            3,
            4,
            MouseEventKind::Release,
        )
        .unwrap();
    assert_eq!(out, b"\x1b[<65;3;4M", "wheel down=65, release still `M`");
}

#[test]
fn encode_mouse_anyevent_reports_bare_motion() {
    let mut t = Terminal::new(4, 80);
    t.advance(b"\x1b[?1003h\x1b[?1006h"); // any-event + SGR
    let out = t
        .encode_mouse(
            MouseButton::None,
            MouseModifiers::default(),
            2,
            2,
            MouseEventKind::Motion,
        )
        .unwrap();
    // None=3 base + 32 motion bit = 35.
    assert_eq!(out, b"\x1b[<35;2;2M");
}

#[test]
fn encode_mouse_buttonevent_drops_motion_without_held_button() {
    let mut t = Terminal::new(4, 80);
    t.advance(b"\x1b[?1002h\x1b[?1006h"); // button-event + SGR
                                          // ?1002 reports motion only while a button is held -> None for None button.
    let none = t.encode_mouse(
        MouseButton::None,
        MouseModifiers::default(),
        1,
        1,
        MouseEventKind::Motion,
    );
    assert!(none.is_none(), "no bare motion under ?1002");
    // Held-button drag IS reported.
    let drag = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            1,
            1,
            MouseEventKind::Motion,
        )
        .unwrap();
    // Left=0 + 32 motion = 32.
    assert_eq!(drag, b"\x1b[<32;1;1M");
}

#[test]
fn encode_mouse_x10_release_collapses_to_button_three() {
    let mut t = Terminal::new(4, 80);
    t.advance(b"\x1b[?1000h"); // X10 encoding (default)
    let out = t
        .encode_mouse(
            MouseButton::Right,
            MouseModifiers::default(),
            1,
            1,
            MouseEventKind::Release,
        )
        .unwrap();
    // X10 release forces low button bits to 3: byte = (3) + 32 = 35 = '#'.
    assert_eq!(out[0], 0x1b);
    assert_eq!(&out[1..3], b"[M");
    assert_eq!(out[3], 35, "release collapses to button 3 (+32)");
}

// ---- DSR cursor position report (CSI 6n) and terminal-OK (CSI 5n) ----

#[test]
fn dsr_cursor_position_report() {
    let mut t = Terminal::new(10, 40);
    t.advance(b"\x1b[3;7H"); // row 2 col 6 (0-based)
    t.advance(b"\x1b[6n"); // DSR cursor position request
    let reply = String::from_utf8(t.take_pty_response()).unwrap();
    assert_eq!(reply, "\x1b[3;7R", "CPR is 1-based row;col");
}

#[test]
fn dsr_terminal_ok() {
    let mut t = Terminal::new(4, 10);
    t.advance(b"\x1b[5n");
    assert_eq!(t.take_pty_response(), b"\x1b[0n");
}

// ---- Device attributes: primary + secondary ----

#[test]
fn da_primary_and_secondary_replies() {
    let mut t = Terminal::new(4, 10);
    t.advance(b"\x1b[c"); // primary DA
    assert_eq!(t.take_pty_response(), b"\x1b[?62;1;6;22c");
    t.advance(b"\x1b[>c"); // secondary DA
    assert_eq!(t.take_pty_response(), b"\x1b[>0;0;0c");
}

// ---- Erase-in-display modes 0/1, erase-in-line 0/1 ----

#[test]
fn erase_display_cursor_to_end_and_start_to_cursor() {
    let mut t = Terminal::new(3, 4);
    t.advance(b"AAAA\r\nBBBB\r\nCCCC");
    t.advance(b"\x1b[2;3H"); // row 1, col 2
    t.advance(b"\x1b[0J"); // erase cursor -> end
    assert_eq!(t.grid().cell(1, 1).unwrap().c, 'B', "before cursor intact");
    assert_eq!(t.grid().cell(1, 2).unwrap().c, ' ', "at cursor erased");
    assert_eq!(t.grid().cell(2, 0).unwrap().c, ' ', "rows below erased");
    // Now mode 1: start -> cursor.
    let mut t = Terminal::new(3, 4);
    t.advance(b"AAAA\r\nBBBB\r\nCCCC");
    t.advance(b"\x1b[2;3H");
    t.advance(b"\x1b[1J");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ', "row above erased");
    assert_eq!(
        t.grid().cell(1, 2).unwrap().c,
        ' ',
        "up to+incl cursor erased"
    );
    assert_eq!(t.grid().cell(1, 3).unwrap().c, 'B', "after cursor intact");
}

#[test]
fn erase_in_line_modes() {
    // Mode 0: cursor -> EOL.
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcdef");
    t.advance(b"\x1b[1;3H"); // col 2
    t.advance(b"\x1b[0K");
    assert_eq!(t.grid().cell(0, 1).unwrap().c, 'b');
    assert_eq!(t.grid().cell(0, 2).unwrap().c, ' ');
    assert_eq!(t.grid().cell(0, 5).unwrap().c, ' ');
    // Mode 1: BOL -> cursor.
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcdef");
    t.advance(b"\x1b[1;3H");
    t.advance(b"\x1b[1K");
    assert_eq!(t.grid().cell(0, 0).unwrap().c, ' ');
    assert_eq!(t.grid().cell(0, 2).unwrap().c, ' ');
    assert_eq!(t.grid().cell(0, 3).unwrap().c, 'd', "after cursor intact");
    // Mode 2: whole line.
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcdef");
    t.advance(b"\x1b[2K");
    for c in 0..6 {
        assert_eq!(t.grid().cell(0, c).unwrap().c, ' ');
    }
}

// ---- DECSTBM reset to full when invalid + scroll-region getter ----

#[test]
fn decstbm_invalid_region_resets_to_full() {
    let mut t = Terminal::new(5, 10);
    t.advance(b"\x1b[2;4r"); // valid region 1..=3
    assert_eq!(t.scroll_region(), (1, 3));
    // top >= bottom is invalid -> reset to full screen.
    t.advance(b"\x1b[4;2r");
    assert_eq!(t.scroll_region(), (0, 4), "invalid region resets to full");
}

#[test]
fn decstbm_no_params_is_full_screen() {
    let mut t = Terminal::new(5, 10);
    t.advance(b"\x1b[2;4r"); // set a region
    t.advance(b"\x1b[r"); // bare DECSTBM -> full screen
    assert_eq!(t.scroll_region(), (0, 4));
    assert_eq!(t.cursor_position(), Some((0, 0)), "DECSTBM homes cursor");
}

#[test]
fn cuu_above_region_bounded_by_physical_top() {
    let mut t = Terminal::new(6, 10);
    t.advance(b"\x1b[3;5r"); // region rows 2..=4; homes cursor to row 2
                             // Move ABOVE the top margin (row 0, where row < scroll_top).
    t.advance(b"\x1b[1;1H"); // row 0
    t.advance(b"\x1b[99A"); // CUU: floor is physical top (row 0) when above region
    assert_eq!(
        t.cursor_position(),
        Some((0, 0)),
        "above-region CUU floored at row 0"
    );
}

// ---- IRM insert getter + DECOM origin getter ----

#[test]
fn insert_mode_getter_tracks_csi_4h_l() {
    let mut t = Terminal::new(2, 10);
    assert!(!t.insert_mode());
    t.advance(b"\x1b[4h");
    assert!(t.insert_mode(), "CSI 4 h enables IRM");
    t.advance(b"\x1b[4l");
    assert!(!t.insert_mode());
}

#[test]
fn origin_mode_getter_tracks_mode_6() {
    let mut t = Terminal::new(5, 10);
    assert!(!t.origin_mode());
    t.advance(b"\x1b[?6h");
    assert!(t.origin_mode());
    t.advance(b"\x1b[?6l");
    assert!(!t.origin_mode());
}

// ---- DECRQM ANSI IRM + extra private modes ----

#[test]
fn decrqm_reports_origin_and_reverse_modes() {
    let mut t = Terminal::new(5, 10);
    t.advance(b"\x1b[?6h"); // origin on
    t.advance(b"\x1b[?6$p"); // DECRQM private mode 6
    let r = String::from_utf8(t.take_pty_response()).unwrap();
    assert_eq!(r, "\x1b[?6;1$y", "origin mode reported as set");
    t.advance(b"\x1b[?5$p"); // reverse-screen mode 5, currently reset
    let r = String::from_utf8(t.take_pty_response()).unwrap();
    assert_eq!(r, "\x1b[?5;2$y", "reverse-screen reported as reset");
}

// ---- DECSTR soft reset (CSI ! p) ----

#[test]
fn decstr_soft_reset_preserves_scrollback() {
    let mut t = Terminal::with_scrollback(2, 8, 100);
    t.advance(b"a\r\nb\r\nc\r\nd"); // build scrollback
    let sb = t.scrollback_len();
    assert!(sb > 0);
    t.advance(b"\x1b[31m\x1b[4h"); // non-default pen + insert mode
    t.advance(b"\x1b[!p"); // DECSTR soft reset
    assert_eq!(t.scrollback_len(), sb, "soft reset keeps scrollback");
    assert!(!t.insert_mode(), "soft reset clears IRM");
    t.advance(b"Z");
    assert_eq!(t.grid().cell(0, 0).unwrap().fg, Color::Default, "pen reset");
}

// ---- XTWINOPS title stack push/pop ----

#[test]
fn xtwinops_title_stack_push_and_pop() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]0;first\x07");
    assert_eq!(t.title(), "first");
    t.advance(b"\x1b[22t"); // push title
    assert_eq!(t.title_stack_depth(), 1);
    t.advance(b"\x1b]0;second\x07");
    assert_eq!(t.title(), "second");
    t.advance(b"\x1b[23t"); // pop title -> back to "first"
    assert_eq!(t.title(), "first");
    assert_eq!(t.title_stack_depth(), 0);
}

#[test]
fn xtwinops_pop_on_empty_stack_is_noop() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]0;solo\x07");
    t.advance(b"\x1b[23t"); // pop with empty stack
    assert_eq!(t.title(), "solo", "title unchanged on empty-stack pop");
}

// ---- OSC 7 cwd + OSC 133 command marks (C/D) ----

#[test]
fn osc7_captures_cwd() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]7;file:///home/user\x07");
    assert_eq!(t.cwd(), Some("file:///home/user"));
}

#[test]
fn osc133_output_start_and_command_end_marks() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;C\x07"); // output start
    t.advance(b"\x1b]133;D;0\x07"); // command end, exit 0
    let marks = t.command_marks();
    assert_eq!(marks.len(), 2);
    assert!(matches!(marks[0].kind, CommandMarkKind::OutputStart));
    assert!(matches!(
        marks[1].kind,
        CommandMarkKind::CommandEnd { exit_code: Some(0) }
    ));
    assert_eq!(t.last_command_exit_code(), Some(Some(0)));
}

#[test]
fn osc133_command_end_without_code() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;D\x07"); // command end, no exit code
    assert_eq!(t.last_command_exit_code(), Some(None));
}

// ---- Charset SO/SI shift via execute (0x0e / 0x0f) ----

#[test]
fn so_si_shift_between_g0_and_g1_charsets() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b)0"); // G1 = line drawing
    t.advance(b"\x0e"); // SO: invoke G1 into GL
    t.advance(b"q"); // 'q' -> horizontal line under G1
    assert_eq!(t.grid().cell(0, 0).unwrap().c, '─');
    t.advance(b"\x0f"); // SI: back to G0 (ASCII)
    t.advance(b"q");
    assert_eq!(t.grid().cell(0, 1).unwrap().c, 'q', "G0 ASCII active again");
}

// ---- Backspace control (0x08) ----

#[test]
fn backspace_moves_cursor_left_and_saturates() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"ab");
    t.advance(&[0x08]); // BS
    assert_eq!(t.cursor_position(), Some((0, 1)));
    t.advance(&[0x08, 0x08, 0x08]); // saturate at col 0
    assert_eq!(t.cursor_position(), Some((0, 0)));
}

// ---- Autowrap off (DECAWM ?7l) clamps instead of wrapping ----

#[test]
fn autowrap_off_overwrites_last_column() {
    let mut t = Terminal::new(2, 3);
    t.advance(b"\x1b[?7l"); // autowrap off
    t.advance(b"abcd"); // 4 chars into 3 cols
                        // No wrap: stays on row 0; last column holds the final glyph.
    assert_eq!(t.cursor_position().unwrap().0, 0, "no wrap to row 1");
    assert_eq!(t.grid().cell(0, 2).unwrap().c, 'd', "last col overwritten");
    assert!(t.grid().cell(1, 0).unwrap().c == ' ', "row 1 untouched");
}

// ---- IRM insert mode actually shifts on print at a populated column ----

#[test]
fn irm_insert_shifts_existing_glyphs_right() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"world");
    t.advance(b"\x1b[1;1H"); // home
    t.advance(b"\x1b[4h"); // IRM on
    t.advance(b"X"); // insert 'X' -> shifts "world" right
    assert_eq!(t.grid().cell(0, 0).unwrap().c, 'X');
    assert_eq!(t.grid().cell(0, 1).unwrap().c, 'w', "world shifted right");
}

// ---- Final edge-branch batch: dynamic-color cursor query, OSC edge arms,
//      mouse middle/modifier arms, malformed Kitty APC, view-scroll helpers,
//      and scrollback-backed visible-row iteration. ----

#[test]
fn osc12_cursor_color_query_replies() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]12;#0a141e\x07"); // set cursor color
    let _ = t.take_pty_response();
    t.advance(b"\x1b]12;?\x07"); // query cursor color
    let reply = String::from_utf8(t.take_pty_response()).unwrap();
    assert_eq!(reply, "\x1b]12;rgb:0a0a/1414/1e1e\x07");
}

#[test]
fn osc10_with_no_spec_is_noop() {
    let mut t = Terminal::new(2, 10);
    let before = t.dynamic_color(DynamicColor::Foreground);
    t.advance(b"\x1b]10\x07"); // OSC 10 with no spec param -> early return
    assert_eq!(t.dynamic_color(DynamicColor::Foreground), before);
    assert!(t.take_pty_response().is_empty());
}

#[test]
fn osc8_empty_uri_is_not_captured() {
    let mut t = Terminal::new(2, 20);
    t.advance(b"\x1b]8;;\x07"); // OSC 8 with an empty URI -> not stored
    assert!(t.hyperlinks().is_empty(), "empty OSC 8 URI is dropped");
}

#[test]
fn osc133_duplicate_prompt_mark_is_deduped() {
    let mut t = Terminal::new(4, 20);
    // Two A marks at the same absolute line -> only one stored (dedup arm).
    t.advance(b"\x1b]133;A\x07");
    t.advance(b"\x1b]133;A\x07");
    assert_eq!(t.prompt_marks().len(), 1, "same-line prompt mark deduped");
}

#[test]
fn osc133_unknown_kind_is_ignored() {
    let mut t = Terminal::new(4, 20);
    t.advance(b"\x1b]133;Z\x07"); // unknown semantic-zone kind -> ignored
    assert!(t.prompt_marks().is_empty());
    assert!(t.command_marks().is_empty());
}

#[test]
fn osc133_command_end_parses_embedded_exit_code() {
    let mut t = Terminal::new(4, 20);
    // `D;aid=7` form: the exit code is the first integer-looking token.
    t.advance(b"\x1b]133;D;aid=7\x07");
    assert_eq!(t.last_command_exit_code(), Some(Some(7)));
}

#[test]
fn dsr_unknown_ps_produces_no_reply() {
    let mut t = Terminal::new(4, 10);
    t.advance(b"\x1b[99n"); // DSR with an unsupported parameter
    assert!(t.take_pty_response().is_empty());
}

#[test]
fn erase_display_and_line_unknown_modes_are_noops() {
    let mut t = Terminal::new(2, 6);
    t.advance(b"abcdef");
    t.advance(b"\x1b[9J"); // unknown ED mode -> default arm, no change
    t.advance(b"\x1b[9K"); // unknown EL mode -> default arm, no change
    assert_eq!(
        t.grid().cell(0, 0).unwrap().c,
        'a',
        "unknown erase modes are no-ops"
    );
}

#[test]
fn xtwinops_unknown_op_is_ignored() {
    let mut t = Terminal::new(2, 10);
    t.advance(b"\x1b]0;keep\x07");
    t.advance(b"\x1b[18t"); // report-size op we intentionally ignore
    assert_eq!(t.title(), "keep");
    assert_eq!(
        t.title_stack_depth(),
        0,
        "non-stack XTWINOPS op left stack alone"
    );
}

#[test]
fn encode_mouse_middle_button_and_modifiers_combine() {
    let mut t = Terminal::new(4, 80);
    t.advance(b"\x1b[?1000h\x1b[?1006h"); // normal + SGR
                                          // Middle button (1) + shift (4) + alt (8) = 13.
    let out = t
        .encode_mouse(
            MouseButton::Middle,
            MouseModifiers {
                shift: true,
                alt: true,
                control: false,
            },
            10,
            20,
            MouseEventKind::Press,
        )
        .unwrap();
    assert_eq!(out, b"\x1b[<13;10;20M", "middle+shift+alt = 1+4+8 = 13");
}

#[test]
fn encode_mouse_left_release_uses_lowercase_m_in_sgr() {
    let mut t = Terminal::new(4, 80);
    t.advance(b"\x1b[?1000h\x1b[?1006h");
    let out = t
        .encode_mouse(
            MouseButton::Left,
            MouseModifiers::default(),
            1,
            1,
            MouseEventKind::Release,
        )
        .unwrap();
    assert_eq!(out, b"\x1b[<0;1;1m", "non-wheel SGR release ends with `m`");
}

#[test]
fn malformed_kitty_apc_is_ignored() {
    let mut t = Terminal::new(4, 20);
    // APC body whose first byte is not 'G' (handled by the prefilter as non-Kitty
    // and swallowed) plus a Kitty APC with an unparseable control string.
    t.advance(b"\x1b_Gnot-a-valid-control\x1b\\");
    assert!(
        t.images().is_empty(),
        "malformed Kitty control produces no image"
    );
}

#[test]
fn kitty_apc_unknown_action_is_ignored() {
    let mut t = Terminal::new(4, 20);
    // a=q is not an action we handle -> the `_ => {}` arm, no image, no panic.
    t.advance(b"\x1b_Ga=q,i=1\x1b\\");
    assert!(t.images().is_empty());
}

#[test]
fn view_scroll_helpers_clamp_correctly() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    t.advance(b"a\r\nb\r\nc\r\nd\r\ne"); // build scrollback
    let sb = t.scrollback_len();
    assert!(sb >= 1);
    // set_view_offset clamps to history length.
    t.set_view_offset(1000);
    assert_eq!(t.view_offset(), sb, "set_view_offset clamps to history");
    // scroll_down_view saturates at 0.
    t.scroll_down_view(1000);
    assert_eq!(t.view_offset(), 0, "scroll_down_view saturates at live");
    // A second scroll_to_bottom while already at bottom is a no-op (early return).
    t.scroll_to_bottom();
    assert_eq!(t.view_offset(), 0);
}

#[test]
fn scroll_to_top_jumps_to_oldest_line_and_reports_movement() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    t.advance(b"a\r\nb\r\nc\r\nd\r\ne"); // build scrollback
    let sb = t.scrollback_len();
    assert!(sb >= 1, "test needs a non-empty scrollback");

    // From the live bottom, scroll_to_top must pin the view at the full history
    // length (oldest retained line at the top) and report that it moved.
    assert_eq!(t.view_offset(), 0, "starts following live output");
    assert!(t.scroll_to_top(), "scroll_to_top reports it moved the view");
    assert_eq!(
        t.view_offset(),
        sb,
        "scroll_to_top pins the view at the maximum scroll-up offset"
    );

    // Idempotent: a second scroll_to_top while already at the top is a no-op
    // and reports no movement (so the caller skips a needless repaint).
    assert!(
        !t.scroll_to_top(),
        "scroll_to_top at the top reports no movement"
    );
    assert_eq!(t.view_offset(), sb, "view stays pinned at the top");

    // scroll_to_top is the exact mirror of scroll_to_bottom.
    t.scroll_to_bottom();
    assert_eq!(
        t.view_offset(),
        0,
        "scroll_to_bottom returns to live output"
    );
}

#[test]
fn for_visible_rows_walks_scrollback_when_offset_set() {
    let mut t = Terminal::with_scrollback(2, 4, 100);
    t.advance(b"L0\r\nL1\r\nL2\r\nL3"); // L0/L1 scroll into history
    assert!(t.scrollback_len() >= 2);
    // Scroll the view back so the visible window includes a history row.
    t.scroll_up_view(2);
    let mut first_visible = String::new();
    t.for_visible_rows(|vr, cells| {
        if vr == 0 {
            first_visible = cells.iter().map(|c| c.c).collect();
        }
    });
    assert!(
        first_visible.starts_with("L0") || first_visible.starts_with("L1"),
        "scrolled-back view surfaces a history row, got {first_visible:?}"
    );
    // display_rows (the allocating wrapper) pads every history row to grid width.
    t.set_view_offset(t.scrollback_len());
    let rows = t.display_rows();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|r| r.len() == 4),
        "rows padded to grid width"
    );
}

#[test]
fn alt_screen_47_form_does_not_save_cursor() {
    let mut t = Terminal::new(4, 10);
    t.advance(b"\x1b[2;3H"); // row 1 col 2 on primary
    t.advance(b"\x1b[?47h"); // enter alt via the 47 form (no cursor save)
    assert!(t.alt_screen_active());
    t.advance(b"\x1b[4;5H"); // move on the alt screen
    t.advance(b"\x1b[?47l"); // leave alt; 47 does NOT restore the cursor
                             // The cursor is left where it was on the alt screen (clamped), not restored
                             // to the primary's saved (1,2).
    assert_eq!(
        t.cursor_position(),
        Some((3, 4)),
        "47 leaves cursor where it sat"
    );
}

#[test]
fn alt_screen_1047_clears_alt_on_exit() {
    let mut t = Terminal::new(4, 10);
    t.advance(b"primary");
    t.advance(b"\x1b[?1047h"); // enter alt via 1047 (homes cursor, no save)
    t.advance(b"ALT");
    t.advance(b"\x1b[?1047l"); // leave; 1047 clears the alt screen on the way out
                               // Primary content is restored.
    assert_eq!(
        t.grid().cell(0, 0).unwrap().c,
        'p',
        "primary restored after 1047"
    );
}

#[test]
fn clamp_scroll_region_after_shrink_resets_when_invalid() {
    let mut t = Terminal::new(10, 10);
    t.advance(b"\x1b[5;8r"); // region rows 4..=7 (custom, not full-screen)
    assert_eq!(t.scroll_region(), (4, 7));
    // Shrink the grid so the region's bottom (7) no longer fits (rows now 3).
    t.resize(3, 10);
    let (top, bottom) = t.scroll_region();
    assert!(bottom < 3, "scroll_bottom clamped within new height");
    assert!(top <= bottom, "region stays well-formed after shrink");
}

mod utf8_tail_boundary_tests {
    use crate::term::utf8_tail_boundary;

    #[test]
    fn complete_or_ascii_tails_hold_nothing() {
        // Empty, pure ASCII, and a complete multibyte tail all end on a boundary.
        assert_eq!(utf8_tail_boundary(b""), 0);
        assert_eq!(utf8_tail_boundary(b"abc"), 3);
        assert_eq!(utf8_tail_boundary("Ŀ".as_bytes()), 2); // C4 BF complete
        assert_eq!(utf8_tail_boundary("日".as_bytes()), 3); // E6 97 A5 complete
        assert_eq!(utf8_tail_boundary("😀".as_bytes()), 4); // 4-byte complete
        assert_eq!(utf8_tail_boundary(b"x\x1b[m"), 4); // ESC is ASCII — complete
    }

    #[test]
    fn incomplete_multibyte_tails_are_held_from_their_lead() {
        // Lone 2-byte lead: hold it.
        assert_eq!(utf8_tail_boundary(&[0xC4]), 0);
        assert_eq!(utf8_tail_boundary(b"A\xC4"), 1);
        // 3-byte lead with only 1 of 2 continuations: hold both.
        assert_eq!(utf8_tail_boundary(&[0xE3, 0x81]), 0);
        assert_eq!(utf8_tail_boundary(b"Z\xE6\x97"), 1);
        // 4-byte lead with 2 of 3 continuations: hold all three.
        assert_eq!(utf8_tail_boundary(&[0xF0, 0x9F, 0x98]), 0);
    }

    #[test]
    fn stray_continuations_are_not_held() {
        // A trailing continuation byte with no lead in view is invalid, not a
        // held partial — leave it so the parser emits a replacement char.
        assert_eq!(utf8_tail_boundary(&[0x80]), 1);
        assert_eq!(utf8_tail_boundary(b"a\xBF"), 2);
    }
}
