//! The lock that keeps two writers of a stored file apart: the trust store
//! and a project's stored approvals.
//!
//! Each change to one of those files reads it, changes it and writes it
//! whole. Two sessions doing that at once would each write what they read
//! plus their own change, and the second write would undo the first: a
//! `revoke` of a project's trust that lands between another session's read
//! and write is put back. So a change holds this lock from before the read
//! until after the write.
//!
//! - **The lock is a file beside the store,** named as the store with
//!   `.lock` added (`trust.toml.lock`), created owner-only and opened
//!   without following a link. It is in the state directory with the store,
//!   which no tool can write.
//! - **It is the operating system's lock on an open file,** so it goes with
//!   its holder however that ends, and there is no stale lock to clear.
//!   The credentials file is kept the same way (`cortex-config`).
//! - **A file system that says it cannot lock gives no lock,** and the
//!   change goes ahead: two writers are then the user's to keep apart, as
//!   they were before there was a lock, and nothing stops working. Any other
//!   failure to lock is an error, and nothing is written.
//! - **Readers take no lock.** A store is replaced in one step, so a reader
//!   sees the old file or the new one.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "blocking file calls; a store is changed at start-up and on the blocking pool"
)]

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use cortex_base::{OpenError, open_regular, private};

/// The lock file of the store at `file`.
pub(crate) fn lock_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Takes the lock of the store at `file`, creating the store's directory
/// owner-only if it is missing. The lock is held until the returned value
/// is dropped. Waits while another process holds it, which is for one read
/// and one write of a small file.
///
/// # Errors
///
/// When the directory or the lock file cannot be made or opened, or a link
/// stands at the lock file's name.
pub(crate) fn hold(file: &Path) -> io::Result<Option<File>> {
    if let Some(directory) = file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        private::create_dir_all(directory)?;
    }
    let path = lock_path(file);
    let refused = |error: OpenError| match error {
        OpenError::Io(source) => source,
        other => io::Error::other(format!("the lock file {}: {other}", path.display())),
    };
    let lock = match open_regular(&path) {
        Ok((lock, _)) => lock,
        Err(OpenError::NotFound) => {
            match private::create_new(&path) {
                Ok(_) => {}
                // Another process made it first, or something else did: the
                // open says which. A link there is not followed.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            open_regular(&path).map(|(lock, _)| lock).map_err(refused)?
        }
        Err(error) => return Err(refused(error)),
    };
    match lock.lock() {
        Ok(()) => Ok(Some(lock)),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs::TryLockError;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A directory in the temporary directory, removed with it.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "cortex-policy-lock-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Whether the lock of the store at `file` is held: a second open of the
    /// lock file cannot take it. Both Linux (`flock`) and Windows
    /// (`LockFileEx`) keep two opens of one file apart, in one process too.
    pub(crate) fn is_held(file: &Path) -> bool {
        let other = File::open(lock_path(file)).expect("the lock file is there");
        match other.try_lock() {
            Ok(()) => false,
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Error(error)) => panic!("{error}"),
        }
    }

    #[test]
    fn a_lock_is_held_until_it_is_dropped_and_its_directory_is_made() {
        let scratch = Scratch::new();
        let store = scratch.0.join("state").join("trust.toml");
        let held = hold(&store).unwrap();
        assert!(held.is_some(), "this file system can lock");
        assert!(lock_path(&store).ends_with("trust.toml.lock"));
        assert!(is_held(&store));
        drop(held);
        assert!(!is_held(&store));
    }
}
