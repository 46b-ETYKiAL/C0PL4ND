//! Tests for the OSC 9 / OSC 777 desktop-notification path.
//!
//! # What is and is not observable here
//!
//! A toast cannot be observed by a headless test on any host: there is no API
//! to read back what the shell rendered, and `ToastNotifier::Show` returns
//! success for a notification that is never displayed (that is precisely the
//! AUMID failure mode this feature exists to fix). So the strategy is:
//!
//! 1. Every DECISION — suppression, last-wins, title fallback, control
//!    stripping, truncation, XML escaping, document shape — is a pure function
//!    and is tested exhaustively here.
//! 2. The AUMID agreement between the Rust constant and the installer shortcut
//!    is asserted STRUCTURALLY, by parsing `packaging/windows/c0pl4nd.wxs`.
//! 3. The pump → attention-flash wire is driven through the REAL
//!    `pump_pane_effects` and observed through egui's viewport commands.
//!
//! What remains UNOBSERVABLE, stated plainly rather than papered over with a
//! test that would pass regardless:
//!
//! - Whether the shell actually DREW a toast. `imp::show` runs in these tests
//!   on Windows and returns `false` (no Start-Menu shortcut is installed for a
//!   `cargo test` run), which is the honest result — it is not asserted as a
//!   success anywhere.
//! - Whether `SetCurrentProcessExplicitAppUserModelID` took effect. There is a
//!   `GetCurrentProcessExplicitAppUserModelID` read-back, but it returns the
//!   value we just wrote and so would pass with the shortcut property missing,
//!   the AUMID misspelled, or the toast code deleted — a test on it is a test
//!   of the Win32 setter, not of this feature. It is deliberately absent.
//! - The pump → TOAST wire. `pump_pane_effects` still collapses notifications
//!   to a `bool` in `pane_term::HostEffects` (a file owned by another author),
//!   so no notification TEXT reaches this module at runtime yet. `show` is
//!   proven to reach its backend seam below; the two-line call site that
//!   connects them is reported, not faked.

use super::*;

// ---------------------------------------------------------------------------
// plan(): the suppression / last-wins truth table
// ---------------------------------------------------------------------------

fn n(title: &str, body: &str) -> Notification {
    Notification {
        title: title.to_string(),
        body: body.to_string(),
    }
}

#[test]
fn plan_is_empty_when_nothing_was_drained() {
    for focused in [Some(true), Some(false), None] {
        assert_eq!(
            plan(&[], focused),
            NotifyPlan::default(),
            "no notification must produce neither a toast nor a flash (focused={focused:?})"
        );
    }
}

#[test]
fn plan_suppresses_everything_while_focused() {
    let drained = [n("", "build done")];
    assert_eq!(
        plan(&drained, Some(true)),
        NotifyPlan::default(),
        "the user is looking at the terminal that printed it — no toast, no flash"
    );
}

#[test]
fn plan_treats_unknown_focus_as_focused() {
    // `focused == None` is the pre-first-focus-event startup state. Matching
    // `taskbar::should_request_attention` here is what stops a notification
    // emitted by a shell rc-file during launch from firing a toast.
    assert_eq!(plan(&[n("", "hi")], None), NotifyPlan::default());
}

#[test]
fn plan_toasts_and_flashes_while_unfocused() {
    let got = plan(&[n("Heads up", "the thing happened")], Some(false));
    assert_eq!(
        got,
        NotifyPlan {
            toast: Some(ToastText {
                title: "Heads up".into(),
                body: "the thing happened".into(),
            }),
            flash: true,
        },
        "the flash is KEPT alongside the toast — it is the only signal on a host \
         where no toast can be shown"
    );
}

#[test]
fn plan_collapses_a_frames_burst_to_the_last_notification() {
    // A loop emitting three OSC 9s inside one 16 ms frame must produce ONE
    // toast, not a stack of three — the same last-wins rule the taskbar
    // progress indicator uses.
    let drained = [n("", "first"), n("", "second"), n("", "third")];
    let got = plan(&drained, Some(false));
    assert_eq!(got.toast.expect("unfocused burst must toast").body, "third");
}

// ---------------------------------------------------------------------------
// toast_text(): title fallback + field projection
// ---------------------------------------------------------------------------

#[test]
fn osc9_has_no_title_so_the_app_name_is_used() {
    // OSC 9 carries only a body; a toast with an empty heading renders a blank
    // line, which is what the fallback exists to prevent.
    assert_eq!(
        toast_text(&n("", "build done")),
        ToastText {
            title: APP_NAME.into(),
            body: "build done".into(),
        }
    );
}

#[test]
fn a_whitespace_only_title_also_falls_back() {
    // Sanitisation turns "\t \n" into "" — the fallback must run AFTER
    // sanitising, not before, or this renders a blank heading.
    assert_eq!(toast_text(&n("\t \n", "body")).title, APP_NAME);
}

#[test]
fn osc777_keeps_both_fields() {
    assert_eq!(
        toast_text(&n("Heads up", "the thing happened")),
        ToastText {
            title: "Heads up".into(),
            body: "the thing happened".into(),
        }
    );
}

#[test]
fn an_empty_body_is_left_empty_not_filled_in() {
    // A title-only notification is legal; inventing body text would put words
    // in the program's mouth.
    assert_eq!(toast_text(&n("Title", "")).body, "");
}

// ---------------------------------------------------------------------------
// sanitize_line(): the XML-1.0 legality guard
// ---------------------------------------------------------------------------

#[test]
fn control_characters_become_spaces_because_xml_forbids_them_even_escaped() {
    // XML 1.0 rejects C0 controls in content even as `&#x1;`, so ONE stray byte
    // from a build log makes `LoadXml` fail and the toast vanish silently.
    assert_eq!(sanitize_line("a\u{1}b"), "a b");
    assert_eq!(sanitize_line("line1\nline2"), "line1 line2");
    assert_eq!(sanitize_line("col1\tcol2"), "col1 col2");
    assert_eq!(sanitize_line("cr\rlf"), "cr lf");
    // DEL and the C1 range (which includes 0x9b, the 8-bit CSI a terminal can
    // legitimately emit) are stripped too.
    assert_eq!(sanitize_line("a\u{7f}b"), "a b");
    assert_eq!(sanitize_line("a\u{9b}b"), "a b");
    // NUL, the byte most likely to arrive from a mis-encoded payload.
    assert_eq!(sanitize_line("a\0b"), "a b");
}

#[test]
fn sanitize_trims_the_result_so_a_leading_newline_does_not_indent_the_toast() {
    assert_eq!(sanitize_line("\n  build done  \n"), "build done");
}

#[test]
fn sanitize_preserves_non_ascii_text() {
    // Notification payloads are routinely non-ASCII; stripping "anything
    // non-ASCII" instead of "anything control" would mangle them.
    assert_eq!(sanitize_line("ビルド完了 ✓ é"), "ビルド完了 ✓ é");
}

// ---------------------------------------------------------------------------
// truncate_chars(): char-boundary safety + the cut marker
// ---------------------------------------------------------------------------

#[test]
fn truncate_leaves_short_and_exactly_max_strings_untouched() {
    assert_eq!(truncate_chars("abc", 5), "abc");
    assert_eq!(
        truncate_chars("abcde", 5),
        "abcde",
        "exactly at the cap is not a truncation — no ellipsis"
    );
}

#[test]
fn truncate_marks_a_real_cut() {
    assert_eq!(truncate_chars("abcdef", 5), "abcde\u{2026}");
}

#[test]
fn truncate_counts_characters_not_bytes() {
    // Each of these is 3 bytes in UTF-8. A byte-based `&s[..max]` would panic
    // on a char boundary here; a byte-based count would cut after 1 character.
    let cjk = "完了完了完了";
    assert_eq!(truncate_chars(cjk, 3), "完了完\u{2026}");
    // A 4-byte astral character (emoji) as the first char past the cap.
    assert_eq!(truncate_chars("ab🎉cd", 2), "ab\u{2026}");
    // …and as the LAST kept char, so the cut lands on a 4-byte boundary.
    assert_eq!(truncate_chars("a🎉bc", 2), "a🎉\u{2026}");
}

#[test]
fn toast_text_applies_the_documented_caps() {
    let long_title = "T".repeat(MAX_TITLE_CHARS + 50);
    let long_body = "B".repeat(MAX_BODY_CHARS + 50);
    let got = toast_text(&n(&long_title, &long_body));
    assert_eq!(
        got.title.chars().count(),
        MAX_TITLE_CHARS + 1,
        "cap plus the one-char ellipsis"
    );
    assert_eq!(got.body.chars().count(), MAX_BODY_CHARS + 1);
    assert!(got.title.ends_with('\u{2026}'));
    assert!(got.body.ends_with('\u{2026}'));
}

// ---------------------------------------------------------------------------
// escape_xml_text() + build_toast_xml(): the injection boundary
// ---------------------------------------------------------------------------

#[test]
fn escaping_replaces_ampersand_first() {
    // `&` LAST would re-escape the ampersands the other replacements introduce
    // and emit `&amp;lt;`. The input carries a bare `&` next to a `<` so a
    // wrong order is visible rather than merely asserted.
    assert_eq!(escape_xml_text("a & <b> c"), "a &amp; &lt;b&gt; c");
    assert_eq!(
        escape_xml_text("&lt;"),
        "&amp;lt;",
        "text that already LOOKS escaped must be escaped again, not passed through"
    );
}

#[test]
fn escaping_leaves_ordinary_text_alone() {
    assert_eq!(escape_xml_text("build done"), "build done");
}

#[test]
fn a_notification_body_cannot_inject_toast_markup() {
    // The payload is PTY bytes, i.e. attacker-influenced. Unescaped, this body
    // would close the <text> element and append real toast elements — silencing
    // the notification sound and adding a button.
    let hostile = "</text><audio silent=\"true\"/><text>owned";
    let xml = build_toast_xml(&toast_text(&n("", hostile)));
    assert!(
        !xml.contains("<audio"),
        "the body must not be able to introduce a new element: {xml}"
    );
    assert_eq!(
        xml.matches("<text>").count(),
        2,
        "exactly the two template <text> elements — no injected third: {xml}"
    );
    assert!(
        xml.contains("&lt;/text&gt;&lt;audio"),
        "the payload must survive as ESCAPED, visible text: {xml}"
    );
}

#[test]
fn the_built_document_is_the_toast_generic_template() {
    let xml = build_toast_xml(&ToastText {
        title: "T".into(),
        body: "B".into(),
    });
    assert_eq!(
        xml,
        "<toast duration=\"short\"><visual><binding template=\"ToastGeneric\">\
<text>T</text><text>B</text></binding></visual></toast>",
        "the exact document handed to XmlDocument::LoadXml"
    );
}

#[test]
fn show_reaches_the_backend_seam_with_the_built_document() {
    // Proves `show` routes the BUILT xml to the backend rather than, say,
    // building it and dropping it. The shell's verdict is deliberately NOT
    // asserted: on a `cargo test` host no Start-Menu shortcut carries the
    // AUMID, so Windows legitimately refuses, and any assertion of `Shown`
    // would be a test of the developer's machine.
    let _guard = test_spy::serial();
    test_spy::reset();
    let text = ToastText {
        title: "Heads up".into(),
        body: "done".into(),
    };
    let outcome = show(&text);
    assert_eq!(
        test_spy::take().as_deref(),
        Some(build_toast_xml(&text).as_str())
    );
    #[cfg(not(windows))]
    assert_eq!(
        outcome,
        ToastOutcome::Unsupported,
        "a non-Windows build must report Unsupported, never a silent success"
    );
    #[cfg(windows)]
    assert!(
        matches!(outcome, ToastOutcome::Shown | ToastOutcome::Failed),
        "a Windows build must reach the shell and report its real verdict"
    );
}

// ---------------------------------------------------------------------------
// AUMID <-> installer correspondence (structural)
// ---------------------------------------------------------------------------
//
// `AUMID` and the shortcut's `System.AppUserModel.ID` are one fact stored in
// two files. If they disagree, `ToastNotifier::Show` still returns success and
// nothing is ever displayed — a failure with no runtime symptom at all. These
// tests are the only thing that can catch it.

const WXS_RAW: &str = include_str!("../../../packaging/windows/c0pl4nd.wxs");

/// The `System.AppUserModel.ID` property key, spelled once.
const AUMID_PROPERTY_KEY: &str = "System.AppUserModel.ID";

/// The installer with every XML comment removed.
///
/// Everything below parses THIS, not the raw file, for two independent reasons.
/// A commented-out `<ShortcutProperty>` would otherwise parse as a live one —
/// the parity check would pass while the MSI shipped no AUMID at all, which is
/// precisely the silent failure these tests exist to catch. And the raw-text
/// sweep in `installer_declares_no_other_app_user_model_id` would count the
/// prose in the comment that EXPLAINS the property, so documenting the feature
/// would break its own test.
fn wxs() -> String {
    strip_xml_comments(WXS_RAW)
}

/// Remove `<!-- … -->` spans. XML forbids `--` inside a comment, so an
/// unterminated `<!--` is malformed input rather than something to tolerate:
/// it is treated as running to end-of-file, which makes the parse visibly empty
/// (and `the_installer_parser_is_not_vacuous` fail) instead of silently partial.
fn strip_xml_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + "-->".len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn unescape_xml_attr(s: &str) -> String {
    // `&amp;` LAST: doing it first would turn `&amp;quot;` into a quote.
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Read one attribute out of an element's attribute text.
///
/// The match is anchored on a preceding WHITESPACE character rather than a
/// literal space, because the `.wxs` indents attributes across CRLF line
/// breaks — a `" {name}=\""` needle would silently fail to find `Id` in
/// `<Shortcut\r\n  Id="…">` and every assertion built on it would go vacuous.
/// A hand parser (as in SCR1B3's `packaging_consistency_tests`) rather than an
/// XML dev-dependency: this reads three attributes out of one generated file.
fn attr(el: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let mut from = 0usize;
    while let Some(rel) = el[from..].find(&needle) {
        let at = from + rel;
        let preceded_by_ws = el[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace());
        if preceded_by_ws {
            let start = at + needle.len();
            let end = start + el[start..].find('"')?;
            return Some(unescape_xml_attr(&el[start..end]));
        }
        from = at + needle.len();
    }
    None
}

/// Every `<Shortcut …>` element as `(attribute text, inner content)`.
///
/// `<ShortcutProperty` shares the `<Shortcut` prefix and is skipped explicitly.
fn shortcut_elements(wxs: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = wxs;
    while let Some(i) = rest.find("<Shortcut") {
        let after = &rest[i + "<Shortcut".len()..];
        if after.starts_with("Property") {
            rest = after;
            continue;
        }
        let gt = after.find('>').expect("an unterminated <Shortcut element");
        let attrs = &after[..gt];
        let tail = &after[gt + 1..];
        let inner = if attrs.trim_end().ends_with('/') {
            String::new()
        } else {
            let end = tail.find("</Shortcut>").expect("an unclosed <Shortcut>");
            tail[..end].to_string()
        };
        out.push((attrs.to_string(), inner));
        rest = tail;
    }
    out
}

/// Every `<ShortcutProperty …/>` in the file as `(key, value)`.
fn shortcut_properties(scope: &str) -> Vec<(String, String)> {
    scope
        .split("<ShortcutProperty")
        .skip(1)
        .map(|chunk| {
            let el = &chunk[..chunk
                .find("/>")
                .expect("an unterminated ShortcutProperty element")];
            (
                attr(el, "Key").expect("ShortcutProperty without a Key"),
                attr(el, "Value").expect("ShortcutProperty without a Value"),
            )
        })
        .collect()
}

/// The attribute reader must find attributes across the file's CRLF-indented
/// layout and must not read one attribute's name out of another's.
///
/// The whole correspondence check rests on this helper: a silently-misreading
/// parser turns both directions below into comparing nothing against nothing,
/// and they would still pass.
#[test]
fn the_attribute_reader_is_not_silently_misreading() {
    let el = "\r\n            Id=\"S\"\r\n            Name=\"C0PL4ND\"\r\n            \
              Target=\"[INSTALLFOLDER]c0pl4nd.exe\" Value=\"a &amp;quot; &amp; &quot;q&quot;\"";
    assert_eq!(
        attr(el, "Id").as_deref(),
        Some("S"),
        "an attribute after a CRLF + indent must still be found"
    );
    assert_eq!(attr(el, "Name").as_deref(), Some("C0PL4ND"));
    assert_eq!(
        attr(el, "Value").as_deref(),
        Some("a &quot; & \"q\""),
        "`&amp;` must be unescaped LAST or `&amp;quot;` collapses to a quote"
    );
    assert_eq!(
        attr(el, "arget"),
        None,
        "a needle must not match the tail of a longer attribute name"
    );
    assert_eq!(attr(el, "Absent"), None);
}

/// Comment stripping is load-bearing, so it gets its own test.
///
/// The case that matters is a COMMENTED-OUT `<ShortcutProperty>`: if it were
/// still parsed, the correspondence checks would report a healthy AUMID while
/// the built MSI carried none — a green test over an installer that ships the
/// exact bug this feature fixes.
#[test]
fn commented_out_markup_is_not_parsed_as_live() {
    let src = "<Shortcut Target=\"[INSTALLFOLDER]c0pl4nd.exe\">
               <!-- <ShortcutProperty Key=\"System.AppUserModel.ID\" Value=\"Ghost\" /> -->
               <ShortcutProperty Key=\"System.AppUserModel.ID\" Value=\"Real\" />
               </Shortcut>";
    let stripped = strip_xml_comments(src);
    assert!(
        !stripped.contains("Ghost"),
        "a commented-out property must not survive stripping: {stripped}"
    );
    let props = shortcut_properties(&stripped);
    assert_eq!(
        props,
        vec![(AUMID_PROPERTY_KEY.to_string(), "Real".to_string())],
        "exactly the LIVE property, and it is still found after stripping"
    );
}

/// Stripping must not eat live markup that merely surrounds a comment, and an
/// unterminated comment must degrade to a VISIBLY empty tail rather than a
/// silently partial parse.
#[test]
fn comment_stripping_keeps_surrounding_markup() {
    assert_eq!(strip_xml_comments("a<!--x-->b<!--y-->c"), "abc");
    assert_eq!(strip_xml_comments("no comments here"), "no comments here");
    assert_eq!(
        strip_xml_comments("kept<!--never closed"),
        "kept",
        "an unterminated comment truncates, which trips the non-vacuity check"
    );
}

/// The parser must actually be reading the installer. A silently-empty parse
/// would make every assertion below vacuously true.
#[test]
fn the_installer_parser_is_not_vacuous() {
    assert!(
        wxs().len() > 2_000,
        "the .wxs did not load or parsed away to nothing (len {})",
        wxs().len()
    );
    let shortcuts = shortcut_elements(&wxs());
    assert!(
        !shortcuts.is_empty(),
        "no <Shortcut> element was parsed out of the installer"
    );
    assert!(
        shortcuts
            .iter()
            .any(|(a, _)| attr(a, "Target").as_deref() == Some("[INSTALLFOLDER]c0pl4nd.exe")),
        "the Start-Menu shortcut targeting c0pl4nd.exe was not found"
    );
}

/// FORWARD: the shortcut that launches c0pl4nd.exe must carry exactly the
/// AUMID this module registers.
#[test]
fn aumid_matches_the_installer_shortcut_property() {
    let (_, inner) = shortcut_elements(&wxs())
        .into_iter()
        .find(|(a, _)| attr(a, "Target").as_deref() == Some("[INSTALLFOLDER]c0pl4nd.exe"))
        .expect("a Start-Menu shortcut targeting c0pl4nd.exe");
    let props = shortcut_properties(&inner);
    let got = props
        .iter()
        .find(|(k, _)| k == AUMID_PROPERTY_KEY)
        .map(|(_, v)| v.as_str());
    assert_eq!(
        got,
        Some(AUMID),
        "the Start-Menu shortcut must carry System.AppUserModel.ID=\"{AUMID}\". \
         Without it (or with a different value) every toast this app raises is \
         accepted by the shell and silently never displayed."
    );
}

/// BACKWARD: nothing else in the installer may declare a DIFFERENT AUMID.
///
/// Forwards alone would pass while a second shortcut (a future Desktop or
/// quick-launch entry) declared a conflicting ID — the shell would then resolve
/// whichever it indexed last and the toast would intermittently disappear.
#[test]
fn installer_declares_no_other_app_user_model_id() {
    let all = shortcut_properties(&wxs());
    let ids: Vec<&str> = all
        .iter()
        .filter(|(k, _)| k == AUMID_PROPERTY_KEY)
        .map(|(_, v)| v.as_str())
        .collect();
    assert!(
        !ids.is_empty(),
        "no System.AppUserModel.ID anywhere in the installer"
    );
    for id in &ids {
        assert_eq!(
            *id, AUMID,
            "installer declares an AUMID this app never registers under: {id:?}"
        );
    }
    // The raw-text sweep catches an ID smuggled in as a RegistryValue or a
    // Property rather than a ShortcutProperty, which the element parser above
    // would not see at all.
    assert_eq!(
        wxs().matches(AUMID_PROPERTY_KEY).count(),
        ids.len(),
        "System.AppUserModel.ID appears somewhere the ShortcutProperty parser \
         cannot see it — it would not be covered by the checks above"
    );
}

/// A `<Component>` no `<Feature>` references compiles into the MSI and is never
/// installed: the shortcut (and its AUMID) would exist in the package and do
/// nothing, which is the same silent no-op with a green parity test.
#[test]
fn the_shortcut_component_is_installed_by_a_feature() {
    assert!(
        wxs().contains("<Component Id=\"ApplicationShortcut\""),
        "the shortcut component was renamed — the ComponentRef check below is now vacuous"
    );
    assert!(
        wxs().contains("<ComponentRef Id=\"ApplicationShortcut\" />"),
        "the shortcut component is not referenced by any Feature, so the MSI \
         would never install it"
    );
}

/// The AUMID string itself must be a legal one. The shell silently refuses an
/// over-long or backslash-bearing ID (`\\` is the reserved application/sub-id
/// separator) — again with no runtime symptom.
#[test]
fn the_aumid_is_a_legal_application_user_model_id() {
    assert!(!AUMID.is_empty());
    assert!(AUMID.chars().count() <= 128, "AUMID exceeds 128 characters");
    assert!(!AUMID.contains('\\'), "`\\` is reserved inside an AUMID");
    assert!(
        !AUMID.contains(' '),
        "an AUMID must not contain whitespace: {AUMID:?}"
    );
    assert!(
        AUMID.contains('.'),
        "the documented form is CompanyName.ProductName"
    );
}
