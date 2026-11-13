//! Files and directories that only the current user can open
//! (`docs/decisions/0029-owner-only-files-on-windows.md`).
//!
//! Session logs, credential files and state files are private to the user.
//! On Linux that is a mode: `0600` for a file, `0700` for a directory. On
//! Windows a new file takes its folder's access rules, so privacy there is
//! an access list set on the file itself: one entry, which allows the
//! current user, with inheritance from the folder turned off.
//!
//! | Function | What it does |
//! |---|---|
//! | [`create_new`] | Creates a new, empty file that is private from the moment it exists, and returns it open for writing. |
//! | [`create_dir_all`] | Creates a directory, and any missing parents, each one private. |
//! | [`is_private`] | Whether only the current user has access to a file or a directory. |
//! | [`access_list`] | Windows only: the access list, read back. |
//!
//! [`write_atomic`](crate::write_atomic) with
//! [`owner_only`](crate::AtomicWriteOptions::owner_only) is the third way to
//! make a private file, and uses [`create_new`] for it.
//!
//! # A private file is made private, never left to its folder
//!
//! Every private file goes through [`create_new`], or through
//! [`write_atomic`](crate::write_atomic) with `owner_only`. Do not rely on
//! the folder:
//!
//! - [`create_dir_all`] leaves a directory that already exists exactly as it
//!   is. The user may have chosen its permissions, and they are not changed
//!   behind the user's back. So a file created by an ordinary call inside
//!   "the private directory" may be inside a directory that was never
//!   private.
//! - On Windows a directory this module creates does hand its access list
//!   down to what is created inside it later. That is a second line of
//!   defence, not the first: a file that inherits is not private by
//!   [`is_private`]'s definition, which asks for inheritance to be off.
//!
//! # The access list is there before the file is
//!
//! The mode, or the access list, is given to the call that creates the file.
//! There is no moment at which the file exists with wider access. Setting it
//! afterwards would leave such a moment, and a program that opened the file
//! in it would keep its handle after the rules changed.
//!
//! # Who is on the list
//!
//! The current user and nobody else, which is what `0600` means. Not the
//! SYSTEM account and not the Administrators group: both can reach any file
//! regardless (an administrator can take ownership, and backup software
//! reads with a privilege that passes access lists by), so leaving them off
//! costs nothing that works and keeps the list equal to the Linux mode. The
//! file's owner is set to the current user as well. Without that, a file
//! created from an elevated prompt would belong to the Administrators group.
//!
//! # Where the promise cannot be kept
//!
//! A file system with no permissions of its own (FAT and exFAT, some network
//! and virtual shares) takes the file and drops the mode or the access list
//! without an error. These functions do not refuse to write there. What
//! they offer instead is [`is_private`], which then says no or fails, for
//! the caller to report.
//!
//! These functions block. Async code calls them on the blocking pool.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "this is a blocking file primitive; async callers run it on the blocking pool"
)]

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{AccessEntry, AccessKind, AccessList, FULL_CONTROL, access_list};

#[cfg(not(any(unix, windows)))]
compile_error!("owner-only files are implemented for Unix and for Windows");

/// Creates a new, empty file that only the current user can open, and
/// returns it open for writing.
///
/// The file is private from the moment it exists. The directory must exist.
/// To append to the file later, open it again: it keeps its mode or its
/// access list.
///
/// # Errors
///
/// [`io::ErrorKind::AlreadyExists`] when something has the name already,
/// including a link: nothing is followed and nothing is replaced. Otherwise
/// what the operating system reports.
pub fn create_new(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The process's umask can only take bits away from this.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        windows::create_new(path)
    }
}

/// Creates a directory, and every parent that is missing, each one open to
/// the current user only.
///
/// **A directory that already exists is left as it is,** `path` itself
/// included: its permissions are the user's choice. Ask [`is_private`] when
/// it matters, and see the module's note on why a private file never relies
/// on its folder.
///
/// # Errors
///
/// When a name on the way is something other than a directory, or the
/// operating system refuses.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // Recursive creation gives the mode to every directory it makes and
        // touches none that exist.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(windows)]
    {
        // Find what is missing, outermost last.
        let mut missing = Vec::new();
        let mut current = path;
        loop {
            match std::fs::metadata(current) {
                Ok(found) if found.is_dir() => break,
                Ok(_) => return Err(io::ErrorKind::NotADirectory.into()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    missing.push(current);
                    match current.parent() {
                        // A relative path with one name left: its parent is
                        // the current directory, which exists.
                        Some(parent) if parent.as_os_str().is_empty() => break,
                        Some(parent) => current = parent,
                        None => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        for directory in missing.into_iter().rev() {
            match windows::create_dir(directory) {
                Ok(()) => {}
                // Another process made it between the look and the call.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if !std::fs::metadata(directory)?.is_dir() {
                        return Err(io::ErrorKind::NotADirectory.into());
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// Whether only the current user has access to the file or directory at
/// `path`. A link is followed: the answer is about what it points to.
///
/// - **Linux:** no permission bit is set for the group or for others.
/// - **Windows:** the owner is the current user, inheritance from the folder
///   is off, and the access list has at least one entry, every one of which
///   allows the current user. A deny entry, an entry for anyone else, an
///   inherited entry, or no access list at all (which Windows reads as
///   "everyone") each make the answer no.
///
/// This is a report, not a guard: the answer can be out of date as soon as
/// it is given.
///
/// # Errors
///
/// When the path cannot be looked at.
pub fn is_private(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        windows::access_list(path).map(|list| list.is_owner_only())
    }
}
