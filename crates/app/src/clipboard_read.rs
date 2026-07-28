//! Host-side system-clipboard READ for OSC 52 clipboard queries.
//!
//! The emulator core deliberately never touches the OS clipboard itself: it
//! parses `OSC 52 ; <sel> ; ?`, applies the default-deny gate, and — only when
//! the user has opted in — parks a
//! [`ClipboardReadRequest`](c0pl4nd_core::term::osc::ClipboardReadRequest) for
//! the host to answer. This module is that host half, and it is the ONLY place
//! in C0PL4ND that pulls text off the system clipboard.
//!
//! # Why a direct `arboard` call
//!
//! egui exposes clipboard WRITES (`Context::copy_text`) but no read API — a
//! paste can only arrive as an OS-delivered [`egui::Event::Paste`], which is
//! driven by the user pressing a key, not by a program asking. An OSC 52 read
//! has no such user gesture to ride on, so the answer has to come from a real
//! clipboard read. `arboard` is already in the dependency graph (egui-winit
//! uses it for exactly this on the write side), so this adds no new
//! supply-chain surface.
//!
//! # Security posture
//!
//! This function is called ONLY behind the core's default-off gate. It is not a
//! gate itself and must never be treated as one — the refusal decision lives in
//! [`c0pl4nd_core::term::Terminal`], which both withholds the request and
//! refuses to emit a reply while reads are denied. A failure to read (no
//! clipboard, non-text content, a busy Win32 clipboard) returns `None`, which
//! the caller turns into the same empty-payload reply a refusal produces — so a
//! transient failure can never hang the requesting program either.
//!
//! The returned text is wrapped in [`Zeroizing`] so the plaintext (routinely a
//! password or token — that is precisely why the read direction is gated) is
//! wiped from its allocation on drop rather than left recoverable in freed
//! memory, matching the write path's `ClipboardWrite` discipline.

use c0pl4nd_core::term::ClipboardSelection;
use zeroize::Zeroizing;

/// Read the system clipboard's text for `selection`, or `None` when it cannot
/// be read (clipboard unavailable, empty, or holding non-text content).
///
/// # Selection mapping
///
/// Both [`ClipboardSelection::Clipboard`] and [`ClipboardSelection::Primary`]
/// read the system clipboard. C0PL4ND is a Windows-first terminal and Windows
/// has no X11-style primary selection, so there is no separate buffer to
/// return; answering the primary query from the system clipboard matches what
/// the user opted into and keeps one code path. The reply still echoes the
/// selection character the program asked about.
pub fn read_selection(selection: ClipboardSelection) -> Option<Zeroizing<String>> {
    let _ = selection;
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            // Never log the clipboard CONTENTS — only that a read failed.
            tracing::debug!("clipboard unavailable for an OSC 52 read: {e}");
            return None;
        }
    };
    match clipboard.get_text() {
        Ok(text) => Some(Zeroizing::new(text)),
        Err(e) => {
            tracing::debug!("OSC 52 clipboard read returned no text: {e}");
            None
        }
    }
}
