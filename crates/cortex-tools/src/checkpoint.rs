//! The seam between the editing tools and the session's checkpoint store
//! (`docs/architecture/05-tools.md`, "Checkpoints";
//! `docs/architecture/07-sessions-and-context.md`, "Snapshots and undo").
//!
//! Before a tool changes or creates a file, what the file held (or the fact
//! that it did not exist) is kept, so that the turn can be undone. That is
//! what lets edits apply without asking. The store is the session's
//! (`cortex-session`), which this crate cannot see, so the tools reach it
//! through [`Checkpoints`]. The core fills the seam with the checkpoint of
//! the tool round that is running.
//!
//! **The tool hands over the bytes it read.** [`Checkpoints::before`] is
//! given the content the tool based its change on, not a path to read
//! again: the file is read once, and what is kept for undo is exactly what
//! the tool replaced, with no moment between two reads in which the file
//! could have become something else.
//!
//! [`Editing`] is what the two tools that write files are made with: the
//! seam, the clock their writes wait on, and the largest file they change.

use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::{AbsPath, ContentHash, SharedClock};

use crate::files::FileSnapshot;

/// The largest file `write` and `edit` change: 16 MiB, which is the most a
/// checkpoint keeps (`cortex-session`'s `KEEP_LIMIT`).
///
/// A change to a larger file could not be undone, and a change that cannot
/// be undone must not apply without asking. The limit also bounds what one
/// call reads into memory and hashes.
pub const EDIT_LIMIT: u64 = 16 * 1024 * 1024;

/// Why a checkpoint could not be taken. The tool then leaves the file as
/// it is: a change that could not be undone is not made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CheckpointError(String);

impl CheckpointError {
    /// An error with the store's own account of what went wrong.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Where the editing tools record what they change, for undo.
///
/// Both methods block, and are called on the blocking pool. They are
/// called in pairs, in this order, for each file a tool changes: `before`,
/// then the write, then `after`.
///
/// **A write that fails is reported too:** `after` is then given what the
/// file still is, which is what `before` was given. The record of the round
/// is so never left to be completed by a later look at the file, which
/// could find a change somebody else made in the meantime and take it for
/// the round's.
pub trait Checkpoints: Send + Sync {
    /// Keeps what `file` held before a tool changes it: `content` is its
    /// bytes as the tool read them, or `None` for a file that does not
    /// exist and is about to be created.
    ///
    /// `file` is a real location from the path resolver, never a link. Only
    /// the first record of a file in a tool round counts: that is the state
    /// the round started from.
    ///
    /// # Errors
    ///
    /// When the content cannot be kept. The tool does not change the file.
    fn before(&self, file: &AbsPath, content: Option<&[u8]>) -> Result<(), CheckpointError>;

    /// Records what the tool left at `file`: the file as it now is, or
    /// `None` when there is no file (one that was to be created, and could
    /// not be).
    fn after(&self, file: &AbsPath, left: Option<&FileSnapshot>);
}

/// What `write` and `edit` are made with.
#[derive(Clone)]
pub struct Editing {
    pub(crate) checkpoints: Arc<dyn Checkpoints>,
    pub(crate) clock: SharedClock,
    pub(crate) limit: u64,
    pub(crate) sync: bool,
}

impl Editing {
    /// Tools that record their changes in `checkpoints`, and take the waits
    /// between attempts to replace a file in use (Windows) from `clock`.
    #[must_use]
    pub fn new(checkpoints: Arc<dyn Checkpoints>, clock: SharedClock) -> Self {
        Self {
            checkpoints,
            clock,
            limit: EDIT_LIMIT,
            sync: true,
        }
    }

    /// Sets the largest file the tools change, in place of [`EDIT_LIMIT`]:
    /// the most the session's checkpoint store keeps.
    #[must_use]
    pub fn with_limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }

    /// Sets whether new content is flushed to the disk before it takes the
    /// file's name. On by default: these are the user's files. A test turns
    /// it off, because a flush takes as long as the disk takes.
    #[must_use]
    pub fn with_sync(mut self, sync: bool) -> Self {
        self.sync = sync;
        self
    }
}

impl std::fmt::Debug for Editing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editing")
            .field("limit", &self.limit)
            .field("sync", &self.sync)
            .finish_non_exhaustive()
    }
}

/// One file a tool changed, as [`MemoryCheckpoints`] keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The file.
    pub file: AbsPath,
    /// What it held before the first change. `None`: it did not exist.
    pub before: Option<Vec<u8>>,
    /// What the last change left. `None`: a change was announced and no
    /// result has been recorded for it yet.
    pub after: Option<FileSnapshot>,
}

/// Checkpoints kept in memory, for a session with no directory: tests of
/// the tools and of the loop, and one-off runs that store nothing.
///
/// Like the session's own, it lists a file only while the file differs
/// from what it was: a change that is reported as having left the file as
/// it was (a write that failed) is forgotten.
#[derive(Debug, Clone, Default)]
pub struct MemoryCheckpoints {
    changes: Arc<Mutex<Vec<Change>>>,
}

impl MemoryCheckpoints {
    /// No changes yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every file changed, in the order each was first changed.
    #[must_use]
    pub fn changes(&self) -> Vec<Change> {
        self.lock().clone()
    }

    /// Forgets every change, as the end of a tool round does once its
    /// checkpoint is written.
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Change>> {
        // Every change is one push or one field.
        self.changes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Checkpoints for MemoryCheckpoints {
    fn before(&self, file: &AbsPath, content: Option<&[u8]>) -> Result<(), CheckpointError> {
        let mut changes = self.lock();
        if !changes.iter().any(|change| change.file.key() == file.key()) {
            changes.push(Change {
                file: file.clone(),
                before: content.map(<[u8]>::to_vec),
                after: None,
            });
        }
        Ok(())
    }

    fn after(&self, file: &AbsPath, left: Option<&FileSnapshot>) {
        let mut changes = self.lock();
        let Some(at) = changes
            .iter()
            .position(|change| change.file.key() == file.key())
        else {
            return;
        };
        let unchanged = match (&changes[at].before, left) {
            (None, None) => true,
            (Some(before), Some(left)) => ContentHash::of(before) == left.hash,
            _ => false,
        };
        if unchanged {
            changes.remove(at);
        } else {
            changes[at].after = left.cloned();
        }
    }
}

#[cfg(test)]
mod tests {
    use cortex_base::PathStyle;

    use super::*;

    #[test]
    fn the_first_state_of_a_file_is_the_one_kept() {
        let checkpoints = MemoryCheckpoints::new();
        let file = AbsPath::parse("/work/a.rs", PathStyle::Posix).unwrap();
        let other = AbsPath::parse("/work/new.rs", PathStyle::Posix).unwrap();
        checkpoints.before(&file, Some(b"one")).unwrap();
        checkpoints.after(&file, Some(&FileSnapshot::of_content(b"two", None)));
        checkpoints.before(&file, Some(b"two")).unwrap();
        checkpoints.after(&file, Some(&FileSnapshot::of_content(b"three", None)));
        checkpoints.before(&other, None).unwrap();

        let changes = checkpoints.changes();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].before.as_deref(), Some(&b"one"[..]));
        assert_eq!(
            changes[0].after,
            Some(FileSnapshot::of_content(b"three", None))
        );
        assert_eq!(changes[1].before, None);
        assert_eq!(changes[1].after, None, "announced, and not yet written");

        // A write that failed left the file as it was: nothing changed, so
        // nothing is listed. The same for a file that was not created.
        checkpoints.after(&other, None);
        assert_eq!(checkpoints.changes().len(), 1);
        let third = AbsPath::parse("/work/c.rs", PathStyle::Posix).unwrap();
        checkpoints.before(&third, Some(b"kept")).unwrap();
        checkpoints.after(&third, Some(&FileSnapshot::of_content(b"kept", None)));
        assert_eq!(checkpoints.changes().len(), 1);
        // A file that is changed and then changed back is not a change.
        checkpoints.before(&file, Some(b"three")).unwrap();
        checkpoints.after(&file, Some(&FileSnapshot::of_content(b"one", None)));
        assert!(checkpoints.changes().is_empty());
        checkpoints.before(&file, Some(b"one")).unwrap();
        checkpoints.clear();
        assert!(checkpoints.changes().is_empty());
    }

    #[test]
    fn the_tools_start_with_the_limit_a_checkpoint_keeps() {
        let editing = Editing::new(
            Arc::new(MemoryCheckpoints::new()),
            Arc::new(cortex_base::FakeClock::default()),
        );
        assert_eq!(editing.limit, EDIT_LIMIT);
        assert!(editing.sync);
        let editing = editing.with_limit(10).with_sync(false);
        assert_eq!(editing.limit, 10);
        assert!(!editing.sync);
        assert_eq!(
            CheckpointError::new("the disk is full").to_string(),
            "the disk is full"
        );
    }
}
