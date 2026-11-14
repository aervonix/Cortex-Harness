//! Workspace paths (`docs/architecture/05-tools.md`, "Paths" and "Windows
//! differences in one place").
//!
//! Every permission and sandbox decision about a file is a decision about a
//! path, so a path has to mean one file. This module is where a path a model
//! or a user wrote becomes one the harness can compare and check.
//!
//! There are two layers.
//!
//! **Text.** [`AbsPath`] and [`RelPath`] are paths that have been read and
//! tidied as text: separators made uniform, `.` and empty steps removed,
//! `..` resolved, and every name checked. They never touch the file system,
//! so they say nothing about symbolic links. A [`PathStyle`] says which
//! rules apply, so the Windows rules are tested on every OS.
//!
//! **The file system.** [`PathResolver`] takes a path as written and returns
//! the real location it leads to ([`ResolvedPath`]): symbolic links and
//! Windows junctions followed, the on-disk spelling of each name, and whether
//! the result is inside the workspace. **A permission check must be made on
//! a `ResolvedPath`**, never on text alone.
//!
//! **Two paths of the system's own** (a canonicalized one, one from the
//! environment) are compared as places with [`same_location`] and
//! [`location_within`], never with `==` or [`Path::starts_with`]: on
//! Windows a canonical path has the extended-length prefix and a plain one
//! does not, and the standard library calls them different.
//!
//! # What is refused
//!
//! By the reasoning of decision 0028 (enforced values fail closed), a path
//! that could mean two things is refused, not guessed at.
//! On every OS: an empty path, a NUL character, a path that is not valid
//! Unicode, and a symbolic link to something that does not exist. On
//! Windows, also: device names (`NUL`, `COM1`, `con.txt`), the device and
//! extended-length forms (`\\.\…`, `\\?\…`), a name that ends with a dot or
//! a space, a colon or any of `< > " | ? *` in a name, a path relative to a
//! drive (`C:name`) or to the current drive's root (`\name`), and a network
//! share that is not on the allow-list. Each has its own [`PathError`].
//!
//! Those Windows rules apply to [`PathStyle::Windows`] only. On Linux,
//! `notes:2026` and `name.` are ordinary file names and are accepted.
//!
//! # What this does not do
//!
//! **It does not freeze the file system.** Resolving a path and then using
//! it are two steps, and the file system can change between them: a
//! directory can be swapped for a symbolic link after the check. A resolved
//! path is therefore the right thing to check and the right thing to open,
//! and it is not a guarantee. Confining a process that is actively racing
//! the harness is the sandbox's job
//! (`docs/architecture/06-permissions-and-sandbox.md`).
//!
//! **It does not know that two names are one file.** A file with a second
//! hard link has two paths, and both are real. And Linux is taken to
//! compare names exactly: on a mount that ignores case there, two spellings
//! of one file resolve to two paths.
//!
//! **Its case folding is close to Windows's, not identical.** Windows
//! compares names by a table of its own that this crate cannot read.
//! [`PathStyle::fold`] follows the same rule with the standard library's
//! tables, which differ for a few letters outside ASCII. The resolver does
//! not depend on it: what exists is spelled by the file system, and inside
//! or outside the workspace is decided by exact comparison. Whoever matches
//! rules against [`AbsPath::key`] should hold that a deny rule may match a
//! little too much and an allow rule must not.

mod lexical;
mod location;
mod resolve;

use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use lexical::{Part, Root};

pub use location::{location_within, same_location, strip_location_prefix};
pub use resolve::{PathOptions, PathResolver, ResolvedPath};

/// How many hexadecimal digits of the hash a project key keeps: 128 bits.
const PROJECT_KEY_DIGITS: usize = 32;

/// Which operating system's rules a path follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathStyle {
    /// Linux: `/` separates, names are compared exactly, and any name
    /// without a NUL or a `/` is valid.
    Posix,
    /// Windows: `/` and `\` separate, names are compared without regard to
    /// case, and the names and forms listed in the [module](self)
    /// documentation are refused.
    Windows,
}

impl PathStyle {
    /// The style of the OS this build runs on.
    pub const NATIVE: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Posix
    };

    /// `text` as this style compares it. On POSIX that is the text. On
    /// Windows it is the text in upper case, by the one-to-one mappings
    /// Windows itself uses, so two spellings of one name fold to the same
    /// string. Rule patterns are folded the same way before they are matched
    /// against [`AbsPath::key`].
    #[must_use]
    pub fn fold(self, text: &str) -> Cow<'_, str> {
        match self {
            Self::Posix => Cow::Borrowed(text),
            Self::Windows => {
                if text
                    .chars()
                    .all(|character| lexical::fold_char(character) == character)
                {
                    Cow::Borrowed(text)
                } else {
                    Cow::Owned(text.chars().map(lexical::fold_char).collect())
                }
            }
        }
    }

    /// Whether two names are the same name in this style.
    #[must_use]
    pub fn same_name(self, first: &str, second: &str) -> bool {
        match self {
            Self::Posix => first == second,
            Self::Windows => first
                .chars()
                .map(lexical::fold_char)
                .eq(second.chars().map(lexical::fold_char)),
        }
    }
}

/// Why a path was refused.
///
/// Every variant is a refusal of one path, never a failure of the caller's
/// whole operation: code that walks a directory reports the entry and goes
/// on to the next.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The path is the empty string.
    #[error("the path is empty")]
    Empty,
    /// The path contains a NUL character, which no file system accepts and
    /// which would cut the path short in a system call.
    #[error("the path `{path}` contains a NUL character")]
    ContainsNul {
        /// The path, with each NUL written as `\0`.
        path: String,
    },
    /// The path, or a name the file system returned, is not valid Unicode.
    /// Rules match paths as text, so such a path cannot be checked.
    #[error("the path `{path}` is not valid Unicode, so it cannot be checked against any rule")]
    NotUnicode {
        /// The path, with each invalid sequence replaced by `U+FFFD`.
        path: String,
    },
    /// A Windows device or extended-length path: `\\.\…` or `\\?\…`.
    #[error(
        "`{path}` is a device or extended-length path (`\\\\.\\` or `\\\\?\\`), which is not \
         accepted: write an ordinary path"
    )]
    DevicePath {
        /// The path as written.
        path: String,
    },
    /// A Windows network path with no server or no share.
    #[error("`{path}` is not a complete network path: it needs a server and a share")]
    MalformedShare {
        /// The path as written.
        path: String,
    },
    /// A network share that the configuration does not allow. Also returned
    /// for a drive letter that is mapped to such a share.
    #[error(
        "`{path}` is on the network share `\\\\{server}\\{share}`, which the configuration \
         does not allow"
    )]
    ShareNotAllowed {
        /// The path as written.
        path: String,
        /// The server's name.
        server: String,
        /// The share's name.
        share: String,
    },
    /// A Windows path relative to a drive's current directory: `C:name`.
    #[error(
        "`{path}` is relative to the current directory of a drive, which is not a fixed \
         place: write `C:\\…` in full, or a path relative to the workspace"
    )]
    DriveRelative {
        /// The path as written.
        path: String,
    },
    /// A Windows path that starts at the root of the current drive: `\name`.
    #[error(
        "`{path}` starts at the root of whichever drive is current: write the drive letter, \
         or a path relative to the workspace"
    )]
    RootRelative {
        /// The path as written.
        path: String,
    },
    /// A name that Windows reserves for a device: `NUL`, `CON`, `COM1`,
    /// with or without an extension.
    #[error("`{path}`: `{name}` is a device name on Windows, not a file")]
    ReservedName {
        /// The path as written.
        path: String,
        /// The reserved name.
        name: String,
    },
    /// A name that ends with a dot or a space. Windows drops them, so the
    /// name would open a different file from the one written.
    #[error(
        "`{path}`: the name `{name}` ends with a dot or a space, which Windows drops, so it \
         would open a different file"
    )]
    TrailingDotOrSpace {
        /// The path as written.
        path: String,
        /// The name.
        name: String,
    },
    /// A name with a character Windows does not allow in file names. A
    /// colon is the important one: it selects an alternate data stream of
    /// the file before it.
    #[error(
        "`{path}`: the name `{name}` contains {character:?}, which Windows does not allow in a file name"
    )]
    InvalidCharacter {
        /// The path as written.
        path: String,
        /// The name.
        name: String,
        /// The character.
        character: char,
    },
    /// An absolute path was needed.
    #[error("`{path}` is not an absolute path")]
    NotAbsolute {
        /// The path as written.
        path: String,
    },
    /// A relative path was needed.
    #[error("`{path}` is not a relative path")]
    NotRelative {
        /// The path as written.
        path: String,
    },
    /// A relative path that climbs out of the directory it is relative to.
    #[error("`{path}` leads out of the directory it is relative to")]
    Escapes {
        /// The path as written.
        path: String,
    },
    /// A single name was needed, and this has a separator in it or is `.`
    /// or `..`.
    #[error("`{name}` is not a single file name")]
    NotAName {
        /// What was given.
        name: String,
    },
    /// The path begins with `~` and the home directory is not known.
    #[error("`{path}` begins with `~`, and the home directory is not known")]
    NoHome {
        /// The path as written.
        path: String,
    },
    /// A symbolic link whose target does not exist. Writing through it
    /// would create a file somewhere the path does not show.
    #[error("`{path}` is a symbolic link to something that does not exist")]
    DanglingLink {
        /// The link.
        path: String,
    },
    /// The workspace root exists and is not a directory.
    #[error("`{path}` is not a directory")]
    NotADirectory {
        /// The path.
        path: String,
    },
    /// The file system refused to resolve the path: no permission, a loop of
    /// links, a file where a directory was needed.
    #[error("`{path}` could not be resolved: {source}")]
    Io {
        /// The path being resolved.
        path: String,
        /// What the operating system reported.
        #[source]
        source: std::io::Error,
    },
}

/// An absolute path, tidied as text.
///
/// It starts at a root (`/`, a drive, or a network share), has no `.` or
/// `..` steps and no empty ones, and every name in it passed its style's
/// checks. It is shown with `/` between names on every OS, which is how
/// paths are shown to a model: `C:/Work/dev/project/src`.
///
/// **Comparison follows the style.** Two Windows paths that differ only in
/// case are equal, hash alike and sort together, and each keeps the spelling
/// it was made with.
///
/// An `AbsPath` made with [`parse`](Self::parse) has not been near the file
/// system. One taken from a [`ResolvedPath`] is the real location.
#[derive(Clone)]
pub struct AbsPath {
    style: PathStyle,
    /// The root, then the names joined by `/`. A root ends with `/`.
    text: String,
    /// How many bytes of `text` are the root.
    root_len: usize,
    /// `text` as the style compares it, when that differs from `text`.
    folded: Option<Box<str>>,
}

impl AbsPath {
    /// Reads an absolute path as text. `..` is resolved against the names
    /// before it, without asking the file system, and stops at the root.
    ///
    /// The result is not checked against the file system: a symbolic link in
    /// it is not followed, and a network share is not checked against any
    /// allow-list. Use [`PathResolver`] for a path that a permission depends
    /// on.
    pub fn parse(input: &str, style: PathStyle) -> Result<Self, PathError> {
        let parsed = lexical::parse(input, style)?;
        let Some(root) = parsed.root else {
            return Err(PathError::NotAbsolute {
                path: input.to_owned(),
            });
        };
        let (_, names) = lexical::collapse(&parsed.parts);
        Ok(Self::build(style, &root, names))
    }

    /// Reads an absolute path the operating system returned, such as the
    /// result of `std::fs::canonicalize`, in the native style.
    ///
    /// A path that is not valid Unicode is refused with
    /// [`PathError::NotUnicode`]. That is an error for this one path: a
    /// directory walk reports it and moves on.
    pub fn from_os(path: &Path) -> Result<Self, PathError> {
        let Some(text) = path.to_str() else {
            return Err(PathError::NotUnicode {
                path: path.to_string_lossy().into_owned(),
            });
        };
        Self::from_os_text(text, PathStyle::NATIVE)
    }

    /// [`from_os`](Self::from_os) for text, in a given style, so that the
    /// Windows forms can be tested everywhere.
    pub(crate) fn from_os_text(text: &str, style: PathStyle) -> Result<Self, PathError> {
        match style {
            PathStyle::Posix => Self::parse(text, style),
            PathStyle::Windows => Self::parse(&lexical::from_extended_length(text)?, style),
        }
    }

    fn build<'a>(style: PathStyle, root: &Root, names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut text = match root {
            Root::Posix => "/".to_owned(),
            Root::Drive(letter) => format!("{letter}:/"),
            Root::Share { server, share } => format!("//{server}/{share}/"),
        };
        let root_len = text.len();
        for name in names {
            if text.len() > root_len {
                text.push('/');
            }
            text.push_str(name);
        }
        Self::from_text(style, text, root_len)
    }

    fn from_text(style: PathStyle, text: String, root_len: usize) -> Self {
        let folded = match style.fold(&text) {
            Cow::Borrowed(_) => None,
            Cow::Owned(folded) => Some(folded.into_boxed_str()),
        };
        Self {
            style,
            text,
            root_len,
            folded,
        }
    }

    /// The style this path follows.
    #[must_use]
    pub fn style(&self) -> PathStyle {
        self.style
    }

    /// The path with `/` between names, as it is shown to a model:
    /// `/srv/dev/project` or `C:/Work/dev/project`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The path as its style compares it: [`as_str`](Self::as_str) on POSIX,
    /// and the same in upper case on Windows. This is what a rule's pattern
    /// is matched against, after the pattern is folded with
    /// [`PathStyle::fold`].
    #[must_use]
    pub fn key(&self) -> &str {
        self.folded.as_deref().unwrap_or(&self.text)
    }

    /// The key that names the project whose workspace root is this path:
    /// the first 32 lowercase hexadecimal digits (128 bits) of the
    /// [`ContentHash`](crate::ContentHash) of the path **as the file system
    /// spells it**.
    ///
    /// Sessions, stored approvals, project trust, statistics and the
    /// project's knowledge are all filed under it
    /// (`docs/architecture/07-sessions-and-context.md`, "On-disk layout"), so
    /// two projects must never share one: the hash is one nobody can make
    /// collide, and 128 bits of it leave a collision between two real
    /// workspaces out of reach. The key says nothing readable about the
    /// path, so a directory listing does not show the names of a user's
    /// projects.
    ///
    /// **Call it on the resolved root of a workspace**
    /// ([`PathResolver::root`]). A resolved path has every link followed and
    /// every name in the case stored on disk, so:
    ///
    /// - a link to a workspace and the workspace itself are one project;
    /// - on Windows, one folder reached under any casing has one key,
    ///   because the resolver has already put each name in its stored case;
    /// - and two folders whose names differ only in case, which NTFS allows
    ///   inside a directory marked case-sensitive, have two keys. That is
    ///   why the names are hashed as they are and not folded: folded, `App`
    ///   and `app` would share their sessions, their approvals and their
    ///   trust.
    ///
    /// Only the root is folded (a drive letter, or a share's server and
    /// name): it has no spelling on disk, and Windows never tells two roots
    /// apart by case.
    ///
    /// A path made with [`parse`](Self::parse) has not been near the file
    /// system, so on Windows two casings of it give two keys.
    #[must_use]
    pub fn project_key(&self) -> cortex_protocol::ProjectKey {
        let (root, names) = self.text.split_at(self.root_len);
        let mut spelled = self.style.fold(root).into_owned();
        spelled.push_str(names);
        let mut hex = crate::ContentHash::of(spelled.as_bytes()).to_hex();
        hex.truncate(PROJECT_KEY_DIGITS);
        cortex_protocol::ProjectKey::new(hex)
    }

    /// The names after the root, in order. Empty for a root.
    pub fn names(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.text[self.root_len..]
            .split('/')
            .filter(|name| !name.is_empty())
    }

    /// Whether this path is a root: `/`, a drive or a share, with no names.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.text.len() == self.root_len
    }

    /// The root this path starts at: `/`, `C:/` or `//server/share/`.
    #[must_use]
    pub fn root(&self) -> Self {
        Self::from_text(
            self.style,
            self.text[..self.root_len].to_owned(),
            self.root_len,
        )
    }

    /// The last name. `None` for a root.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.names().next_back()
    }

    /// The path one name shorter. `None` for a root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let cut = match self.text.rfind('/') {
            Some(slash) if slash >= self.root_len => slash,
            _ => self.root_len,
        };
        Some(Self::from_text(
            self.style,
            self.text[..cut].to_owned(),
            self.root_len,
        ))
    }

    /// The network share this path is on, as its server and share names.
    /// `None` for a path on a local drive.
    #[must_use]
    pub fn share(&self) -> Option<(&str, &str)> {
        let root = self.text[..self.root_len].strip_prefix("//")?;
        root.trim_end_matches('/').split_once('/')
    }

    /// This path with one more name, as a directory listing gives it.
    ///
    /// The name is checked like any other: it must be valid Unicode
    /// ([`PathError::NotUnicode`]) and pass the style's rules. An error is
    /// about this one entry. A directory walk reports it and goes on with
    /// the next entry.
    pub fn child(&self, name: &OsStr) -> Result<Self, PathError> {
        let Some(name) = name.to_str() else {
            return Err(PathError::NotUnicode {
                path: format!("{self}/{}", name.to_string_lossy()),
            });
        };
        let not_a_name = || PathError::NotAName {
            name: name.to_owned(),
        };
        if name.is_empty() || name.contains('\0') || matches!(name, "." | "..") {
            return Err(not_a_name());
        }
        if name.contains('/') || (self.style == PathStyle::Windows && name.contains('\\')) {
            return Err(not_a_name());
        }
        lexical::check_name(name, self.style)
            .map_err(|problem| problem.into_error(&format!("{self}/{name}"), name))?;
        let mut text = String::with_capacity(self.text.len() + 1 + name.len());
        text.push_str(&self.text);
        if !self.is_root() {
            text.push('/');
        }
        text.push_str(name);
        Ok(Self::from_text(self.style, text, self.root_len))
    }

    /// This path extended by a relative path. The relative path is read as
    /// text in this path's style and may not climb out: `a/../b` is fine and
    /// `../b` is [`PathError::Escapes`].
    pub fn join(&self, relative: &str) -> Result<Self, PathError> {
        let relative = RelPath::parse(relative, self.style)?;
        Ok(self.join_rel(&relative))
    }

    /// [`join`](Self::join) for a relative path already read in this style.
    fn join_rel(&self, relative: &RelPath) -> Self {
        if relative.is_empty() {
            return self.clone();
        }
        let mut text = String::with_capacity(self.text.len() + 1 + relative.text.len());
        text.push_str(&self.text);
        if !self.is_root() {
            text.push('/');
        }
        text.push_str(&relative.text);
        Self::from_text(self.style, text, self.root_len)
    }

    /// Whether this path is `base` or lies below it, name by name, as the
    /// style compares names. `/work/app2` does not start with `/work/app`.
    #[must_use]
    pub fn starts_with(&self, base: &Self) -> bool {
        self.strip_prefix(base).is_some()
    }

    /// This path relative to `base`: empty when they are the same place,
    /// `None` when this path is not below `base`.
    #[must_use]
    pub fn strip_prefix(&self, base: &Self) -> Option<RelPath> {
        if self.style != base.style {
            return None;
        }
        let style = self.style;
        if !style.same_name(&self.text[..self.root_len], &base.text[..base.root_len]) {
            return None;
        }
        let mut mine = self.names();
        for theirs in base.names() {
            if !style.same_name(mine.next()?, theirs) {
                return None;
            }
        }
        let rest: Vec<&str> = mine.collect();
        Some(RelPath::from_text(style, rest.join("/")))
    }

    /// [`strip_prefix`](Self::strip_prefix) with the names compared exactly,
    /// whatever the style. For two paths that are both in the file system's
    /// own spelling, which is how the resolver decides what is inside the
    /// workspace: an exact match cannot be wrong about a letter whose upper
    /// case Windows and this crate see differently.
    pub(crate) fn strip_prefix_exactly(&self, base: &Self) -> Option<RelPath> {
        if self.style != base.style {
            return None;
        }
        // The root is a drive letter or a server and share, which have no
        // spelling on disk: compared as the style compares names.
        if !self
            .style
            .same_name(&self.text[..self.root_len], &base.text[..base.root_len])
        {
            return None;
        }
        let mut mine = self.names();
        for theirs in base.names() {
            if mine.next()? != theirs {
                return None;
            }
        }
        let rest: Vec<&str> = mine.collect();
        Some(RelPath::from_text(self.style, rest.join("/")))
    }

    /// [`starts_with`](Self::starts_with) with the names compared exactly,
    /// whatever the style. For two paths that are both in the file system's
    /// own spelling, as resolved paths are.
    ///
    /// This is the test for anything that **grants**: what a lease's scope
    /// covers, what an allow rule reaches. On Windows a directory can be
    /// marked case-sensitive, and there `App` and `app` are two folders, so
    /// a folded comparison would put one inside a scope that names the
    /// other. The root (a drive letter, a share's server and name) has no
    /// spelling on disk and is still compared as the style compares names.
    #[must_use]
    pub fn starts_with_exactly(&self, base: &Self) -> bool {
        self.strip_prefix_exactly(base).is_some()
    }

    /// The path in the operating system's own spelling, for showing to a
    /// person or handing to another program: `C:\Work\dev\project`.
    #[must_use]
    pub fn to_native_string(&self) -> String {
        match self.style {
            PathStyle::Posix => self.text.clone(),
            PathStyle::Windows => self.text.replace('/', "\\"),
        }
    }

    /// The path for the standard library's file functions.
    ///
    /// On Windows this is the extended-length form, `\\?\C:\Work\…`. In
    /// that form Windows applies none of its rewriting: no limit of 260
    /// characters, no dropped dots and spaces, no device names. The path is
    /// opened exactly as it was checked. It is for file calls only, and is
    /// not shown to anyone.
    #[must_use]
    pub fn to_fs_path(&self) -> PathBuf {
        PathBuf::from(self.fs_string())
    }

    fn fs_string(&self) -> String {
        match self.style {
            PathStyle::Posix => self.text.clone(),
            PathStyle::Windows => {
                let native = self.to_native_string();
                match native.strip_prefix(r"\\") {
                    Some(share) => format!(r"\\?\UNC\{share}"),
                    None => format!(r"\\?\{native}"),
                }
            }
        }
    }
}

impl std::fmt::Display for AbsPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::fmt::Debug for AbsPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AbsPath({:?})", self.text)
    }
}

impl PartialEq for AbsPath {
    fn eq(&self, other: &Self) -> bool {
        self.style == other.style && self.key() == other.key()
    }
}

impl Eq for AbsPath {}

impl std::hash::Hash for AbsPath {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl PartialOrd for AbsPath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AbsPath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.style == PathStyle::Windows, self.key())
            .cmp(&(other.style == PathStyle::Windows, other.key()))
    }
}

/// A relative path that stays inside the directory it is relative to,
/// tidied as text: `src/main.rs`.
///
/// It has no root, no `.` or `..` steps and no empty ones, and every name
/// passed its style's checks. The empty path is the directory itself, and is
/// shown as `.`. Comparison follows the style, as for [`AbsPath`].
///
/// Paths inside a workspace travel in the protocol in this form.
#[derive(Clone)]
pub struct RelPath {
    style: PathStyle,
    /// The names joined by `/`.
    text: String,
    folded: Option<Box<str>>,
}

impl RelPath {
    /// Reads a relative path as text. `..` is resolved against the names
    /// before it. A path that is absolute is [`PathError::NotRelative`], and
    /// one that climbs out of where it started is [`PathError::Escapes`].
    pub fn parse(input: &str, style: PathStyle) -> Result<Self, PathError> {
        let parsed = lexical::parse(input, style)?;
        if parsed.root.is_some() {
            return Err(PathError::NotRelative {
                path: input.to_owned(),
            });
        }
        let (climbs, names) = lexical::collapse(&parsed.parts);
        if climbs > 0 {
            return Err(PathError::Escapes {
                path: input.to_owned(),
            });
        }
        Ok(Self::from_text(style, names.join("/")))
    }

    fn from_text(style: PathStyle, text: String) -> Self {
        let folded = match style.fold(&text) {
            Cow::Borrowed(_) => None,
            Cow::Owned(folded) => Some(folded.into_boxed_str()),
        };
        Self {
            style,
            text,
            folded,
        }
    }

    /// The names joined by `/`. Empty for the directory itself.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The path as its style compares it. See [`AbsPath::key`].
    #[must_use]
    pub fn key(&self) -> &str {
        self.folded.as_deref().unwrap_or(&self.text)
    }

    /// Whether this is the directory itself.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The names, in order.
    pub fn names(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.text.split('/').filter(|name| !name.is_empty())
    }

    /// The style this path follows.
    #[must_use]
    pub fn style(&self) -> PathStyle {
        self.style
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.text.is_empty() {
            "."
        } else {
            &self.text
        })
    }
}

impl std::fmt::Debug for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RelPath({:?})", self.text)
    }
}

impl PartialEq for RelPath {
    fn eq(&self, other: &Self) -> bool {
        self.style == other.style && self.key() == other.key()
    }
}

impl Eq for RelPath {}

impl std::hash::Hash for RelPath {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

/// Splits a path as written into its root and steps, for the resolver.
pub(crate) fn steps(
    input: &str,
    style: PathStyle,
) -> Result<(Option<Root>, Vec<Part<'_>>), PathError> {
    let parsed = lexical::parse(input, style)?;
    Ok((parsed.root, parsed.parts))
}

/// The root of an absolute path, as a path of its own.
pub(crate) fn root_path(style: PathStyle, root: &Root) -> AbsPath {
    AbsPath::build(style, root, [])
}

#[cfg(test)]
mod tests;
