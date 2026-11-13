//! The lock files that keep two writers apart: a session's `lock`, held by
//! the one process that writes its log, and a project's `index.lock`, held
//! for one read and one write of the index.
//!
//! Both are taken the same way, so both treat a file system that cannot
//! lock (some network and FUSE file systems) the same way: as no lock at
//! all. One writer is then the user's to keep to, as it was before locks,
//! and the work goes on rather than stops.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

use cortex_base::private;

use crate::error::{Result, SessionError};

/// Whether to wait for a lock another process holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wait {
    /// Do not wait: another holder is [`Taken::Busy`].
    No,
    /// Wait until it is free.
    Yes,
}

/// What taking a lock came to.
#[derive(Debug)]
pub(crate) enum Taken {
    /// The lock is held, or the file system has no locks: held until the
    /// file is closed.
    Held(File),
    /// Another process holds it, and [`Wait::No`] was asked for.
    Busy,
}

/// Opens the lock file at `path`, creating it private if it is missing,
/// and takes its lock.
pub(crate) fn take(path: &Path, wait: Wait) -> Result<Taken> {
    let open = || OpenOptions::new().read(true).write(true).open(path);
    let file = match open() {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match private::create_new(path) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(SessionError::io("create the lock", path)(error)),
            }
            open().map_err(SessionError::io("open the lock", path))?
        }
        Err(error) => return Err(SessionError::io("open the lock", path)(error)),
    };
    match settle(wait, |wait| match wait {
        Wait::No => file.try_lock(),
        Wait::Yes => file.lock().map_err(TryLockError::Error),
    }) {
        Ok(true) => Ok(Taken::Held(file)),
        Ok(false) => Ok(Taken::Busy),
        Err(error) => Err(SessionError::io("lock", path)(error)),
    }
}

/// Makes a lock attempt and says what it means: `true` held (or no locks to
/// hold), `false` busy.
fn settle(wait: Wait, attempt: impl FnOnce(Wait) -> Result<(), TryLockError>) -> io::Result<bool> {
    match attempt(wait) {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        // A file system that cannot lock: no lock, and no reason to stop.
        Err(TryLockError::Error(error)) if error.kind() == io::ErrorKind::Unsupported => Ok(true),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failing(kind: io::ErrorKind) -> impl FnOnce(Wait) -> Result<(), TryLockError> {
        move |_| Err(TryLockError::Error(io::Error::from(kind)))
    }

    #[test]
    fn a_file_system_without_locks_is_no_lock_whether_or_not_one_waits() {
        // The session's lock tries; the index's waits. Both go on.
        for wait in [Wait::No, Wait::Yes] {
            assert!(settle(wait, failing(io::ErrorKind::Unsupported)).unwrap());
            assert!(settle(wait, failing(io::ErrorKind::PermissionDenied)).is_err());
        }
        assert!(!settle(Wait::No, |_| Err(TryLockError::WouldBlock)).unwrap());
    }

    #[test]
    fn a_lock_is_taken_and_kept_apart() {
        let path = std::env::temp_dir().join(format!("cortex-session-lock-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let Taken::Held(first) = take(&path, Wait::No).unwrap() else {
            panic!("a free lock is taken")
        };
        assert!(matches!(take(&path, Wait::No).unwrap(), Taken::Busy));
        drop(first);
        assert!(matches!(take(&path, Wait::Yes).unwrap(), Taken::Held(_)));
        let _ = std::fs::remove_file(&path);
    }
}
