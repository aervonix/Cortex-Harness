//! What `write` and `edit` share: reading the file that is there, the
//! staleness guard, and putting the new content in its place
//! (`docs/architecture/05-tools.md`, "`write`", "`edit`", "Text files",
//! "Checkpoints", "Atomic writes").
//!
//! The order of a change, and what each step protects:
//!
//! 1. **The path is resolved again.** The call was planned, and possibly
//!    waited for an approval, with the path leading to one place. If it now
//!    leads to another (a link was put on the way), nothing is read and
//!    nothing is written.
//! 2. **The file is opened without following a link and without waiting**
//!    ([`cortex_base::open_regular`]), and read whole, up to the limit.
//! 3. **It must be UTF-8 text.** `read` shows a file that is not with the
//!    bad bytes replaced, and writing that back would damage the file.
//! 4. **The staleness guard:** it must have been read in this session, and
//!    be what was read.
//! 5. The tool works out the new content. A call that fails here has
//!    touched nothing.
//! 6. **The file is looked at once more**, just before the write: if it is
//!    no longer what was opened, the call stops.
//! 7. **The checkpoint is given the bytes that were read.** If they cannot
//!    be kept, the file is not changed.
//! 8. **The write is atomic** and follows no link: a reader sees the old
//!    file or the new one, and a write that fails leaves the old one.
//! 9. The table of seen files and the checkpoint are told what was left.
//!
//! A change is never abandoned half-way: the work runs to its end on the
//! blocking pool whatever happens to the call's token, which is looked at
//! once, before step 6. A call cancelled before that changes nothing and
//! says so; one cancelled after it has made its change, and says that.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "reading and replacing a file blocks; the tools run this on the blocking pool"
)]

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use cortex_base::{
    AtomicWriteError, AtomicWriteOptions, CancelToken, ContentHash, OpenError, PathResolver,
    ResolvedPath, write_atomic,
};

use crate::checkpoint::Editing;
use crate::context::RunCx;
use crate::files::{FileSnapshot, FileTable, Staleness};
use crate::output::size;
use crate::result::ToolResult;
use crate::textfile::{self, NotText};

/// The largest diff, in bytes, that a result's payload carries for front
/// ends. A larger one is left out, and the payload says so: the payload is
/// stored in the session log with the result.
pub(crate) const MAX_PAYLOAD_DIFF: usize = 256 * 1024;

/// What a change needs of the session, owned so that it can cross to the
/// blocking pool.
pub(crate) struct Env {
    pub(crate) paths: Arc<PathResolver>,
    pub(crate) files: Arc<FileTable>,
    pub(crate) editing: Editing,
    /// The call's own token. The work is not stopped by it: it asks it.
    pub(crate) cancel: CancelToken,
}

impl Env {
    pub(crate) fn new(cx: &RunCx, editing: &Editing) -> Self {
        Self {
            paths: Arc::clone(&cx.paths),
            files: Arc::clone(&cx.files),
            editing: editing.clone(),
            cancel: cx.cancel.clone(),
        }
    }
}

/// The file a call changes.
pub(crate) struct Target {
    pub(crate) path: ResolvedPath,
    /// The path as the model is shown it.
    pub(crate) shown: String,
    pub(crate) fs_path: PathBuf,
}

/// What was at the target when the call ran.
pub(crate) enum Found {
    /// Nothing: the file is to be created.
    Missing,
    File(Existing),
}

/// A file as it was opened.
pub(crate) struct Existing {
    pub(crate) bytes: Vec<u8>,
    pub(crate) hash: ContentHash,
    stamp: Stamp,
}

/// What tells a file from another that has taken its name: enough to see,
/// just before a write, that the file is still the one that was read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    /// The device and the inode, where the system has them.
    identity: Option<(u64, u64)>,
}

impl Stamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let identity = None;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            identity,
        }
    }
}

impl Target {
    /// The target of a planned call, checked to lead where it led when the
    /// call was planned (step 1).
    pub(crate) fn new(path: &ResolvedPath, paths: &PathResolver) -> Result<Self, ToolResult> {
        let target = Self {
            shown: path.display(),
            fs_path: path.to_fs_path(),
            path: path.clone(),
        };
        target.still_leads_there(paths)?;
        Ok(target)
    }

    /// Resolves the target's location again. Every name on the way that
    /// exists is real, so resolving it gives the same location back, unless
    /// something on the way has become a link since.
    fn still_leads_there(&self, paths: &PathResolver) -> Result<(), ToolResult> {
        let moved = || {
            ToolResult::error(format!(
                "`{}` no longer leads where it did when this call was checked: something on \
                 the way to it is now a link. Nothing was changed.",
                self.shown
            ))
        };
        match paths.resolve(self.path.abs().as_str()) {
            Ok(again) if again.abs() == self.path.abs() => Ok(()),
            Ok(_) | Err(_) => Err(moved()),
        }
    }

    /// Opens and reads what is at the target (step 2).
    pub(crate) fn find(&self, env: &Env) -> Result<Found, ToolResult> {
        let shown = &self.shown;
        let refuse = |why: String| Err(ToolResult::error(why));
        let (file, metadata) = match cortex_base::open_regular(&self.fs_path) {
            Ok(opened) => opened,
            Err(OpenError::NotFound) => return Ok(Found::Missing),
            Err(OpenError::Link) => {
                return refuse(format!(
                    "`{shown}` is now a link, which it was not when this call was checked. It \
                     was not followed, and nothing was changed."
                ));
            }
            Err(OpenError::Directory) => {
                return refuse(format!("`{shown}` is a directory, not a file."));
            }
            Err(OpenError::NotAFile) => {
                return refuse(format!(
                    "`{shown}` is not a regular file (it is a device, a pipe or a socket), so \
                     it is not changed."
                ));
            }
            Err(error) if error.is_in_use() => return refuse(in_use(shown)),
            Err(OpenError::Io(error)) => {
                return refuse(format!("`{shown}` could not be read: {error}."));
            }
        };
        let limit = env.editing.limit;
        if metadata.len() > limit {
            return refuse(too_large(shown, metadata.len(), limit));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        // One byte past the limit is enough to know that the file grew
        // past it since its length was asked.
        if let Err(error) = file.take(limit + 1).read_to_end(&mut bytes) {
            return refuse(match error.raw_os_error() {
                // A region another program has locked (Windows).
                Some(33) if cfg!(windows) => in_use(shown),
                _ => format!("`{shown}` could not be read: {error}."),
            });
        }
        if bytes.len() as u64 > limit {
            return refuse(too_large(shown, bytes.len() as u64, limit));
        }
        Ok(Found::File(Existing {
            hash: ContentHash::of(&bytes),
            stamp: Stamp::of(&metadata),
            bytes,
        }))
    }

    /// The staleness guard (step 4). `verb` is what the call would do to
    /// the file: `overwrite`, `edit`.
    pub(crate) fn guard(
        &self,
        env: &Env,
        existing: &Existing,
        verb: &str,
    ) -> Result<(), ToolResult> {
        let shown = &self.shown;
        match env.files.staleness(self.path.abs(), &existing.hash) {
            Staleness::Current => Ok(()),
            Staleness::Unseen => Err(ToolResult::error(format!(
                "`{shown}` exists and has not been read in this session. Read it first, then \
                 {verb} it: this keeps a file you have not seen from being changed. Nothing \
                 was changed."
            ))),
            Staleness::Stale => Err(ToolResult::error(format!(
                "`{shown}` has changed on disk since it was last read or written in this \
                 session. Read it again, then {verb} it. Nothing was changed."
            ))),
        }
    }

    /// The file's content as text, with whether it starts with a byte-order
    /// mark (step 3).
    pub(crate) fn text<'a>(&self, existing: &'a Existing) -> Result<(bool, &'a str), ToolResult> {
        let shown = &self.shown;
        textfile::decode(&existing.bytes).map_err(|not_text| {
            ToolResult::error(match not_text {
                NotText::Binary(what) => format!(
                    "`{shown}` is not a text file: it is {what}, {}. The editing tools change \
                     text files only. Nothing was changed.",
                    size(existing.bytes.len())
                ),
                NotText::NotUtf8 { line } => format!(
                    "`{shown}` is not valid UTF-8: the first byte that is not is on line \
                     {line}. `read` showed it with such bytes replaced, and writing that back \
                     would damage the file, so the editing tools refuse it. Nothing was \
                     changed. To change it, use a shell command that works on bytes."
                ),
            })
        })
    }

    /// Puts `new` in the target's place (steps 6 to 9), and returns what
    /// the file now is.
    pub(crate) fn commit(
        &self,
        env: &Env,
        found: &Found,
        new: &[u8],
    ) -> Result<FileSnapshot, ToolResult> {
        let shown = &self.shown;
        if env.cancel.is_cancelled() {
            return Err(ToolResult::interrupted(format!(
                "The call was interrupted before `{shown}` was written. Nothing was changed."
            )));
        }
        if matches!(found, Found::Missing)
            && let Some(parent) = self.fs_path.parent()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                ToolResult::error(format!(
                    "The directory for `{shown}` could not be created: {error}. Nothing was \
                     written."
                ))
            })?;
            // The directories are new names on the way: look again.
            self.still_leads_there(&env.paths)?;
        }
        self.still_as_found(found)?;

        let abs = self.path.abs();
        let before = match found {
            Found::Missing => None,
            Found::File(existing) => Some(existing.bytes.as_slice()),
        };
        env.editing
            .checkpoints
            .before(abs, before)
            .map_err(|error| {
                ToolResult::error(format!(
                    "What `{shown}` held could not be kept for undo ({error}), so it was not \
                     changed."
                ))
            })?;

        let options = AtomicWriteOptions {
            sync: env.editing.sync,
            // The location was resolved and checked. A link that appears
            // there now must not carry the write anywhere else.
            follow_links: false,
            ..AtomicWriteOptions::default()
        };
        write_atomic(&self.fs_path, new, &options, env.editing.clock.as_ref())
            .map_err(|error| ToolResult::error(self.not_written(&error)))?;

        let modified = std::fs::symlink_metadata(&self.fs_path)
            .and_then(|metadata| metadata.modified())
            .ok();
        let snapshot = FileSnapshot::of_content(new, modified);
        env.editing.checkpoints.after(abs, &snapshot);
        env.files.record_write(abs, snapshot.clone());
        Ok(snapshot)
    }

    /// Whether the target is still what [`find`](Self::find) found
    /// (step 6): the same file, of the same length, not written since.
    fn still_as_found(&self, found: &Found) -> Result<(), ToolResult> {
        let shown = &self.shown;
        let changed = || {
            ToolResult::error(format!(
                "`{shown}` changed on disk while this call was working on it. Read it again \
                 and repeat the call. Nothing was changed."
            ))
        };
        let now = std::fs::symlink_metadata(&self.fs_path);
        match (found, now) {
            (Found::Missing, Err(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            (Found::File(existing), Ok(now))
                if now.is_file() && Stamp::of(&now) == existing.stamp =>
            {
                Ok(())
            }
            _ => Err(changed()),
        }
    }

    /// Why a write did not happen, for the model. The file is as it was.
    fn not_written(&self, error: &AtomicWriteError) -> String {
        let shown = &self.shown;
        match error {
            AtomicWriteError::ReadOnly { .. } => format!(
                "`{shown}` is read-only, so it was not changed. Make it writable first if it \
                 is meant to be changed."
            ),
            AtomicWriteError::InUse {
                attempts, waited, ..
            } => format!(
                "`{shown}` is open in another program, which kept it locked through {attempts} \
                 attempts over {waited:?}. Nothing was changed. Close it there, or wait for \
                 that program to finish, and repeat the call."
            ),
            AtomicWriteError::IsALink { .. } => format!(
                "`{shown}` is now a link, which it was not when this call was checked. It was \
                 not written through, and nothing was changed."
            ),
            AtomicWriteError::IsADirectory { .. } => {
                format!("`{shown}` is a directory, not a file. Nothing was changed.")
            }
            AtomicWriteError::NotAFile { .. } => format!(
                "`{shown}` is not a regular file (it is a device, a pipe or a socket), so it \
                 was not changed."
            ),
            AtomicWriteError::Io { step, source, .. } => {
                format!("`{shown}` was not changed: could not {step}: {source}.")
            }
        }
    }
}

fn in_use(shown: &str) -> String {
    format!(
        "`{shown}` is open in another program, which does not let it be read. Nothing was \
         changed. Close it there, or wait for that program to finish, and repeat the call."
    )
}

fn too_large(shown: &str, bytes: u64, limit: u64) -> String {
    let human = |bytes: u64| size(usize::try_from(bytes).unwrap_or(usize::MAX));
    format!(
        "`{shown}` is {}, and the editing tools change files of up to {}: what a larger file \
         held could not be kept, so the change could not be undone. Nothing was changed. To \
         change it all the same, use a shell command (for example `sed`, or a short script).",
        human(bytes),
        human(limit)
    )
}

/// The diff a result's payload carries, or `null` when it is too large to
/// carry, with the flag that says so.
pub(crate) fn payload_diff(diff: Option<&crate::diff::Diff>) -> (serde_json::Value, bool) {
    match diff {
        Some(diff) if diff.text.len() <= MAX_PAYLOAD_DIFF => {
            (serde_json::Value::String(diff.text.clone()), false)
        }
        _ => (serde_json::Value::Null, true),
    }
}
