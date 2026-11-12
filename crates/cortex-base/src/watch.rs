//! Noticing that a loaded file has changed, without a watcher and without a
//! timer (`docs/architecture/08-configuration.md`, "Reloading without a
//! restart").
//!
//! The harness remembers a [`FileStamp`] for each file it loaded: its
//! modification time and size. At the moments that page lists (the terminal
//! regains focus, a key press, the start of a turn) it takes the stamps
//! again and compares. That is a handful of `stat` calls and nothing in
//! between, so an idle harness costs nothing.
//!
//! [`WatchList`] keeps the stamps and does the comparing.

#![allow(
    clippy::disallowed_methods,
    reason = "a stat call per watched file; async callers run the check on the blocking pool"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::clock::SharedClock;
use crate::time::UtcTime;

/// How close to "now" a file's modification time can be for a later change
/// to go unseen: file systems record times in steps as coarse as two
/// seconds, so a second write inside the same step leaves the time as it
/// was.
const UNSETTLED_WINDOW: Duration = Duration::from_secs(2);

/// What a file looked like at one moment: enough to tell, later, that it
/// has changed.
///
/// Two stamps of an untouched file are equal. A stamp differs after the file
/// is written, replaced, created, removed, or turned into a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStamp {
    /// Nothing is there.
    Missing,
    /// Something is there and cannot be looked at: no permission, or a
    /// broken link.
    Unreadable(std::io::ErrorKind),
    /// A file or a directory.
    Present {
        /// When it was last modified. `None` where the file system does not
        /// say.
        modified: Option<SystemTime>,
        /// Its size in bytes.
        len: u64,
        /// Whether it is a directory.
        is_dir: bool,
        /// Which file this is and when its metadata last changed, where the
        /// OS says (Unix). It tells a replaced file from the one it replaced
        /// even when the time and the size are the same.
        identity: Option<(u64, i64, i64)>,
    },
}

impl FileStamp {
    /// The stamp of `path` now. Symbolic links are followed: the stamp is of
    /// the file a read would open. It never fails: a missing or unreadable
    /// file is a state, and a change of state is a change.
    #[must_use]
    pub fn of(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(metadata) => Self::Present {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                is_dir: metadata.is_dir(),
                identity: identity(&metadata),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::Missing,
            Err(error) => Self::Unreadable(error.kind()),
        }
    }

    /// Whether the file existed when the stamp was taken.
    #[must_use]
    pub fn exists(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    fn modified(&self) -> Option<SystemTime> {
        match self {
            Self::Present { modified, .. } => *modified,
            Self::Missing | Self::Unreadable(_) => None,
        }
    }
}

#[cfg(unix)]
fn identity(metadata: &std::fs::Metadata) -> Option<(u64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.ino(), metadata.ctime(), metadata.ctime_nsec()))
}

#[cfg(not(unix))]
fn identity(_: &std::fs::Metadata) -> Option<(u64, i64, i64)> {
    None
}

/// A [`FileStamp`] and the moment it was taken. Made by
/// [`WatchList::stamp`]. The moment matters because a file written just
/// before it is not trusted yet (see [`WatchList`]), however long the
/// reading and parsing between the stamp and its registration took.
#[derive(Debug, Clone)]
pub struct TimedStamp {
    stamp: FileStamp,
    taken: UtcTime,
}

impl TimedStamp {
    /// What the file looked like.
    #[must_use]
    pub fn stamp(&self) -> &FileStamp {
        &self.stamp
    }
}

/// The files the harness loaded, and what each looked like when it did.
///
/// # Use
///
/// Take the stamp **before** reading a file, and register both:
///
/// 1. `let stamp = list.stamp(path);`
/// 2. read and parse the file;
/// 3. `list.watch_with(path, stamp);`
///
/// A change that lands between steps 1 and 2 is then reported by the next
/// check, not lost. [`watch`](Self::watch) does step 1 itself, for a file
/// that is registered before it is read.
///
/// # A file that was just written
///
/// A file modified within two seconds of its stamp is **unsettled**: a
/// second write could land in the same step of the file system's clock and
/// leave the time and the size as they were. Such a file is reported as
/// changed by every check until two seconds have passed, so that the second
/// write cannot be missed. The cost is a few reloads of a file that did not
/// change again, so a caller compares what it loaded before telling the user
/// that anything changed.
#[derive(Debug)]
pub struct WatchList {
    clock: SharedClock,
    entries: Vec<Entry>,
    /// The clock's monotonic time at the last check.
    last_check: Option<Duration>,
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    stamp: FileStamp,
    /// The date and time when `stamp` was taken.
    taken: UtcTime,
}

impl WatchList {
    /// An empty list. `clock` is used to hold checks apart
    /// ([`check_at_most_every`](Self::check_at_most_every)) and to tell a
    /// file that was only just written.
    #[must_use]
    pub fn new(clock: SharedClock) -> Self {
        Self {
            clock,
            entries: Vec::new(),
            last_check: None,
        }
    }

    /// What `path` looks like now, and when that was: to be taken before a
    /// file is read and handed to [`watch_with`](Self::watch_with) after.
    #[must_use]
    pub fn stamp(&self, path: &Path) -> TimedStamp {
        TimedStamp {
            // The time first: if the file is written in between, the stamp
            // is the newer of the two and the file counts as fresh.
            taken: self.clock.now(),
            stamp: FileStamp::of(path),
        }
    }

    /// Watches `path`, as it is now. A path already on the list gets a fresh
    /// stamp.
    pub fn watch(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        let stamp = self.stamp(&path);
        self.watch_with(path, stamp);
    }

    /// Watches `path` with a stamp taken earlier, before the file was read.
    /// A file that does not exist can be watched: its creation is a change.
    pub fn watch_with(&mut self, path: impl Into<PathBuf>, stamp: TimedStamp) {
        let path = path.into();
        let TimedStamp { stamp, taken } = stamp;
        match self.entries.iter_mut().find(|entry| entry.path == path) {
            Some(entry) => {
                entry.stamp = stamp;
                entry.taken = taken;
            }
            None => self.entries.push(Entry { path, stamp, taken }),
        }
    }

    /// Stops watching `path`.
    pub fn unwatch(&mut self, path: &Path) {
        self.entries.retain(|entry| entry.path != path);
    }

    /// Stops watching everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The watched paths, in the order they were added.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.entries.iter().map(|entry| entry.path.as_path())
    }

    /// How many paths are watched.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is watched.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks at every watched file and returns the ones that changed since
    /// they were last stamped, in the order they were added. Each is stamped
    /// afresh, so a change is reported once, apart from the unsettled files
    /// described on [`WatchList`].
    pub fn check(&mut self) -> Vec<PathBuf> {
        let now = self.clock.now();
        self.last_check = Some(self.clock.monotonic());
        let mut changed = Vec::new();
        for entry in &mut self.entries {
            let current = FileStamp::of(&entry.path);
            if current != entry.stamp || unsettled(&entry.stamp, entry.taken) {
                changed.push(entry.path.clone());
            }
            entry.stamp = current;
            entry.taken = now;
        }
        changed
    }

    /// [`check`](Self::check), unless a check ran less than `interval` ago,
    /// in which case nothing is looked at and `None` is returned. For the
    /// check on a key press, which runs at most once a second.
    pub fn check_at_most_every(&mut self, interval: Duration) -> Option<Vec<PathBuf>> {
        let now = self.clock.monotonic();
        match self.last_check {
            Some(last) if now.saturating_sub(last) < interval => None,
            _ => Some(self.check()),
        }
    }
}

/// Whether a file was modified so shortly before (or, by a skewed clock,
/// after) its stamp was taken that another write could hide behind the same
/// modification time.
fn unsettled(stamp: &FileStamp, taken: UtcTime) -> bool {
    let Some(modified) = stamp.modified().and_then(UtcTime::from_system) else {
        return false;
    };
    let apart = taken
        .duration_since(modified)
        .or_else(|| modified.duration_since(taken));
    apart.is_some_and(|apart| apart < UNSETTLED_WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(modified_secs: u64, len: u64) -> FileStamp {
        FileStamp::Present {
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_secs)),
            len,
            is_dir: false,
            identity: None,
        }
    }

    fn at(secs: i64) -> UtcTime {
        UtcTime::from_unix(secs, 0).unwrap()
    }

    #[test]
    fn stamps_compare_by_time_size_and_kind() {
        assert_eq!(present(100, 5), present(100, 5));
        assert_ne!(present(100, 5), present(101, 5));
        assert_ne!(present(100, 5), present(100, 6));
        assert_ne!(present(100, 5), FileStamp::Missing);
        assert_ne!(
            FileStamp::Missing,
            FileStamp::Unreadable(std::io::ErrorKind::PermissionDenied)
        );
        assert!(present(100, 5).exists());
        assert!(!FileStamp::Missing.exists());
    }

    #[test]
    fn a_file_written_within_two_seconds_of_its_stamp_is_unsettled() {
        assert!(unsettled(&present(1000, 5), at(1000)));
        assert!(unsettled(&present(1000, 5), at(1001)));
        assert!(!unsettled(&present(1000, 5), at(1002)));
        assert!(!unsettled(&present(1000, 5), at(5000)));
        // A file-system clock a little ahead of ours.
        assert!(unsettled(&present(1001, 5), at(1000)));
        assert!(!unsettled(&present(1002, 5), at(1000)));
        assert!(!unsettled(&FileStamp::Missing, at(1000)));
        let no_time = FileStamp::Present {
            modified: None,
            len: 1,
            is_dir: false,
            identity: None,
        };
        assert!(!unsettled(&no_time, at(1000)));
    }
}
