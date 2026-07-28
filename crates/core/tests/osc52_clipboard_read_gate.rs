//! OSC 52 clipboard-READ gate: the `clipboard_read_allow` config key and the
//! end-to-end parser → gate → reply behaviour it drives.
//!
//! Clipboard WRITES from a program are always accepted (worst case: a clobbered
//! clipboard). A clipboard READ is an exfiltration primitive — anything that can
//! write to the tty (a compromised tool, a hostile file `cat`ted, output relayed
//! over ssh/tmux) could siphon whatever the user last copied, routinely a
//! password or an API token. So the read direction is DEFAULT-DENY, and these
//! tests pin that default from the config surface all the way to the bytes that
//! reach the PTY.

use std::path::PathBuf;

use c0pl4nd_core::config::Config;
use c0pl4nd_core::term::{ClipboardSelection, Terminal};

/// The shipping default must DENY. If this ever flips, every C0PL4ND user's
/// clipboard becomes readable by anything with a handle on the tty.
#[test]
fn clipboard_read_allow_defaults_to_deny() {
    assert!(
        !Config::default().clipboard_read_allow,
        "OSC 52 clipboard READ must be OFF by default — an on-by-default read is \
         a clipboard-exfiltration hole"
    );
}

/// A config file written before this key existed must load with reads DENIED,
/// not silently opted in. `#[serde(default)]` on a `bool` gives `false`; this
/// pins that, because a `default = "default_true"`-style slip would silently
/// open the hole for every existing user on upgrade.
#[test]
fn a_config_without_the_key_loads_denied() {
    let p = PathBuf::from("test.toml");
    let c = Config::from_toml("theme = \"itasha-corp\"\n", &p).expect("a pre-key config must load");
    assert!(
        !c.clipboard_read_allow,
        "an existing config that predates the key must stay denied on upgrade"
    );
}

/// The key is a real, round-tripping setting — opting in is possible and
/// survives a save/load cycle (otherwise the Settings checkbox would be dead).
#[test]
fn clipboard_read_allow_opts_in_and_round_trips() {
    let p = PathBuf::from("test.toml");
    let c = Config::from_toml("clipboard_read_allow = true\n", &p).expect("parses");
    assert!(c.clipboard_read_allow);

    let serialized = toml::to_string(&c).expect("serialize");
    let back = Config::from_toml(&serialized, &p).expect("reparse");
    assert!(
        back.clipboard_read_allow,
        "the opt-in must survive a config round-trip"
    );
}

/// End-to-end with the config default applied to a terminal: a program asking
/// for the clipboard gets a refusal carrying zero bytes, and the host is never
/// handed a request it could answer.
#[test]
fn config_default_applied_to_terminal_refuses_reads_without_leaking() {
    let cfg = Config::default();
    let mut t = Terminal::new(10, 40);
    t.set_clipboard_read_enabled(cfg.clipboard_read_allow);

    t.advance(b"\x1b]52;c;?\x07");

    assert!(
        t.take_clipboard_reads().is_empty(),
        "under the default config the host is never asked to read the clipboard"
    );
    assert_eq!(
        t.take_pty_response(),
        b"\x1b]52;c;\x07".to_vec(),
        "the refusal carries an empty payload"
    );
}

/// End-to-end with the opt-in applied: the request reaches the host, and the
/// host's text is what comes back — the terminal never sources it itself.
#[test]
fn config_opt_in_applied_to_terminal_routes_the_read_through_the_host() {
    let p = PathBuf::from("test.toml");
    let cfg = Config::from_toml("clipboard_read_allow = true\n", &p).expect("parses");
    let mut t = Terminal::new(10, 40);
    t.set_clipboard_read_enabled(cfg.clipboard_read_allow);

    t.advance(b"\x1b]52;c;?\x07");
    let reqs = t.take_clipboard_reads();
    assert_eq!(reqs.len(), 1, "the opt-in surfaces the request to the host");
    assert_eq!(reqs[0].selection, ClipboardSelection::Clipboard);
    assert!(
        t.take_pty_response().is_empty(),
        "nothing goes out until the host supplies text"
    );

    // The host answers with what IT read.
    t.respond_clipboard_read(reqs[0].selection, "from-the-host");
    let wire = String::from_utf8(t.take_pty_response()).expect("ASCII reply");
    assert_eq!(wire, "\x1b]52;c;ZnJvbS10aGUtaG9zdA==\x07");
}

/// A WRITE must keep working regardless of the read gate — the two directions
/// are independently governed, and denying reads must not break copy-from-program.
#[test]
fn the_read_gate_does_not_affect_clipboard_writes() {
    let mut t = Terminal::new(10, 40);
    assert!(!t.clipboard_read_enabled());
    t.advance(b"\x1b]52;c;aGk=\x07"); // base64("hi")
    let writes = t.take_clipboard_writes();
    assert_eq!(
        writes.len(),
        1,
        "writes stay honoured while reads are denied"
    );
    assert_eq!(writes[0].text, "hi");
}
