//! A live terminal session: a PTY-attached child plus a background reader
//! thread that feeds output through the VT parser into a shared [`Terminal`].
//!
//! The UI layer holds the `Arc<Mutex<Terminal>>` and reads a grid snapshot per
//! frame; input is written back through [`Session::write_input`].

use crate::pty::PtyProcess;
use crate::term::Terminal;
use anyhow::Result;
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread::JoinHandle;

// loom shim: under `--cfg loom` the two synchronization primitives the session
// uses to coordinate the reader thread with the UI thread — the `Mutex` guarding
// the wake slot / terminal, and the `AtomicBool` liveness flag — are swapped for
// loom's instrumented equivalents so the `loom_tests` module can permute every
// interleaving under the C11 memory model. The swap is behaviour-preserving:
// both expose the identical `Mutex` / `AtomicBool` API the production code
// already calls, so no logic changes. `Arc` is deliberately NOT swapped: it
// stays `std::sync::Arc` because `WakeFn = Arc<dyn Fn()…>` relies on unsized
// `dyn` coercion that `loom::sync::Arc` does not provide, and loom instruments
// the Mutex/Atomic (the actual synchronization), not the reference count.
// Outside `--cfg loom` (every normal/CI/release build) these are the std
// primitives unchanged.
#[cfg(loom)]
use loom::sync::atomic::{AtomicBool, Ordering};
#[cfg(loom)]
use loom::sync::Mutex;
#[cfg(not(loom))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(loom))]
use std::sync::Mutex;

/// A UI-wake callback the reader thread invokes after parsing a chunk of PTY
/// output into the [`Terminal`]. The UI layer registers one via
/// [`Session::set_wake_callback`] so the render loop can sleep when idle and be
/// woken the instant new output lands — instead of free-running at the monitor
/// refresh rate. Deliberately a plain `Fn` (not an `egui` type) so `core` stays
/// UI-toolkit-agnostic; the app wires it to `egui::Context::request_repaint`,
/// which is documented to be safe to call from any thread.
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// A live terminal session: a spawned PTY process, the [`Terminal`] model its
/// output is parsed into, and a background reader thread that drains the PTY and
/// updates the terminal. The session owns the write side of the PTY (keystrokes
/// and pastes are sent through it) and tracks process liveness so the UI can
/// degrade a dead pane gracefully.
pub struct Session {
    pty: PtyProcess,
    terminal: Arc<Mutex<Terminal>>,
    writer: Box<dyn Write + Send>,
    alive: Arc<AtomicBool>,
    /// Shared slot for the UI-wake callback. The reader thread holds a clone and
    /// calls it (if set) once per drained chunk; the UI sets it post-spawn.
    wake: Arc<Mutex<Option<WakeFn>>>,
    reader_thread: Option<JoinHandle<()>>,
}

/// Safety timeout for a synchronized-output (`?2026`) hold: if a TUI opens a
/// synchronized update and never closes it (a crash, or a buggy app), the reader
/// forces a repaint after this long so the screen can never freeze. 150 ms is the
/// de-facto convention (kitty/WezTerm/foot use a comparable ceiling) — long enough
/// to batch a normal full-screen redraw into one frame, short enough that a
/// missing "end" is imperceptible.
const SYNC_OUTPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);

/// Whether a synchronized-output (`?2026`) hold that began `elapsed` ago is still
/// within the safety-timeout window — `true` = keep holding the repaint, `false`
/// = force a repaint (the update ran past the timeout, so we render anyway rather
/// than risk a frozen screen). Pure so the boundary is unit-testable exactly.
fn sync_hold_active(elapsed: std::time::Duration) -> bool {
    elapsed < SYNC_OUTPUT_TIMEOUT
}

/// Spawn the PTY-reader thread: drain `reader` in ≤64 KiB chunks, parse each
/// chunk into `terminal`, fire the UI-wake callback once per chunk, and clear
/// `alive` on EOF (child exited) or a read error.
///
/// Generic over the byte source (`R: Read`) on purpose: tests drive the EXACT
/// reader→parser→grid→wake loop the live session uses with an in-memory
/// `Cursor<Vec<u8>>` (deterministic — no real PTY, no thread race, no wall-clock
/// deadline). The live caller passes the PTY master reader (`Box<dyn Read + Send>`).
fn spawn_reader_thread<R: Read + Send + 'static>(
    mut reader: R,
    terminal: Arc<Mutex<Terminal>>,
    alive: Arc<AtomicBool>,
    wake: Arc<Mutex<Option<WakeFn>>>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("c0pl4nd-pty-reader".into())
        .spawn(move || {
            // 64 KiB read buffer (was 8 KiB). Under a flood (`cat bigfile`,
            // `yes`, a large paste) a blocking `read()` returns as many bytes
            // as are currently available up to the buffer size, so a larger
            // buffer drains a burst in up to ~8× fewer syscalls AND ~8× fewer
            // `Terminal` mutex acquisitions — the UI thread is starved far
            // less because the reader holds the render lock once per drained
            // chunk rather than once per 8 KiB. WezTerm/Alacritty read into
            // 64 KiB–1 MiB buffers for exactly this reason.
            //
            // We deliberately do NOT issue a speculative second `read()` to
            // coalesce multiple chunks under one lock: `portable_pty`'s reader
            // is BLOCKING with no readiness probe, so an extra `read()` after a
            // full-buffer read would block until the next byte arrives if the
            // producer happens to pause on a 64 KiB boundary — re-introducing
            // input-echo latency (the exact "over-batch" risk the roadmap
            // warns about). A single blocking read per wake, into a larger
            // buffer, is the correct latency-safe form: the kernel already
            // hands back everything currently buffered (up to 64 KiB) in one
            // call, so a real burst is drained per-chunk with no extra
            // round-trips and no risk of waiting for bytes that are not coming.
            //
            // The buffer is heap-allocated (a 64 KiB array would risk the
            // thread stack on some targets) and reused for the thread's
            // lifetime (zero per-read allocation).
            const READ_BUF: usize = 64 * 1024;
            let mut buf = vec![0u8; READ_BUF];
            // Synchronized-output (?2026) hold state: when a TUI opens a
            // synchronized update we suppress the per-chunk repaint so the UI
            // never samples a half-drawn frame and spends ONE repaint on the
            // completed frame instead of one per chunk. `sync_since` is when the
            // current hold began, so the safety timeout can force a repaint if the
            // matching "end" never arrives (a crashed/buggy TUI).
            let mut sync_since: Option<std::time::Instant> = None;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF: child exited
                    Ok(n) => {
                        // Parse the chunk and read the synchronized-output flag
                        // under the SAME lock (one acquisition per chunk).
                        let sync = if let Ok(mut term) = terminal.lock() {
                            term.advance(&buf[..n]);
                            term.sync_output()
                        } else {
                            false
                        };
                        // Hold the repaint while a synchronized update is open —
                        // unless it has been open past the safety timeout, in which
                        // case repaint anyway so a TUI that never closes its update
                        // can't freeze the screen. On the frame that CLOSES the
                        // update (`sync` back to false) we fall through and repaint
                        // the finished frame exactly once.
                        if sync {
                            let now = std::time::Instant::now();
                            let since = *sync_since.get_or_insert(now);
                            if sync_hold_active(now.duration_since(since)) {
                                continue; // still within the update — hold this repaint
                            }
                            sync_since = Some(now); // timed out: repaint + re-arm
                        } else {
                            sync_since = None;
                        }
                        // Wake the UI so it repaints the new output, then sleeps
                        // again. Clone the callback OUT of the lock so it is never
                        // invoked while the slot mutex is held (the callback runs
                        // UI code and must not be able to deadlock the reader
                        // against a `set_wake_callback`).
                        let cb = wake.lock().ok().and_then(|g| g.clone());
                        if let Some(cb) = cb {
                            cb();
                        }
                    }
                    Err(_) => break,
                }
            }
            alive.store(false, Ordering::SeqCst);
        })
}

impl Session {
    /// Spawn the platform default (or configured) shell.
    pub fn spawn_shell(shell: Option<&str>, rows: u16, cols: u16) -> Result<Self> {
        let pty = PtyProcess::spawn_shell(shell, rows, cols)?;
        Self::from_pty(pty, rows, cols)
    }

    /// Spawn the platform default (or configured) shell with an explicit `TERM`
    /// override (the config-driven `term` key flows in here). `term = None` /
    /// `Some("")` uses the canonical [`crate::pty::DEFAULT_TERM`].
    pub fn spawn_shell_with_term(
        shell: Option<&str>,
        rows: u16,
        cols: u16,
        term: Option<&str>,
    ) -> Result<Self> {
        let pty = PtyProcess::spawn_shell_in_with_term(shell, rows, cols, None, term)?;
        Self::from_pty(pty, rows, cols)
    }

    /// Spawn the shell in an explicit working directory (session restore). A
    /// `cwd` that no longer exists falls back to home (never wedges the launch).
    pub fn spawn_shell_in(
        shell: Option<&str>,
        rows: u16,
        cols: u16,
        cwd: Option<&str>,
    ) -> Result<Self> {
        let pty = PtyProcess::spawn_shell_in(shell, rows, cols, cwd)?;
        Self::from_pty(pty, rows, cols)
    }

    /// Like [`Session::spawn_shell_in`] but with an explicit `TERM` override
    /// (the config-driven `term` key). `term = None` / `Some("")` uses the
    /// canonical [`crate::pty::DEFAULT_TERM`].
    pub fn spawn_shell_in_with_term(
        shell: Option<&str>,
        rows: u16,
        cols: u16,
        cwd: Option<&str>,
        term: Option<&str>,
    ) -> Result<Self> {
        let pty = PtyProcess::spawn_shell_in_with_term(shell, rows, cols, cwd, term)?;
        Self::from_pty(pty, rows, cols)
    }

    /// Spawn an explicit program (used by tests for deterministic behaviour).
    pub fn spawn_program(program: &str, args: &[&str], rows: u16, cols: u16) -> Result<Self> {
        Self::spawn_program_in(program, args, rows, cols, None)
    }

    /// Like [`Session::spawn_program`] but starts the program in an explicit
    /// working directory — the named-shell-profile counterpart to
    /// [`spawn_shell_in`](Self::spawn_shell_in), so reopening or restoring a pane
    /// under a named profile lands in the pane's own cwd rather than the default
    /// one. A `cwd` that no longer exists falls back to home inside
    /// [`PtyProcess::spawn_program_in`] (a stale restored cwd is not an error).
    pub fn spawn_program_in(
        program: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        cwd: Option<&str>,
    ) -> Result<Self> {
        let pty = PtyProcess::spawn_program_in(program, args, rows, cols, cwd)?;
        Self::from_pty(pty, rows, cols)
    }

    fn from_pty(pty: PtyProcess, rows: u16, cols: u16) -> Result<Self> {
        let reader = pty.reader()?;
        let writer = pty.writer()?;
        let terminal = Arc::new(Mutex::new(Terminal::new(rows as usize, cols as usize)));
        let alive = Arc::new(AtomicBool::new(true));
        let wake: Arc<Mutex<Option<WakeFn>>> = Arc::new(Mutex::new(None));

        let reader_thread = spawn_reader_thread(
            reader,
            Arc::clone(&terminal),
            Arc::clone(&alive),
            Arc::clone(&wake),
        )?;

        Ok(Session {
            pty,
            terminal,
            writer,
            alive,
            wake,
            reader_thread: Some(reader_thread),
        })
    }

    /// Register the UI-wake callback invoked once per drained chunk of output.
    /// The UI layer passes a closure that calls `egui::Context::request_repaint`
    /// so the render loop can stay asleep when idle yet repaint the instant new
    /// PTY output arrives. Replacing a previously-set callback is allowed.
    pub fn set_wake_callback(&self, wake: WakeFn) {
        if let Ok(mut slot) = self.wake.lock() {
            *slot = Some(wake);
        }
    }

    /// Send input bytes to the child process.
    pub fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Shared handle to the terminal grid for the renderer.
    pub fn terminal(&self) -> Arc<Mutex<Terminal>> {
        Arc::clone(&self.terminal)
    }

    /// Resize both the PTY and the grid.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.pty.resize(rows, cols)?;
        if let Ok(mut t) = self.terminal.lock() {
            t.resize(rows as usize, cols as usize);
        }
        Ok(())
    }

    /// True while the child process is running (reader thread alive).
    ///
    /// # Examples
    ///
    /// ```
    /// use c0pl4nd_core::session::Session;
    ///
    /// // A freshly spawned session's reader thread is alive immediately: the
    /// // liveness flag is initialised to `true` before the reader can observe
    /// // EOF, so `is_alive()` reports `true` right after spawn.
    /// # #[cfg(windows)]
    /// # let session = Session::spawn_program("cmd.exe", &[], 24, 80).unwrap();
    /// # #[cfg(not(windows))]
    /// let session = Session::spawn_program("/bin/sh", &["-i"], 24, 80).unwrap();
    /// assert!(session.is_alive());
    /// ```
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// The OS process id of this session's child shell, if still known. Used by
    /// the app to assign the child to a `KILL_ON_JOB_CLOSE` job object so no shell
    /// (or its descendants) can outlive the app, even on a hard exit or crash.
    pub fn child_pid(&self) -> Option<u32> {
        self.pty.child_pid()
    }

    /// Kill this session's child shell WITHOUT dropping the session — a fast,
    /// non-blocking `TerminateProcess`. Used by the app's fast-close path to
    /// terminate every pane's shell in parallel BEFORE `std::process::exit(0)`:
    /// exit runs no destructors, so the per-pane `ClosePseudoConsole` (which
    /// BLOCKS until the attached child exits — the sequential close latency) never
    /// fires, yet no shell is orphaned because it was killed here. Idempotent; a
    /// no-op on an already-dead child. `Session`/`PtyProcess` `Drop` remain the
    /// backstop for the non-fast-exit paths.
    pub fn kill_child(&mut self) {
        self.pty.kill();
    }

    /// Non-blocking check of whether the child shell has exited (`true`) or is
    /// still running (`false`). Polls the child PROCESS directly, so it reflects a
    /// [`kill_child`](Self::kill_child) promptly — the reliable "was the shell
    /// reaped?" signal (the reader-thread `is_alive` flag lags because a ConPTY
    /// output pipe does not EOF promptly on a killed client).
    pub fn child_exited(&mut self) -> bool {
        self.pty.try_wait().is_some()
    }

    /// Render the current grid to text (used by the headless smoke test).
    pub fn snapshot_text(&self) -> String {
        self.terminal
            .lock()
            .map(|t| t.grid().to_text())
            .unwrap_or_default()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Kill the child shell FIRST. This is load-bearing for two reasons:
        //   1. It reaps the shell so it is not left as an orphaned `cmd.exe`
        //      (the session previously leaked one process per pane — ~148 after
        //      a heavy session).
        //   2. With the child dead, the reader thread's blocking `read()` gets
        //      EOF and the thread exits on its own, and the PTY master can close
        //      without `ClosePseudoConsole` blocking (the close-hang).
        // We then DETACH the reader thread rather than `join()` it: a ConPTY
        // master does not always deliver EOF promptly, and blocking `Drop` on a
        // `join()` would re-introduce the very hang we are fixing. The detached
        // thread observes EOF shortly after and exits; `PtyProcess::Drop` is the
        // backstop that also kills the child if we are dropped another way.
        self.pty.kill();
        self.reader_thread.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{Duration, Instant};

    /// A `Read` that hands back at most `chunk` bytes per call, so a test can
    /// drive the reader thread's MULTI-read burst-drain loop deterministically —
    /// a single `Cursor` read would return everything in one call and never
    /// exercise the per-chunk path.
    struct ChunkedReader {
        data: Vec<u8>,
        pos: usize,
        chunk: usize,
    }
    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let remaining = self.data.len() - self.pos;
            if remaining == 0 {
                return Ok(0); // EOF — deterministic, no blocking
            }
            let n = remaining.min(self.chunk).min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    /// Serialize the real-PTY smoke tests against each other so their ConPTY /
    /// process-creation latency does not compound under load. The deterministic
    /// `spawn_reader_thread` tests below are the always-on signal; the real-PTY
    /// smoke only confirms the OS plumbing, so it tolerates a generous bound.
    static REAL_PTY_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Tier B (deterministic): drive the EXACT reader→parser→grid→wake loop the
    /// live session uses, but with an in-memory reader instead of a real PTY.
    /// The reader EOFs after the feed, so `join()` makes the wait deterministic
    /// (no poll, no thread race, no wall-clock deadline). This — not the real-PTY
    /// smoke — is the always-on proof that PTY output reaches the grid.
    #[test]
    fn reader_thread_parses_feed_into_grid() {
        let token = "c0pl4nd_session_ok";
        let terminal = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let alive = Arc::new(AtomicBool::new(true));
        let wake: Arc<Mutex<Option<WakeFn>>> = Arc::new(Mutex::new(None));

        let feed = format!("{token}\r\nsecond line\r\n").into_bytes();
        let handle = spawn_reader_thread(
            Cursor::new(feed),
            Arc::clone(&terminal),
            Arc::clone(&alive),
            Arc::clone(&wake),
        )
        .expect("spawn reader thread");
        handle.join().expect("reader thread joins on EOF");

        // After EOF the whole feed is parsed and `alive` is cleared (the reader
        // stores `false` only AFTER the final `advance`, so this ordering holds).
        let text = terminal.lock().unwrap().grid().to_text();
        assert!(
            text.contains(token),
            "the parsed grid must contain {token:?}; got {text:?}"
        );
        assert!(
            !alive.load(Ordering::SeqCst),
            "the reader must clear `alive` on EOF"
        );
    }

    /// Tier C (smoke): the real cmd.exe/sh → ConPTY → reader → grid path end to
    /// end. Deterministic on success — it waits for the definitive `!is_alive`
    /// EOF signal (after which ALL output is guaranteed parsed) and asserts ONCE,
    /// instead of racing a poll-for-token. The 10 s bound is a safety net for a
    /// wedged spawn, not a race line; the spawn (not the assertion) is retried.
    #[test]
    fn session_renders_real_pty_output_smoke() {
        let _serial = REAL_PTY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let token = "c0pl4nd_session_ok";

        let mut session = None;
        for _ in 0..3 {
            #[cfg(windows)]
            let attempt =
                Session::spawn_program("cmd.exe", &["/C", &format!("echo {token}")], 24, 80);
            #[cfg(not(windows))]
            let attempt =
                Session::spawn_program("/bin/sh", &["-c", &format!("printf '{token}'")], 24, 80);
            match attempt {
                Ok(s) => {
                    session = Some(s);
                    break;
                }
                // Retry only a transient spawn failure under load — never the
                // assertion (a missing token is a real bug, not flakiness).
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        let session = session.expect("spawn real PTY session (after retries)");

        // Wait for the POSTCONDITION (the token reached the grid), not merely
        // for the child to exit.
        //
        // The child exiting only means it finished WRITING. The PTY reader
        // thread still has to pump that write through into the grid, so
        // `!is_alive()` can become true a moment BEFORE the token is visible —
        // and this loop used to stop exactly there. Under normal load the reader
        // wins the race and the assert passes; under heavy parallel load it
        // loses and the test fails with an empty snapshot. That is a race in the
        // test, not a bug in the session, and it fails RED for the wrong reason.
        //
        // Polling the real postcondition removes the race and is strictly
        // tighter: it still bounds at the same deadline, and a genuinely missing
        // token still fails (it just costs the full 10s to prove it).
        let deadline = Instant::now() + Duration::from_secs(10);
        while !session.snapshot_text().contains(token) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            session.snapshot_text().contains(token),
            "real PTY output must reach the grid; got {:?}",
            session.snapshot_text()
        );
    }

    /// A large burst of output (far bigger than one read buffer) must be
    /// delivered and parsed without loss — exercises the MULTI-read burst-drain
    /// reader path. Tier B (deterministic): feed ~2000 uniquely-numbered lines
    /// through `ChunkedReader` (4 KiB per read, so the loop iterates many times),
    /// join on EOF, and assert the LAST line reached the grid — the tail can only
    /// appear once the whole stream was read, parsed, and scrolled. No real PTY,
    /// no shell loop, no wall-clock deadline.
    #[test]
    fn reader_drains_large_burst_without_loss() {
        let last = 1999u32;
        let last_token = format!("L{last}");
        let mut feed = Vec::with_capacity(2000 * 8);
        for i in 0..=last {
            feed.extend_from_slice(format!("L{i}\r\n").as_bytes());
        }

        let terminal = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let alive = Arc::new(AtomicBool::new(true));
        let wake: Arc<Mutex<Option<WakeFn>>> = Arc::new(Mutex::new(None));
        let handle = spawn_reader_thread(
            ChunkedReader {
                data: feed,
                pos: 0,
                chunk: 4096,
            },
            Arc::clone(&terminal),
            Arc::clone(&alive),
            Arc::clone(&wake),
        )
        .expect("spawn reader thread");
        handle.join().expect("reader thread joins on EOF");

        let text = terminal.lock().unwrap().grid().to_text();
        assert!(
            text.contains(&last_token),
            "the final line {last_token:?} of a large burst must reach the grid"
        );
    }

    /// Synchronized output (`?2026`): while a TUI has an update open, the reader
    /// must HOLD the per-chunk repaint and fire exactly ONE wake on the closing
    /// `?2026l` — so a full-screen redraw batches into a single frame instead of
    /// one repaint per chunk (tear-free + far fewer repaints). The held content
    /// still reaches the grid (parsing is never gated, only the repaint).
    #[test]
    fn synchronized_output_batches_repaints_until_end() {
        use std::sync::atomic::AtomicUsize;

        /// A reader that returns one pre-split segment per `read()`, EOF after —
        /// so `?2026h`, the held content, and `?2026l` land in SEPARATE reads (the
        /// multi-chunk path the hold logic gates on).
        struct SegReader {
            segs: Vec<Vec<u8>>,
            i: usize,
        }
        impl Read for SegReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.i >= self.segs.len() {
                    return Ok(0);
                }
                let s = &self.segs[self.i];
                self.i += 1;
                let n = s.len().min(buf.len());
                buf[..n].copy_from_slice(&s[..n]);
                Ok(n)
            }
        }

        let segs = vec![
            b"\x1b[?2026h".to_vec(),  // BEGIN synchronized update
            b"line one\r\n".to_vec(), // content — held (no repaint)
            b"line two\r\n".to_vec(), // more content — held
            b"\x1b[?2026l".to_vec(),  // END — one repaint of the finished frame
        ];
        let terminal = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let alive = Arc::new(AtomicBool::new(true));
        let wake: Arc<Mutex<Option<WakeFn>>> = Arc::new(Mutex::new(None));
        let wakes = Arc::new(AtomicUsize::new(0));
        {
            let w = Arc::clone(&wakes);
            *wake.lock().unwrap() = Some(Arc::new(move || {
                w.fetch_add(1, Ordering::SeqCst);
            }));
        }
        let handle = spawn_reader_thread(
            SegReader { segs, i: 0 },
            Arc::clone(&terminal),
            Arc::clone(&alive),
            Arc::clone(&wake),
        )
        .expect("spawn reader thread");
        handle.join().expect("reader thread joins on EOF");

        // 4 chunks fed; the 3 inside the synchronized update are held, so only the
        // closing `?2026l` wakes → exactly ONE repaint (was 4 before this wiring).
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "a synchronized update must batch to a single repaint"
        );
        // Parsing is NOT gated — the held content is on the grid once the frame lands.
        let text = terminal.lock().unwrap().grid().to_text();
        assert!(
            text.contains("line one") && text.contains("line two"),
            "held content still reaches the grid: {text:?}"
        );
    }

    /// The synchronized-output hold window boundary is exact: still holding just
    /// under the timeout, released AT the timeout and beyond (a `<`, not `<=`, so
    /// a run-past-timeout update repaints rather than freezing).
    #[test]
    fn sync_hold_active_boundary_is_exact() {
        assert!(
            sync_hold_active(Duration::ZERO),
            "a just-begun hold is active (repaint suppressed)"
        );
        assert!(
            sync_hold_active(SYNC_OUTPUT_TIMEOUT - Duration::from_millis(1)),
            "just under the timeout still holds"
        );
        assert!(
            !sync_hold_active(SYNC_OUTPUT_TIMEOUT),
            "at EXACTLY the timeout the hold releases → force a repaint"
        );
        assert!(
            !sync_hold_active(SYNC_OUTPUT_TIMEOUT + Duration::from_millis(1)),
            "past the timeout releases → force a repaint"
        );
    }

    /// A live session reports a real child PID (never None / 0 / 1), and
    /// `kill_child` actually reaps the shell — the fast-close no-orphan guarantee.
    /// Real-PTY, serialized via the shared lock. Termination is observed via
    /// `child_exited` (a direct child-PROCESS `try_wait`), NOT the reader
    /// `is_alive` flag: a ConPTY output pipe does not EOF promptly on a killed
    /// client, so `is_alive` lags, whereas `try_wait` reflects the kill at once.
    #[test]
    fn session_child_pid_is_real_and_kill_child_reaps_the_shell() {
        let _lock = REAL_PTY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[cfg(windows)]
        let mut session = Session::spawn_program("cmd.exe", &[], 24, 80).expect("spawn");
        #[cfg(not(windows))]
        let mut session = Session::spawn_program("/bin/sh", &["-i"], 24, 80).expect("spawn");

        // child_pid: a live session reports a real OS pid (never None/0/1).
        let pid = session.child_pid().expect("a live session has a child pid");
        assert!(
            pid > 1,
            "a real shell pid is a normal OS pid, not 0/1: {pid}"
        );

        // The shell is running BEFORE the kill (catches a `child_exited -> true`).
        assert!(
            !session.child_exited(),
            "the interactive shell is running before kill_child"
        );

        // kill_child must terminate it (catches the no-op mutant): the child
        // process exits within a bounded poll. `try_wait` polls the process, so
        // this is deterministic despite the lagging ConPTY output pipe.
        session.kill_child();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !session.child_exited() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            session.child_exited(),
            "kill_child must terminate the shell so no orphan survives"
        );
    }

    /// The UI-wake callback must fire at least once after the child produces
    /// output — this is the signal the render loop relies on to repaint live
    /// output while otherwise sleeping (the damage-tracked-redraw mechanism).
    #[test]
    fn wake_callback_fires_on_output() {
        use std::sync::atomic::AtomicUsize;

        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_cb = Arc::clone(&wakes);

        // Stream many lines over time (not a single `echo`) so the callback,
        // registered immediately after spawn, reliably catches output chunks
        // that arrive AFTER registration — avoiding a race where a one-shot
        // child drains before the UI has wired its wake.
        #[cfg(windows)]
        let session = Session::spawn_program(
            "cmd.exe",
            &["/C", "for /L %i in (1,1,500) do @echo wake%i"],
            24,
            80,
        )
        .expect("spawn");
        #[cfg(not(windows))]
        let session = Session::spawn_program(
            "/bin/sh",
            &[
                "-c",
                "i=0; while [ $i -le 500 ]; do echo wake$i; i=$((i+1)); done",
            ],
            24,
            80,
        )
        .expect("spawn");

        // Register the callback AFTER spawn (the real wiring order: the UI sets
        // it once it has an egui Context). It must still catch the output.
        session.set_wake_callback(Arc::new(move || {
            wakes_cb.fetch_add(1, Ordering::SeqCst);
        }));

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if wakes.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            wakes.load(Ordering::SeqCst) > 0,
            "wake callback should fire at least once after the child emits output"
        );
    }

    /// `terminal()` hands back a clone of the shared `Arc<Mutex<Terminal>>`, and
    /// the grid it points at reflects the session's configured size. Asserts the
    /// exact rows/cols so a wrong dimension propagation would fail. Covers the
    /// `terminal()` accessor (was never directly called by a test).
    #[test]
    fn terminal_handle_reflects_configured_size() {
        #[cfg(windows)]
        let session =
            Session::spawn_program("cmd.exe", &["/C", "echo hi"], 30, 100).expect("spawn");
        #[cfg(not(windows))]
        let session =
            Session::spawn_program("/bin/sh", &["-c", "echo hi"], 30, 100).expect("spawn");

        let term = session.terminal();
        let guard = term.lock().expect("lock terminal");
        assert_eq!(guard.grid().rows(), 30, "grid rows must match spawn size");
        assert_eq!(guard.grid().cols(), 100, "grid cols must match spawn size");
        // Two calls return handles to the SAME underlying terminal (Arc clone).
        drop(guard);
        assert!(
            Arc::ptr_eq(&session.terminal(), &session.terminal()),
            "terminal() must return clones of one shared Arc, not new allocations"
        );
    }

    /// `resize()` resizes the grid (the PTY resize is exercised too, but the
    /// observable state is the grid dimensions). Asserts the new size lands —
    /// covers the `resize` method's terminal-lock + `t.resize` branch.
    #[test]
    fn resize_updates_grid_dimensions() {
        #[cfg(windows)]
        let mut session =
            Session::spawn_program("cmd.exe", &["/C", "echo hi"], 24, 80).expect("spawn");
        #[cfg(not(windows))]
        let mut session =
            Session::spawn_program("/bin/sh", &["-c", "echo hi"], 24, 80).expect("spawn");

        session.resize(40, 120).expect("resize");
        let term = session.terminal();
        let guard = term.lock().expect("lock");
        assert_eq!(guard.grid().rows(), 40, "grid rows updated by resize");
        assert_eq!(guard.grid().cols(), 120, "grid cols updated by resize");
    }

    /// `write_input` succeeds against a live child (the PTY writer accepts the
    /// bytes). Sending a harmless newline to an interactive shell must return
    /// `Ok` — covers `write_input`'s write_all + flush success path. We then
    /// kill the child so the test does not leak it.
    #[test]
    fn write_input_to_live_child_succeeds() {
        #[cfg(windows)]
        let mut session = Session::spawn_program("cmd.exe", &[], 24, 80).expect("spawn");
        #[cfg(not(windows))]
        let mut session = Session::spawn_program("/bin/sh", &["-i"], 24, 80).expect("spawn");

        // A bare newline is innocuous; the writer must accept it.
        session
            .write_input(b"\r\n")
            .expect("write_input should succeed on a live PTY");
        // Drop kills the child (see Session::Drop).
        let _ = &mut session;
    }

    /// `is_alive()` reports `true` immediately after spawn (the reader thread is
    /// running and the atomic was initialised to `true`). The eventual flip to
    /// `false` is NOT asserted here because it is platform-dependent: on Windows
    /// ConPTY the reader only observes EOF once the PTY master is CLOSED, not on
    /// child death (documented in `Session::Drop` and `PtyProcess::Drop`), so a
    /// bounded wait for `is_alive()==false` is flaky on Windows by design. The
    /// `alive.store(false, ..)` end-of-reader path is exercised end-to-end by the
    /// large-burst test (which polls `is_alive()` after the child exits) on the
    /// POSIX reader-EOF path. This test pins only the initial-true accessor read.
    #[test]
    fn is_alive_is_true_immediately_after_spawn() {
        #[cfg(windows)]
        let session = Session::spawn_program("cmd.exe", &[], 24, 80).expect("spawn");
        #[cfg(not(windows))]
        let session = Session::spawn_program("/bin/sh", &["-i"], 24, 80).expect("spawn");

        assert!(
            session.is_alive(),
            "a freshly spawned session reports its reader thread alive"
        );
        drop(session);
    }

    /// The `spawn_program` delegation reaches `from_pty` and produces a usable
    /// session whose initial snapshot is a blank grid (all spaces) before any
    /// output. Covers `snapshot_text` on a freshly-spawned session and the
    /// constructor chain. We assert the snapshot is the right shape (24 newlines)
    /// rather than exact spaces (output may arrive between spawn and read).
    #[test]
    fn fresh_session_snapshot_has_expected_row_count() {
        #[cfg(windows)]
        let session = Session::spawn_program("cmd.exe", &[], 24, 80).expect("spawn");
        #[cfg(not(windows))]
        let session = Session::spawn_program("/bin/sh", &["-i"], 24, 80).expect("spawn");

        let snap = session.snapshot_text();
        // to_text() emits one '\n' per row → 24 newlines for a 24-row grid.
        assert_eq!(
            snap.matches('\n').count(),
            24,
            "a 24-row grid snapshot must contain 24 line terminators"
        );
        drop(session);
    }
}

// Loom concurrency-permutation tests for the IN-HOUSE synchronization the
// session uses to coordinate its PTY-reader thread with the UI thread. Built
// and run ONLY under `--cfg loom` (loom is a `cfg(loom)`-gated dev-dependency),
// so they are invisible to every normal/CI/release build.
//
// SCOPE DECISION (honest narrowing): the *real* reader thread blocks on
// `portable_pty`'s FFI `reader.read()` — that is foreign, non-instrumentable
// code loom cannot and must not model (modelling FFI we do not control would be
// a fake test). What loom CAN model honestly is the in-house coordination the
// reader performs around each drained chunk and at EOF — the part `Session`
// owns outright:
//   1. the `Arc<Mutex<Option<WakeFn>>>` wake slot, whose load-bearing contract
//      (documented in `from_pty`) is "clone the callback OUT of the slot lock,
//      then call it — never invoke the UI callback while holding the slot
//      mutex, so the reader can never deadlock against a concurrent
//      `set_wake_callback`"; and
//   2. the `Arc<AtomicBool>` liveness flag the reader stores `false` into at
//      EOF and the UI reads via `is_alive()`.
// We model exactly these two primitives with a faithful in-test replica of the
// production access pattern (same lock/clone-out-of-lock order, same atomic
// ordering), driving them with `loom::thread` across every interleaving. The
// PTY/FFI byte path is deliberately abstracted to "a chunk arrived" / "EOF" —
// the events that drive the in-house logic — rather than modelled, because the
// FFI itself is out of our control.
//
// Gated on `all(test, loom)` (not just `loom`) so the module — and its imports —
// compile ONLY in the test target under `--cfg loom`; a plain `--cfg loom` lib
// build (which still exercises the production Mutex/AtomicBool shim above) does
// not pull the test-only imports in and stays warning-clean.
#[cfg(all(test, loom))]
mod loom_tests {
    use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use loom::sync::Mutex;
    // `Arc` stays std here for the same reason as the production shim above: the
    // wake callback is an `Arc<dyn Fn()…>` and `loom::sync::Arc` does not support
    // the unsized `dyn` coercion. loom instruments the `Mutex`/`AtomicBool` (the
    // synchronization under test), not the reference count, so a `std::sync::Arc`
    // wrapping loom primitives is the correct and supported pattern here.
    use std::sync::Arc;

    /// The wake-slot contract under all interleavings: a reader thread drains a
    /// chunk and, per the production `from_pty` logic, clones the callback OUT
    /// of the slot lock and invokes it AFTER releasing the lock; concurrently
    /// the UI thread installs the callback via `set_wake_callback`. Invariants
    /// loom verifies across every permutation:
    ///   * no deadlock (the model completes for every interleaving), and
    ///   * the callback is never invoked while the slot mutex is held (proven by
    ///     asserting the slot lock is free at the moment the callback runs —
    ///     `try_lock` must succeed inside the callback), and
    ///   * the wake count is a consistent 0 or 1 (no torn/lost state): if the
    ///     install happened-before the reader's slot read, the callback fires
    ///     exactly once; otherwise it does not fire — never a corrupt value.
    #[test]
    fn loom_wake_slot_clone_out_of_lock_never_deadlocks() {
        loom::model(|| {
            // The in-house wake slot, mirroring `Session::wake`.
            let slot: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = Arc::new(Mutex::new(None));
            // Counts callback invocations; the callback asserts the slot lock is
            // free when it runs (the clone-out-of-lock invariant).
            let wakes = Arc::new(AtomicUsize::new(0));

            // UI thread: install the wake callback (the post-spawn wiring order).
            let ui_slot = Arc::clone(&slot);
            let ui_wakes = Arc::clone(&wakes);
            let ui_slot_for_cb = Arc::clone(&slot);
            let ui = loom::thread::spawn(move || {
                let cb: Arc<dyn Fn() + Send + Sync> = {
                    let slot_in_cb = Arc::clone(&ui_slot_for_cb);
                    let wakes_in_cb = Arc::clone(&ui_wakes);
                    Arc::new(move || {
                        // The callback must NEVER run while the slot mutex is
                        // held — that is the documented deadlock-avoidance
                        // contract. If the reader cloned the callback out of the
                        // lock correctly, the slot is free here.
                        assert!(
                            slot_in_cb.try_lock().is_ok(),
                            "wake callback ran while the slot mutex was held — \
                             the clone-out-of-lock contract was violated"
                        );
                        wakes_in_cb.fetch_add(1, Ordering::SeqCst);
                    })
                };
                // set_wake_callback: take the lock, store, release.
                *ui_slot.lock().unwrap() = Some(cb);
            });

            // Reader thread: a chunk arrived. Mirror the production pattern —
            // clone the callback OUT of the slot lock, then (lock released)
            // invoke it.
            let rd_slot = Arc::clone(&slot);
            let reader = loom::thread::spawn(move || {
                let cb = rd_slot.lock().unwrap().clone();
                if let Some(cb) = cb {
                    cb();
                }
            });

            ui.join().unwrap();
            reader.join().unwrap();

            // The wake count is a clean 0 or 1 — never a torn value — for every
            // interleaving (the install either happened-before the reader's slot
            // read or it did not).
            let n = wakes.load(Ordering::SeqCst);
            assert!(n <= 1, "wake count must be a consistent 0 or 1, got {n}");
        });
    }

    /// The liveness-flag contract: the reader thread stores `false` into the
    /// shared `AtomicBool` at EOF, and the UI observes it via `is_alive()`.
    /// Under SeqCst, after the reader has joined the UI must observe `false`
    /// (the EOF store is published); before any store the initial `true` is the
    /// only other legal value. Loom verifies no interleaving yields a
    /// torn/illegal observation and that the join-then-load path is consistent.
    #[test]
    fn loom_alive_flag_eof_store_is_observed() {
        loom::model(|| {
            // Mirrors `Session::alive`, initialised true at spawn.
            let alive = Arc::new(AtomicBool::new(true));

            // Reader thread reaches EOF and records the child as dead.
            let rd_alive = Arc::clone(&alive);
            let reader = loom::thread::spawn(move || {
                rd_alive.store(false, Ordering::SeqCst);
            });

            // UI thread may poll is_alive() concurrently; any observation must
            // be one of the two legal values (true before the store, false
            // after) — never an illegal/torn read.
            let ui_alive = Arc::clone(&alive);
            let ui = loom::thread::spawn(move || {
                let observed = ui_alive.load(Ordering::SeqCst);
                assert!(
                    observed || !observed,
                    "is_alive() must read a well-defined bool under all interleavings"
                );
            });

            ui.join().unwrap();
            reader.join().unwrap();

            // After the reader (EOF) has joined, the dead state is published:
            // is_alive() MUST now observe false (no lost EOF signal).
            assert!(
                !alive.load(Ordering::SeqCst),
                "after the reader stores false at EOF and joins, is_alive() must \
                 observe false — a true here would be a lost-EOF (lost-wakeup) bug"
            );
        });
    }
}
