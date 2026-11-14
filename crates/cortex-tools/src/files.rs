//! File snapshots and the session's table of seen files
//! (`docs/architecture/05-tools.md`, "File snapshots").
//!
//! When a tool reads or writes a file it records the file's size,
//! modification time and content hash in the [`FileTable`]. Two things are
//! built on the table:
//!
//! - **The staleness guard** of the editing tools: a file may be changed
//!   only if it has been read in this session and is still what was read
//!   ([`FileTable::staleness`]).
//! - **Changed-only re-reads**: the table also remembers which lines of a
//!   file the model has been shown in the current context epoch, so that a
//!   second read can say "unchanged" or send a diff
//!   (`docs/architecture/15-efficiency.md`).

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "taking a snapshot reads the file; async callers run it on the blocking pool"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use cortex_base::{AbsPath, ContentHash, ContentHasher};

/// What a file was when a tool read or wrote it: enough to tell, later,
/// whether it is still the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    /// Its size in bytes.
    pub size: u64,
    /// When it was last modified, where the file system says.
    pub modified: Option<SystemTime>,
    /// The hash of its content. This is what decides: the size and the time
    /// are recorded for the session's carried state and for display.
    pub hash: ContentHash,
}

impl FileSnapshot {
    /// Takes the snapshot of the file at `path` by reading all of it.
    ///
    /// `path` is a real location from the resolver
    /// ([`ResolvedPath::to_fs_path`](cortex_base::ResolvedPath::to_fs_path)).
    pub fn of_file(path: &Path) -> io::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        let modified = file.metadata()?.modified().ok();
        let mut hasher = ContentHasher::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut size = 0u64;
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    hasher.update(&buffer[..count]);
                    size += count as u64;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(Self {
            size,
            modified,
            hash: hasher.finish(),
        })
    }

    /// The snapshot of content that is at hand, with the modification time
    /// the caller read beside it.
    #[must_use]
    pub fn of_content(content: &[u8], modified: Option<SystemTime>) -> Self {
        Self {
            size: content.len() as u64,
            modified,
            hash: ContentHash::of(content),
        }
    }
}

/// A set of line numbers, counted from 1, kept as ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineRanges {
    /// Inclusive ranges, in order, with a gap of at least one line between
    /// two of them.
    ranges: Vec<(u64, u64)>,
}

impl LineRanges {
    /// No lines.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The lines `first` to `last`, both included. No lines if `first` is 0
    /// or after `last`.
    #[must_use]
    pub fn of(first: u64, last: u64) -> Self {
        let mut ranges = Self::new();
        ranges.insert(first, last);
        ranges
    }

    /// Adds the lines `first` to `last`, both included.
    pub fn insert(&mut self, first: u64, last: u64) {
        if first == 0 || first > last {
            return;
        }
        let (mut first, mut last) = (first, last);
        let mut merged = Vec::with_capacity(self.ranges.len() + 1);
        let mut placed = false;
        for &(start, end) in &self.ranges {
            if end.saturating_add(1) < first {
                merged.push((start, end));
            } else if last.saturating_add(1) < start {
                if !placed {
                    merged.push((first, last));
                    placed = true;
                }
                merged.push((start, end));
            } else {
                first = first.min(start);
                last = last.max(end);
            }
        }
        if !placed {
            merged.push((first, last));
        }
        self.ranges = merged;
    }

    /// Whether every line from `first` to `last` is in the set. An empty
    /// range is covered by any set.
    #[must_use]
    pub fn covers(&self, first: u64, last: u64) -> bool {
        if first > last {
            return true;
        }
        self.ranges
            .iter()
            .any(|&(start, end)| start <= first && last <= end)
    }

    /// Whether the set has no lines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The ranges, in order, as first and last line.
    pub fn iter(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.ranges.iter().copied()
    }

    /// How many lines are in the set.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.ranges
            .iter()
            .map(|&(start, end)| end - start + 1)
            .sum()
    }
}

impl fmt::Display for LineRanges {
    /// `1-120`, `7`, or `1-50, 80-120`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, &(start, end)) in self.ranges.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            if start == end {
                write!(f, "{start}")?;
            } else {
                write!(f, "{start}-{end}")?;
            }
        }
        Ok(())
    }
}

/// What the model has been shown of one file in the current context epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The version of the file the lines belong to.
    pub hash: ContentHash,
    /// The lines of that version the model has been given.
    pub lines: LineRanges,
    /// How many lines that version has.
    pub total_lines: u64,
    /// The turn in which it was last shown, counted from 1. 0 when the
    /// caller did not say.
    pub turn: u32,
    /// Whether that version's content is in the content store, so that a
    /// later read can diff against it.
    pub kept: bool,
}

/// Whether a file may be changed by an editing tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staleness {
    /// No tool has read or written the file in this session. The model has
    /// not seen it, and must read it first.
    Unseen,
    /// The file is what a tool last read or wrote.
    Current,
    /// The file has changed since. The model must read it again.
    Stale,
}

/// The session's table of seen files.
///
/// One table belongs to one session and is shared by its tool calls, which
/// may run at the same time on the blocking pool, so it locks. Every method
/// holds the lock for a map lookup and no longer.
#[derive(Debug, Default)]
pub struct FileTable {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    epoch: u64,
    files: BTreeMap<AbsPath, Record>,
    /// Files a command changed that no tool has a snapshot of: what they
    /// hold now was not read.
    changed: BTreeSet<AbsPath>,
}

#[derive(Debug)]
struct Record {
    snapshot: FileSnapshot,
    /// What the model was shown, and in which epoch.
    shown: Option<(u64, Shown)>,
    /// A tool read it in this session.
    read: bool,
    /// A tool, or a command, changed it in this session.
    changed: bool,
}

/// A file the session's tools have read or changed, as compaction carries
/// it across (`docs/architecture/07-sessions-and-context.md`, "Compaction").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenFile {
    /// The file.
    pub path: AbsPath,
    /// What a tool last found or left it as. `None` for a file a command
    /// changed and no tool has read since.
    pub snapshot: Option<FileSnapshot>,
    /// A tool read it.
    pub read: bool,
    /// A tool or a command changed it.
    pub changed: bool,
}

impl FileTable {
    /// An empty table, in its first context epoch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Every change is one insert or one field, so a panic elsewhere
        // while the lock was held cannot have left an entry half-written.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a new context epoch: after compaction, or a change of model.
    /// The model no longer has what it was shown, so the next read of any
    /// file is in full. The snapshots stay, because the staleness guard is
    /// about the session, not the epoch.
    pub fn begin_epoch(&self) {
        self.lock().epoch += 1;
    }

    /// Records that a tool read `path`, and what it showed the model.
    pub fn record_read(&self, path: &AbsPath, snapshot: FileSnapshot, shown: Shown) {
        let mut inner = self.lock();
        let epoch = inner.epoch;
        let changed = inner.changed.remove(path)
            || inner.files.get(path).is_some_and(|record| record.changed);
        inner.files.insert(
            path.clone(),
            Record {
                snapshot,
                shown: Some((epoch, shown)),
                read: true,
                changed,
            },
        );
    }

    /// Records that a tool wrote `path`, leaving it as `snapshot`. What the
    /// model was shown of the old content no longer describes the file, and
    /// is dropped.
    pub fn record_write(&self, path: &AbsPath, snapshot: FileSnapshot) {
        let mut inner = self.lock();
        inner.changed.remove(path);
        let read = inner.files.get(path).is_some_and(|record| record.read);
        inner.files.insert(
            path.clone(),
            Record {
                snapshot,
                shown: None,
                read,
                changed: true,
            },
        );
    }

    /// Notes that a command changed `path` (created, modified or deleted
    /// it). Nothing is known of what it holds now, so the snapshot a tool
    /// took of it stays as it was, and the staleness guard sees the change
    /// for itself when the file is next edited.
    pub fn note_changed(&self, path: &AbsPath) {
        let mut inner = self.lock();
        match inner.files.get_mut(path) {
            Some(record) => record.changed = true,
            None => {
                inner.changed.insert(path.clone());
            }
        }
    }

    /// Forgets what a tool recorded of a file: the loop calls it for the
    /// reads of a run it discarded, which the model was never shown.
    pub fn forget(&self, path: &AbsPath) {
        self.lock().files.remove(path);
    }

    /// Forgets every file: the session's history changed under it (a
    /// rewind), and what it recorded may be of turns the history no longer
    /// has. A file is then read again before it is changed. Unlike
    /// [`begin_epoch`](Self::begin_epoch), which compaction starts, this
    /// drops the snapshots too: after compaction the model still saw the
    /// files in this branch's history, and after a rewind it may not have.
    pub fn forget_all(&self) {
        let mut inner = self.lock();
        inner.files.clear();
        inner.changed.clear();
        inner.epoch += 1;
    }

    /// The snapshot last recorded for `path`.
    #[must_use]
    pub fn snapshot(&self, path: &AbsPath) -> Option<FileSnapshot> {
        self.lock()
            .files
            .get(path)
            .map(|record| record.snapshot.clone())
    }

    /// What the model has been shown of `path` in the current epoch.
    #[must_use]
    pub fn shown(&self, path: &AbsPath) -> Option<Shown> {
        let inner = self.lock();
        let (epoch, shown) = inner.files.get(path)?.shown.as_ref()?;
        (*epoch == inner.epoch).then(|| shown.clone())
    }

    /// The staleness guard: whether the file at `path`, whose content now
    /// hashes to `on_disk`, is what a tool last read or wrote.
    #[must_use]
    pub fn staleness(&self, path: &AbsPath, on_disk: &ContentHash) -> Staleness {
        match self.lock().files.get(path) {
            None => Staleness::Unseen,
            Some(record) if record.snapshot.hash == *on_disk => Staleness::Current,
            Some(_) => Staleness::Stale,
        }
    }

    /// Every file the session's tools have read or written, with its last
    /// snapshot, in path order.
    #[must_use]
    pub fn seen(&self) -> Vec<(AbsPath, FileSnapshot)> {
        self.lock()
            .files
            .iter()
            .map(|(path, record)| (path.clone(), record.snapshot.clone()))
            .collect()
    }

    /// Every file the session's tools have read, or its tools or commands
    /// have changed, in path order, with what is known of each. Compaction
    /// carries this list across.
    #[must_use]
    pub fn carried(&self) -> Vec<SeenFile> {
        let inner = self.lock();
        let mut files: Vec<SeenFile> = inner
            .files
            .iter()
            .map(|(path, record)| SeenFile {
                path: path.clone(),
                snapshot: Some(record.snapshot.clone()),
                read: record.read,
                changed: record.changed,
            })
            .chain(inner.changed.iter().map(|path| SeenFile {
                path: path.clone(),
                snapshot: None,
                read: false,
                changed: true,
            }))
            .collect();
        files.sort_by(|one, other| one.path.cmp(&other.path));
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortex_base::PathStyle;

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text, PathStyle::Posix).expect("an absolute path")
    }

    fn shown(content: &[u8], first: u64, last: u64) -> Shown {
        Shown {
            hash: ContentHash::of(content),
            lines: LineRanges::of(first, last),
            total_lines: last,
            turn: 1,
            kept: true,
        }
    }

    #[test]
    fn ranges_merge_when_they_touch_or_overlap() {
        let mut lines = LineRanges::new();
        assert!(lines.is_empty());
        lines.insert(10, 20);
        lines.insert(30, 40);
        assert_eq!(lines.to_string(), "10-20, 30-40");
        lines.insert(21, 29);
        assert_eq!(lines.to_string(), "10-40");
        lines.insert(1, 3);
        lines.insert(50, 50);
        assert_eq!(lines.to_string(), "1-3, 10-40, 50");
        lines.insert(4, 9);
        assert_eq!(lines.to_string(), "1-40, 50");
        lines.insert(35, 60);
        assert_eq!(lines.to_string(), "1-60");
        assert_eq!(lines.count(), 60);
        assert_eq!(lines.iter().collect::<Vec<_>>(), vec![(1, 60)]);
    }

    #[test]
    fn nothing_is_added_for_a_range_that_has_no_lines() {
        let mut lines = LineRanges::of(5, 4);
        assert!(lines.is_empty());
        lines.insert(0, 3);
        assert!(lines.is_empty());
        assert_eq!(lines.to_string(), "");
    }

    #[test]
    fn a_range_is_covered_only_by_one_unbroken_range() {
        let mut lines = LineRanges::of(1, 10);
        lines.insert(20, 30);
        assert!(lines.covers(1, 10));
        assert!(lines.covers(3, 7));
        assert!(lines.covers(20, 30));
        assert!(!lines.covers(5, 25), "lines 11 to 19 are missing");
        assert!(!lines.covers(10, 11));
        assert!(!lines.covers(31, 31));
        assert!(lines.covers(8, 7), "an empty range needs no lines");
        assert!(LineRanges::new().covers(1, 0));
        assert!(!LineRanges::new().covers(1, 1));
    }

    #[test]
    fn inserting_ranges_in_any_order_gives_the_same_set() {
        use cortex_base::{Rng, SeededRng};
        for seed in 0..200 {
            let mut rng = SeededRng::new(seed);
            let ranges: Vec<(u64, u64)> = (0..rng.below(6) + 1)
                .map(|_| {
                    let first = rng.below(40) + 1;
                    (first, first + rng.below(8))
                })
                .collect();
            let mut forward = LineRanges::new();
            for &(first, last) in &ranges {
                forward.insert(first, last);
            }
            let mut backward = LineRanges::new();
            for &(first, last) in ranges.iter().rev() {
                backward.insert(first, last);
            }
            assert_eq!(forward, backward, "seed {seed}: {ranges:?}");
            for line in 1..=50 {
                let expected = ranges
                    .iter()
                    .any(|&(first, last)| first <= line && line <= last);
                assert_eq!(
                    forward.covers(line, line),
                    expected,
                    "seed {seed}, line {line}"
                );
            }
            let pairs: Vec<_> = forward.iter().collect();
            for pair in pairs.windows(2) {
                assert!(
                    pair[0].1 + 1 < pair[1].0,
                    "seed {seed}: {pairs:?} are apart"
                );
            }
        }
    }

    #[test]
    fn what_compaction_carries_says_what_was_read_and_what_changed() {
        let table = FileTable::new();
        let (read, written, both, by_command, deleted) = (
            path("/work/a.rs"),
            path("/work/b.rs"),
            path("/work/c.rs"),
            path("/work/d.rs"),
            path("/work/e.rs"),
        );
        let snapshot = FileSnapshot::of_content(b"one\n", None);
        table.record_read(&read, snapshot.clone(), shown(b"one\n", 1, 1));
        table.record_write(&written, snapshot.clone());
        table.record_read(&both, snapshot.clone(), shown(b"one\n", 1, 1));
        table.record_write(&both, snapshot.clone());
        table.note_changed(&by_command);
        // A read the loop discarded is not carried.
        table.record_read(&deleted, snapshot.clone(), shown(b"one\n", 1, 1));
        table.forget(&deleted);
        // A read after a command changed a file keeps it changed.
        table.note_changed(&read);

        let carried: Vec<(String, bool, bool, bool)> = table
            .carried()
            .into_iter()
            .map(|file| {
                (
                    file.path.to_string(),
                    file.read,
                    file.changed,
                    file.snapshot.is_some(),
                )
            })
            .collect();
        assert_eq!(
            carried,
            [
                ("/work/a.rs".to_owned(), true, true, true),
                ("/work/b.rs".to_owned(), false, true, true),
                ("/work/c.rs".to_owned(), true, true, true),
                ("/work/d.rs".to_owned(), false, true, false),
            ]
        );
        table.begin_epoch();
        assert_eq!(table.carried().len(), 4, "an epoch keeps the list");
        table.forget_all();
        assert!(table.carried().is_empty(), "a change of history does not");
    }

    #[test]
    fn the_guard_knows_unseen_current_and_stale() {
        let table = FileTable::new();
        let file = path("/work/src/main.rs");
        let original = FileSnapshot::of_content(b"one\n", None);
        assert_eq!(table.staleness(&file, &original.hash), Staleness::Unseen);

        table.record_read(&file, original.clone(), shown(b"one\n", 1, 1));
        assert_eq!(table.staleness(&file, &original.hash), Staleness::Current);
        assert_eq!(
            table.staleness(&file, &ContentHash::of(b"two\n")),
            Staleness::Stale
        );
        assert_eq!(table.snapshot(&file), Some(original));

        let written = FileSnapshot::of_content(b"two\n", None);
        table.record_write(&file, written.clone());
        assert_eq!(table.staleness(&file, &written.hash), Staleness::Current);
        assert_eq!(table.shown(&file), None, "a write drops what was shown");

        table.forget(&file);
        assert_eq!(table.staleness(&file, &written.hash), Staleness::Unseen);
    }

    #[test]
    fn a_new_epoch_forgets_what_was_shown_and_keeps_the_snapshots() {
        let table = FileTable::new();
        let file = path("/work/notes.md");
        let snapshot = FileSnapshot::of_content(b"a\nb\n", None);
        table.record_read(&file, snapshot.clone(), shown(b"a\nb\n", 1, 2));
        assert_eq!(
            table.shown(&file).map(|shown| shown.lines.to_string()),
            Some("1-2".to_owned())
        );

        table.begin_epoch();
        assert_eq!(table.shown(&file), None);
        assert_eq!(table.staleness(&file, &snapshot.hash), Staleness::Current);
        assert_eq!(table.seen(), vec![(file.clone(), snapshot.clone())]);

        table.record_read(&file, snapshot, shown(b"a\nb\n", 1, 1));
        assert!(table.shown(&file).is_some(), "shown again in the new epoch");
    }

    #[test]
    fn forgetting_every_file_drops_the_snapshots_too() {
        let table = FileTable::new();
        let file = path("/work/notes.md");
        let snapshot = FileSnapshot::of_content(b"a\n", None);
        table.record_read(&file, snapshot.clone(), shown(b"a\n", 1, 1));
        table.forget_all();
        assert_eq!(table.shown(&file), None);
        assert_eq!(table.staleness(&file, &snapshot.hash), Staleness::Unseen);
        assert!(table.seen().is_empty());
    }

    #[test]
    fn seen_files_come_back_in_path_order() {
        let table = FileTable::new();
        for name in ["/work/b.rs", "/work/a.rs", "/work/src/z.rs"] {
            table.record_write(&path(name), FileSnapshot::of_content(name.as_bytes(), None));
        }
        let names: Vec<String> = table
            .seen()
            .into_iter()
            .map(|(path, _)| path.to_string())
            .collect();
        assert_eq!(names, ["/work/a.rs", "/work/b.rs", "/work/src/z.rs"]);
    }
}
