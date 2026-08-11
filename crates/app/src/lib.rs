//! The C0PL4ND app library — the egui shell's testable surface.
//!
//! # Why this crate has a lib target
//!
//! `crates/app` was binary-only. Integration tests in `tests/` therefore could
//! not `use c0pl4nd::…`; they reached the app by `#[path]`-including
//! `../src/egui_app/mod.rs` and its siblings, which compiles a SECOND, private
//! copy of the whole module tree into every test binary.
//!
//! That is invisible to `cargo test` (the tests pass, and they genuinely drive
//! the real `frame_tick`) but it wrecks coverage attribution: `cargo llvm-cov`
//! reports the `c0pl4nd` **bin** object, and the `#[path]` copies live in the
//! test binaries instead. Measured directly — running the `egui_chrome` suite
//! alone reported **0.00%** for every app file while 400+ of its tests passed.
//! The consequence was that the app's reported coverage counted only the in-file
//! `#[cfg(test)]` unit tests, and roughly 3,900 real UI test executions across
//! nine `egui_kittest` suites contributed nothing to the number. `chrome.rs`
//! read 17.5% — exactly its in-file `mod tests` block, and no more.
//!
//! Exposing the module tree as a library makes the tests link the SAME
//! compilation the binary ships, so coverage lands on the real object and the
//! reported number means what it says. Nothing about what the tests exercise
//! changes — only whether the measurement can see it.
//!
//! # Scope
//!
//! These four modules are a closed set under `crate::` (they reference only one
//! another), which is why they move together and why the binaries' remaining
//! modules can stay where they are. `panic_hook` resolves `crate::reporting`
//! through the binary's root re-export.

/// `--cwd <path>` / `-d <path>` parsing + the one-shot startup-directory store
/// the initial pane's PTY spawn consumes. In the lib (not a binary module) so
/// the wiring suites in `tests/` drive the SAME store the shipping binary sets.
pub mod cli_cwd;
/// Host-side system-clipboard READ, the answering half of an opted-in OSC 52
/// clipboard query. The only place in C0PL4ND that pulls text off the OS
/// clipboard; always called behind the core's default-off gate.
pub mod clipboard_read;
pub mod egui_app;
pub mod issue_intake;
/// Real Windows desktop notifications (WinRT toasts) for OSC 9 / OSC 777, plus
/// the pure suppression/sanitising/escaping decisions behind them and the
/// `System.AppUserModel.ID` the installer shortcut must carry. A lib-root
/// module (not `egui_app::notify`) because `egui_app`'s submodules are private,
/// which would make this unreachable — and therefore dead — until its call site
/// in the pump lands; see the module docs for that seam.
pub mod notify;
pub mod reporting;
pub mod user_error;
