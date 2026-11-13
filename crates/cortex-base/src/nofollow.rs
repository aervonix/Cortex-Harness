//! Opening a regular file without following a link and without waiting
//! (`docs/architecture/05-tools.md`, "Paths", "What resolution does not
//! promise").
//!
//! A path is checked, and then it is opened. Between the two, whatever is
//! at the path can be swapped: for a link to a file somewhere else, or for
//! a FIFO, whose open waits until another program opens its other end.
//! [`open_regular`] closes that gap for the last name of the path. It does
//! not look and then open. It opens, in a way that neither follows a link
//! nor waits, and then asks the handle it holds what it is. Whatever the
//! caller goes on to read is the thing that was checked.
//!
//! | | How |
//! |---|---|
//! | Linux | `O_NOFOLLOW` and `O_NONBLOCK` on the open: a link is an error, and a FIFO opens at once. The handle's own metadata then says whether it is a regular file. |
//! | Windows | The open asks for the reparse point itself, not for what it leads to. A link, a junction or any other link-like reparse point is refused. |
//!
//! **What it does not cover.** Only the last name is protected. A directory
//! earlier in the path that is swapped for a link is still followed: a
//! process that races the harness that way is the sandbox's to confine
//! (`docs/architecture/06-permissions-and-sandbox.md`). Callers pass a
//! path the resolver has already resolved, so in the ordinary case no name
//! on the way is a link.
//!
//! **Placeholders on Windows.** Files in a folder synced with a cloud
//! service, or projected by a virtual file system, are reparse points that
//! are not links: their content is fetched or decompressed by a filter when
//! they are opened in the ordinary way. For those, and only for the tags
//! listed in [`is_placeholder_tag`], the file is opened a second time in
//! the ordinary way. Between the two opens the placeholder could be swapped
//! for a link, which the second open would follow: a window this module
//! leaves open, for that kind of file only. A reparse tag that is not on
//! the list is refused as a link.
//!
//! These functions block. Async code calls them on the blocking pool.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "this is a blocking file primitive; async callers run it on the blocking pool"
)]

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

/// Why [`open_regular`] did not return a file. Nothing was read.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// Nothing has that name.
    #[error("there is no such file")]
    NotFound,
    /// The name is a symbolic link, or on Windows a junction or another
    /// link-like reparse point. It was not followed.
    #[error("it is a link, which is not followed here")]
    Link,
    /// The name is a directory.
    #[error("it is a directory")]
    Directory,
    /// The name is a device, a FIFO or a socket. It was not read.
    #[error("it is not a regular file")]
    NotAFile,
    /// The operating system refused the open: no permission, or on Windows
    /// a file another program holds without sharing.
    #[error("{0}")]
    Io(#[source] io::Error),
}

impl OpenError {
    /// Whether another program holds the file open in a way that forbids
    /// reading it. Only Windows refuses an open for that.
    #[must_use]
    pub fn is_in_use(&self) -> bool {
        // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION.
        cfg!(windows)
            && matches!(self, Self::Io(error) if matches!(error.raw_os_error(), Some(32 | 33)))
    }
}

/// Opens the regular file at `path` for reading, and returns it with the
/// metadata of the handle itself.
///
/// The last name of `path` is never followed if it is a link, and the open
/// never waits on a FIFO or a device. What is returned is a regular file,
/// checked on the open handle, so it cannot have been swapped since.
///
/// # Errors
///
/// [`OpenError`] says what was there instead. Nothing was read from it.
pub fn open_regular(path: &Path) -> Result<(File, Metadata), OpenError> {
    let file = open_unfollowed(path)?;
    let metadata = file.metadata().map_err(OpenError::Io)?;
    let kind = metadata.file_type();
    if kind.is_symlink() {
        // Only Windows can hand back a handle to a link itself.
        Err(OpenError::Link)
    } else if kind.is_dir() {
        Err(OpenError::Directory)
    } else if kind.is_file() {
        Ok((file, metadata))
    } else {
        Err(OpenError::NotAFile)
    }
}

/// Whether the current user may open the file at `path` for writing, asked
/// without following a link and without waiting: a FIFO nobody reads is
/// not waited for. Opening changes nothing.
///
/// Anything but a refusal for lack of permission counts as "may": the
/// caller is about to replace the name, and finds out the rest there.
#[cfg(unix)]
pub(crate) fn may_open_for_writing(path: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    match OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::PermissionDenied,
    }
}

#[cfg(unix)]
fn open_unfollowed(path: &Path) -> Result<File, OpenError> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        // No link is followed at the last name, and nothing is waited for:
        // a FIFO opens at once, with or without a writer. On a regular file
        // the second flag changes nothing.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            _ if error.kind() == io::ErrorKind::NotFound => OpenError::NotFound,
            // What `O_NOFOLLOW` reports for a link: `ELOOP` on Linux, and
            // `EMLINK` on some other Unix systems.
            Some(libc::ELOOP | libc::EMLINK) => OpenError::Link,
            // A socket, or a device with nothing behind it.
            Some(libc::ENXIO | libc::ENODEV) => OpenError::NotAFile,
            _ => OpenError::Io(error),
        })
}

#[cfg(windows)]
fn open_unfollowed(path: &Path) -> Result<File, OpenError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    let from_io = |error: io::Error| {
        if error.kind() == io::ErrorKind::NotFound {
            OpenError::NotFound
        } else {
            OpenError::Io(error)
        }
    };
    // "Backup semantics" is how Windows spells "this may be a directory":
    // without it a directory is refused as if for lack of permission, and
    // could not be told from a file the user may not read.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(from_io)?;
    let metadata = file.metadata().map_err(OpenError::Io)?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(file);
    }
    let tag = crate::private::reparse_tag(&file).map_err(OpenError::Io)?;
    if !is_placeholder_tag(tag) {
        return Err(OpenError::Link);
    }
    // A placeholder, not a link. The handle above is to the stub; the
    // content comes through an ordinary open, which lets the filter that
    // owns the placeholder do its work. See the module's note on the
    // window this leaves.
    drop(file);
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(from_io)
}

/// Whether a Windows reparse tag marks a file whose content a filter
/// supplies (a cloud placeholder, a projected, deduplicated or compressed
/// file), as opposed to a name that leads somewhere else.
///
/// It is an allow-list: a tag that is not on it is treated as a link and
/// refused, which is the safe answer for a tag this build has not heard
/// of. Not on it, among others: symbolic links and junctions, WSL's links
/// and special files, app execution aliases, and the NFS and DFS tags.
#[must_use]
pub fn is_placeholder_tag(tag: u32) -> bool {
    /// `IO_REPARSE_TAG_CLOUD` and its sixteen numbered variants differ in
    /// one hexadecimal digit.
    const CLOUD_MASK: u32 = 0xffff_0fff;
    const CLOUD: u32 = 0x9000_001a;
    const OTHERS: [u32; 10] = [
        0xc000_0004, // HSM
        0x8000_0006, // HSM2
        0x8000_0007, // SIS (single-instance storage)
        0x8000_0008, // WIM
        0x8000_0013, // DEDUP
        0x8000_0015, // FILE_PLACEHOLDER
        0x8000_0017, // WOF (compressed by the system)
        0x9000_001c, // PROJFS (a projected file system, as VFS for Git)
        0x8000_001e, // STORAGE_SYNC
        0x8000_0021, // ONEDRIVE
    ];
    tag & CLOUD_MASK == CLOUD || OTHERS.contains(&tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tags_whose_content_a_filter_supplies_are_placeholders() {
        // Cloud files, in every numbered variant.
        assert!(is_placeholder_tag(0x9000_001a));
        assert!(is_placeholder_tag(0x9000_101a));
        assert!(is_placeholder_tag(0x9000_f01a));
        assert!(is_placeholder_tag(0x9000_001c), "ProjFS");
        assert!(is_placeholder_tag(0x8000_0021), "OneDrive");
        assert!(is_placeholder_tag(0x8000_0013), "deduplicated");
        assert!(is_placeholder_tag(0x8000_0017), "compressed");

        // Everything that leads somewhere else, and everything unknown.
        for (tag, what) in [
            (0xa000_000c, "a symbolic link"),
            (0xa000_0003, "a junction"),
            (0xa000_001d, "a WSL symbolic link"),
            (0x8000_001b, "an app execution alias"),
            (0xa000_0019, "a global reparse point"),
            (0x8000_000a, "DFS"),
            (0x8000_0012, "DFSR"),
            (0x8000_0014, "NFS"),
            (0x8000_0023, "a WSL socket"),
            (0x8000_0024, "a WSL FIFO"),
            (0x8000_0025, "a WSL character device"),
            (0x8000_0026, "a WSL block device"),
            (0, "no tag"),
            (0x1234_5678, "a tag nobody has heard of"),
            (0x9000_001b, "a neighbour of the cloud tag"),
        ] {
            assert!(!is_placeholder_tag(tag), "{what} is not a placeholder");
        }
    }

    #[test]
    fn an_error_says_what_was_there() {
        assert_eq!(OpenError::NotFound.to_string(), "there is no such file");
        assert_eq!(
            OpenError::Link.to_string(),
            "it is a link, which is not followed here"
        );
        assert_eq!(OpenError::Directory.to_string(), "it is a directory");
        assert_eq!(OpenError::NotAFile.to_string(), "it is not a regular file");
        let denied = OpenError::Io(io::ErrorKind::PermissionDenied.into());
        assert!(!denied.is_in_use());
        let sharing = OpenError::Io(io::Error::from_raw_os_error(32));
        assert_eq!(sharing.is_in_use(), cfg!(windows));
    }
}
