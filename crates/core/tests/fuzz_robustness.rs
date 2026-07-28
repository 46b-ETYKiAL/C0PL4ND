//! Security / robustness tests: hostile, malformed, and oversized input must
//! never panic, hang, or exhaust memory.
//!
//! A terminal consumes a fully untrusted byte stream (the PTY child is the
//! attacker model). These tests assert the three load-bearing invariants:
//!   1. **No panic** — any byte sequence is survivable; `advance` always returns.
//!   2. **No unbounded memory** — APC / Sixel / Kitty size caps hold.
//!   3. **No exfiltration / no break-out** — OSC 52 read stays default-off,
//!      OSC 133 never replies, and a paste / APC payload cannot smuggle a
//!      terminator that breaks out of its frame.
//!
//! All randomness is a deterministic LCG seeded by a constant — the build
//! forbids nondeterminism, so `rand`-style sources are not used.

use c0pl4nd_core::layout_persist::WorkspaceSnapshot;
use c0pl4nd_core::term::osc::base64_decode;
use c0pl4nd_core::Terminal;

/// A tiny deterministic LCG (Numerical Recipes constants). Seeded by a constant
/// so every run feeds the engine the exact same "random" stream — reproducible
/// by construction, never `Math.random`-style nondeterminism.
struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next_u32(&mut self) -> u32 {
        // x = x * 1664525 + 1013904223 (mod 2^32), advanced in 64-bit space.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn next_byte(&mut self) -> u8 {
        (self.next_u32() & 0xff) as u8
    }
}

/// (a) Large volumes of deterministic random bytes plus a corpus of
/// truncated / malformed / deeply-nested escape sequences must never panic.
#[test]
fn random_and_malformed_input_never_panics() {
    // 1 MB of deterministic pseudo-random bytes, fed in irregular chunks so
    // escape sequences straddle advance() boundaries (the realistic PTY-read
    // split). Chunk sizes themselves come from the LCG.
    let mut rng = Lcg::new(0x00C0_FFEE_1337_5EEDu64);
    let mut t = Terminal::with_scrollback(24, 80, 1_000);
    let mut produced = 0usize;
    while produced < 1_000_000 {
        let chunk_len = (rng.next_u32() % 64 + 1) as usize;
        let chunk: Vec<u8> = (0..chunk_len).map(|_| rng.next_byte()).collect();
        t.advance(&chunk);
        produced += chunk_len;
    }
    // Survived: the grid is still a sane shape.
    assert_eq!(t.grid().rows(), 24);
    assert_eq!(t.grid().cols(), 80);

    // A focused corpus of pathological escape sequences (huge/negative params,
    // unterminated OSC/DCS/APC, deep nesting). Each must be survivable both as
    // one chunk and byte-by-byte.
    let hostile: &[&[u8]] = &[
        b"\x1b[999999999999999999999999999999999m", // CSI param overflow
        b"\x1b[-1;-1H",                             // "negative" params (parsed as 0)
        b"\x1b[;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;m", // many empty params
        b"\x1b[38;2;",                              // truncated truecolor SGR
        b"\x1b]0;unterminated title forever",       // OSC, no terminator
        b"\x1b]8;;http://evil",                     // OSC 8 hyperlink, truncated
        b"\x1bP",                                   // bare DCS introducer
        b"\x1bPq#0;2;100;0;0",                      // Sixel DCS, no ST
        b"\x1b_Gf=32,s=1,v=1;",                     // Kitty APC, no payload, no ST
        b"\x1b_G",                                  // bare APC G, no body
        b"\x1b\x1b\x1b\x1b\x1b\x1b\x1b\x1b",        // ESC storm
        b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20m", // deep param list
        b"\x90\x9d\x9e\x9f",                        // raw C1 control bytes
        b"\x1b[6n\x1b[5n\x1b[c\x1b[>c",             // back-to-back query storm
    ];
    for seq in hostile {
        // Whole-chunk.
        let mut a = Terminal::with_scrollback(10, 40, 200);
        a.advance(seq);
        // Drain any responses so query storms cannot accumulate unboundedly.
        let _ = a.take_pty_response();
        // Byte-by-byte (sequences straddle reads).
        let mut b = Terminal::with_scrollback(10, 40, 200);
        for &byte in *seq {
            b.advance(&[byte]);
        }
        let _ = b.take_pty_response();
    }
}

/// (b) Sixel / Kitty / base64 decoders on malformed and OVERSIZED input must
/// return bounded results (or None) — never panic, never OOM. In particular
/// the >8 MiB APC body cap must drop the payload rather than buffer it.
#[test]
fn image_and_base64_decoders_are_bounded_on_hostile_input() {
    // --- base64: malformed inputs return None, never panic ---
    assert!(base64_decode(b"!!!!not base64").is_none());
    assert!(base64_decode(b"aGVsbG8==").is_none()); // over-padded
    assert!(base64_decode(b"aGV").is_none()); // bad length
                                              // A huge but well-formed base64 string decodes to a bounded buffer. Build
                                              // it from a real 600 KiB byte buffer so the padding (if any) lands only at
                                              // the very end — `"...".repeat()` would embed mid-string `==` and be
                                              // (correctly) rejected as malformed.
    let raw: Vec<u8> = (0..600_000u32).map(|i| (i & 0xff) as u8).collect();
    let big_b64 = c0pl4nd_core::term::osc::base64_encode(&raw);
    let decoded = base64_decode(big_b64.as_bytes());
    assert_eq!(
        decoded.as_deref(),
        Some(raw.as_slice()),
        "a large well-formed base64 string must round-trip without panicking or truncating"
    );

    // --- Sixel: malformed / empty / garbage never panics ---
    let mut t = Terminal::new(10, 40);
    t.advance(b"\x1bPq\x1b\\"); // empty sixel body
    t.advance(b"\x1bPq#####!!!!----$$$$\x1b\\"); // garbage control chars only
    t.advance(b"\x1bPq#0;2;999;999;999~\x1b\\"); // out-of-range colour components
                                                 // No drawable pixels in the garbage cases ⇒ no image; the valid one draws.
                                                 // (We assert only that we got here without panicking.)
    let _ = t.images();

    // --- APC size cap: a >8 MiB Kitty APC body must be DROPPED, not buffered ---
    // 9 MiB of base64 payload inside one APC. The engine caps accumulation at
    // 8 MiB and drops the over-cap body — the image must NOT appear.
    let mut huge = Vec::with_capacity(9 * 1024 * 1024 + 64);
    huge.extend_from_slice(b"\x1b_Gf=32,s=2048,v=2048,a=T;");
    huge.resize(huge.len() + 9 * 1024 * 1024, b'A'); // 9 MiB of 'A' (valid b64 char)
    huge.extend_from_slice(b"\x1b\\");
    let mut t2 = Terminal::new(10, 40);
    t2.advance(&huge);
    assert_eq!(
        t2.images().len(),
        0,
        "an APC body over the 8 MiB cap must be dropped, not decoded"
    );

    // A Kitty APC whose declared dimensions mismatch the payload length must
    // decode to None (length guard), not panic or allocate width*height blindly.
    let mut t3 = Terminal::new(10, 40);
    // Claims 1000x1000 RGBA (4 MB) but ships a 4-byte payload.
    t3.advance(b"\x1b_Gf=32,s=1000,v=1000,a=T;QUJDRA==\x1b\\");
    assert_eq!(
        t3.images().len(),
        0,
        "a dimension/length mismatch must reject the image, not over-allocate"
    );
}

/// (c) OSC 52 clipboard READ must stay default-off: an `OSC 52 ; c ; ?` query
/// must NOT return any clipboard data (the canonical host-clipboard-
/// exfiltration vector). The refusal is an EMPTY-payload OSC 52 reply rather
/// than silence, so the asking program is released instead of blocking on an
/// answer that never comes — the no-leak guarantee is unchanged, and asserted
/// on the exact bytes.
#[test]
fn osc52_clipboard_read_is_default_off_and_leaks_nothing() {
    let mut t = Terminal::new(10, 40);
    assert!(
        !t.clipboard_read_enabled(),
        "clipboard read must be OFF by default"
    );
    // A program requests the host clipboard contents.
    t.advance(b"\x1b]52;c;?\x07");
    assert_eq!(
        t.take_pty_response(),
        b"\x1b]52;c;\x07".to_vec(),
        "a denied OSC 52 read must reply with an EMPTY payload — no clipboard \
         bytes, but also not silence (silence hangs the caller)"
    );

    // A WRITE request is still honoured (that direction is safe / expected):
    // "echo" base64 of "hi" -> aGk=
    t.advance(b"\x1b]52;c;aGk=\x07");
    let writes = t.take_clipboard_writes();
    assert_eq!(writes.len(), 1, "OSC 52 WRITE must be captured");
    assert_eq!(writes[0].text, "hi");

    // Even after opting in, a read only ever replies with host-SUPPLIED text —
    // never the terminal reading the clipboard itself. With no host text
    // supplied, the bare query still produces nothing on the wire; it only
    // parks a request for the host to answer.
    t.set_clipboard_read_enabled(true);
    t.advance(b"\x1b]52;c;?\x07");
    assert!(
        t.take_pty_response().is_empty(),
        "an opted-in read still requires the HOST to supply text; the core never reads the clipboard"
    );
    assert_eq!(
        t.take_clipboard_reads().len(),
        1,
        "an opted-in read parks a request for the host instead of answering it"
    );
}

/// (d) `WorkspaceSnapshot::load` on corrupt / garbage / hostile files must
/// never panic and must return the documented single-tab fallback.
#[test]
fn workspace_load_on_corrupt_file_falls_back_never_panics() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();

    let hostile_contents: &[&[u8]] = &[
        b"this is { not ] valid json at all",
        b"",                                  // empty file
        b"\x00\x01\x02\x03\xff\xfe",          // binary garbage
        b"[]",                                // valid json, wrong shape
        b"{\"version\": 99999, \"tabs\": []}", // unknown future version
        br#"{"version":2,"tabs":[{"version":1,"root":{"kind":"split","axis":"horizontal","children":[]},"focused_ordinal":999999}],"active":42}"#, // empty split + OOB indices
    ];

    for (i, contents) in hostile_contents.iter().enumerate() {
        let path = dir.join(format!("c0pl4nd-fuzz-ws-{pid}-{i}.json"));
        std::fs::write(&path, contents).unwrap();

        // The safe loader must degrade to a single default tab — never panic.
        let ws = WorkspaceSnapshot::load(&path);
        assert_eq!(
            ws.tabs.len(),
            1,
            "corrupt file {i} must fall back to one tab"
        );
        assert_eq!(ws.active, 0, "fallback active index must be 0 for file {i}");
        let restored = ws
            .restore_all()
            .expect("fallback workspace always restores");
        assert_eq!(restored.tabs.len(), 1);
        assert_eq!(restored.tabs[0].layout.leaf_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    // Loading a path that does not exist also degrades gracefully.
    let missing = dir.join(format!("c0pl4nd-fuzz-absent-{pid}.json"));
    let _ = std::fs::remove_file(&missing);
    let ws = WorkspaceSnapshot::load(&missing);
    assert_eq!(ws.tabs.len(), 1);
}

/// (e) A payload that embeds an APC terminator (ST) or a bracketed-paste end
/// sentinel mid-stream cannot break out of its frame: the embedded terminator
/// ends THAT frame's accumulation, and subsequent bytes are handled as the
/// terminal's own input — they cannot, e.g., forge a device-attributes reply
/// or smuggle commands into a captured image.
#[test]
fn embedded_terminators_cannot_break_out_of_their_frame() {
    // --- APC with an embedded ST: the first ST ends the APC. The trailing
    // bytes after it are parsed as ordinary terminal input, NOT as more APC
    // payload — so a smuggled second image body is not silently appended. ---
    let mut t = Terminal::new(10, 40);
    // A complete 1x1 RGBA Kitty image, then an ST, then attacker bytes that
    // try to look like a second transmission without a fresh APC introducer.
    // "AAAA" base64 = 3 zero bytes; we use a real 1x1 payload "AAD/" style is
    // awkward, so use the known-good 4-byte red pixel: base64 of FF 00 00 FF.
    // FF0000FF -> "/wAA/w==".
    t.advance(b"\x1b_Gf=32,s=1,v=1,a=T;/wAA/w==\x1b\\");
    let after_first = t.images().len();
    assert_eq!(after_first, 1, "the well-formed APC image must decode once");
    // Now feed bytes that WOULD be a second image body, but with NO `\x1b_G`
    // introducer — just raw "f=32,...;payload\x1b\\". These must be treated as
    // printable text, never as a second graphics transmission.
    t.advance(b"f=32,s=1,v=1,a=T;/wAA/w==\x1b\\");
    assert_eq!(
        t.images().len(),
        after_first,
        "bytes without a fresh APC introducer must not be parsed as a second image"
    );

    // --- An OSC string carrying escape-like bytes in its PAYLOAD must capture
    // those bytes as inert title text and never re-emit them. This is the core
    // anti-exfiltration guarantee: attacker-controlled OSC payload content is
    // DATA, never echoed back to the PTY. The OSC ends at the BEL; the bytes
    // inside the title (including a forged-looking `[?62c`) stay captured. ---
    let mut t2 = Terminal::new(10, 40);
    // OSC 2 title whose payload literally contains the text "[?62c" — if the
    // terminal echoed title content this would look like a forged DA reply.
    t2.advance(b"\x1b]2;harmless [?62c title\x07");
    assert!(
        t2.take_pty_response().is_empty(),
        "OSC title payload content must never be echoed back to the PTY"
    );
    assert_eq!(
        t2.title(),
        "harmless [?62c title",
        "the escape-like bytes are captured as inert title text"
    );

    // --- OSC 133 prompt-mark injection: even a stream densely packed with
    // OSC 133 marks must never produce a single PTY reply (capture-only). ---
    let mut t3 = Terminal::new(10, 40);
    for _ in 0..1000 {
        t3.advance(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07");
    }
    assert!(
        t3.take_pty_response().is_empty(),
        "OSC 133 marks must never produce a PTY reply, even under flood"
    );
}

/// A focused regression: feeding 256 distinct single bytes (every possible
/// byte value) one at a time, repeatedly, must never panic and must leave the
/// grid in a coherent state. Catches off-by-one / control-byte mishandling.
#[test]
fn every_byte_value_is_survivable() {
    let mut t = Terminal::with_scrollback(8, 24, 100);
    for round in 0..4 {
        for b in 0u16..=255 {
            t.advance(&[b as u8]);
            // Interleave an ESC every so often to keep the parser state churning.
            if (b as usize + round).is_multiple_of(17) {
                t.advance(b"\x1b");
            }
        }
    }
    let _ = t.take_pty_response();
    assert_eq!(t.grid().rows(), 8);
    assert_eq!(t.grid().cols(), 24);
}
