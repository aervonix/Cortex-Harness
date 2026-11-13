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
//!    file or the new one, and a write that fails leaves the old one. The
//!    directories a new file needs are made here and not before, and are
//!    removed again if the write does not go through: a call that says
//!    "nothing was changed" has left nothing behind.
//! 9. The table of seen files and the checkpoint are told what was left.
//!    The checkpoint is told when the write failed, too: the file is then
//!    as it was, and saying so now means that nobody else's later change
//!    to it can be taken for this round's.
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
use crate::output::{count, size};
use crate::result::ToolResult;
use crate::textfile::{self, NotText};

/// The largest diff, in bytes, that a result's payload carries for front
/// ends. A larger one is left out, and the payload says so: the payload is
/// stored in the session log with the result.
pub(crate) const MAX_PAYLOAD_DIFF: usize = 256 * 1024;

/// The most lines a file may have for the editing tools to change it.
///
/// The size limit alone does not bound the work: a file of 16 MiB that is
/// nothing but line breaks has sixteen million lines, and the tools keep a
/// few words for each. Two million is far past any source file, and keeps
/// what a call holds in memory to tens of megabytes.
pub(crate) const MAX_LINES: usize = 2_000_000;

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
            Ok(_) => Err(moved()),
            // A link to nothing, or a name the rules now refuse.
            Err(error) => Err(ToolResult::error(format!(
                "`{}` can no longer be reached as it was when this call was checked: {error}. \
                 Nothing was changed.",
                self.shown
            ))),
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
        let lines = existing.bytes.iter().filter(|&&byte| byte == b'\n').count();
        if lines > MAX_LINES {
            return Err(ToolResult::error(format!(
                "`{shown}` has {} lines, and the editing tools change files of up to {}. \
                 Nothing was changed. To change it, use a shell command (for example `sed`, or \
                 a short script).",
                count(lines),
                count(MAX_LINES)
            )));
        }
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

        // From here on the round has been told that the file will change.
        // Whatever stops the write, it is told that the file is as it was.
        if let Err(refused) = self.put(env, found, new) {
            let still = match found {
                Found::Missing => None,
                Found::File(existing) => Some(FileSnapshot {
                    size: existing.bytes.len() as u64,
                    modified: existing.stamp.modified,
                    hash: existing.hash,
                }),
            };
            env.editing.checkpoints.after(abs, still.as_ref());
            return Err(refused);
        }

        let modified = std::fs::symlink_metadata(&self.fs_path)
            .and_then(|metadata| metadata.modified())
            .ok();
        let snapshot = FileSnapshot::of_content(new, modified);
        env.editing.checkpoints.after(abs, Some(&snapshot));
        env.files.record_write(abs, snapshot.clone());
        Ok(snapshot)
    }

    /// Writes `new` at the target (step 8), making the directories a new
    /// file needs. A failure leaves nothing behind: the file as it was, and
    /// none of the directories this call made.
    fn put(&self, env: &Env, found: &Found, new: &[u8]) -> Result<(), ToolResult> {
        self.put_with(env, found, || {
            let options = AtomicWriteOptions {
                sync: env.editing.sync,
                // The location was resolved and checked. A link that
                // appears there now must not carry the write anywhere else.
                follow_links: false,
                ..AtomicWriteOptions::default()
            };
            write_atomic(&self.fs_path, new, &options, env.editing.clock.as_ref())
                .map_err(|error| ToolResult::error(self.not_written(&error)))
        })
    }

    /// [`put`](Self::put), with the write itself as `write`: the
    /// directories are made before it and, if it fails, removed after it.
    fn put_with(
        &self,
        env: &Env,
        found: &Found,
        write: impl FnOnce() -> Result<(), ToolResult>,
    ) -> Result<(), ToolResult> {
        let made = if matches!(found, Found::Missing) {
            self.make_parents()?
        } else {
            Vec::new()
        };
        let written = if made.is_empty() {
            write()
        } else {
            // The directories are new names on the way: look again.
            self.still_leads_there(&env.paths).and_then(|()| write())
        };
        if written.is_err() {
            // Deepest first. Each is removed only if it is empty, which it
            // is unless someone else has put something in it since.
            for directory in &made {
                let _ = std::fs::remove_dir(directory);
            }
        }
        written
    }

    /// Creates the directories a new file needs, and returns the ones it
    /// made, deepest first.
    fn make_parents(&self) -> Result<Vec<PathBuf>, ToolResult> {
        let Some(parent) = self.fs_path.parent() else {
            return Ok(Vec::new());
        };
        let mut missing = Vec::new();
        let mut directory = parent;
        while matches!(
            std::fs::symlink_metadata(directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        ) {
            missing.push(directory.to_path_buf());
            match directory.parent() {
                Some(above) => directory = above,
                None => break,
            }
        }
        if missing.is_empty() {
            return Ok(missing);
        }
        if let Err(error) = std::fs::create_dir_all(parent) {
            for directory in &missing {
                let _ = std::fs::remove_dir(directory);
            }
            return Err(ToolResult::error(format!(
                "The directory for `{}` could not be created: {error}. Nothing was changed.",
                self.shown
            )));
        }
        Ok(missing)
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

#[cfg(test)]
mod tests {
    use cortex_base::{FakeClock, PathOptions, Rng, SystemRng};

    use super::*;
    use crate::checkpoint::MemoryCheckpoints;
    use crate::files::Shown;

    /// A workspace of one test, with what a change runs with.
    struct Bench {
        dir: PathBuf,
        env: Env,
        checkpoints: MemoryCheckpoints,
    }

    impl Bench {
        fn new(test: &str) -> Self {
            // A unit test has no directory of its own from Cargo: one in
            // the system's, under a name no other run has.
            let dir = std::env::temp_dir().join(format!(
                "cortex-tools-change-{test}-{:016x}",
                SystemRng::new().next_u64()
            ));
            std::fs::create_dir_all(&dir).expect("create the workspace");
            let paths = PathResolver::new(&dir, PathOptions::new()).expect("a workspace");
            let checkpoints = MemoryCheckpoints::new();
            let editing = Editing::new(
                Arc::new(checkpoints.clone()),
                Arc::new(FakeClock::default()),
            )
            .with_sync(false);
            Self {
                env: Env {
                    paths: Arc::new(paths),
                    files: Arc::new(FileTable::new()),
                    editing,
                    cancel: CancelToken::new(),
                },
                dir,
                checkpoints,
            }
        }

        fn target(&self, name: &str) -> Target {
            let path = self
                .env
                .paths
                .resolve(name)
                .expect("a path in the workspace");
            match Target::new(&path, &self.env.paths) {
                Ok(target) => target,
                Err(refused) => panic!("{}", refused.text()),
            }
        }

        /// A file the session has read.
        fn seen(&self, name: &str, content: &[u8]) -> Target {
            std::fs::write(self.dir.join(name), content).expect("write the file");
            let target = self.target(name);
            self.env.files.record_read(
                target.path.abs(),
                FileSnapshot::of_content(content, None),
                Shown {
                    hash: ContentHash::of(content),
                    lines: crate::files::LineRanges::new(),
                    total_lines: 0,
                    turn: 1,
                    kept: false,
                },
            );
            target
        }

        fn read(&self, name: &str) -> Option<Vec<u8>> {
            std::fs::read(self.dir.join(name)).ok()
        }
    }

    impl Drop for Bench {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn refusal(outcome: Result<FileSnapshot, ToolResult>) -> ToolResult {
        outcome.expect_err("the change must be refused")
    }

    /// Step 6. The file is read, and before the write someone else changes
    /// it: the write must not happen, or their change is lost.
    #[test]
    fn a_file_that_changes_between_the_read_and_the_write_is_not_replaced() {
        let bench = Bench::new("changed_between");
        let target = bench.seen("notes.txt", b"as it was read\n");
        let found = target
            .find(&bench.env)
            .unwrap_or_else(|_| panic!("it opens"));

        // Another program rewrites the file: another length.
        std::fs::write(bench.dir.join("notes.txt"), b"rewritten by someone else\n").unwrap();
        let refused = refusal(target.commit(&bench.env, &found, b"the tool's version\n"));
        assert_eq!(
            refused.text(),
            "`notes.txt` changed on disk while this call was working on it. Read it again and \
             repeat the call. Nothing was changed."
        );
        assert_eq!(
            bench.read("notes.txt").as_deref(),
            Some(&b"rewritten by someone else\n"[..])
        );
        assert!(
            bench.checkpoints.changes().is_empty(),
            "nothing was announced"
        );

        // Another program replaces the file with one of the same length: it
        // is another file all the same.
        let target = bench.seen("same.txt", b"aaaa\n");
        let found = target
            .find(&bench.env)
            .unwrap_or_else(|_| panic!("it opens"));
        std::fs::remove_file(bench.dir.join("same.txt")).unwrap();
        std::fs::write(bench.dir.join("other.txt"), b"x").unwrap();
        std::fs::write(bench.dir.join("same.txt"), b"bbbb\n").unwrap();
        let outcome = target.commit(&bench.env, &found, b"the tool's version\n");
        if cfg!(unix) {
            refusal(outcome);
            assert_eq!(bench.read("same.txt").as_deref(), Some(&b"bbbb\n"[..]));
        }
    }

    /// Step 6, for a file that was to be created: one that has appeared
    /// since is not overwritten.
    #[test]
    fn a_file_that_appears_where_one_was_to_be_created_is_not_replaced() {
        let bench = Bench::new("appeared");
        let target = bench.target("new.txt");
        let found = target
            .find(&bench.env)
            .unwrap_or_else(|_| panic!("nothing there"));
        assert!(matches!(found, Found::Missing));
        std::fs::write(bench.dir.join("new.txt"), b"made by someone else\n").unwrap();
        refusal(target.commit(&bench.env, &found, b"the tool's version\n"));
        assert_eq!(
            bench.read("new.txt").as_deref(),
            Some(&b"made by someone else\n"[..])
        );
        assert!(bench.checkpoints.changes().is_empty());
    }

    /// A new file's directories are made for the write, and a write that
    /// does not go through takes them away again.
    #[test]
    fn directories_made_for_a_file_that_is_not_written_are_removed_again() {
        let bench = Bench::new("parents");
        std::fs::create_dir(bench.dir.join("there")).unwrap();
        let target = bench.target("there/new/deep/file.txt");

        // The write fails, for whatever reason a write fails.
        let refused = target.put_with(&bench.env, &Found::Missing, || {
            assert!(
                bench.dir.join("there/new/deep").is_dir(),
                "the directories are there for the write"
            );
            Err(ToolResult::error("the disk is full"))
        });
        assert_eq!(
            refused.expect_err("the write failed").text(),
            "the disk is full"
        );
        assert!(
            !bench.dir.join("there/new").exists(),
            "the directories the call made are gone"
        );
        assert!(bench.dir.join("there").is_dir(), "one that was there stays");

        // A directory someone else has used in the meantime is left.
        let refused = target.put_with(&bench.env, &Found::Missing, || {
            std::fs::write(bench.dir.join("there/new/theirs.txt"), b"x").unwrap();
            Err(ToolResult::error("the disk is full"))
        });
        assert!(refused.is_err());
        assert!(!bench.dir.join("there/new/deep").exists());
        assert_eq!(
            bench.read("there/new/theirs.txt").as_deref(),
            Some(&b"x"[..])
        );

        // The whole change, when the write goes through: the directories
        // stay, with the file in them.
        let found = target
            .find(&bench.env)
            .unwrap_or_else(|_| panic!("nothing there"));
        target
            .commit(&bench.env, &found, b"content\n")
            .unwrap_or_else(|refused| panic!("{}", refused.text()));
        assert_eq!(
            bench.read("there/new/deep/file.txt").as_deref(),
            Some(&b"content\n"[..])
        );
    }

    #[test]
    fn a_file_of_too_many_lines_is_refused_before_its_lines_are_listed() {
        let bench = Bench::new("lines");
        let target = bench.target("blank.txt");
        // The content as it would have been read: no file of this size has
        // to be written for the test.
        let read = |lines: usize| Existing {
            bytes: vec![b'\n'; lines],
            hash: ContentHash::of(b""),
            stamp: Stamp {
                len: lines as u64,
                modified: None,
                identity: None,
            },
        };
        let refused = target
            .text(&read(MAX_LINES + 1))
            .expect_err("too many lines");
        assert_eq!(
            refused.text(),
            "`blank.txt` has 2,000,001 lines, and the editing tools change files of up to \
             2,000,000. Nothing was changed. To change it, use a shell command (for example \
             `sed`, or a short script)."
        );
        // At the bound it is text like any other.
        assert!(target.text(&read(MAX_LINES)).is_ok());
    }

    /// The token is asked once, before the write. A call cancelled while it
    /// was reading and matching changes nothing, and says so.
    #[test]
    fn a_call_cancelled_before_the_write_changes_nothing() {
        let bench = Bench::new("cancelled_between");
        let target = bench.seen("notes.txt", b"as it was\n");
        let found = target
            .find(&bench.env)
            .unwrap_or_else(|_| panic!("it opens"));
        bench.env.cancel.cancel();
        let refused = refusal(target.commit(&bench.env, &found, b"changed\n"));
        assert_eq!(
            refused.outcome(),
            &cortex_protocol::item::ToolOutcome::Interrupted
        );
        assert_eq!(
            refused.text(),
            "The call was interrupted before `notes.txt` was written. Nothing was changed."
        );
        assert_eq!(
            bench.read("notes.txt").as_deref(),
            Some(&b"as it was\n"[..])
        );
        assert!(bench.checkpoints.changes().is_empty());
        assert_eq!(
            bench
                .env
                .files
                .staleness(target.path.abs(), &ContentHash::of(b"as it was\n")),
            Staleness::Current,
            "the session still knows the file as it is"
        );
    }

    /// A write that goes through tells the checkpoint and the table of seen
    /// files the same thing: what the file now is.
    #[test]
    fn a_change_that_goes_through_is_recorded_as_what_the_file_now_is() {
        let bench = Bench::new("committed");
        let target = bench.seen("notes.txt", b"before\n");
        let found = target
            .find(&bench.env)
            .unwrap_or_else(|_| panic!("it opens"));
        let left = target
            .commit(&bench.env, &found, b"after\n")
            .unwrap_or_else(|refused| panic!("{}", refused.text()));
        assert_eq!(left.hash, ContentHash::of(b"after\n"));
        assert_eq!(bench.read("notes.txt").as_deref(), Some(&b"after\n"[..]));
        let changes = bench.checkpoints.changes();
        assert_eq!(changes[0].before.as_deref(), Some(&b"before\n"[..]));
        assert_eq!(changes[0].after.as_ref(), Some(&left));
        assert_eq!(bench.env.files.snapshot(target.path.abs()), Some(left));
    }
}
