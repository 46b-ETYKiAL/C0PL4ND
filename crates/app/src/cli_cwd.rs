//! `--cwd <path>` (alias `-d <path>`) — the working directory the INITIAL shell
//! starts in.
//!
//! # Why this exists
//!
//! This is the argument the Windows Explorer "Open C0PL4ND here" shell verb
//! passes (`c0pl4nd.exe --cwd "%V"`), and the flag every other terminal exposes
//! for the same job (Windows Terminal spells it `-d` / `--startingDirectory`, so
//! `-d` is accepted as the alias).
//!
//! # Why the value is threaded to the spawn seam, not `set_current_dir`
//!
//! Applying the directory with `std::env::set_current_dir` would be silently
//! ignored. [`c0pl4nd_core::pty::PtyProcess::spawn_program_in_with_term`] ALWAYS
//! sets the child's directory explicitly — the requested `cwd` when it names a
//! real directory, otherwise the HOME fallback — so the child never inherits the
//! process working directory and the flag would appear to do nothing. The value
//! therefore travels to the PTY spawn call itself:
//!
//! ```text
//! egui_main::main
//!   -> cli_cwd::parse_startup_cwd(&args)      (parse + validate)
//!   -> cli_cwd::set_startup_cwd(&dir)         (one-shot store)
//!        ...
//!      egui_app::render_pane_body (deferred first-pane spawn)
//!   -> cli_cwd::take_startup_cwd()            (consumed exactly once)
//!   -> PaneTerm::spawn_in_with_term(.., Some(&dir))
//!   -> Session::spawn_shell_in_with_term -> PtyProcess -> CommandBuilder::cwd
//! ```
//!
//! The store is a one-shot: [`take_startup_cwd`] consumes the value, so the flag
//! affects the FIRST pane the app spawns and never leaks into later tabs/splits
//! (which follow the shell profile, exactly like Windows Terminal's `-d`).
//!
//! # The value is untrusted input
//!
//! `%V` is whatever directory the user right-clicked, and the flag can be typed
//! by hand. A path that does not exist, or that names a file rather than a
//! directory, is REJECTED with a clear message and a non-zero exit — never
//! silently ignored (which would look like the verb is broken) and never a
//! panic. The path is echoed back in the message because it is the user's own
//! argument and the error is unactionable without it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The long form of the flag.
pub const FLAG_LONG: &str = "--cwd";
/// The short form, matching Windows Terminal's `-d`.
pub const FLAG_SHORT: &str = "-d";

/// Why a `--cwd` / `-d` value was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CwdArgError {
    /// The flag was the last argument (or was followed by nothing usable).
    MissingValue,
    /// The path could not be made absolute (e.g. an empty string).
    Unresolvable(String),
    /// Nothing exists at that path.
    NotFound(PathBuf),
    /// Something exists there, but it is not a directory.
    NotADirectory(PathBuf),
}

impl CwdArgError {
    /// A short, plain-language sentence naming the problem and the fix. Shown on
    /// stderr and (on a windowed release build, which has no console) in the
    /// startup-error dialog.
    pub fn user_message(&self) -> String {
        match self {
            Self::MissingValue => format!(
                "{FLAG_LONG} needs a directory, e.g. `c0pl4nd {FLAG_LONG} C:\\projects`."
            ),
            Self::Unresolvable(raw) => format!(
                "Couldn't understand the {FLAG_LONG} directory {raw:?}. Pass a real folder path."
            ),
            Self::NotFound(p) => format!(
                "The {FLAG_LONG} directory doesn't exist: {}. Pass a folder that is already there.",
                p.display()
            ),
            Self::NotADirectory(p) => format!(
                "The {FLAG_LONG} path is a file, not a folder: {}. Pass the folder that contains it.",
                p.display()
            ),
        }
    }
}

/// Parse and validate `--cwd <path>` / `-d <path>` out of a full `argv`.
///
/// Both the separated form (`--cwd C:\src`) and the joined form (`--cwd=C:\src`)
/// are accepted; the joined form is what a shell verb or shortcut is most likely
/// to be edited into by hand. The FIRST occurrence wins.
///
/// Returns `Ok(None)` when the flag is absent — the normal launch. The returned
/// path is absolute and lexically normalised (`std::path::absolute`, so no
/// `\\?\` verbatim prefix that `cmd.exe` would then echo in the prompt), and is
/// proven to be an existing directory.
pub fn parse_startup_cwd(args: &[String]) -> Result<Option<PathBuf>, CwdArgError> {
    let Some(raw) = find_raw_value(args)? else {
        return Ok(None);
    };
    validate_dir(&raw).map(Some)
}

/// Locate the flag's raw (unvalidated) value in `argv`. `Ok(None)` = flag absent.
fn find_raw_value(args: &[String]) -> Result<Option<String>, CwdArgError> {
    // The separated form reads its value by INDEX (`args.get(idx + 1)`) rather
    // than by advancing the iterator, so a plain `for` is correct here — the
    // loop never consumes the value item itself.
    for (idx, arg) in args.iter().enumerate().skip(1) {
        // argv[0] is the program path, hence the skip(1).
        let joined = arg
            .strip_prefix(&format!("{FLAG_LONG}="))
            .or_else(|| arg.strip_prefix(&format!("{FLAG_SHORT}=")));
        if let Some(value) = joined {
            // `--cwd=` with nothing after it is a missing value, not an empty dir.
            return if value.is_empty() {
                Err(CwdArgError::MissingValue)
            } else {
                Ok(Some(value.to_string()))
            };
        }
        if arg == FLAG_LONG || arg == FLAG_SHORT {
            return match args.get(idx + 1) {
                Some(v) if !v.is_empty() => Ok(Some(v.clone())),
                _ => Err(CwdArgError::MissingValue),
            };
        }
    }
    Ok(None)
}

/// Turn a raw path into an absolute, existing DIRECTORY, or explain why not.
fn validate_dir(raw: &str) -> Result<PathBuf, CwdArgError> {
    let abs = std::path::absolute(Path::new(raw))
        .map_err(|_| CwdArgError::Unresolvable(raw.to_string()))?;
    // `metadata` follows symlinks, so a directory symlink (a legitimate target
    // for the shell verb) resolves to its directory and is accepted.
    match std::fs::metadata(&abs) {
        Ok(md) if md.is_dir() => Ok(abs),
        Ok(_) => Err(CwdArgError::NotADirectory(abs)),
        Err(_) => Err(CwdArgError::NotFound(abs)),
    }
}

/// The one-shot startup-directory store. `None` = no `--cwd` was given (or it has
/// already been consumed by the first pane's spawn).
static STARTUP_CWD: Mutex<Option<String>> = Mutex::new(None);

/// Record the validated startup directory. Called ONCE from the binary entry
/// point, before the window/event loop exists.
pub fn set_startup_cwd(dir: &Path) {
    if let Ok(mut slot) = STARTUP_CWD.lock() {
        *slot = Some(dir.to_string_lossy().into_owned());
    }
}

/// Consume the startup directory, if one was given. Called from the deferred
/// first-pane spawn in the shell's frame path; consuming it is what keeps the
/// flag scoped to the INITIAL pane instead of every later tab/split.
///
/// A poisoned lock (only reachable if a holder panicked, which none of these
/// tiny critical sections can) degrades to "no startup directory" — the pane
/// still opens, in the default dir.
pub fn take_startup_cwd() -> Option<String> {
    STARTUP_CWD.lock().ok()?.take()
}

/// Clear the store. Test-support for the wiring suites, which must start from a
/// known state; the shipping binary never needs it (the process starts empty).
pub fn clear_startup_cwd() {
    if let Ok(mut slot) = STARTUP_CWD.lock() {
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(rest: &[&str]) -> Vec<String> {
        std::iter::once("c0pl4nd.exe".to_string())
            .chain(rest.iter().map(|s| s.to_string()))
            .collect()
    }

    #[test]
    fn absent_flag_is_not_an_error() {
        assert_eq!(parse_startup_cwd(&argv(&["--demo"])), Ok(None));
    }

    #[test]
    fn a_real_directory_is_accepted_and_made_absolute() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parsed = parse_startup_cwd(&argv(&["--cwd", &dir.path().to_string_lossy()]))
            .expect("a real directory must parse")
            .expect("the flag was present");
        assert!(parsed.is_absolute(), "got {parsed:?}");
        assert!(parsed.is_dir(), "got {parsed:?}");
        // No `\\?\` verbatim prefix: `cmd.exe` would echo it in the prompt.
        assert!(
            !parsed.to_string_lossy().starts_with(r"\\?\"),
            "verbatim prefix leaked into the spawn cwd: {parsed:?}"
        );
    }

    #[test]
    fn the_short_alias_matches_windows_terminal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parsed = parse_startup_cwd(&argv(&["-d", &dir.path().to_string_lossy()]))
            .expect("-d must parse")
            .expect("the flag was present");
        assert!(parsed.is_dir());
    }

    #[test]
    fn the_joined_form_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let joined = format!("--cwd={}", dir.path().display());
        let parsed = parse_startup_cwd(&argv(&[&joined]))
            .expect("--cwd=<path> must parse")
            .expect("the flag was present");
        assert!(parsed.is_dir());
    }

    #[test]
    fn a_trailing_flag_with_no_value_is_rejected() {
        assert_eq!(
            parse_startup_cwd(&argv(&["--cwd"])),
            Err(CwdArgError::MissingValue),
        );
        assert_eq!(
            parse_startup_cwd(&argv(&["-d"])),
            Err(CwdArgError::MissingValue),
        );
        assert_eq!(
            parse_startup_cwd(&argv(&["--cwd="])),
            Err(CwdArgError::MissingValue),
        );
    }

    #[test]
    fn a_nonexistent_directory_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("no-such-folder");
        let err = parse_startup_cwd(&argv(&["--cwd", &missing.to_string_lossy()]))
            .expect_err("a nonexistent path must be refused, not silently ignored");
        assert!(matches!(err, CwdArgError::NotFound(_)), "got {err:?}");
        assert!(err.user_message().contains("doesn't exist"));
    }

    #[test]
    fn a_file_is_rejected_as_not_a_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"x").expect("seed file");
        let err = parse_startup_cwd(&argv(&["--cwd", &file.to_string_lossy()]))
            .expect_err("a file must be refused, not used as a working directory");
        assert!(matches!(err, CwdArgError::NotADirectory(_)), "got {err:?}");
        assert!(err.user_message().contains("file, not a folder"));
    }

    #[test]
    fn every_error_message_names_the_flag_and_a_fix() {
        for err in [
            CwdArgError::MissingValue,
            CwdArgError::Unresolvable("".into()),
            CwdArgError::NotFound(PathBuf::from("/nope")),
            CwdArgError::NotADirectory(PathBuf::from("/nope.txt")),
        ] {
            let msg = err.user_message();
            assert!(msg.contains(FLAG_LONG), "{msg}");
            assert!(msg.ends_with('.'), "{msg}");
        }
    }

    #[test]
    fn the_store_is_one_shot_so_only_the_initial_pane_gets_it() {
        // Serialised against the other store test by `CWD_STORE_LOCK`.
        let _guard = CWD_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_startup_cwd();
        let dir = tempfile::tempdir().expect("tempdir");
        set_startup_cwd(dir.path());
        assert_eq!(
            take_startup_cwd().as_deref(),
            Some(dir.path().to_string_lossy().as_ref()),
            "the first take must yield the startup directory",
        );
        assert_eq!(
            take_startup_cwd(),
            None,
            "a second take must be empty — later tabs/splits use the profile default",
        );
    }

    #[test]
    fn clear_empties_the_store() {
        let _guard = CWD_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        set_startup_cwd(dir.path());
        clear_startup_cwd();
        assert_eq!(take_startup_cwd(), None);
    }

    /// The store is process-global, so the two tests that drive it must not run
    /// concurrently with each other.
    static CWD_STORE_LOCK: Mutex<()> = Mutex::new(());
}
