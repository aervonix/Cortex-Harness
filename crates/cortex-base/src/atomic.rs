//! Atomic file writes (`docs/architecture/05-tools.md`, "Atomic writes").
//!
//! New content goes to a temporary file in the same directory, is flushed
//! to disk, and is renamed over the target. A reader sees the old file or
//! the new one, never part of each, and a crash or a cancellation never
//! leaves a half-written file under the target's name.
//!
//! - **Permissions are kept.** The new file gets the mode of the one it
//!   replaces. On Windows it gets the directory's inherited access rules;
//!   rules set on the old file itself are not carried over.
//! - **Unless the write asks for an owner-only file.** With
//!   [`owner_only`](AtomicWriteOptions::owner_only) the file the write
//!   leaves can be opened by the current user alone, on both OSes, whether
//!   it existed before or not: mode `0600` on Linux, and on Windows an
//!   access list with one entry and inheritance off ([`crate::private`]).
//!   It is private from the moment its temporary file is created, before
//!   any content is in it.
//! - **A target that could not have been written in place is refused.** A
//!   rename needs only the right to change the directory, so without this
//!   it would replace a file its owner had protected. Refused: a file marked
//!   read-only (on Linux, one with no write permission at all; on Windows,
//!   one with the read-only attribute), and on Linux also a file this user
//!   may not open for writing.
//! - **Only a regular file is replaced.** A directory, a device, a socket
//!   or a pipe under the target's name is refused, not swapped for a file.
//! - **A symbolic link is written through:** the file it points to is
//!   replaced and the link stays. A link to nothing is an error.
//! - **Unless the write says the target must not be a link.** With
//!   [`follow_links`](AtomicWriteOptions::follow_links) off, a link under
//!   the target's name is refused, and nothing on the way to the rename
//!   follows one: the editing tools write to a location the path resolver
//!   has checked, and a link that appears there afterwards must not carry
//!   the write somewhere else. A rename replaces a name and never follows
//!   it, so a link swapped in at the last moment is itself replaced.
//! - **A file in use is retried.** On Windows another program can hold a
//!   file open in a way that forbids replacing it. The replace is tried
//!   again for about half a second and then fails with
//!   [`AtomicWriteError::InUse`].
//!
//! Not kept: hard links (the other names keep the old content), the owner
//! when the file belonged to someone else, and on Windows the hidden and
//! system attributes.
//!
//! Two limits. A process that is killed in the middle of a write leaves its
//! temporary file (`.cortex-….tmp`) behind. And with `sync` off, an error
//! that the OS reports only when a file is closed (a full disk, on some
//! network file systems) is not seen.
//!
//! These functions block. Async code calls them on the blocking pool. They
//! run to completion and take no cancellation token: a write that has begun
//! finishes, which is what keeps cancellation from leaving debris.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "this is the blocking file primitive; async callers run it on the blocking pool"
)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::clock::Clock;
use crate::rng::{Rng, SystemRng};

/// How an atomic write behaves.
#[derive(Debug, Clone)]
pub struct AtomicWriteOptions {
    /// Flush the new content to the disk before it takes the target's name,
    /// so that a power failure cannot leave the name pointing at an empty
    /// file. On by default. Off trades that for speed, for files that can be
    /// rebuilt.
    pub sync: bool,
    /// Leave a file that only the current user can open: mode `0600` on
    /// Linux, and on Windows an access list that allows the current user
    /// alone, with inheritance from the directory off. For session,
    /// credential and state files.
    ///
    /// It holds whether or not the target existed: a file that was readable
    /// by others is private after the write. The option only ever tightens.
    /// Without it, a new file gets the default permissions and an existing
    /// one keeps its mode.
    pub owner_only: bool,
    /// Write through a symbolic link under the target's name: replace the
    /// file it points to, and keep the link. On by default.
    ///
    /// Off, a link there is refused with [`AtomicWriteError::IsALink`], and
    /// the write never follows one. For a caller that has already resolved
    /// the path and must write exactly there.
    pub follow_links: bool,
    /// How long to wait before each new attempt to replace a file that
    /// another program holds open. Their sum is the longest the write waits.
    /// Used on Windows only: elsewhere a file in use can be replaced.
    pub retry_waits: Vec<Duration>,
}

impl Default for AtomicWriteOptions {
    /// Synced, default permissions for new files, links written through,
    /// and six retries over half a second: 10, 20, 40, 80, 150 and 200 ms.
    fn default() -> Self {
        Self {
            sync: true,
            owner_only: false,
            follow_links: true,
            retry_waits: [10, 20, 40, 80, 150, 200]
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
        }
    }
}

/// Why an atomic write did not happen. The target is unchanged in every
/// case, and the temporary file has been removed.
#[derive(Debug, thiserror::Error)]
pub enum AtomicWriteError {
    /// The target is marked read-only, or this user may not write to it.
    #[error("`{path}` is read-only, so it was not replaced")]
    ReadOnly {
        /// The target.
        path: String,
    },
    /// The target is a symbolic link, and the write was told not to follow
    /// one ([`AtomicWriteOptions::follow_links`]).
    #[error("`{path}` is a link, so it was not written through")]
    IsALink {
        /// The target.
        path: String,
    },
    /// The target is a directory.
    #[error("`{path}` is a directory, not a file")]
    IsADirectory {
        /// The target.
        path: String,
    },
    /// The target is something other than a file or a directory: a device,
    /// a socket, a pipe.
    #[error("`{path}` is not a regular file, so it was not replaced")]
    NotAFile {
        /// The target.
        path: String,
    },
    /// Another program held the target open for the whole time the write
    /// was prepared to wait.
    #[error(
        "`{path}` is open in another program, which kept it locked for {waited:?} \
         ({attempts} attempts): close it there and try again"
    )]
    InUse {
        /// The target.
        path: String,
        /// How many times the replace was tried.
        attempts: usize,
        /// How long the write waited in all.
        waited: Duration,
        /// What the operating system reported on the last attempt.
        #[source]
        source: io::Error,
    },
    /// The file system refused a step of the write.
    #[error("could not {step} for `{path}`: {source}")]
    Io {
        /// The target.
        path: String,
        /// The step that failed, as a phrase: "create a temporary file".
        step: &'static str,
        /// What the operating system reported.
        #[source]
        source: io::Error,
    },
}

/// Replaces the content of `path` with `contents`, atomically, or creates
/// the file. The directory must exist.
///
/// `clock` supplies the waits between retries, so a test with a
/// [`FakeClock`](crate::FakeClock) never really waits.
pub fn write_atomic(
    path: &Path,
    contents: &[u8],
    options: &AtomicWriteOptions,
    clock: &dyn Clock,
) -> Result<(), AtomicWriteError> {
    let shown = path.display().to_string();
    let io_error = |step: &'static str| {
        let path = shown.clone();
        move |source: io::Error| AtomicWriteError::Io { path, step, source }
    };

    // What is there now, and where the bytes really go.
    let (target, existing) = if options.follow_links {
        let target = match fs::symlink_metadata(path) {
            Ok(found) if found.file_type().is_symlink() => {
                fs::canonicalize(path).map_err(io_error("follow the symbolic link"))?
            }
            _ => path.to_path_buf(),
        };
        let existing = match fs::metadata(&target) {
            Ok(found) => Some(found),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error("look at the file")(error)),
        };
        (target, existing)
    } else {
        // The name itself is looked at, and nothing below follows it.
        let existing = match fs::symlink_metadata(path) {
            Ok(found) if found.file_type().is_symlink() => {
                return Err(AtomicWriteError::IsALink { path: shown });
            }
            Ok(found) => Some(found),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error("look at the file")(error)),
        };
        (path.to_path_buf(), existing)
    };
    if let Some(found) = &existing {
        if found.is_dir() {
            return Err(AtomicWriteError::IsADirectory { path: shown });
        }
        if !found.is_file() {
            return Err(AtomicWriteError::NotAFile { path: shown });
        }
        if found.permissions().readonly() || !may_write(&target) {
            return Err(AtomicWriteError::ReadOnly { path: shown });
        }
    }

    let directory = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    // The temporary file must never be easier to read than the file it
    // will become. An owner-only file is created owner-only and stays so.
    // Otherwise, when it replaces a file, it starts private and takes that
    // file's mode once the content is in it.
    let access = if options.owner_only {
        NewFile::OwnerOnly
    } else if existing.is_some() {
        NewFile::PrivateForNow
    } else {
        NewFile::Ordinary
    };
    let (temporary, mut file) =
        create_temporary(&directory, access).map_err(io_error("create a temporary file"))?;

    // From here on, any failure removes the temporary file.
    let prepared = (|| {
        file.write_all(contents)
            .map_err(io_error("write the new content"))?;
        if let Some(found) = &existing
            && !options.owner_only
        {
            file.set_permissions(found.permissions())
                .map_err(io_error("copy the file's permissions"))?;
        }
        if options.sync {
            file.sync_all().map_err(io_error("flush the new content"))?;
        }
        Ok(())
    })();
    // Closed before the rename: Windows will not rename an open file.
    drop(file);
    let finished = prepared.and_then(|()| {
        replace_with_retry(
            || fs::rename(&temporary, &target),
            |error| in_use(error, &target),
            &options.retry_waits,
            clock,
        )
        .map_err(|failure| match failure {
            ReplaceFailure::InUse {
                attempts,
                waited,
                source,
            } => AtomicWriteError::InUse {
                path: shown.clone(),
                attempts,
                waited,
                source,
            },
            ReplaceFailure::Other(source) => io_error("put the new file in place")(source),
        })
    });
    if finished.is_err() {
        // Best effort: the write has already failed, and that is the error
        // to report.
        let _ = fs::remove_file(&temporary);
        return finished;
    }
    if options.sync {
        sync_directory(&directory);
    }
    Ok(())
}

/// Who may open a temporary file when it is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewFile {
    /// Whoever may open any new file here.
    Ordinary,
    /// The owner alone, until the file takes the mode of the one it
    /// replaces. On Windows this is `Ordinary`: the file it replaces has the
    /// directory's access rules, and so does this one.
    PrivateForNow,
    /// The owner alone, for good.
    OwnerOnly,
}

/// Creates a new, empty file with a name of its own in `directory`.
fn create_temporary(directory: &Path, access: NewFile) -> io::Result<(PathBuf, File)> {
    let mut rng = SystemRng::new();
    let mut last_error = None;
    for _ in 0..16 {
        let name = format!(".cortex-{:016x}.tmp", rng.next_u64());
        let path = directory.join(name);
        match create_file(&path, access) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::ErrorKind::AlreadyExists.into()))
}

/// Creates one new file, open for writing. It fails if the name exists, and
/// does not follow a link someone put there.
fn create_file(path: &Path, access: NewFile) -> io::Result<File> {
    if access == NewFile::OwnerOnly {
        // The mode, or the access list, is given to the call that creates
        // the file: it never exists without it.
        return crate::private::create_new(path);
    }
    let mut open = OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The process's umask still applies to the wider mode, as it does
        // for any file a program creates.
        open.mode(if access == NewFile::PrivateForNow {
            0o600
        } else {
            0o666
        });
    }
    open.open(path)
}

/// Makes the rename itself durable, where the OS has a way to. Failure is
/// ignored: the content is in place, and some file systems cannot do this.
#[cfg(unix)]
fn sync_directory(directory: &Path) {
    if let Ok(handle) = File::open(directory) {
        let _ = handle.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) {}

/// Whether this user may open the file for writing, which is what writing
/// it in place would have needed. Opening changes nothing.
///
/// The open follows no link and waits for nothing: the look at the target
/// was a moment ago, and a FIFO put there since would otherwise hold the
/// write until some program read from it.
#[cfg(unix)]
fn may_write(target: &Path) -> bool {
    crate::nofollow::may_open_for_writing(target)
}

/// On Windows the read-only attribute is the whole check. Opening the file
/// would fail for a file that is merely in use, which is not a refusal but
/// something to wait for.
#[cfg(not(unix))]
fn may_write(_: &Path) -> bool {
    true
}

/// What a failed rename says about the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "only Windows refuses to replace a file in use; the retry loop is tested everywhere"
    )
)]
enum Blocked {
    /// Another program has it open: worth waiting for.
    InUse,
    /// The rename was refused and nothing says why. It may be a program
    /// that had the file open a moment ago, so it is worth trying again,
    /// and if it goes on it is reported as what the OS said, not as a file
    /// in use.
    Unclear,
    /// Something waiting will not fix.
    No,
}

/// What a failed rename says about the target.
#[cfg(windows)]
fn in_use(error: &io::Error, target: &Path) -> Blocked {
    // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION, and
    // ERROR_USER_MAPPED_FILE (another program has the file mapped into its
    // memory).
    const LOCKED: [i32; 3] = [32, 33, 1224];
    // ERROR_ACCESS_DENIED: what a rename reports for a target that someone
    // has open, and also what a real permission problem reports.
    const ACCESS_DENIED: i32 = 5;
    match error.raw_os_error() {
        Some(code) if LOCKED.contains(&code) => Blocked::InUse,
        Some(ACCESS_DENIED) => {
            // Tell the two apart by opening the target for writing. If that
            // is refused for sharing, someone holds it. If that is refused
            // for access too, waiting will not help.
            match OpenOptions::new().write(true).open(target) {
                Ok(_) => Blocked::Unclear,
                Err(probe)
                    if probe
                        .raw_os_error()
                        .is_some_and(|code| LOCKED.contains(&code)) =>
                {
                    Blocked::InUse
                }
                Err(_) => Blocked::No,
            }
        }
        _ => Blocked::No,
    }
}

/// What a failed rename says about the target. Off Windows it never means
/// "in use": a file in use can be replaced.
#[cfg(not(windows))]
fn in_use(_: &io::Error, _: &Path) -> Blocked {
    Blocked::No
}

/// Why a replace gave up.
#[derive(Debug)]
enum ReplaceFailure {
    /// The target stayed in use through every retry.
    InUse {
        attempts: usize,
        waited: Duration,
        source: io::Error,
    },
    /// Anything else: something that was not retried, or a refusal with no
    /// stated reason that outlasted the retries.
    Other(io::Error),
}

/// Runs `replace`, and while it fails in a way that waiting may fix, waits
/// and runs it again, once per entry of `waits`.
fn replace_with_retry(
    mut replace: impl FnMut() -> io::Result<()>,
    blocked: impl Fn(&io::Error) -> Blocked,
    waits: &[Duration],
    clock: &dyn Clock,
) -> Result<(), ReplaceFailure> {
    let mut waited = Duration::ZERO;
    let mut attempts = 0;
    let mut waits = waits.iter();
    loop {
        attempts += 1;
        let error = match replace() {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        let state = blocked(&error);
        if state == Blocked::No {
            return Err(ReplaceFailure::Other(error));
        }
        let Some(&wait) = waits.next() else {
            return Err(match state {
                Blocked::InUse => ReplaceFailure::InUse {
                    attempts,
                    waited,
                    source: error,
                },
                Blocked::Unclear | Blocked::No => ReplaceFailure::Other(error),
            });
        };
        clock.sleep(wait);
        waited += wait;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::clock::FakeClock;

    fn locked() -> io::Error {
        io::Error::other("locked")
    }

    fn is_locked(error: &io::Error) -> Blocked {
        match error.to_string().as_str() {
            "locked" => Blocked::InUse,
            "refused" => Blocked::Unclear,
            _ => Blocked::No,
        }
    }

    #[test]
    fn a_replace_that_works_is_tried_once_and_does_not_wait() {
        let clock = FakeClock::default();
        let calls = Cell::new(0);
        let outcome = replace_with_retry(
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
            is_locked,
            &AtomicWriteOptions::default().retry_waits,
            &clock,
        );
        assert!(outcome.is_ok());
        assert_eq!(calls.get(), 1);
        assert!(clock.sleeps().is_empty());
    }

    #[test]
    fn a_file_that_is_let_go_is_replaced_after_the_waits_so_far() {
        let clock = FakeClock::default();
        let calls = Cell::new(0);
        let outcome = replace_with_retry(
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 4 {
                    Err(locked())
                } else {
                    Ok(())
                }
            },
            is_locked,
            &AtomicWriteOptions::default().retry_waits,
            &clock,
        );
        assert!(outcome.is_ok());
        assert_eq!(calls.get(), 4);
        assert_eq!(
            clock.sleeps(),
            [10, 20, 40].map(Duration::from_millis),
            "it waited before each new attempt, and no longer than needed"
        );
    }

    #[test]
    fn a_file_that_stays_locked_fails_after_the_whole_schedule() {
        let clock = FakeClock::default();
        let calls = Cell::new(0);
        let waits = AtomicWriteOptions::default().retry_waits;
        let outcome = replace_with_retry(
            || {
                calls.set(calls.get() + 1);
                Err(locked())
            },
            is_locked,
            &waits,
            &clock,
        );
        match outcome {
            Err(ReplaceFailure::InUse {
                attempts,
                waited,
                source,
            }) => {
                assert_eq!(attempts, 7, "the first try and six retries");
                assert_eq!(waited, Duration::from_millis(500));
                assert_eq!(source.to_string(), "locked");
            }
            other => panic!("expected the file to stay in use, got {other:?}"),
        }
        assert_eq!(calls.get(), 7);
        assert_eq!(clock.sleeps(), waits);
        assert_eq!(clock.slept(), Duration::from_millis(500));
    }

    #[test]
    fn any_other_failure_is_not_retried() {
        let clock = FakeClock::default();
        let calls = Cell::new(0);
        let outcome = replace_with_retry(
            || {
                calls.set(calls.get() + 1);
                Err(io::ErrorKind::PermissionDenied.into())
            },
            is_locked,
            &AtomicWriteOptions::default().retry_waits,
            &clock,
        );
        assert!(matches!(
            outcome,
            Err(ReplaceFailure::Other(error)) if error.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(calls.get(), 1);
        assert!(clock.sleeps().is_empty());
    }

    #[test]
    fn an_unexplained_refusal_is_retried_and_reported_as_what_it_was() {
        // It goes away: someone had the file open for a moment.
        let clock = FakeClock::default();
        let calls = Cell::new(0);
        let outcome = replace_with_retry(
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(io::Error::other("refused"))
                } else {
                    Ok(())
                }
            },
            is_locked,
            &AtomicWriteOptions::default().retry_waits,
            &clock,
        );
        assert!(outcome.is_ok());
        assert_eq!(clock.sleeps().len(), 2);

        // It stays: the report is the refusal itself, not "in use".
        let clock = FakeClock::default();
        let outcome = replace_with_retry(
            || Err(io::Error::other("refused")),
            is_locked,
            &AtomicWriteOptions::default().retry_waits,
            &clock,
        );
        assert!(matches!(
            outcome,
            Err(ReplaceFailure::Other(error)) if error.to_string() == "refused"
        ));
        assert_eq!(clock.slept(), Duration::from_millis(500));
    }

    #[test]
    fn with_no_retries_a_locked_file_fails_at_once() {
        let clock = FakeClock::default();
        let outcome = replace_with_retry(|| Err(locked()), is_locked, &[], &clock);
        assert!(matches!(
            outcome,
            Err(ReplaceFailure::InUse { attempts: 1, waited, .. }) if waited == Duration::ZERO
        ));
    }

    /// The temporary file of a write that replaces a file holds that file's
    /// new content before it has that file's mode, so it must start private.
    #[cfg(unix)]
    #[test]
    fn a_private_temporary_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("cortex-base-{:016x}", SystemRng::new().next_u64()));
        fs::create_dir(&directory).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;

        let (private, _) = create_temporary(&directory, NewFile::PrivateForNow).unwrap();
        let (owner_only, _) = create_temporary(&directory, NewFile::OwnerOnly).unwrap();
        let (ordinary, _) = create_temporary(&directory, NewFile::Ordinary).unwrap();
        let plain = directory.join("plain");
        fs::write(&plain, "x").unwrap();
        let outcome = (mode(&private), mode(&ordinary), mode(&plain));
        let owner_only = mode(&owner_only);
        fs::remove_dir_all(&directory).unwrap();

        assert_eq!(outcome.0, 0o600);
        assert_eq!(owner_only, 0o600);
        assert_eq!(outcome.1, outcome.2, "what any new file gets here");
        let name = private.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(".cortex-") && name.ends_with(".tmp"),
            "{name}"
        );
        assert_ne!(private, ordinary);
    }

    #[test]
    fn the_messages_say_what_to_do() {
        let error = AtomicWriteError::InUse {
            path: "notes.md".to_owned(),
            attempts: 7,
            waited: Duration::from_millis(500),
            source: locked(),
        };
        let text = error.to_string();
        assert!(
            text.contains("notes.md") && text.contains("open in another program"),
            "{text}"
        );
        assert!(
            text.contains("500ms") && text.contains("7 attempts"),
            "{text}"
        );

        let error = AtomicWriteError::Io {
            path: "notes.md".to_owned(),
            step: "create a temporary file",
            source: io::ErrorKind::PermissionDenied.into(),
        };
        assert!(
            error
                .to_string()
                .starts_with("could not create a temporary file for `notes.md`")
        );
    }
}
