//! The Windows half of [`super`]: access lists.
//!
//! **This is the one module of `cortex-base` that may use `unsafe`**
//! (`docs/decisions/0029-owner-only-files-on-windows.md`). The standard
//! library can create a file and cannot say who may open it, so the access
//! list goes through the Windows API. Everything unsafe in the crate is in
//! this file, every block says why it is sound, and nothing here is generic:
//! each function does one fixed thing.
//!
//! How it is kept small:
//!
//! - **The access list is written as text.** A security descriptor is built
//!   from a string in Windows's own description language, in which
//!   `O:<user>D:P(A;;FA;;;<user>)` reads: owned by the user; an access list
//!   that is protected from inheritance (`P`); one entry that allows (`A`)
//!   full access to files (`FA`) to the user. Windows parses it and
//!   allocates the result. No list is laid out by hand.
//! - **Windows frees what Windows allocated.** Every such pointer goes
//!   straight into a [`LocalMemory`], whose `Drop` gives it back.
//! - **Handles are owned by standard-library types** as soon as they exist,
//!   so they are closed once, on every path.
//! - **The descriptor is given to the call that creates the object.** A file
//!   or a directory never exists without its access list.

#![allow(
    unsafe_code,
    reason = "decision 0029: an access list can only be set through the Windows API; \
              this is the one module of cortex-base where unsafe is allowed"
)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    ERROR_SUCCESS, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, INHERITED_ACE, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, READ_CONTROL,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The rights of the one entry this module writes: everything that can be
/// done to a file or a directory.
pub const FULL_CONTROL: u32 = FILE_ALL_ACCESS;

/// The two kinds of entry whose layout this module reads. They are the
/// first two values of the entry-type byte, and have been since Windows NT.
const ACE_TYPE_ALLOW: u8 = 0;
const ACE_TYPE_DENY: u8 = 1;

/// A path at least this long, in UTF-16 units, is given to Windows in its
/// extended-length form. The limit for a directory is 248 and for a file
/// 260; one lower number serves both.
const LONG_PATH: usize = 240;

/// What an entry of an access list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    /// It allows the rights to the trustee.
    Allow,
    /// It denies the rights to the trustee.
    Deny,
    /// Another kind of entry (an audit, a conditional or an object entry).
    /// Its trustee and rights are not read.
    Other,
}

/// One entry of an access list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessEntry {
    /// Whether it allows or denies.
    pub kind: AccessKind,
    /// Who it is about, as a security identifier in text (`S-1-5-…`). Empty
    /// for an entry of a kind that is not read.
    pub trustee: String,
    /// Whether the trustee is the user this process runs as.
    pub current_user: bool,
    /// The rights, as Windows's access mask. [`FULL_CONTROL`] is all of them.
    pub rights: u32,
    /// Whether the entry came down from the folder, as opposed to being set
    /// on the object itself.
    pub inherited: bool,
}

/// Who may do what to a file or a directory, read back from the file system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessList {
    /// The owner, as a security identifier in text.
    pub owner: String,
    /// Whether the owner is the user this process runs as.
    pub owned_by_current_user: bool,
    /// Whether the object takes entries from its folder. Off for everything
    /// this module creates.
    pub inherits: bool,
    /// Whether there is no access list at all, which Windows reads as
    /// "everyone may do everything".
    pub unrestricted: bool,
    /// The entries, in the order Windows applies them.
    pub entries: Vec<AccessEntry>,
}

impl AccessList {
    /// Whether only the current user has access: the user owns the object,
    /// nothing is inherited, and there is at least one entry, every one of
    /// which allows the current user.
    #[must_use]
    pub fn is_owner_only(&self) -> bool {
        self.owned_by_current_user
            && !self.inherits
            && !self.unrestricted
            && !self.entries.is_empty()
            && self.entries.iter().all(|entry| {
                entry.kind == AccessKind::Allow && entry.current_user && !entry.inherited
            })
    }
}

/// Memory that Windows allocated for its caller and that is given back with
/// `LocalFree`. Holding it here means it is freed once, on every path.
struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer was returned by a Windows function that is
            // documented to allocate its result with `LocalAlloc` and to
            // leave freeing it to the caller. It is owned by this value
            // alone, so it is freed exactly once, and no reference into the
            // memory outlives this value.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// The security identifier of the user this process runs as, in text. It is
/// read once: a process's user does not change. It is the process's user,
/// not the calling thread's: the harness never impersonates, and a thread
/// that did would still get a file for the process's user.
fn current_user() -> io::Result<&'static str> {
    static USER: OnceLock<String> = OnceLock::new();
    if let Some(user) = USER.get() {
        return Ok(user);
    }
    let user = read_current_user()?;
    Ok(USER.get_or_init(|| user))
}

fn read_current_user() -> io::Result<String> {
    /// What `GetTokenInformation` writes for `TokenUser`: the structure, and
    /// directly after it the identifier the structure points to. An
    /// identifier is never longer than `SECURITY_MAX_SID_SIZE`.
    #[repr(C)]
    struct UserBuffer {
        user: TOKEN_USER,
        identifier: [u8; SECURITY_MAX_SID_SIZE as usize],
    }
    const BUFFER_BYTES: u32 = size_of::<UserBuffer>() as u32;

    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always
    // valid and is never closed. `token` is a live, writable location for
    // the one handle the call returns.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `token` is an open handle that nothing
    // else knows about. `OwnedHandle` takes sole ownership and closes it.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };

    let mut buffer = UserBuffer {
        user: TOKEN_USER {
            User: SID_AND_ATTRIBUTES {
                Sid: ptr::null_mut(),
                Attributes: 0,
            },
        },
        identifier: [0; SECURITY_MAX_SID_SIZE as usize],
    };
    let mut written = 0_u32;
    // SAFETY: `token` is open with `TOKEN_QUERY`, the right this query
    // needs. `buffer` is writable for `BUFFER_BYTES` bytes, which is its
    // whole size, and is aligned for the `TOKEN_USER` at its start.
    // `written` is a live `u32`.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            (&raw mut buffer).cast(),
            BUFFER_BYTES,
            &raw mut written,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `buffer.user.User.Sid` points to a
    // valid identifier, which Windows wrote inside `buffer`. `buffer` is a
    // local that has not moved since the call and is alive until this
    // function returns.
    let user = unsafe { identifier_text(buffer.user.User.Sid) }?;
    // The text goes into a descriptor string below. An identifier in text is
    // `S`, digits and hyphens; anything else is refused, so nothing else can
    // ever be spliced into that string.
    if user.starts_with("S-")
        && user
            .bytes()
            .all(|byte| byte == b'S' || byte == b'-' || byte.is_ascii_digit())
    {
        Ok(user)
    } else {
        Err(io::Error::other(
            "the current user's security identifier is not in the expected form",
        ))
    }
}

/// A security identifier as text, such as `S-1-5-18`.
///
/// # Safety
///
/// `identifier` must point to a valid security identifier that stays alive
/// and unchanged for the duration of the call.
unsafe fn identifier_text(identifier: PSID) -> io::Result<String> {
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: the caller guarantees `identifier` is valid. `text` is a live,
    // writable location for the pointer the call returns.
    if unsafe { ConvertSidToStringSidW(identifier, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // From here the string is freed on every path.
    let memory = LocalMemory(text.cast());
    let mut length = 0_usize;
    // SAFETY: the call succeeded, so `text` points to a UTF-16 string that
    // Windows allocated and ended with a zero. Each read is at or before
    // that zero, so inside the allocation, which `memory` keeps alive.
    while unsafe { text.add(length).read() } != 0 {
        length += 1;
    }
    // SAFETY: the `length` units before the zero were just read one by one,
    // so they are initialised and inside the allocation. `u16` has no
    // invalid values, the pointer is aligned for it (Windows allocates
    // strings of `u16`), and `memory` outlives the slice.
    let units = unsafe { std::slice::from_raw_parts(text, length) };
    let converted = String::from_utf16(units)
        .map_err(|_| io::Error::other("a security identifier did not convert to text"));
    drop(memory);
    converted
}

/// A security descriptor that makes the current user the owner and the only
/// one with access. `for_directory` makes the entry one that files and
/// directories created inside later inherit.
fn owner_only_descriptor(for_directory: bool) -> io::Result<LocalMemory> {
    let user = current_user()?;
    // `OI` and `CI`: inherited by the files and by the directories inside.
    let inheritance = if for_directory { "OICI" } else { "" };
    let description = format!("O:{user}D:P(A;{inheritance};FA;;;{user})");
    let wide: Vec<u16> = description.encode_utf16().chain(Some(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `wide` is a zero-terminated UTF-16 string that is alive for
    // the call. `descriptor` is a live, writable location for the pointer
    // the call returns. The last argument is an optional output and may be
    // null.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalMemory(descriptor))
}

/// The structure through which a creating call takes a descriptor. It
/// borrows the descriptor: `descriptor` must outlive every use of the
/// result.
fn security_attributes(descriptor: &LocalMemory) -> SECURITY_ATTRIBUTES {
    const BYTES: u32 = size_of::<SECURITY_ATTRIBUTES>() as u32;
    SECURITY_ATTRIBUTES {
        nLength: BYTES,
        lpSecurityDescriptor: descriptor.0,
        // The handle is not handed to child processes.
        bInheritHandle: 0,
    }
}

/// A path as Windows takes it: absolute, UTF-16, ended with a zero, and in
/// the extended-length form when it is long. The standard library does the
/// same for the calls it makes itself.
fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path contains a zero character",
        ));
    }
    let mut wide = extended_length(wide);
    wide.push(0);
    Ok(wide)
}

/// Gives a long absolute path the prefix that lifts Windows's length limit.
/// A short path, and one that already has a prefix of that family, is
/// returned as it is.
fn extended_length(wide: Vec<u16>) -> Vec<u16> {
    let starts_with = |prefix: &str| {
        wide.len() >= prefix.len()
            && prefix
                .encode_utf16()
                .eq(wide[..prefix.len()].iter().copied())
    };
    if wide.len() < LONG_PATH || starts_with(r"\\?\") || starts_with(r"\\.\") {
        return wide;
    }
    let (prefix, rest) = if starts_with(r"\\") {
        // `\\server\share\…` becomes `\\?\UNC\server\share\…`.
        (r"\\?\UNC\", &wide[2..])
    } else {
        (r"\\?\", &wide[..])
    };
    prefix.encode_utf16().chain(rest.iter().copied()).collect()
}

/// Creates a new, empty file with the owner-only access list, open for
/// writing. See [`super::create_new`].
pub(super) fn create_new(path: &Path) -> io::Result<File> {
    let name = wide_path(path)?;
    let descriptor = owner_only_descriptor(false)?;
    let attributes = security_attributes(&descriptor);
    // SAFETY: `name` is a zero-terminated UTF-16 string and `attributes` a
    // fully initialised structure; both are alive for the call, and so is
    // the descriptor `attributes` points to, which `descriptor` owns until
    // this function returns. The template handle is optional and null.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            // What the standard library shares by default, so that this file
            // behaves like any other the crate opens.
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &raw const attributes,
            // Fails if the name exists.
            CREATE_NEW,
            // With "open the link itself", a link under the name counts as
            // the name existing: it is not followed, and nothing is created
            // where it points.
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        // Read before anything else can change the thread's last error.
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `handle` is an open file handle that
    // nothing else knows about. `File` takes sole ownership and closes it.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Creates one directory with the owner-only access list, which what is
/// created inside it later inherits. The parent must exist.
pub(super) fn create_dir(path: &Path) -> io::Result<()> {
    let name = wide_path(path)?;
    let descriptor = owner_only_descriptor(true)?;
    let attributes = security_attributes(&descriptor);
    // SAFETY: `name` is a zero-terminated UTF-16 string and `attributes` a
    // fully initialised structure; both are alive for the call, and so is
    // the descriptor `attributes` points to, which `descriptor` owns until
    // this function returns.
    if unsafe { CreateDirectoryW(name.as_ptr(), &raw const attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Reads the owner and the access list of the file or directory at `path`.
/// A link is followed.
///
/// For the tests of this module's own work, and for diagnostics: `cortex`
/// can say why it does not consider a file private.
///
/// # Errors
///
/// When the path cannot be opened to read its permissions, or Windows
/// refuses to report them.
pub fn access_list(path: &Path) -> io::Result<AccessList> {
    // The standard library opens it, so that long paths and odd names are
    // handled as everywhere else. The one right asked for is the right to
    // read permissions. "Backup semantics" is how Windows spells "this may
    // be a directory".
    let object = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let user = current_user()?;

    let mut owner: PSID = ptr::null_mut();
    let mut list: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `object` is an open handle with the right to read permissions,
    // alive for the call. `owner`, `list` and `descriptor` are live,
    // writable locations for the pointers the call returns. The group and
    // the audit list are not asked for, and their outputs are null.
    let status = unsafe {
        GetSecurityInfo(
            object.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            &raw mut list,
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        // This call returns its error; it does not set the thread's.
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    // `owner` and `list` point into this allocation. It is alive until the
    // end of the function, after their last use.
    let descriptor = LocalMemory(descriptor);

    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: the call above succeeded, so `descriptor` holds a valid
    // security descriptor. `control` and `revision` are live locations of
    // the types the call writes.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &raw mut control, &raw mut revision) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }

    let owner = if owner.is_null() {
        String::new()
    } else {
        // SAFETY: a non-null owner from a successful `GetSecurityInfo` is a
        // valid identifier inside `descriptor`, which is alive.
        unsafe { identifier_text(owner) }?
    };

    let mut entries = Vec::new();
    if !list.is_null() {
        let mut size = ACL_SIZE_INFORMATION {
            AceCount: 0,
            AclBytesInUse: 0,
            AclBytesFree: 0,
        };
        const SIZE_BYTES: u32 = size_of::<ACL_SIZE_INFORMATION>() as u32;
        // SAFETY: a non-null list from a successful `GetSecurityInfo` is a
        // valid access list inside `descriptor`, which is alive. `size` is
        // writable for `SIZE_BYTES` bytes, its whole size, and is the
        // structure this information class fills.
        if unsafe {
            GetAclInformation(list, (&raw mut size).cast(), SIZE_BYTES, AclSizeInformation)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        for index in 0..size.AceCount {
            let mut entry: *mut c_void = ptr::null_mut();
            // SAFETY: `list` is valid, as above, and `entry` is a live
            // location for the pointer the call returns.
            if unsafe { GetAce(list, index, &raw mut entry) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `entry` is the entry at `index`, inside the list.
            entries.push(unsafe { read_entry(entry, user) }?);
        }
    }

    let read = AccessList {
        owned_by_current_user: !owner.is_empty() && owner == user,
        owner,
        inherits: control & SE_DACL_PROTECTED == 0,
        unrestricted: list.is_null(),
        entries,
    };
    drop(descriptor);
    Ok(read)
}

/// Reads one entry of an access list.
///
/// # Safety
///
/// `entry` must point to an entry inside a valid access list that stays
/// alive and unchanged for the duration of the call, as `GetAce` returns.
unsafe fn read_entry(entry: *mut c_void, user: &str) -> io::Result<AccessEntry> {
    /// Where the trustee's identifier begins in an allow or a deny entry.
    const IDENTIFIER_AT: usize = offset_of!(ACCESS_ALLOWED_ACE, SidStart);

    // SAFETY: every entry begins with this header, and the caller guarantees
    // the entry is inside a valid list. The read is unaligned because an
    // access list is a byte layout, not a Rust value.
    let header = unsafe { entry.cast::<ACE_HEADER>().read_unaligned() };
    let inherited = u32::from(header.AceFlags) & INHERITED_ACE != 0;
    let kind = match header.AceType {
        ACE_TYPE_ALLOW => AccessKind::Allow,
        ACE_TYPE_DENY => AccessKind::Deny,
        _ => AccessKind::Other,
    };
    let unread = AccessEntry {
        kind: AccessKind::Other,
        trustee: String::new(),
        current_user: false,
        rights: 0,
        inherited,
    };
    let length = usize::from(header.AceSize);
    // An identifier is at least eight bytes. An entry too short to hold one
    // is not read as one.
    if kind == AccessKind::Other || length < IDENTIFIER_AT + 8 {
        return Ok(unread);
    }
    // SAFETY: an allow and a deny entry share this layout, and the entry is
    // `length` bytes, which was just checked to cover the whole structure.
    let body = unsafe { entry.cast::<ACCESS_ALLOWED_ACE>().read_unaligned() };
    // SAFETY: `IDENTIFIER_AT` is inside the entry's `length` bytes, so the
    // result points into the same allocation.
    let identifier: PSID = unsafe { entry.cast::<u8>().add(IDENTIFIER_AT) }.cast();
    // SAFETY: `identifier` points to at least eight readable bytes inside
    // the entry, which is all `IsValidSid` reads before it has checked the
    // identifier's own length field; `GetLengthSid` is called only once the
    // identifier is known to be valid.
    let fits = unsafe {
        IsValidSid(identifier) != 0
            && usize::try_from(GetLengthSid(identifier))
                .is_ok_and(|bytes| bytes <= length - IDENTIFIER_AT)
    };
    if !fits {
        return Ok(unread);
    }
    // SAFETY: `identifier` was just checked to be a valid identifier that
    // lies wholly inside the entry, which the caller keeps alive.
    let trustee = unsafe { identifier_text(identifier) }?;
    Ok(AccessEntry {
        kind,
        current_user: trustee == user,
        trustee,
        rights: body.Mask,
        inherited,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn a_short_path_is_passed_as_it_is() {
        let short = wide(r"C:\work\project\notes.txt");
        assert_eq!(extended_length(short.clone()), short);
    }

    #[test]
    fn a_long_path_gets_the_extended_length_prefix() {
        let long = format!(r"C:\{}\file.txt", "d".repeat(LONG_PATH));
        assert_eq!(extended_length(wide(&long)), wide(&format!(r"\\?\{long}")),);

        let share = format!(r"\\server\share\{}\file.txt", "d".repeat(LONG_PATH));
        assert_eq!(
            extended_length(wide(&share)),
            wide(&format!(r"\\?\UNC\{}", &share[2..])),
        );
    }

    #[test]
    fn a_path_that_has_a_prefix_keeps_it() {
        for prefix in [r"\\?\C:\", r"\\.\pipe\"] {
            let already = format!("{prefix}{}", "d".repeat(LONG_PATH));
            assert_eq!(extended_length(wide(&already)), wide(&already));
        }
    }

    #[test]
    fn the_current_user_is_an_identifier_and_is_read_once() {
        let first = current_user().unwrap();
        assert!(first.starts_with("S-1-"), "{first}");
        assert!(std::ptr::eq(first, current_user().unwrap()));
    }

    #[test]
    fn owner_only_means_one_owner_no_inheritance_and_only_allow_entries_for_the_user() {
        let user = |kind, current_user, inherited| AccessEntry {
            kind,
            trustee: "S-1-5-21-1-2-3-1001".to_owned(),
            current_user,
            rights: FULL_CONTROL,
            inherited,
        };
        let private = AccessList {
            owner: "S-1-5-21-1-2-3-1001".to_owned(),
            owned_by_current_user: true,
            inherits: false,
            unrestricted: false,
            entries: vec![user(AccessKind::Allow, true, false)],
        };
        assert!(private.is_owner_only());

        let not_private = [
            AccessList {
                owned_by_current_user: false,
                ..private.clone()
            },
            AccessList {
                inherits: true,
                ..private.clone()
            },
            AccessList {
                unrestricted: true,
                entries: Vec::new(),
                ..private.clone()
            },
            AccessList {
                entries: Vec::new(),
                ..private.clone()
            },
            AccessList {
                entries: vec![
                    user(AccessKind::Allow, true, false),
                    user(AccessKind::Allow, false, false),
                ],
                ..private.clone()
            },
            AccessList {
                entries: vec![
                    user(AccessKind::Allow, true, false),
                    user(AccessKind::Deny, true, false),
                ],
                ..private.clone()
            },
            AccessList {
                entries: vec![user(AccessKind::Allow, true, true)],
                ..private.clone()
            },
            AccessList {
                entries: vec![user(AccessKind::Other, false, false)],
                ..private.clone()
            },
        ];
        for list in not_private {
            assert!(!list.is_owner_only(), "{list:?}");
        }
    }
}
