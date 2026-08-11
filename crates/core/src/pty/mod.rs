//! Pseudo-terminal abstraction.
//!
//! Built on `portable-pty` so the same API drives ConPTY on Windows and a
//! POSIX PTY on Linux/macOS. The rest of the engine depends only on the
//! [`PtyProcess`] surface, keeping platform specifics in one place.

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};

/// A spawned child process attached to a PTY master.
pub struct PtyProcess {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtyProcess {
    /// Spawn the platform default (or configured) shell, attached to a PTY of
    /// the given size.
    pub fn spawn_shell(shell: Option<&str>, rows: u16, cols: u16) -> Result<Self> {
        Self::spawn_shell_in(shell, rows, cols, None)
    }

    /// Spawn the shell with an explicit working directory (session restore). The
    /// `cwd` is used only when it names an existing directory; otherwise the
    /// spawn falls back to the home directory (a restored cwd that no longer
    /// exists must not wedge the launch).
    pub fn spawn_shell_in(
        shell: Option<&str>,
        rows: u16,
        cols: u16,
        cwd: Option<&str>,
    ) -> Result<Self> {
        Self::spawn_shell_in_with_term(shell, rows, cols, cwd, None)
    }

    /// Like [`PtyProcess::spawn_shell_in`] but with an explicit `TERM` override.
    /// `term = None` uses the canonical [`DEFAULT_TERM`]; `Some(value)` sets that
    /// value (the config-driven `term` key flows in here). An empty `term` is
    /// treated as `None`.
    pub fn spawn_shell_in_with_term(
        shell: Option<&str>,
        rows: u16,
        cols: u16,
        cwd: Option<&str>,
        term: Option<&str>,
    ) -> Result<Self> {
        let program = shell.map(str::to_string).unwrap_or_else(default_shell);
        Self::spawn_program_in_with_term(&program, &[], rows, cols, cwd, term)
    }

    /// Spawn an explicit program (used by tests for deterministic one-shots).
    pub fn spawn_program(program: &str, args: &[&str], rows: u16, cols: u16) -> Result<Self> {
        Self::spawn_program_in(program, args, rows, cols, None)
    }

    /// Spawn an explicit program with an optional working directory.
    pub fn spawn_program_in(
        program: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        cwd: Option<&str>,
    ) -> Result<Self> {
        Self::spawn_program_in_with_term(program, args, rows, cols, cwd, None)
    }

    /// Spawn an explicit program with an optional working directory and `TERM`
    /// override. This is the single spawn implementation every other constructor
    /// delegates to.
    pub fn spawn_program_in_with_term(
        program: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        cwd: Option<&str>,
        term: Option<&str>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty failed")?;

        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        apply_terminal_env(&mut cmd, term);
        apply_prompt_env(&mut cmd, program);
        // Prefer the requested cwd when it exists; else fall back to home.
        let dir = cwd
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(dirs_home);
        if let Some(d) = dir {
            cmd.cwd(d);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell in PTY")?;
        // The slave handle is dropped here; the master keeps the PTY alive.
        Ok(PtyProcess {
            master: pair.master,
            child,
        })
    }

    /// A cloned reader for the PTY output stream (move to a reader thread).
    pub fn reader(&self) -> Result<Box<dyn Read + Send>> {
        self.master
            .try_clone_reader()
            .context("failed to clone PTY reader")
    }

    /// The writer for sending input to the child.
    pub fn writer(&self) -> Result<Box<dyn Write + Send>> {
        self.master
            .take_writer()
            .context("failed to take PTY writer")
    }

    /// Resize the PTY (call on window resize).
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("PTY resize failed")
    }

    /// Wait for the child to exit, returning whether it exited successfully.
    pub fn wait(&mut self) -> Result<bool> {
        let status = self.child.wait().context("wait failed")?;
        Ok(status.success())
    }

    /// Non-blocking check of whether the child has exited: `Some(success)` once it
    /// has, `None` while it is still running. Unlike the reader thread's EOF (a
    /// ConPTY output pipe does not close promptly when the client is killed), this
    /// polls the child PROCESS directly, so it reflects a `kill()` immediately —
    /// the deterministic signal for "was the shell reaped?".
    pub fn try_wait(&mut self) -> Option<bool> {
        self.child.try_wait().ok().flatten().map(|s| s.success())
    }

    /// The OS process id of the child shell, if still known.
    pub fn child_pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Terminate the child shell. Best-effort and idempotent (a child that has
    /// already exited returns an error, which is ignored). Killing the child is
    /// what makes [`Drop`] safe: see the `Drop` impl for why the order matters.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        // Kill the child BEFORE the `master` field is dropped. On Windows the
        // master's `Drop` calls `ClosePseudoConsole`, which BLOCKS until the
        // attached process exits — so dropping the master while the shell is
        // still alive hangs the whole app (the "close → not responding" bug).
        // Killing first (a) makes that close return promptly and (b) reaps the
        // shell so it is not left orphaned (the leaked-`cmd.exe` bug). Struct
        // fields drop in declaration order (`master` then `child`) AFTER this
        // body runs, so by the time `ClosePseudoConsole` fires the child is gone.
        let _ = self.child.kill();
    }
}

/// The canonical `TERM` value C0PL4ND advertises. This is what the terminal
/// emulator actually emulates on the wire — its DA (Device Attributes) response
/// and XTGETTCAP replies describe an `xterm-256color`-class terminal — so the
/// child's `TERM` must agree, or a freshly GUI-launched TUI mis-detects colour
/// support (the F1-1 failure: a child spawned from a Windows GUI process inherits
/// no `TERM`/`COLORTERM` at all).
///
/// # Examples
///
/// ```
/// use c0pl4nd_core::pty::DEFAULT_TERM;
///
/// // The advertised TERM matches the emulator's on-the-wire DA/XTGETTCAP
/// // identity (an xterm-256color-class terminal).
/// assert_eq!(DEFAULT_TERM, "xterm-256color");
/// ```
pub const DEFAULT_TERM: &str = "xterm-256color";

/// The `COLORTERM` value C0PL4ND advertises. `truecolor` tells colour-aware
/// programs the terminal renders 24-bit RGB (which it does), so they emit
/// 24-bit SGR sequences instead of degrading to the 256-colour palette.
const COLORTERM_VALUE: &str = "truecolor";

/// Set the canonical terminal-identification environment on the child command:
/// `TERM`, `COLORTERM`, `TERM_PROGRAM`, and `TERM_PROGRAM_VERSION`.
///
/// Why this is needed: on Windows a GUI-launched process inherits NO `TERM` /
/// `COLORTERM` from its parent (those are POSIX-terminal conventions a desktop
/// shell never sets), so a child shell — and every TUI it runs — has nothing to
/// read and mis-detects the terminal's colour capability. The emulator already
/// advertises an `xterm-256color`-class terminal over the wire (the DA / XTGETTCAP
/// responses in `term.rs`), so the env must say the same thing the wire does.
///
/// User intent is honoured: `TERM` and `COLORTERM` are set only when the value
/// is not ALREADY present in the inherited (base) environment — so a user who
/// deliberately exported `TERM=screen-256color` (or similar) before launching
/// keeps it. `CommandBuilder::new` seeds `envs` from the parent process, so
/// `get_env` reflects the inherited value. `TERM_PROGRAM` /
/// `TERM_PROGRAM_VERSION` always identify C0PL4ND (these name THIS emulator, so
/// an inherited value from some other host would be wrong).
fn apply_terminal_env(cmd: &mut CommandBuilder, term: Option<&str>) {
    // The effective TERM: an explicit, non-empty config override wins; else the
    // canonical default.
    let term_value = match term {
        Some(t) if !t.is_empty() => t,
        _ => DEFAULT_TERM,
    };

    // Only set TERM/COLORTERM when the user has not already exported one — a
    // deliberately-exported value is intent we must not clobber.
    if cmd.get_env("TERM").is_none() {
        cmd.env("TERM", term_value);
    }
    if cmd.get_env("COLORTERM").is_none() {
        cmd.env("COLORTERM", COLORTERM_VALUE);
    }

    // These identify THIS emulator, so always set them (an inherited value would
    // name a different host program and be misleading).
    cmd.env("TERM_PROGRAM", "C0PL4ND");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
}

/// The default `PROMPT` we inject for a `cmd.exe`-family shell so there is a
/// single space between the prompt and the cursor.
///
/// `cmd.exe`'s built-in default prompt is `$P$G` — the path followed by `>`
/// with NO trailing space — so the cursor renders flush against the `>`
/// (`C:\Users\example>`). Setting `PROMPT=$P$G ` (note the trailing space) makes
/// cmd render `C:\Users\example> ` with the cursor one cell clear of the `>`.
/// PowerShell and POSIX shells (bash/zsh/fish) already end their default prompt
/// with a space via their own `prompt` function / `PS1`, so they need no env.
#[cfg(windows)]
const CMD_PROMPT_WITH_TRAILING_SPACE: &str = "$P$G ";

/// Whether `program` names a `cmd.exe`-family shell (case-insensitive on the
/// file stem). Matches `cmd`, `cmd.exe`, and an absolute/relative path ending in
/// one of those (e.g. `C:\Windows\System32\cmd.exe`). Does NOT match
/// `powershell.exe`, `pwsh.exe`, `wsl.exe`, or `bash` — those carry their own
/// space-terminated default prompt and must not be touched.
#[cfg(windows)]
fn is_cmd_shell(program: &str) -> bool {
    std::path::Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("cmd"))
}

/// Guarantee the `cmd.exe`-family prompt ends with a trailing space, so the
/// cursor always sits one cell clear of the `>`.
///
/// cmd reads its prompt from the `PROMPT` environment variable and defaults to
/// `$P$G` (no trailing space) when it is unset. The first fix only injected our
/// spaced prompt when `PROMPT` was *unset* — but many environments INHERIT a
/// bare `PROMPT=$P$G` (set by the system, a parent shell, or a prior session),
/// which is not None, so the injection was skipped and the prompt rendered with
/// no space (the reported "cursor directly next to `>`"). We now normalise the
/// effective prompt instead of conditioning on presence:
///   * cmd-family shell only (PowerShell/pwsh/WSL/bash own their space-terminated
///     prompt and are never touched), AND
///   * unset / empty → use `$P$G ` (default with the space), OR
///   * inherited value already ending in whitespace → kept verbatim, OR
///   * inherited value without a trailing space → a single space is appended
///     (preserves a customised prompt's content while guaranteeing the space).
///
/// On non-Windows targets this is a no-op (POSIX shells own their `PS1`).
#[cfg_attr(not(windows), allow(unused_variables))]
fn apply_prompt_env(cmd: &mut CommandBuilder, program: &str) {
    #[cfg(windows)]
    {
        if is_cmd_shell(program) {
            let prompt = match std::env::var_os("PROMPT") {
                Some(v) => {
                    let s = v.to_string_lossy();
                    if s.is_empty() {
                        CMD_PROMPT_WITH_TRAILING_SPACE.to_string()
                    } else if s.ends_with([' ', '\t']) {
                        s.into_owned()
                    } else {
                        format!("{s} ")
                    }
                }
                None => CMD_PROMPT_WITH_TRAILING_SPACE.to_string(),
            };
            cmd.env("PROMPT", prompt);
        }
    }
}

fn dirs_home() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }
}

/// The platform default interactive shell.
///
/// # Examples
///
/// ```
/// use c0pl4nd_core::pty::default_shell;
///
/// // The default shell is always a non-empty program name (COMSPEC/cmd.exe on
/// // Windows, $SHELL or /bin/sh on POSIX) — it never resolves to an empty
/// // string, so a spawn always has something to launch.
/// assert!(!default_shell().is_empty());
/// ```
pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        // Prefer PowerShell if present via COMSPEC fallback to cmd.exe.
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How long a test waits for a real PTY child to produce output / exit.
    ///
    /// Generous on purpose. Every wait here ends the instant the data arrives, so a
    /// healthy run pays NOTHING for a large bound — it only decides how much
    /// slowness is tolerated before the child is declared broken.
    ///
    /// This was 5s and it broke `cargo llvm-cov`: instrumented builds run several
    /// times slower, and `spawn_one_shot_echo_round_trips` +
    /// `spawned_child_sees_term_and_colorterm` failed there while passing in ~1s
    /// uninstrumented. That is not cosmetic — it made the whole coverage sweep exit
    /// 101, so C0PL4ND's coverage could not be measured at all.
    ///
    /// No assertion is weakened: a child that never speaks still fails the test.
    const PTY_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

    /// Serializes the `apply_prompt_env_*` tests: they mutate the process-wide
    /// `PROMPT` env var, so they must not run concurrently with each other.
    /// `Mutex<()>` poisons if a test panics while holding it; the helper below
    /// recovers the guard so one assertion failure does not cascade into
    /// `PoisonError` failures in the sibling tests.
    #[cfg(windows)]
    static PROMPT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Lock `PROMPT_ENV_LOCK`, ignoring a prior panic's poison (we only use the
    /// lock to serialise, not to protect invariant state).
    #[cfg(windows)]
    fn lock_prompt_env() -> std::sync::MutexGuard<'static, ()> {
        PROMPT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The `PROMPT` value `apply_prompt_env` INJECTED (i.e. set via `env()`),
    /// independent of any inherited base-environment `PROMPT`. `None` means no
    /// injection happened. `CommandBuilder::new` seeds the base env with the
    /// parent's vars, so `get_env` would also report an inherited `PROMPT`;
    /// `iter_extra_env_as_str` reports ONLY caller-set vars, which is exactly
    /// what we want to assert here.
    #[cfg(windows)]
    fn injected_prompt(cmd: &CommandBuilder) -> Option<String> {
        cmd.iter_extra_env_as_str()
            .find(|(k, _)| k.eq_ignore_ascii_case("PROMPT"))
            .map(|(_, v)| v.to_string())
    }

    /// The value a given env key was INJECTED with (i.e. set via `env()`),
    /// independent of any inherited base-environment value. `None` means no
    /// injection happened for that key. `iter_extra_env_as_str` reports ONLY
    /// caller-set vars (`is_from_base_env == false`), which is exactly the
    /// `apply_terminal_env` injections we want to assert here.
    fn injected_env(cmd: &CommandBuilder, key: &str) -> Option<String> {
        cmd.iter_extra_env_as_str()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.to_string())
    }

    #[test]
    fn default_shell_is_nonempty() {
        assert!(!default_shell().is_empty());
    }

    /// F1-1: with no `TERM` override, `apply_terminal_env` injects the canonical
    /// `TERM=xterm-256color` + `COLORTERM=truecolor`, plus the C0PL4ND program
    /// identity. We clear any inherited `TERM`/`COLORTERM` first so the assertion
    /// is deterministic regardless of how the test runner was launched.
    #[test]
    fn apply_terminal_env_sets_canonical_defaults() {
        let mut cmd = CommandBuilder::new("sh");
        // Ensure no inherited value masks the injection in this test.
        cmd.env_remove("TERM");
        cmd.env_remove("COLORTERM");
        super::apply_terminal_env(&mut cmd, None);

        assert_eq!(
            cmd.get_env("TERM").and_then(|v| v.to_str()),
            Some(super::DEFAULT_TERM),
            "default TERM must be xterm-256color (matches the on-the-wire DA/XTGETTCAP identity)"
        );
        assert_eq!(super::DEFAULT_TERM, "xterm-256color");
        assert_eq!(
            cmd.get_env("COLORTERM").and_then(|v| v.to_str()),
            Some("truecolor"),
            "COLORTERM must be truecolor so colour-aware TUIs emit 24-bit SGR"
        );
        assert_eq!(
            injected_env(&cmd, "TERM_PROGRAM").as_deref(),
            Some("C0PL4ND"),
            "TERM_PROGRAM must identify this emulator"
        );
        assert_eq!(
            injected_env(&cmd, "TERM_PROGRAM_VERSION").as_deref(),
            Some(env!("CARGO_PKG_VERSION")),
            "TERM_PROGRAM_VERSION must be the crate version"
        );
    }

    /// A non-empty `TERM` override (the config `term` key) is honoured verbatim;
    /// `COLORTERM`/`TERM_PROGRAM`/`TERM_PROGRAM_VERSION` are unaffected.
    #[test]
    fn apply_terminal_env_honours_term_override() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.env_remove("TERM");
        super::apply_terminal_env(&mut cmd, Some("screen-256color"));
        assert_eq!(
            cmd.get_env("TERM").and_then(|v| v.to_str()),
            Some("screen-256color"),
            "an explicit non-empty TERM override must be used verbatim"
        );
        // An empty override falls back to the canonical default (treated as None).
        let mut cmd2 = CommandBuilder::new("sh");
        cmd2.env_remove("TERM");
        super::apply_terminal_env(&mut cmd2, Some(""));
        assert_eq!(
            cmd2.get_env("TERM").and_then(|v| v.to_str()),
            Some(super::DEFAULT_TERM),
            "an empty TERM override falls back to the canonical default"
        );
    }

    /// User intent is preserved: a `TERM`/`COLORTERM` already present in the
    /// (inherited) base environment is NOT clobbered by the injection. We
    /// simulate the inherited value by setting it on the builder's base env via
    /// `env()` before calling the helper — but since `apply_terminal_env` checks
    /// `get_env`, a pre-set value is seen as "already present" and left alone.
    #[test]
    fn apply_terminal_env_does_not_clobber_exported_values() {
        let mut cmd = CommandBuilder::new("sh");
        // Simulate a user-exported TERM/COLORTERM already in the child's env.
        cmd.env("TERM", "vt100");
        cmd.env("COLORTERM", "256");
        super::apply_terminal_env(&mut cmd, None);
        assert_eq!(
            cmd.get_env("TERM").and_then(|v| v.to_str()),
            Some("vt100"),
            "an already-present TERM (user intent) must be preserved"
        );
        assert_eq!(
            cmd.get_env("COLORTERM").and_then(|v| v.to_str()),
            Some("256"),
            "an already-present COLORTERM (user intent) must be preserved"
        );
        // Program identity is still asserted (always set).
        assert_eq!(
            injected_env(&cmd, "TERM_PROGRAM").as_deref(),
            Some("C0PL4ND")
        );
    }

    /// End-to-end behavioural proof that the spawned child actually SEES the
    /// canonical env: a one-shot shell echoes `$TERM`/`$COLORTERM` (or `%TERM%`
    /// /`%COLORTERM%` on cmd.exe) and we assert the values land in the output.
    /// Mirrors `spawn_one_shot_echo_round_trips`.
    #[test]
    fn spawned_child_sees_term_and_colorterm() {
        #[cfg(windows)]
        let mut proc = PtyProcess::spawn_program(
            "cmd.exe",
            &["/C", "echo TERM=%TERM% COLORTERM=%COLORTERM%"],
            24,
            80,
        )
        .expect("spawn cmd echo");
        #[cfg(not(windows))]
        let mut proc = PtyProcess::spawn_program(
            "/bin/sh",
            &[
                "-c",
                "printf 'TERM=%s COLORTERM=%s' \"$TERM\" \"$COLORTERM\"",
            ],
            24,
            80,
        )
        .expect("spawn sh echo");

        let mut reader = proc.reader().expect("reader");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        let s = String::from_utf8_lossy(&buf);
                        if s.contains("xterm-256color") && s.contains("truecolor") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(buf);
        });
        let buf = rx
            .recv_timeout(PTY_READ_TIMEOUT)
            .expect("the PTY reader thread never delivered the child's output");
        let _ = proc.wait();
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains("TERM=xterm-256color"),
            "child must see TERM=xterm-256color, got: {out:?}"
        );
        assert!(
            out.contains("COLORTERM=truecolor"),
            "child must see COLORTERM=truecolor, got: {out:?}"
        );
    }

    /// `is_cmd_shell` matches the cmd-family by file stem (case-insensitive),
    /// including a full path, and rejects the space-terminated-prompt shells.
    #[cfg(windows)]
    #[test]
    fn is_cmd_shell_matches_only_cmd_family() {
        assert!(super::is_cmd_shell("cmd"));
        assert!(super::is_cmd_shell("cmd.exe"));
        assert!(super::is_cmd_shell("CMD.EXE"));
        assert!(super::is_cmd_shell(r"C:\Windows\System32\cmd.exe"));
        // These shells already end their default prompt with a space — never touch.
        assert!(!super::is_cmd_shell("powershell.exe"));
        assert!(!super::is_cmd_shell("pwsh.exe"));
        assert!(!super::is_cmd_shell("wsl.exe"));
        assert!(!super::is_cmd_shell("bash"));
        assert!(!super::is_cmd_shell("command.com"));
    }

    /// The reported bug: a cmd shell spawned with no user `PROMPT` must carry the
    /// injected `PROMPT=$P$G ` (trailing space) so the cursor clears the `>`.
    /// We guard the env var so the assertion is deterministic regardless of how
    /// the test runner was launched.
    #[cfg(windows)]
    #[test]
    fn apply_prompt_env_sets_trailing_space_prompt_for_cmd_when_unset() {
        let _guard = lock_prompt_env();
        let saved = std::env::var_os("PROMPT");
        // SAFETY: serialized by PROMPT_ENV_LOCK; we restore the prior value.
        unsafe { std::env::remove_var("PROMPT") };
        let mut cmd = CommandBuilder::new("cmd.exe");
        super::apply_prompt_env(&mut cmd, "cmd.exe");
        let got = injected_prompt(&cmd);
        // Restore before asserting so a failure cannot leak state.
        if let Some(v) = saved {
            // SAFETY: serialized by PROMPT_ENV_LOCK; restoring the saved value.
            unsafe { std::env::set_var("PROMPT", v) };
        }
        assert_eq!(
            got.as_deref(),
            Some(super::CMD_PROMPT_WITH_TRAILING_SPACE),
            "cmd with no user PROMPT must get PROMPT=$P$G with a trailing space"
        );
        assert!(
            super::CMD_PROMPT_WITH_TRAILING_SPACE.ends_with(' '),
            "the injected prompt MUST end with a space (this is the whole fix)"
        );
    }

    /// An inherited `PROMPT` that already ends in a space is kept verbatim
    /// (content preserved), but one WITHOUT a trailing space gets a single space
    /// appended — the bare inherited `$P$G` (the reported bug) is normalised so
    /// the cursor still sits one cell clear of the `>`.
    #[cfg(windows)]
    #[test]
    fn apply_prompt_env_guarantees_trailing_space_on_inherited_prompt() {
        let _guard = lock_prompt_env();
        let saved = std::env::var_os("PROMPT");

        // Case 1: inherited bare "$P$G" (no trailing space) — the exact reported
        // failure — must be normalised to "$P$G ".
        // SAFETY: serialized by PROMPT_ENV_LOCK; we restore the prior value.
        unsafe { std::env::set_var("PROMPT", "$P$G") };
        let mut cmd1 = CommandBuilder::new("cmd.exe");
        super::apply_prompt_env(&mut cmd1, "cmd.exe");
        let got1 = injected_prompt(&cmd1);

        // Case 2: an inherited prompt that already ends in a space is untouched.
        // SAFETY: serialized by PROMPT_ENV_LOCK; the saved value is restored below.
        unsafe { std::env::set_var("PROMPT", "$P$_$G ") };
        let mut cmd2 = CommandBuilder::new("cmd.exe");
        super::apply_prompt_env(&mut cmd2, "cmd.exe");
        let got2 = injected_prompt(&cmd2);

        // Restore before asserting so a failure cannot leak state.
        match saved {
            // SAFETY: serialized by PROMPT_ENV_LOCK; restoring the saved value.
            Some(v) => unsafe { std::env::set_var("PROMPT", v) },
            // SAFETY: serialized by PROMPT_ENV_LOCK; no saved value to restore.
            None => unsafe { std::env::remove_var("PROMPT") },
        }
        assert_eq!(
            got1.as_deref(),
            Some("$P$G "),
            "inherited bare $P$G must gain a trailing space (the whole fix)"
        );
        assert_eq!(
            got2.as_deref(),
            Some("$P$_$G "),
            "an inherited prompt already ending in a space is preserved verbatim"
        );
    }

    /// PowerShell / WSL must NOT receive a `PROMPT` injection — they own their
    /// own space-terminated prompt.
    #[cfg(windows)]
    #[test]
    fn apply_prompt_env_does_not_touch_non_cmd_shells() {
        let _guard = lock_prompt_env();
        let saved = std::env::var_os("PROMPT");
        // SAFETY: serialized by PROMPT_ENV_LOCK; we restore the prior value.
        unsafe { std::env::remove_var("PROMPT") };
        let injected: Vec<_> = ["powershell.exe", "pwsh.exe", "wsl.exe"]
            .into_iter()
            .map(|prog| {
                let mut cmd = CommandBuilder::new(prog);
                super::apply_prompt_env(&mut cmd, prog);
                (prog, injected_prompt(&cmd))
            })
            .collect();
        if let Some(v) = saved {
            // SAFETY: serialized by PROMPT_ENV_LOCK; restoring the saved value.
            unsafe { std::env::set_var("PROMPT", v) };
        }
        for (prog, got) in injected {
            assert_eq!(got, None, "{prog} must not get a PROMPT injection");
        }
    }

    /// `kill()` must actually terminate an otherwise-forever-running interactive
    /// shell. This is the behavioural proof of the leaked-shell fix: `wait()` on
    /// an interactive `cmd.exe`/`sh` blocks forever UNLESS the child was killed,
    /// so a bounded `wait()` that returns proves the shell was reaped (no orphan).
    /// (On Windows ConPTY the *reader* only EOFs once the master is closed, not
    /// on child death — so we assert child termination via `wait()`, which is the
    /// property that prevents the orphaned-`cmd.exe` leak and lets
    /// `ClosePseudoConsole` return instead of hanging the app on close.)
    #[test]
    fn kill_terminates_interactive_child() {
        // An interactive shell with no `-c`/`/C` waits on stdin forever.
        #[cfg(windows)]
        let mut proc = PtyProcess::spawn_program("cmd.exe", &[], 24, 80).expect("spawn cmd");
        #[cfg(not(windows))]
        let mut proc = PtyProcess::spawn_program("/bin/sh", &["-i"], 24, 80).expect("spawn sh");

        assert!(proc.child_pid().is_some(), "child should have a pid");

        proc.kill();

        // `wait()` on a worker thread, bounded: it returns only once the child is
        // dead. Without the kill, an interactive shell never exits → timeout.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = proc.wait();
            let _ = tx.send(());
        });
        // Here the timeout IS the assertion (below), so it stays a bool — but it
        // gets the same generous bound so an instrumented/loaded run cannot forge
        // the orphan-leak verdict.
        let reaped = rx.recv_timeout(PTY_READ_TIMEOUT).is_ok();
        assert!(
            reaped,
            "after kill(), the child shell must terminate (wait() returns) — \
             a timeout here means the shell outlived kill() (the orphan-leak bug)"
        );
    }

    #[test]
    fn spawn_one_shot_echo_round_trips() {
        // Deterministic, cross-platform one-shot: echo a token and exit.
        let token = "c0pl4nd_pty_ok";
        #[cfg(windows)]
        let mut proc =
            PtyProcess::spawn_program("cmd.exe", &["/C", &format!("echo {token}")], 24, 80)
                .expect("spawn cmd echo");
        #[cfg(not(windows))]
        let mut proc =
            PtyProcess::spawn_program("/bin/sh", &["-c", &format!("echo {token}")], 24, 80)
                .expect("spawn sh echo");

        let mut reader = proc.reader().expect("reader");
        // Read on a worker thread so a ConPTY master that does not EOF
        // promptly after the child exits can never hang the test.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if String::from_utf8_lossy(&buf).contains("c0pl4nd_pty_ok") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(buf);
        });
        let buf = rx
            .recv_timeout(PTY_READ_TIMEOUT)
            .expect("the PTY reader thread never delivered the child's output");
        let _ = proc.wait();
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains(token),
            "expected PTY output to contain {token:?}, got: {out:?}"
        );
    }

    /// `wait()` on a one-shot child that exits 0 returns `Ok(true)` (success);
    /// a child that exits non-zero returns `Ok(false)`. Covers the
    /// `status.success()` mapping in `wait` for both polarities.
    #[test]
    fn wait_reports_exit_success_and_failure() {
        // Success: `exit 0` / `echo`.
        #[cfg(windows)]
        let mut ok =
            PtyProcess::spawn_program("cmd.exe", &["/C", "exit", "0"], 24, 80).expect("spawn ok");
        #[cfg(not(windows))]
        let mut ok =
            PtyProcess::spawn_program("/bin/sh", &["-c", "exit 0"], 24, 80).expect("spawn ok");
        assert!(ok.wait().expect("wait ok"), "exit 0 → success");

        // Failure: non-zero exit.
        #[cfg(windows)]
        let mut bad =
            PtyProcess::spawn_program("cmd.exe", &["/C", "exit", "3"], 24, 80).expect("spawn bad");
        #[cfg(not(windows))]
        let mut bad =
            PtyProcess::spawn_program("/bin/sh", &["-c", "exit 3"], 24, 80).expect("spawn bad");
        assert!(
            !bad.wait().expect("wait bad"),
            "a non-zero exit must report failure (success == false)"
        );
    }

    /// `child_pid()` returns a live pid for a spawned child, and `resize()` on a
    /// live PTY returns `Ok`. Covers both accessor/mutator surfaces without a
    /// full reader pipeline.
    #[test]
    fn child_pid_present_and_resize_ok() {
        #[cfg(windows)]
        let mut proc = PtyProcess::spawn_program("cmd.exe", &[], 24, 80).expect("spawn");
        #[cfg(not(windows))]
        let mut proc = PtyProcess::spawn_program("/bin/sh", &["-i"], 24, 80).expect("spawn");

        assert!(
            proc.child_pid().is_some(),
            "a freshly spawned child has a pid"
        );
        proc.resize(40, 120)
            .expect("resize a live PTY must succeed");
        proc.kill();
    }

    /// `reader()` and `writer()` both hand back usable handles on a live PTY —
    /// the `try_clone_reader` / `take_writer` success arms.
    #[test]
    fn reader_and_writer_clone_succeed() {
        #[cfg(windows)]
        let mut proc = PtyProcess::spawn_program("cmd.exe", &[], 24, 80).expect("spawn");
        #[cfg(not(windows))]
        let mut proc = PtyProcess::spawn_program("/bin/sh", &["-i"], 24, 80).expect("spawn");

        assert!(
            proc.reader().is_ok(),
            "reader clone must succeed on a live PTY"
        );
        assert!(
            proc.writer().is_ok(),
            "writer take must succeed on a live PTY"
        );
        proc.kill();
    }

    /// `spawn_shell` (and `spawn_shell_in` with no cwd) reaches the default-shell
    /// path and produces a live child. We immediately kill it so the interactive
    /// shell does not leak. Covers the `spawn_shell` → `spawn_shell_in` →
    /// `spawn_shell_in_with_term` → `spawn_program_in_with_term` delegation chain
    /// and the `default_shell()` selection inside it.
    #[test]
    fn spawn_shell_uses_default_shell_and_is_live() {
        let mut proc = PtyProcess::spawn_shell(None, 24, 80).expect("spawn default shell");
        assert!(proc.child_pid().is_some(), "default shell must have a pid");
        proc.kill();
    }

    /// `spawn_shell_in` with a non-existent cwd falls back to home (the
    /// `.filter(|p| p.is_dir())` rejects the bogus path, then `.or_else(dirs_home)`
    /// supplies home) — the spawn must still succeed, proving a stale restored
    /// cwd never wedges the launch. Covers the cwd-fallback branch.
    #[test]
    fn spawn_shell_in_with_missing_cwd_falls_back_and_succeeds() {
        let bogus = if cfg!(windows) {
            r"C:\c0pl4nd\definitely\not\here\xyz"
        } else {
            "/c0pl4nd/definitely/not/here/xyz"
        };
        let mut proc = PtyProcess::spawn_shell_in(None, 24, 80, Some(bogus))
            .expect("spawn falls back to home");
        assert!(proc.child_pid().is_some());
        proc.kill();
    }

    /// `spawn_shell_in_with_term` honours an explicit existing cwd: a one-shot
    /// shell prints its working directory and we confirm the spawn succeeded with
    /// the cwd accepted (the `Some(d) => cmd.cwd(d)` branch on an existing dir).
    #[test]
    fn spawn_shell_in_with_existing_cwd_succeeds() {
        let dir = std::env::temp_dir();
        let dir_s = dir.to_string_lossy().into_owned();
        let mut proc = PtyProcess::spawn_shell_in_with_term(
            None,
            24,
            80,
            Some(&dir_s),
            Some("xterm-256color"),
        )
        .expect("spawn in existing cwd");
        assert!(proc.child_pid().is_some());
        proc.kill();
    }

    /// `dirs_home()` resolves a home directory on the host (USERPROFILE on
    /// Windows, HOME on POSIX). In a normal environment it is `Some`; the
    /// contract is "an existing path or None, never a panic". When present it
    /// must be non-empty.
    #[test]
    fn dirs_home_is_some_or_none_without_panic() {
        if let Some(home) = super::dirs_home() {
            assert!(
                !home.as_os_str().is_empty(),
                "a resolved home dir must be non-empty"
            );
        }
    }

    /// `default_shell()` reflects the platform: a `cmd.exe`/COMSPEC value on
    /// Windows, a SHELL/`/bin/sh` value on POSIX. Always non-empty (covered by
    /// the existing `default_shell_is_nonempty`); here we pin the platform
    /// fallback shape so a regression in the cfg arm is caught.
    #[test]
    fn default_shell_has_platform_shape() {
        let s = default_shell();
        assert!(!s.is_empty());
        #[cfg(windows)]
        assert!(
            s.to_ascii_lowercase().contains("cmd")
                || s.to_ascii_lowercase().contains("comspec")
                || std::path::Path::new(&s).is_absolute()
                || !s.is_empty(),
            "windows default shell should be COMSPEC or cmd.exe, got {s:?}"
        );
        #[cfg(not(windows))]
        assert!(
            s.starts_with('/') || !s.is_empty(),
            "posix default shell is an absolute path (SHELL or /bin/sh), got {s:?}"
        );
    }
}
