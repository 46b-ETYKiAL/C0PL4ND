//! Real Windows **desktop notifications** (WinRT toasts) for OSC 9 / OSC 777 —
//! the escalation of the taskbar-attention FLASH that `egui_app::taskbar`
//! currently issues.
//!
//! # What was wrong
//!
//! `OSC 9` / `OSC 777` are the terminal protocol for "tell the user something
//! happened" (`ESC ] 9 ; long build finished ST`). Every peer emulator —
//! Windows Terminal, WezTerm, Ghostty, kitty — turns that into a real OS
//! notification. C0PL4ND turned it into `ViewportCommand::RequestUserAttention`,
//! which winit implements on Windows as `FlashWindowEx(FLASHW_TRAY |
//! FLASHW_TIMERNOFG)` (winit-0.30.13 `platform_impl/windows/window.rs:935`) —
//! a taskbar-button flash. A flash is invisible the moment the user is not
//! looking at the taskbar, carries no text, and leaves nothing in the
//! Notification Centre.
//!
//! This module builds the toast. The flash is deliberately **kept** (see
//! [`NotifyPlan`]): it is the cross-platform fallback for every host where a
//! toast cannot be shown, and removing it would weaken shipped behaviour.
//!
//! # The AUMID is load-bearing, not decoration
//!
//! A Win32 (non-packaged) process may only raise a toast under an **Application
//! User Model ID** that resolves to an installed Start-Menu shortcut carrying
//! that same ID in its `System.AppUserModel.ID` property. With no such
//! shortcut, `ToastNotifier::Show` returns success and **nothing appears** —
//! the failure is completely silent. So [`AUMID`] here and the
//! `<ShortcutProperty>` in `packaging/windows/c0pl4nd.wxs` are ONE fact stored
//! twice, and `notify_tests::aumid_*` assert the two copies agree rather than
//! trusting two string literals to stay in sync by hand.
//!
//! # Privacy: display is not logging
//!
//! `pane_term::HostEffects` deliberately refuses to surface notification TEXT
//! because it can carry a 2FA code or a secret URL and must never reach a log.
//! That constraint is about **logging**, not about **showing**: showing the text
//! to the user at the moment they asked for it is the entire point of OSC 9.
//! Nothing in this module traces, logs, or persists the payload — it is built
//! into an XML string, handed to the shell, and dropped.
//!
//! # Module placement
//!
//! A lib-root module rather than `egui_app::notify`. Two reasons, both
//! structural: `egui_app/mod.rs` is concurrently owned by another author, so a
//! `mod notify;` line there would conflict; and `egui_app`'s submodules are
//! PRIVATE, which would make every function here unreachable from outside the
//! crate and therefore `dead_code` under CI's `cargo clippy --all-targets
//! -D warnings` until its call site lands. Suppressing that with an
//! `#[allow(dead_code)]` would be exactly the dormant-code laundering this
//! repo has been bitten by; being a real public API of the app lib is honest
//! about what is built and lets `tests/` reach it too.
//!
//! # Wiring status — read this before assuming the toast fires
//!
//! The runtime seam is NOT connected. `egui_app::pane_term::HostEffects`
//! collapses OSC 9/777 to a `notified: bool` and discards the text, and
//! `egui_app::mod::pump_pane_effects` turns that bool into the flash. Both
//! files are owned by another author. Connecting this module needs exactly two
//! changes there, listed in the crate-level report and reproduced here so the
//! next reader of this file does not have to go looking:
//!
//! 1. `pane_term.rs` — add `pub notifications: Vec<Notification>` to
//!    `HostEffects` and fill it from `term.take_notifications()` (which is
//!    already called; its result is currently only length-tested).
//! 2. `mod.rs::pump_pane_effects` — collect those into a frame-level `Vec`,
//!    then replace the `should_request_attention` block with:
//!
//!    ```ignore
//!    let plan = crate::notify::plan(&notifications, focused);
//!    if let Some(text) = &plan.toast {
//!        crate::notify::show(text);
//!    }
//!    if plan.flash {
//!        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
//!            egui::UserAttentionType::Informational,
//!        ));
//!    }
//!    ```
//!
//! `plan` reproduces `taskbar::should_request_attention`'s focus semantics
//! exactly (asserted by `notify_tests::plan_*`), so the flash behaviour is
//! unchanged by that swap.

use c0pl4nd_core::term::Notification;

/// The Application User Model ID this process registers and raises toasts
/// under. MUST equal the `System.AppUserModel.ID` `<ShortcutProperty>` on the
/// Start-Menu shortcut in `packaging/windows/c0pl4nd.wxs` — a mismatch makes
/// every toast silently no-op. Pinned in both directions by
/// `notify_tests::aumid_matches_the_installer_shortcut_property` and
/// `notify_tests::installer_declares_no_other_app_user_model_id`.
///
/// Format is the documented `CompanyName.ProductName` form: no spaces, no
/// backslash (which is reserved as the AUMID's application/sub-id separator),
/// and at most 128 characters.
pub const AUMID: &str = "Itasha.Corp.C0PL4ND";

/// Title shown when the notification carries none. `OSC 9` has no title field
/// at all (only `OSC 777;notify;title;body` does), so the overwhelmingly common
/// case lands here — a toast with an empty title renders as a blank line.
pub const APP_NAME: &str = "C0PL4ND";

/// Hard cap on the rendered title, in CHARACTERS (not bytes). The Windows
/// `ToastGeneric` template shows one title line and elides the rest, but an
/// unbounded string still travels through XML build + IPC, and a hostile
/// program can emit megabytes on one OSC. Truncation is ours so the cost is
/// bounded before the shell ever sees it.
pub const MAX_TITLE_CHARS: usize = 96;

/// Hard cap on the rendered body, in CHARACTERS. `ToastGeneric` renders up to
/// two wrapped body lines; anything beyond is invisible anyway.
pub const MAX_BODY_CHARS: usize = 512;

/// Appended when [`truncate_chars`] actually cut something, so a clipped
/// notification reads as clipped instead of as a complete-but-wrong message.
const ELLIPSIS: char = '\u{2026}';

/// The two text fields a toast renders, already sanitised and truncated but NOT
/// yet XML-escaped (escaping happens in [`build_toast_xml`], so a `ToastText`
/// stays comparable as plain text in tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastText {
    /// Heading line. Never empty — falls back to [`APP_NAME`].
    pub title: String,
    /// Body line(s). May be empty (a title-only notification is legal).
    pub body: String,
}

/// What the shell should do with one frame's drained OSC 9 / OSC 777 reports.
///
/// Both effects are described together so the flash-vs-toast relationship is a
/// single testable decision rather than two call sites that can drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NotifyPlan {
    /// The toast to raise, or `None` to raise none.
    pub toast: Option<ToastText>,
    /// Whether to also request taskbar attention (the pre-existing
    /// `FlashWindowEx` behaviour). Kept alongside the toast on purpose: it is
    /// the only signal on hosts where no toast can be shown, and it is what
    /// leaves the taskbar button highlighted after the toast auto-dismisses.
    pub flash: bool,
}

/// The outcome of an attempted toast. Deliberately three-valued: a headless or
/// non-Windows host must be distinguishable from a Windows host where the
/// shell REFUSED the toast, so a caller (or a health counter) never reads
/// "nothing happened" as "it worked".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastOutcome {
    /// The shell accepted the toast.
    Shown,
    /// The shell rejected it (bad XML, no notifier for this AUMID, notifications
    /// disabled by policy, COM/WinRT unavailable).
    Failed,
    /// This build has no toast backend (any non-Windows target). NOT a failure
    /// — the taskbar flash in [`NotifyPlan::flash`] is the behaviour there.
    Unsupported,
}

/// Decide what a frame's drained notifications should produce.
///
/// - Nothing drained → no toast, no flash.
/// - Focused → **suppressed entirely**. The user is looking at the terminal
///   that just printed the message; a toast for the window you are staring at
///   is the notification-spam every peer emulator suppresses too. `focused ==
///   None` (before the first focus event, i.e. startup) is treated as focused,
///   matching [`super::should_request_attention`] so a notification during
///   launch cannot spuriously fire.
/// - Unfocused → toast the LAST report of the frame, plus the flash. Last-wins
///   mirrors `super::latest_progress`: a program that emits three notifications
///   in one 16 ms frame gets one toast, not a stack of three.
///
/// Pure, so every arm is unit-testable without a live window.
pub fn plan(drained: &[Notification], focused: Option<bool>) -> NotifyPlan {
    let Some(last) = drained.last() else {
        return NotifyPlan::default();
    };
    if focused.unwrap_or(true) {
        return NotifyPlan::default();
    }
    NotifyPlan {
        toast: Some(toast_text(last)),
        flash: true,
    }
}

/// Project one [`Notification`] onto the two rendered lines.
///
/// `OSC 9` carries a body and an empty title; `OSC 777` carries both. An empty
/// (or whitespace-only) title falls back to [`APP_NAME`] so the toast never
/// renders a blank heading. Both fields are control-stripped then truncated.
pub fn toast_text(n: &Notification) -> ToastText {
    let title = sanitize_line(&n.title);
    let title = if title.is_empty() {
        APP_NAME.to_string()
    } else {
        truncate_chars(&title, MAX_TITLE_CHARS)
    };
    ToastText {
        title,
        body: truncate_chars(&sanitize_line(&n.body), MAX_BODY_CHARS),
    }
}

/// Replace every control character with a space and trim the result.
///
/// Load-bearing, not cosmetic: XML 1.0 forbids C0 control characters in
/// document content **even when escaped** (`&#x1;` is as illegal as a raw
/// `0x01`), so a single stray control byte — trivially emitted by a program
/// writing an OSC payload straight out of a build log — makes `XmlDocument::
/// LoadXml` reject the whole document and the toast vanish silently. C1
/// (`0x7f..=0x9f`) is stripped for the same reason and because it renders as
/// garbage. Newlines and tabs become spaces rather than surviving: the toast
/// template renders one title line and wraps the body itself.
pub fn sanitize_line(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Truncate to at most `max` CHARACTERS, appending [`ELLIPSIS`] when anything
/// was cut. Char-based (never byte-based) so a multi-byte grapheme is never
/// split into invalid UTF-8 — an OSC payload is arbitrary user text and is
/// routinely non-ASCII.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push(ELLIPSIS);
    out
}

/// Escape text for XML **element content**.
///
/// The toast payload is an XML document built from PTY bytes, i.e. from
/// attacker-influenced text: a body containing `</text><audio silent="true"/>`
/// would otherwise rewrite the document. `&` is replaced FIRST — replacing it
/// last would re-escape the ampersands introduced by the other replacements and
/// emit `&amp;lt;`. (This is the mirror image of the UNESCAPE order used when
/// reading the `.wxs` in the tests, where `&amp;` must go last.)
pub fn escape_xml_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Build the `ToastGeneric` XML document for a notification.
///
/// `duration="short"` matches the peers (an unattended terminal message is not
/// an alarm) and `<audio silent="false"/>` is left implicit so the user's own
/// notification-sound preference wins. Pure — the returned string is exactly
/// what is handed to `XmlDocument::LoadXml`, so the escaping is testable
/// without a live shell.
pub fn build_toast_xml(text: &ToastText) -> String {
    format!(
        "<toast duration=\"short\"><visual><binding template=\"ToastGeneric\">\
<text>{}</text><text>{}</text>\
</binding></visual></toast>",
        escape_xml_text(&text.title),
        escape_xml_text(&text.body),
    )
}

/// Raise a desktop notification. Windows-only in effect; a clean
/// [`ToastOutcome::Unsupported`] elsewhere.
///
/// Never panics and never propagates a WinRT error: a terminal must not die
/// because the notification platform is unavailable (RDP session, notifications
/// disabled by group policy, Explorer restarting). Under `cfg(test)` the built
/// XML is recorded in [`test_spy`] so the pump → toast wire can be asserted
/// without a shell.
pub fn show(text: &ToastText) -> ToastOutcome {
    let xml = build_toast_xml(text);
    #[cfg(test)]
    test_spy::record(&xml);
    #[cfg(windows)]
    {
        // In a headless unit test this still runs and simply fails (no
        // notifier resolves for an uninstalled AUMID), which keeps `imp` live
        // code rather than something only the installed build compiles.
        if imp::show(&xml) {
            ToastOutcome::Shown
        } else {
            ToastOutcome::Failed
        }
    }
    #[cfg(not(windows))]
    {
        let _ = xml;
        ToastOutcome::Unsupported
    }
}

/// Test-only spy recording the last XML handed to the toast backend, so a
/// wiring test can prove the pump reached this seam. A test that called
/// [`build_toast_xml`] directly would pass forever even if nothing ever called
/// [`show`].
#[cfg(test)]
pub mod test_spy {
    use std::sync::Mutex;

    static LAST: Mutex<Option<String>> = Mutex::new(None);

    /// Serialises the tests sharing [`LAST`] (a process-global `static` against
    /// `cargo test`'s parallel threads), mirroring `super::super::test_spy`.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Acquire the serial guard. Poison-tolerant so one panicking spy test does
    /// not cascade into a wall of `PoisonError` failures hiding the real one.
    pub fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record the XML of the most recent [`super::show`] call.
    pub fn record(xml: &str) {
        *LAST.lock().unwrap() = Some(xml.to_string());
    }

    /// Take (and clear) the most recently recorded XML.
    pub fn take() -> Option<String> {
        LAST.lock().unwrap().take()
    }

    /// Clear any recorded XML (call at the START of a wiring test so a prior
    /// test's record cannot leak in through the shared `static`).
    pub fn reset() {
        *LAST.lock().unwrap() = None;
    }
}

#[cfg(windows)]
mod imp {
    //! The WinRT toast backend. `ToastNotificationManager` / `XmlDocument` are
    //! projected as SAFE Rust by the `windows` crate (they return
    //! `windows::core::Result`), so the only `unsafe` here is the one Win32
    //! call that has no WinRT equivalent:
    //! `SetCurrentProcessExplicitAppUserModelID`.
    #![allow(unsafe_code)]

    use std::sync::Once;

    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    static AUMID_ONCE: Once = Once::new();

    /// Stamp this process with [`super::AUMID`] exactly once.
    ///
    /// Without it the process inherits an AUMID derived from its executable
    /// path, which does NOT match the Start-Menu shortcut's
    /// `System.AppUserModel.ID`; the shell then attributes the toast to an
    /// unknown app and drops it. Called lazily from [`show`] rather than at
    /// startup so this whole feature needs no seam in the (separately-owned)
    /// app bootstrap.
    ///
    /// Idempotence is enforced by `Once`, but the call is idempotent anyway —
    /// setting the same ID twice is a documented no-op.
    fn ensure_aumid() {
        AUMID_ONCE.call_once(|| {
            let id = HSTRING::from(super::AUMID);
            // SAFETY: `SetCurrentProcessExplicitAppUserModelID` takes one
            // null-terminated wide string and mutates only this process's own
            // shell identity. `HSTRING` guarantees a live, NUL-terminated UTF-16
            // buffer that outlives the call (`id` is dropped after it returns),
            // and the function retains no pointer to it. The `HRESULT` is
            // ignored deliberately: a failure means the toast below simply does
            // not appear, which must never be fatal to a terminal.
            let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(&id) };
        });
    }

    /// Load `xml` and raise it as a toast. `true` only when the shell accepted
    /// it; every WinRT failure is swallowed into `false` (the caller maps that
    /// to [`super::ToastOutcome::Failed`]).
    pub fn show(xml: &str) -> bool {
        ensure_aumid();
        let Ok(doc) = XmlDocument::new() else {
            return false;
        };
        // Rejects malformed XML — the reason `sanitize_line` strips control
        // characters and `escape_xml_text` escapes the markup metacharacters.
        if doc.LoadXml(&HSTRING::from(xml)).is_err() {
            return false;
        }
        let Ok(toast) = ToastNotification::CreateToastNotification(&doc) else {
            return false;
        };
        let Ok(notifier) =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(super::AUMID))
        else {
            return false;
        };
        notifier.Show(&toast).is_ok()
    }
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod notify_tests;
