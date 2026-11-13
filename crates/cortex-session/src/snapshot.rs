//! Per-turn snapshots, and putting files back
//! (`docs/architecture/07-sessions-and-context.md`, "Snapshots and undo").
//!
//! Before a tool changes a file, its content as it was (or the fact that it
//! did not exist) is kept in the checkpoint store. A [`RoundCheckpoint`]
//! gathers what one tool round changed, and becomes the round's
//! `checkpoint` entry. The checkpoints of a turn are its snapshot.
//!
//! A rewind is planned first ([`RewindPlan`]) and then carried out. The
//! plan says, for each file the undone rounds changed, what it will be put
//! back to, and whether that is safe: a file someone edited after the
//! harness left it is not overwritten unless the user says so, and a file
//! whose earlier content was too large to keep cannot be put back at all.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::PathBuf;

use cortex_base::{AbsPath, ContentHasher, PathStyle};
use cortex_protocol::ids::TurnId;

use crate::entry::{Body, Checkpoint, EntryId, FileChange, FileVersion, RewindReason};
use crate::error::{Result, SessionError};
use crate::store::{DiskStore, KEEP_LIMIT};

/// What one tool round changed, gathered as the tools run.
#[derive(Debug, Clone)]
pub struct RoundCheckpoint {
    root: AbsPath,
    limit: u64,
    files: Vec<Touched>,
    by_path: HashMap<String, usize>,
}

/// One file the round touched.
#[derive(Debug, Clone)]
struct Touched {
    file: AbsPath,
    path: String,
    before: Option<FileVersion>,
    /// What the round left, once it is known.
    after: Option<Option<FileVersion>>,
}

impl RoundCheckpoint {
    /// A round in the workspace at `root`, which has changed nothing yet.
    #[must_use]
    pub fn new(root: AbsPath) -> Self {
        Self::with_limit(root, KEEP_LIMIT)
    }

    /// A round whose files over `limit` bytes are recorded by hash and size
    /// only, in place of [`KEEP_LIMIT`].
    #[must_use]
    pub fn with_limit(root: AbsPath, limit: u64) -> Self {
        Self {
            root,
            limit,
            files: Vec::new(),
            by_path: HashMap::new(),
        }
    }

    /// Keeps the content of `file` as it is now, before a tool changes it.
    /// Only the first call for a file in a round counts: that is the state
    /// the round started from. A file over the limit ([`KEEP_LIMIT`] unless
    /// another was given) is recorded by hash and size only.
    ///
    /// # Errors
    ///
    /// When the file exists and cannot be read, or the store cannot be
    /// written. The tool should not change the file then: its change could
    /// not be undone.
    pub fn before(&mut self, store: &DiskStore, file: &AbsPath) -> Result<()> {
        let path = self.key(file);
        if self.by_path.contains_key(&path) {
            return Ok(());
        }
        let before = store.put_file(&file.to_fs_path(), self.limit)?;
        self.by_path.insert(path.clone(), self.files.len());
        self.files.push(Touched {
            file: file.clone(),
            path,
            before,
            after: None,
        });
        Ok(())
    }

    /// Records what a tool left at `file`: its hash and size, or `None` for
    /// a file it removed. A later call replaces an earlier one. A file whose
    /// result is not recorded is looked at when the round finishes.
    ///
    /// A file with no [`before`](Self::before) is recorded too, as a change
    /// that cannot be undone, so that a restore knows it was touched.
    pub fn after(&mut self, file: &AbsPath, after: Option<FileVersion>) {
        let path = self.key(file);
        match self.by_path.get(&path) {
            Some(&index) => self.files[index].after = Some(after),
            None => {
                self.by_path.insert(path.clone(), self.files.len());
                self.files.push(Touched {
                    file: file.clone(),
                    path,
                    before: None,
                    after: Some(after),
                });
            }
        }
    }

    /// Whether the round has touched nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The round's `checkpoint` entry, with what each file is now where a
    /// tool did not say. `None` when no file changed: a round that changes
    /// nothing costs nothing.
    ///
    /// # Errors
    ///
    /// When a file whose result was not recorded cannot be read.
    pub fn finish(self) -> Result<Option<Body>> {
        let mut files = Vec::new();
        for touched in self.files {
            let after = match touched.after {
                Some(after) => after,
                None => hash_file(&touched.file.to_fs_path())?,
            };
            let unchanged = match (&touched.before, &after) {
                (None, None) => true,
                (Some(before), Some(after)) => before.hash == after.hash,
                _ => false,
            };
            if !unchanged {
                files.push(FileChange {
                    path: touched.path,
                    before: touched.before,
                    after,
                });
            }
        }
        Ok((!files.is_empty()).then_some(Body::Checkpoint(Checkpoint { files })))
    }

    /// How a checkpoint names a file: relative to the workspace root, or
    /// absolute when it is outside.
    fn key(&self, file: &AbsPath) -> String {
        match file.strip_prefix(&self.root) {
            Some(relative) => relative.as_str().to_owned(),
            None => file.as_str().to_owned(),
        }
    }
}

/// The hash and size of the file at `path` now, streamed. `None` when there
/// is no file.
pub(crate) fn hash_file(path: &std::path::Path) -> Result<Option<FileVersion>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SessionError::io("read the file", path.to_path_buf())(error)),
    };
    let mut hasher = ContentHasher::new();
    let bytes = io::copy(&mut file, &mut hasher)
        .map_err(SessionError::io("read the file", path.to_path_buf()))?;
    Ok(Some(FileVersion {
        hash: hasher.finish(),
        bytes,
        kept: true,
    }))
}

/// Where a file a checkpoint names is: inside the workspace for a relative
/// name.
pub(crate) fn locate(root: &AbsPath, name: &str) -> Result<AbsPath> {
    if let Ok(absolute) = AbsPath::parse(name, PathStyle::NATIVE) {
        return Ok(absolute);
    }
    root.join(name)
        .map_err(|error| SessionError::Invalid(format!("the checkpoint names `{name}`: {error}")))
}

/// Whether a file is where the harness left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// The file is as the harness left it: putting it back is safe.
    AsLeft,
    /// The file is already as it will be put back.
    AlreadyRestored,
    /// Someone changed the file after the harness left it. It is put back
    /// only if the user says so.
    EditedSince,
    /// Its earlier content was too large to keep, so it cannot be put back.
    CannotRestore,
}

/// What a rewind does to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRestore {
    /// The file, as the checkpoints name it.
    pub path: String,
    /// What it is put back to. `None`: it did not exist, and is removed.
    pub restore: Option<FileVersion>,
    /// What the harness last left there. `None`: it removed the file.
    pub left: Option<FileVersion>,
    /// What it is now. `None`: there is no file.
    pub now: Option<FileVersion>,
    /// Whether putting it back is safe.
    pub status: FileStatus,
}

/// A rewind, worked out and not yet done: what it undoes and what it would
/// put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindPlan {
    /// The entry that becomes the active position.
    pub to: EntryId,
    /// Why.
    pub reason: RewindReason,
    /// The active position before the rewind.
    pub from: Option<EntryId>,
    /// The turns it undoes, wholly or in part, oldest first.
    pub turns: Vec<TurnId>,
    /// The files those turns changed, in the order they were first changed.
    pub files: Vec<FileRestore>,
}

impl RewindPlan {
    /// The files that cannot be put back, to tell the user before the
    /// rewind.
    #[must_use]
    pub fn cannot_restore(&self) -> Vec<&FileRestore> {
        self.files
            .iter()
            .filter(|file| file.status == FileStatus::CannotRestore)
            .collect()
    }

    /// The files someone changed since the harness left them, which a
    /// restore asks about.
    #[must_use]
    pub fn edited_since(&self) -> Vec<&FileRestore> {
        self.files
            .iter()
            .filter(|file| file.status == FileStatus::EditedSince)
            .collect()
    }
}

/// What to do with files when rewinding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restore {
    /// Put files back. Without it the rewind moves only the conversation.
    pub files: bool,
    /// Files edited since the harness left them that may be overwritten
    /// all the same, by the names in the plan.
    pub overwrite_edited: Vec<String>,
}

impl Restore {
    /// Move the conversation only.
    #[must_use]
    pub fn conversation_only() -> Self {
        Self::default()
    }

    /// Put files back, except those edited since.
    #[must_use]
    pub fn files() -> Self {
        Self {
            files: true,
            overwrite_edited: Vec::new(),
        }
    }
}

/// What a rewind did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindDone {
    /// The `rewind` entry: the new active position.
    pub entry: EntryId,
    /// The files put back.
    pub restored: Vec<String>,
    /// The files left as they are, with the reason.
    pub not_restored: Vec<(String, String)>,
    /// Tool calls at the new position that had no result, and were given
    /// an "interrupted" one.
    pub repaired_calls: Vec<cortex_protocol::ids::CallId>,
}

/// Works out the status of each file of a plan from what is on disk now.
pub(crate) fn file_status(
    root: &AbsPath,
    path: &str,
    restore: Option<FileVersion>,
    left: Option<FileVersion>,
) -> Result<FileRestore> {
    let now = hash_file(&locate(root, path)?.to_fs_path())?;
    let same = |a: Option<FileVersion>, b: Option<FileVersion>| match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.hash == b.hash,
        _ => false,
    };
    let status = if same(now, restore) {
        FileStatus::AlreadyRestored
    } else if restore.is_some_and(|version| !version.kept) {
        FileStatus::CannotRestore
    } else if same(now, left) {
        FileStatus::AsLeft
    } else {
        FileStatus::EditedSince
    };
    Ok(FileRestore {
        path: path.to_owned(),
        restore,
        left,
        now,
        status,
    })
}

/// Puts one file back as `restore` says.
pub(crate) fn put_back(
    store: &DiskStore,
    root: &AbsPath,
    file: &FileRestore,
    clock: &dyn cortex_base::Clock,
) -> Result<()> {
    let target: PathBuf = locate(root, &file.path)?.to_fs_path();
    match file.restore {
        None => match std::fs::remove_file(&target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SessionError::io("remove the file", target)(error)),
        },
        Some(version) => {
            let content = store
                .get_blob(&version.hash)?
                .ok_or_else(|| SessionError::Corrupt {
                    hash: version.hash.to_hex(),
                })?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(SessionError::io(
                    "create the file's directory",
                    parent.to_path_buf(),
                ))?;
            }
            cortex_base::write_atomic(
                &target,
                &content,
                &cortex_base::AtomicWriteOptions::default(),
                clock,
            )
            .map_err(|error| SessionError::Io {
                action: "put the file back",
                path: target.clone(),
                source: io::Error::other(error.to_string()),
            })
        }
    }
}
