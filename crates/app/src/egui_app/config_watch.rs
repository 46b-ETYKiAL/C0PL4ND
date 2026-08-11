//! Live config-file watching — the mechanism behind config HOT RELOAD.
//!
//! Editing `config.toml` in an external editor used to require a relaunch: the
//! app read the file exactly once, in [`super::C0pl4ndApp::bootstrap_with`], and
//! never looked at it again. Only the in-app Settings window could change a
//! setting live, so the file was a write-only surface for everything else.
//!
//! This module is the pure, testable half of the fix: it stat-polls the config
//! file and reports when it CHANGED. Applying the reload (parse, swap, re-theme,
//! repaint) is [`super::C0pl4ndApp::config_hot_reload_tick`], which shares its
//! apply path with the Settings window so the two can never drift apart.
//!
//! ## Why stat-polling rather than a filesystem-notification API
//!
//! A `ReadDirectoryChangesW` / inotify watcher would need a new dependency, a
//! background thread, and a cross-platform shim — for a file the app already
//! knows the exact path of and which changes at human speed. A `stat` every
//! [`POLL_INTERVAL`] costs one metadata read per 400 ms and is exactly as
//! responsive as a human editor round-trip needs. It also degrades honestly on
//! network/virtualised filesystems where change notifications are unreliable.
//!
//! ## Why (len, mtime) and not a content hash
//!
//! The fingerprint is the file's length plus its modification time. Every real
//! write path bumps at least one: an editor's write-temp-then-rename (the common
//! case, including this app's own [`c0pl4nd_core::Config::save_to`]) always sets
//! a fresh mtime, and an in-place edit that somehow preserved the mtime would
//! still have to preserve the byte count too. Hashing would mean reading the
//! whole file every 400 ms to detect a change that happens once an hour.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How often the config file's metadata is read. Fast enough that an editor
/// save feels immediate, slow enough that the per-frame cost is a rounding
/// error (one `stat` per 400 ms, versus 60+ frames in the same window).
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// A cheap identity for the config file's current contents: its byte length and
/// its modification time. Compared for INEQUALITY only — a change in either
/// field means "the file on disk is not the one we last read".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStamp {
    len: u64,
    /// `None` when the platform/filesystem does not report a modification time.
    /// Two stamps that both lack an mtime then compare on length alone, which is
    /// weaker but never WRONG — it can miss a same-length edit, not invent one.
    mtime: Option<SystemTime>,
}

// Counts the metadata reads performed on the CURRENT THREAD, so the throttle
// tests can assert that a throttled poll does not touch the filesystem at all —
// rather than only that it returned `None`, which is equally true of a poll that
// DID stat the file and found it unchanged. Avoiding the stat is the entire
// purpose of the throttle, so it is the thing worth asserting.
//
// Thread-local (libtest gives each test its own thread) and read as a DELTA, so
// concurrent tests can never see each other's counts.
#[cfg(test)]
thread_local! {
    static STAT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The number of [`stamp_of`] calls made on this thread so far. Compare two
/// readings; never assert on the absolute value.
#[cfg(test)]
pub(crate) fn stat_calls() -> usize {
    STAT_CALLS.with(std::cell::Cell::get)
}

/// Read the current stamp of `path`, or `None` when it does not exist (or its
/// metadata cannot be read — a locked file mid-rename, a permission change).
/// An unreadable file is deliberately indistinguishable from an absent one:
/// both mean "nothing to reload right now", never "reload from nothing".
pub(crate) fn stamp_of(path: &Path) -> Option<FileStamp> {
    #[cfg(test)]
    STAT_CALLS.with(|c| c.set(c.get() + 1));
    let md = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        len: md.len(),
        mtime: md.modified().ok(),
    })
}

/// Throttled change-detector for a single config file.
///
/// The default value watches NOTHING and its [`ConfigWatcher::poll`] is a
/// permanent `None`, so an app constructed without a resolvable config path
/// simply never hot-reloads.
#[derive(Debug, Default)]
pub(crate) struct ConfigWatcher {
    path: Option<PathBuf>,
    /// The stamp of the file as of the last time we accepted its contents —
    /// either at construction or at the last reported change.
    stamp: Option<FileStamp>,
    last_poll: Option<Instant>,
}

impl ConfigWatcher {
    /// Watch `path`, treating its CURRENT contents as already-loaded. The first
    /// [`Self::poll`] therefore reports no change: the caller has just read this
    /// file, and re-reporting it would reload the config on the first frame of
    /// every launch.
    pub(crate) fn watching(path: PathBuf) -> Self {
        let stamp = stamp_of(&path);
        Self {
            path: Some(path),
            stamp,
            last_poll: None,
        }
    }

    /// Re-stamp the watched file WITHOUT reporting a change. Called after the
    /// app itself writes the config (a Settings edit, the font-zoom persist,
    /// the save-on-close) so our own write is not mistaken for an external edit
    /// and immediately re-applied. Purely an optimisation: re-applying our own
    /// config is idempotent, but it would churn the theme + visuals every time
    /// the user nudges a slider.
    pub(crate) fn mark_self_written(&mut self) {
        if let Some(p) = self.path.as_deref() {
            self.stamp = stamp_of(p);
        }
    }

    /// Returns the watched path when the file has CHANGED since it was last
    /// accepted, otherwise `None`. Throttled to one metadata read per
    /// [`POLL_INTERVAL`]; `now` is injected so the throttle is testable without
    /// sleeping.
    ///
    /// A file that has become ABSENT (or unreadable) is never reported as a
    /// change: the app keeps the config it already has rather than reverting the
    /// user's whole setup because an editor deleted-and-recreated the file, or
    /// because a network share blinked. The recreated file's next stamp differs
    /// from the last ACCEPTED one, so the real edit still lands on the poll
    /// after it reappears.
    pub(crate) fn poll(&mut self, now: Instant) -> Option<PathBuf> {
        let path = self.path.clone()?;
        if let Some(last) = self.last_poll {
            if now.duration_since(last) < POLL_INTERVAL {
                return None;
            }
        }
        self.last_poll = Some(now);
        let current = stamp_of(&path);
        // Absent/unreadable: remember that state (so the file reappearing is a
        // change) but do not ask the caller to reload from a file that is gone.
        if current.is_none() {
            self.stamp = None;
            return None;
        }
        if current == self.stamp {
            return None;
        }
        self.stamp = current;
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `body` to `path` and force a strictly-later mtime than any stamp
    /// taken before the call, so the test never races the filesystem's
    /// modification-time granularity.
    fn write_later(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        let later = SystemTime::now() + Duration::from_secs(60);
        // `set_modified` is best-effort across filesystems; when it is refused
        // the length difference between the test's bodies still distinguishes
        // the stamps.
        let _ = std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(later));
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "c0pl4nd_cfgwatch_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_watcher_with_no_path_never_reports_a_change() {
        let mut w = ConfigWatcher::default();
        assert!(
            w.poll(Instant::now()).is_none(),
            "a watcher with no resolvable config path must never hot-reload"
        );
    }

    #[test]
    fn the_first_poll_of_an_unchanged_file_reports_nothing() {
        let dir = temp_dir("unchanged");
        let p = dir.join("config.toml");
        std::fs::write(&p, "theme = \"itasha-corp\"\n").unwrap();

        let mut w = ConfigWatcher::watching(p.clone());
        // Far past the throttle, so `None` here means "no change", not "too soon".
        assert!(w.poll(Instant::now() + POLL_INTERVAL * 4).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_edited_file_is_reported_once_then_settles() {
        let dir = temp_dir("edited");
        let p = dir.join("config.toml");
        std::fs::write(&p, "theme = \"itasha-corp\"\n").unwrap();
        let mut w = ConfigWatcher::watching(p.clone());

        write_later(&p, "theme = \"ghost-paper\"\n# a longer file\n");

        let t = Instant::now() + POLL_INTERVAL * 4;
        assert_eq!(w.poll(t), Some(p.clone()), "the edit must be reported");
        assert!(
            w.poll(t + POLL_INTERVAL * 4).is_none(),
            "the same edit must not be reported twice (no reload loop)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn polls_inside_the_interval_are_throttled() {
        let dir = temp_dir("throttle");
        let p = dir.join("config.toml");
        std::fs::write(&p, "theme = \"itasha-corp\"\n").unwrap();
        let mut w = ConfigWatcher::watching(p.clone());

        let t0 = Instant::now();
        // Establish `last_poll` with a poll that is itself past the throttle.
        assert!(w.poll(t0).is_none());

        write_later(&p, "theme = \"ghost-paper\"\n# a longer file\n");

        // OBSERVE the stat, don't infer it. `poll(..).is_none()` alone is also
        // true of a poll that stats the file and compares equal, so on its own
        // it cannot tell a working throttle from a removed one — and skipping
        // the filesystem is what the throttle is FOR.
        let before = stat_calls();
        assert!(
            w.poll(t0 + POLL_INTERVAL / 2).is_none(),
            "a poll inside the interval must report no change"
        );
        assert_eq!(
            stat_calls(),
            before,
            "a poll inside the interval must not even stat the file"
        );

        assert_eq!(
            w.poll(t0 + POLL_INTERVAL * 2),
            Some(p.clone()),
            "the same edit is reported once the interval has elapsed"
        );
        assert_eq!(
            stat_calls(),
            before + 1,
            "the poll past the interval must stat the file exactly once"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_vanished_file_is_not_a_reload_but_its_return_is() {
        let dir = temp_dir("vanish");
        let p = dir.join("config.toml");
        std::fs::write(&p, "theme = \"itasha-corp\"\n").unwrap();
        let mut w = ConfigWatcher::watching(p.clone());

        std::fs::remove_file(&p).unwrap();
        let t = Instant::now() + POLL_INTERVAL * 4;
        assert!(
            w.poll(t).is_none(),
            "a deleted config must never trigger a reload (keep last-known-good)"
        );

        // The delete-then-recreate an editor performs still lands the real edit.
        write_later(&p, "theme = \"ghost-paper\"\n");
        assert_eq!(
            w.poll(t + POLL_INTERVAL * 4),
            Some(p.clone()),
            "the recreated file is a change and must be reloaded"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mark_self_written_suppresses_our_own_write() {
        let dir = temp_dir("selfwrite");
        let p = dir.join("config.toml");
        std::fs::write(&p, "theme = \"itasha-corp\"\n").unwrap();
        let mut w = ConfigWatcher::watching(p.clone());

        write_later(&p, "theme = \"ghost-paper\"\n# app's own save\n");
        w.mark_self_written();

        assert!(
            w.poll(Instant::now() + POLL_INTERVAL * 4).is_none(),
            "the app's own save must not bounce back as an external edit"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
