//! Per-turn snapshots, and putting files back
//! (`docs/architecture/07-sessions-and-context.md`, "Snapshots and undo").
//!
//! Before a tool changes a file, its content as it was (or the fact that it
//! did not exist) is kept in the checkpoint store. A [`RoundCheckpoint`]
//! gathers what one tool round changed, and becomes the round's
//! `checkpoint` entry. The checkpoints of a turn are its snapshot.
//!
//! What a file was before the round is a [`Before`]: absent, a version, or
//! **not known**. A change seen only after it was made (a shell command's)
//! may have no earlier content to go back to. Such a change is recorded so
//! that an undo can say so, and a rewind never removes or overwrites that
//! file.
//!
//! A rewind is planned first ([`RewindPlan`]) and then carried out. The
//! plan says, for each file the undone rounds changed, what it will be put
//! back to, and whether that is safe: a file someone edited after the
//! harness left it is not overwritten unless the user says so, and a file
//! whose earlier content was too large to keep, or is not known, cannot be
//! put back at all.
//!
//! # Recording a shell command's changes
//!
//! A command does not say what it changes; the harness looks before and
//! after (`docs/architecture/07-sessions-and-context.md`, "Changes made by
//! shell commands"). For each path it saw change, the caller records what
//! it knows with [`RoundCheckpoint::record_before`], and
//! [`RoundCheckpoint::finish`] looks at each file for what the command
//! left:
//!
//! - a file that was not there before: [`Before::Absent`];
//! - a file whose earlier content can be read from git: that content,
//!   stored with [`DiskStore::put_stream`] under the same size limit, as
//!   [`Before::Version`];
//! - any other changed file: [`Before::Unknown`].

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use cortex_base::{AbsPath, ContentHash, ContentHasher, OpenError, PathStyle};
use cortex_protocol::ids::{SessionId, TurnId};

use crate::entry::{Before, Body, Checkpoint, EntryId, FileChange, FileVersion, RewindReason};
use crate::error::{Result, SessionError};
use crate::store::{DiskStore, KEEP_LIMIT};

/// What one tool round changed, gathered as the tools run.
#[derive(Debug, Clone)]
pub struct RoundCheckpoint {
    root: AbsPath,
    limit: u64,
    files: Vec<Touched>,
    /// By [`AbsPath::key`], so that two spellings of one file (on Windows,
    /// `Src/A.rs` and `src/a.rs`) are one file.
    by_key: HashMap<String, usize>,
}

/// One file the round touched.
#[derive(Debug, Clone)]
struct Touched {
    file: AbsPath,
    /// The name the checkpoint gives it: the first spelling the round saw.
    path: String,
    before: Before,
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
            by_key: HashMap::new(),
        }
    }

    /// The largest file whose content this round keeps: [`KEEP_LIMIT`]
    /// unless another was given. What a caller stores for
    /// [`record_before`](Self::record_before) keeps to it too.
    #[must_use]
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Keeps the content of `file` as it is now, before a tool changes it.
    /// Only the first record of a file in a round counts: that is the state
    /// the round started from. A file over the limit is recorded by hash
    /// and size only.
    ///
    /// Only a regular file is kept ([`DiskStore::put_file`]): a link is
    /// never followed, so a tool that changes a file through a link passes
    /// the file the link resolves to.
    ///
    /// # Errors
    ///
    /// When the file exists and cannot be read, is not a regular file, or
    /// the store cannot be written. The tool should not change the file
    /// then: its change could not be undone.
    pub fn before(&mut self, store: &DiskStore, file: &AbsPath) -> Result<()> {
        if self.by_key.contains_key(file.key()) {
            return Ok(());
        }
        let before = Before::from(store.put_file(&file.to_fs_path(), self.limit)?);
        self.touch(file, before, None);
        Ok(())
    }

    /// Records what `file` was before the round, when the caller knows it
    /// some other way than by reading the file now: for a change that was
    /// seen only after it was made (see the [module](self) documentation).
    /// Only the first record of a file in a round counts.
    ///
    /// A [`Before::Version`] names content already in the store, or is
    /// marked as not kept; one whose content is not in the store is shown
    /// by a rewind's plan as a file it cannot put back.
    pub fn record_before(&mut self, file: &AbsPath, before: Before) {
        if !self.by_key.contains_key(file.key()) {
            self.touch(file, before, None);
        }
    }

    /// Records what a tool left at `file`: its hash and size, or `None` for
    /// a file it removed. A later call replaces an earlier one. A file whose
    /// result is not recorded is looked at when the round finishes.
    ///
    /// A file with no record of what it was before is recorded as changed,
    /// with its earlier state [`Before::Unknown`]: an undo says it cannot
    /// put the file back, and never removes it.
    pub fn after(&mut self, file: &AbsPath, after: Option<FileVersion>) {
        match self.by_key.get(file.key()) {
            Some(&index) => self.files[index].after = Some(after),
            None => self.touch(file, Before::Unknown, Some(after)),
        }
    }

    /// Records that `file` was seen changed by something that did not say
    /// what it left: a shell command. For a file the round has no record
    /// of, this is [`record_before`](Self::record_before). For one it has
    /// (an editing tool changed it earlier in the round), the first record
    /// of what it was before stands, and what the tool said it left is
    /// forgotten: the file is looked at when the round finishes, so that
    /// the checkpoint holds what the round really left.
    pub fn seen_changed(&mut self, file: &AbsPath, before: Before) {
        match self.by_key.get(file.key()) {
            Some(&index) => self.files[index].after = None,
            None => self.touch(file, before, None),
        }
    }

    fn touch(&mut self, file: &AbsPath, before: Before, after: Option<Option<FileVersion>>) {
        self.by_key.insert(file.key().to_owned(), self.files.len());
        self.files.push(Touched {
            file: file.clone(),
            path: self.name(file),
            before,
            after,
        });
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
            // A change whose earlier state is not known is always kept: the
            // round may have changed the file and changed it back, but an
            // undo still has to say that it cannot vouch for it.
            if !touched.before.is(after.as_ref()) {
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
    fn name(&self, file: &AbsPath) -> String {
        match file.strip_prefix(&self.root) {
            Some(relative) => relative.as_str().to_owned(),
            None => file.as_str().to_owned(),
        }
    }
}

/// The hash and size of the file at `path` now, streamed. `None` when there
/// is no file.
///
/// It reads as [`DiskStore::put_file`] does: a regular file only, opened
/// without following a link or waiting, and no more than the length it had
/// when it was opened. Anything else at the path (a link, a directory, a
/// device, a FIFO) is never read, and is known by what it is: see
/// [`not_a_file`].
pub(crate) fn hash_file(path: &std::path::Path) -> Result<Option<FileVersion>> {
    let read_error = || SessionError::io("read the file", path.to_path_buf());
    // The open is the check, as in `put_file`: no link is followed and no
    // FIFO waited on, whatever was put at the path a moment ago.
    let (file, opened) = match cortex_base::open_regular(path) {
        Ok(regular) => regular,
        Err(OpenError::NotFound) => return Ok(None),
        Err(OpenError::Link | OpenError::Directory | OpenError::NotAFile) => {
            // What it is, asked of the name itself and never of what a
            // link leads to.
            return match fs::symlink_metadata(path) {
                Ok(found) => Ok(Some(not_a_file(path, &found))),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(read_error()(error)),
            };
        }
        Err(OpenError::Io(error)) => return Err(read_error()(error)),
    };
    let mut hasher = ContentHasher::new();
    let bytes = io::copy(&mut file.take(opened.len()), &mut hasher).map_err(read_error())?;
    Ok(Some(FileVersion {
        hash: hasher.finish(),
        bytes,
        kept: true,
    }))
}

/// What stands for something at a path that is not a regular file: a hash
/// of what it is (its kind, and for a link the text it holds), size 0,
/// content not kept. It compares equal to the same thing seen again, and
/// unequal to any file, so a rewind can tell whether it is as the harness
/// left it, and never reads it.
fn not_a_file(path: &std::path::Path, found: &fs::Metadata) -> FileVersion {
    let kind = found.file_type();
    let what = if kind.is_symlink() {
        let target = fs::read_link(path)
            .map(|target| target.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("link\0{target}")
    } else if kind.is_dir() {
        "directory".to_owned()
    } else {
        "special".to_owned()
    };
    FileVersion {
        hash: ContentHash::of(format!("\0cortex-harness: not a regular file\0{what}").as_bytes()),
        bytes: 0,
        kept: false,
    }
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
    /// It cannot be put back: its earlier content was too large to keep,
    /// is not known, or is missing from the store, or a checkpoint of the
    /// undone turns cannot be read ([`FileRestore::why_not`] says which).
    /// The file is left as it is.
    CannotRestore,
}

/// What a rewind does to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRestore {
    /// The file, as the checkpoints name it.
    pub path: String,
    /// What it is put back to: removed when it did not exist, left alone
    /// when what it was is not known.
    pub restore: Before,
    /// What the harness last left there. `None`: it removed the file.
    pub left: Option<FileVersion>,
    /// What it is now. `None`: there is no file.
    pub now: Option<FileVersion>,
    /// Whether putting it back is safe.
    pub status: FileStatus,
    /// Why it cannot be put back, for a [`FileStatus::CannotRestore`] file.
    pub why_not: Option<&'static str>,
}

/// Why a file cannot be put back.
mod why_not {
    pub(super) const NOT_RECORDED: &str = "what it held before was not recorded (it was changed \
                                           by a command, and its earlier content could not be \
                                           recovered), so it cannot be put back";
    pub(super) const TOO_LARGE: &str =
        "its earlier content was too large to keep, so it cannot be put back";
    pub(super) const MISSING: &str =
        "its earlier content is no longer in the session's store, so it cannot be put back";
    pub(super) const UNREADABLE: &str = "a checkpoint of the undone turns cannot be read, so what \
                                         they changed is not fully known, and no file is put back";
}

/// A rewind, worked out and not yet done: what it undoes and what it would
/// put back. Only [`Session::plan_rewind`](crate::Session::plan_rewind) and
/// [`Session::plan_undo`](crate::Session::plan_undo) make one, and
/// [`Session::rewind`](crate::Session::rewind) refuses a plan that was made
/// for another session, or before the session moved on.
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
    /// Checkpoints of those turns that this build cannot read: damaged, or
    /// written in a newer format. What they changed is not known, so when
    /// there is one, no file is put back.
    pub unreadable: Vec<EntryId>,
    /// The session it was made for.
    pub(crate) session: SessionId,
}

/// One file a rewind would put back: its name, what it goes back to, and
/// what the harness last left there.
pub(crate) type Change = (String, Before, Option<FileVersion>);

impl RewindPlan {
    /// What the plan would do to files, whatever they are now.
    pub(crate) fn changes(&self) -> Vec<Change> {
        self.files
            .iter()
            .map(|file| (file.path.clone(), file.restore, file.left))
            .collect()
    }

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
    /// Tool calls at the new position that had no result, each with the
    /// "interrupted" result it was given.
    pub repaired: Vec<Repaired>,
}

/// A tool call that had no result, and the "interrupted" result the store
/// gave it. Its item id is one the store made: a caller that makes its own
/// item ids continues after it ([`Log::last_ids`](crate::Log::last_ids)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repaired {
    /// The call.
    pub call: cortex_protocol::ids::CallId,
    /// The id of the result's item.
    pub item: cortex_protocol::ids::ItemId,
    /// The entry that holds it.
    pub entry: EntryId,
}

/// What a switch of branch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchDone {
    /// The `rewind` entry: the new active position.
    pub entry: EntryId,
    /// Tool calls at the new position that had no result, each with the
    /// "interrupted" result it was given.
    pub repaired: Vec<Repaired>,
}

/// Works out the status of one file of a plan from what is on disk now and
/// what the store holds. `unreadable`: a checkpoint of the undone turns
/// could not be read, so no file is put back.
pub(crate) fn file_status(
    store: &DiskStore,
    root: &AbsPath,
    (path, restore, left): Change,
    unreadable: bool,
) -> Result<FileRestore> {
    let now = hash_file(&locate(root, &path)?.to_fs_path())?;
    let same = |a: Option<FileVersion>, b: Option<FileVersion>| match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.hash == b.hash,
        _ => false,
    };
    let cannot = |why: &'static str| (FileStatus::CannotRestore, Some(why));
    let (status, why_not) = match restore {
        _ if unreadable => cannot(why_not::UNREADABLE),
        // Nothing to go back to: never removed, never overwritten.
        Before::Unknown => cannot(why_not::NOT_RECORDED),
        _ if restore.is(now.as_ref()) => (FileStatus::AlreadyRestored, None),
        Before::Version(version) if !version.kept => cannot(why_not::TOO_LARGE),
        Before::Version(version) if !store.has_blob(&version.hash) => cannot(why_not::MISSING),
        _ if same(now, left) => (FileStatus::AsLeft, None),
        _ => (FileStatus::EditedSince, None),
    };
    Ok(FileRestore {
        path,
        restore,
        left,
        now,
        status,
        why_not,
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
        Before::Unknown => Err(SessionError::Invalid(format!(
            "`{}` cannot be put back: what it held before is not known",
            file.path
        ))),
        Before::Version(version) if !version.kept => Err(SessionError::Invalid(format!(
            "`{}` cannot be put back: its earlier content was too large to keep",
            file.path
        ))),
        Before::Absent => match std::fs::remove_file(&target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SessionError::io("remove the file", target)(error)),
        },
        Before::Version(version) => {
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
