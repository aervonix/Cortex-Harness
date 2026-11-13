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

/// What became of an attempt to lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Held {
    /// The lock is held.
    Locked,
    /// The file system cannot lock: nothing is held, and the work goes on.
    NoLocks,
    /// Another process holds it.
    Busy,
}

/// Takes the lock at `path` if no other process holds it: `None` when one
/// does. The file is created private if it is missing, and the lock is held
/// until it is closed. The answer says whether a lock is really held, or
/// the file system has none to give.
pub(crate) fn try_take(path: &Path) -> Result<Option<(File, Held)>> {
    let file = open(path)?;
    let held = settle(|| file.try_lock()).map_err(SessionError::io("lock", path))?;
    Ok((held != Held::Busy).then_some((file, held)))
}

/// Takes the lock at `path`, waiting while another process holds it. The
/// file is created private if it is missing, and the lock is held until it
/// is closed.
pub(crate) fn take_waiting(path: &Path) -> Result<File> {
    let file = open(path)?;
    // A lock that is waited for is never busy, so `settle` says held.
    settle(|| file.lock().map_err(TryLockError::Error)).map_err(SessionError::io("lock", path))?;
    Ok(file)
}

/// Opens the lock file at `path`, creating it private if it is missing.
fn open(path: &Path) -> Result<File> {
    let open = || OpenOptions::new().read(true).write(true).open(path);
    match open() {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match private::create_new(path) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(SessionError::io("create the lock", path)(error)),
            }
            open().map_err(SessionError::io("open the lock", path))
        }
        Err(error) => Err(SessionError::io("open the lock", path)(error)),
    }
}

/// Makes a lock attempt and says what it means.
fn settle(attempt: impl FnOnce() -> Result<(), TryLockError>) -> io::Result<Held> {
    match attempt() {
        Ok(()) => Ok(Held::Locked),
        Err(TryLockError::WouldBlock) => Ok(Held::Busy),
        // A file system that cannot lock: no lock, and no reason to stop.
        Err(TryLockError::Error(error)) if error.kind() == io::ErrorKind::Unsupported => {
            Ok(Held::NoLocks)
        }
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn failing(kind: io::ErrorKind) -> impl FnOnce() -> Result<(), TryLockError> {
        move || Err(TryLockError::Error(io::Error::from(kind)))
    }

    #[test]
    fn a_file_system_without_locks_is_no_lock() {
        assert_eq!(
            settle(failing(io::ErrorKind::Unsupported)).unwrap(),
            Held::NoLocks
        );
        assert!(settle(failing(io::ErrorKind::PermissionDenied)).is_err());
        assert_eq!(
            settle(|| Err(TryLockError::WouldBlock)).unwrap(),
            Held::Busy
        );
        assert_eq!(settle(|| Ok(())).unwrap(), Held::Locked);
    }

    /// A lock file in the temporary directory, removed with it.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_lock_is_taken_and_kept_apart() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "cortex-session-lock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        let _ = std::fs::remove_file(&scratch.0);
        let first = try_take(&scratch.0).unwrap().expect("a free lock is taken");
        assert!(try_take(&scratch.0).unwrap().is_none(), "and kept apart");
        drop(first);
        let _waited = take_waiting(&scratch.0).unwrap();
        assert!(try_take(&scratch.0).unwrap().is_none());
    }
}
