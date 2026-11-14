//! Two paths as one place on the file system, whatever form each is in.
//!
//! On Windows `std::fs::canonicalize` returns the extended-length form
//! (`\\?\C:\Work\app`, `\\?\UNC\server\share\app`), and so does
//! [`AbsPath::to_fs_path`](super::AbsPath::to_fs_path). A path from the
//! environment, the configuration or a test is written plainly
//! (`C:\work\App`). The standard library compares paths by their text,
//! component by component, so the two are different paths to it, and a
//! check of "is this under that root" said no to a path that was there. It
//! cost CI round trips twice: step 15's instruction discovery, and the
//! program search's avoided roots in step 26's fixes.
//!
//! [`same_location`], [`location_within`] and [`strip_location_prefix`]
//! compare by place:
//!
//! - **On Windows** the extended-length prefix is taken off (`\\?\C:\` is
//!   `C:\`, `\\?\UNC\server\share` is `\\server\share`); `/` and `\` are
//!   one separator; empty and `.` steps are dropped and `..` takes off the
//!   name before it, as Windows reads a plain path; and names are compared
//!   with case folded as NTFS folds it ([`PathStyle::same_name`]).
//! - **On Linux** paths are compared as they are, by
//!   [`Path`]'s own comparison: a doubled separator, a `.` step and a
//!   trailing separator do not count, `..` does (it may climb out of a
//!   symbolic link), and case does.
//!
//! **What it does not do.** It reads text and never the file system: two
//! spellings through a link, a short (8.3) name against its long one, or a
//! hard link, are not seen as one place. Canonicalize both sides for that,
//! then compare with these, since canonicalizing can fail and leave one
//! side plain. A name with trailing dots or spaces, which Windows trims in
//! a plain path and keeps in an extended-length one, is compared as
//! written. A path that is not valid Unicode on Windows is compared exactly.

use std::path::{Path, PathBuf};

use super::PathStyle;

/// Whether `first` and `second` name the same place, as the system this
/// build runs on reads them (the [module](self) documentation says how).
#[must_use]
pub fn same_location(first: &Path, second: &Path) -> bool {
    PathStyle::NATIVE.same_location(first, second)
}

/// Whether `path` is `base` or a place under it, as the system this build
/// runs on reads them. This is [`Path::starts_with`] by place.
#[must_use]
pub fn location_within(path: &Path, base: &Path) -> bool {
    PathStyle::NATIVE.location_within(path, base)
}

/// What is left of `path` below `base`, when `path` is `base` or a place
/// under it, as the system this build runs on reads them: an empty path for
/// `base` itself. This is [`Path::strip_prefix`] by place. The names that
/// are left are spelled as `path` spells them.
#[must_use]
pub fn strip_location_prefix(path: &Path, base: &Path) -> Option<PathBuf> {
    PathStyle::NATIVE.strip_location_prefix(path, base)
}

impl PathStyle {
    /// [`strip_location_prefix`] in this style. On Windows the names left
    /// are joined with `\`.
    #[must_use]
    pub fn strip_location_prefix(self, path: &Path, base: &Path) -> Option<PathBuf> {
        match self {
            Self::Posix => path.strip_prefix(base).ok().map(Path::to_path_buf),
            Self::Windows => match (Place::read(path), Place::read(base)) {
                (Some(read), Some(base_read)) => self
                    .location_within(path, base)
                    .then(|| PathBuf::from(read.names[base_read.names.len()..].join("\\"))),
                _ => path.strip_prefix(base).ok().map(Path::to_path_buf),
            },
        }
    }

    /// [`same_location`] in this style, so that the Windows rules are
    /// tested on every OS.
    #[must_use]
    pub fn same_location(self, first: &Path, second: &Path) -> bool {
        match self {
            Self::Posix => first == second,
            Self::Windows => match (Place::read(first), Place::read(second)) {
                (Some(first), Some(second)) => {
                    first.root_matches(&second) && first.names.len() == second.names.len() && {
                        first
                            .names
                            .iter()
                            .zip(&second.names)
                            .all(|(one, other)| self.same_name(one, other))
                    }
                }
                _ => first == second,
            },
        }
    }

    /// [`location_within`] in this style.
    #[must_use]
    pub fn location_within(self, path: &Path, base: &Path) -> bool {
        match self {
            Self::Posix => path.starts_with(base),
            Self::Windows => match (Place::read(path), Place::read(base)) {
                (Some(path), Some(base)) => {
                    path.root_matches(&base)
                        && path.names.len() >= base.names.len()
                        && path
                            .names
                            .iter()
                            .zip(&base.names)
                            .all(|(one, other)| self.same_name(one, other))
                }
                _ => path.starts_with(base),
            },
        }
    }
}

/// A Windows path read as a root and the names under it.
#[derive(Debug)]
struct Place<'a> {
    root: Root<'a>,
    names: Vec<&'a str>,
}

#[derive(Debug)]
enum Root<'a> {
    /// `C:` and what follows it: rooted when a separator follows the colon.
    Drive { letter: char, rooted: bool },
    /// `\\server\share`.
    Share { server: &'a str, share: &'a str },
    /// `\name`: the current drive's root.
    Rooted,
    /// `name`: relative to the current directory.
    Relative,
}

fn is_separator(character: char) -> bool {
    character == '\\' || character == '/'
}

impl<'a> Place<'a> {
    /// `None` for a path that is not valid Unicode, or an extended-length
    /// path that names neither a drive nor a share (a volume by its GUID, a
    /// device): those are compared exactly.
    fn read(path: &'a Path) -> Option<Self> {
        let text = path.to_str()?;
        let (root, rest) = if let Some(rest) = strip_verbatim(text, "UNC\\") {
            share(rest)?
        } else if let Some(rest) = strip_verbatim(text, "") {
            let (root, rest) = drive(rest)?;
            if !matches!(root, Root::Drive { rooted: true, .. }) {
                return None;
            }
            (root, rest)
        } else if text.len() >= 2 && text.chars().take(2).all(is_separator) {
            share(&text[2..])?
        } else if let Some(found) = drive(text) {
            found
        } else if text.starts_with(is_separator) {
            (Root::Rooted, text)
        } else {
            (Root::Relative, text)
        };
        let mut names: Vec<&str> = Vec::new();
        for name in rest.split(is_separator) {
            match name {
                "" | "." => {}
                ".." => {
                    names.pop();
                }
                name => names.push(name),
            }
        }
        Some(Self { root, names })
    }

    fn root_matches(&self, other: &Self) -> bool {
        let same = |one: &str, other: &str| PathStyle::Windows.same_name(one, other);
        match (&self.root, &other.root) {
            (
                Root::Drive { letter, rooted },
                Root::Drive {
                    letter: other_letter,
                    rooted: other_rooted,
                },
            ) => letter.eq_ignore_ascii_case(other_letter) && rooted == other_rooted,
            (
                Root::Share { server, share },
                Root::Share {
                    server: other_server,
                    share: other_share,
                },
            ) => same(server, other_server) && same(share, other_share),
            (Root::Rooted, Root::Rooted) | (Root::Relative, Root::Relative) => true,
            _ => false,
        }
    }
}

/// The text after `\\?\` and `then`, with either separator in the prefix
/// and `then` in any case.
fn strip_verbatim<'a>(text: &'a str, then: &str) -> Option<&'a str> {
    let mut chars = text.char_indices();
    for expected in ['\\', '\\', '?', '\\'] {
        let (_, found) = chars.next()?;
        if found != expected && !(is_separator(expected) && is_separator(found)) {
            return None;
        }
    }
    let rest = &text[4..];
    let head = rest.get(..then.len())?;
    let matches = head.chars().zip(then.chars()).all(|(found, expected)| {
        found.eq_ignore_ascii_case(&expected) || (is_separator(found) && is_separator(expected))
    });
    matches.then(|| &rest[then.len()..])
}

/// `C:` at the start of `text`, and what follows it.
fn drive(text: &str) -> Option<(Root<'_>, &str)> {
    let mut chars = text.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    let rest = &text[2..];
    Some((
        Root::Drive {
            letter,
            rooted: rest.starts_with(is_separator),
        },
        rest,
    ))
}

/// `server\share` at the start of `text`, and what follows it.
fn share(text: &str) -> Option<(Root<'_>, &str)> {
    let mut parts = text.splitn(3, is_separator);
    let server = parts.next().filter(|name| !name.is_empty())?;
    let share = parts.next().filter(|name| !name.is_empty())?;
    Some((Root::Share { server, share }, parts.next().unwrap_or("")))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::PathStyle::{Posix, Windows};

    fn same(style: super::PathStyle, first: &str, second: &str) -> bool {
        style.same_location(Path::new(first), Path::new(second))
    }

    fn within(style: super::PathStyle, path: &str, base: &str) -> bool {
        style.location_within(Path::new(path), Path::new(base))
    }

    #[test]
    fn windows_reads_the_extended_length_forms_as_the_plain_ones() {
        let pairs = [
            (r"\\?\C:\Work\app", r"C:\Work\app"),
            (r"\\?\C:\Work\app", "C:/Work/app"),
            (r"\\?\C:\", r"C:\"),
            (r"\\?\UNC\server\share\app", r"\\server\share\app"),
            (r"\\?\unc\Server\Share\app", "//server/share/app"),
            (r"\\?\UNC\server\share", r"\\server\share\"),
            // Both sides extended.
            (r"\\?\C:\Work", r"\\?\c:\work"),
            // The prefix written with the other separator.
            (r"//?/C:/Work", r"C:\Work"),
        ];
        for (first, second) in pairs {
            assert!(same(Windows, first, second), "{first} and {second}");
            assert!(same(Windows, second, first), "{second} and {first}");
            assert!(within(Windows, first, second), "{first} in {second}");
        }
    }

    #[test]
    fn windows_folds_case_as_ntfs_does() {
        assert!(same(Windows, r"C:\Work\App", r"c:\WORK\app"));
        assert!(same(Windows, r"\\?\C:\Ünï", r"c:\üNÏ"));
        // `ß` has no one-character upper case, and the dotless `ı` is kept
        // apart from `i`, as NTFS keeps them.
        assert!(!same(Windows, r"C:\straße", r"C:\STRASSE"));
        assert!(!same(Windows, r"C:\ı", r"C:\i"));
        // Linux keeps case.
        assert!(!same(Posix, "/work/App", "/work/app"));
    }

    #[test]
    fn dot_steps_are_read_as_windows_reads_them_and_as_linux_compares_them() {
        assert!(same(Windows, r"C:\Work\.\app\", r"\\?\C:\Work\app"));
        assert!(same(Windows, r"C:\Work\other\..\app", r"\\?\C:\Work\app"));
        assert!(same(Windows, r"C:\Work\\app", r"C:\Work\app"));
        // `..` does not climb above the root.
        assert!(same(Windows, r"C:\..\Work", r"C:\Work"));
        // On Linux a `.` step and a doubled or trailing separator do not
        // count, and `..` does: it may climb out of a link.
        assert!(same(Posix, "/work/./app/", "/work//app"));
        assert!(!same(Posix, "/work/other/../app", "/work/app"));
    }

    #[test]
    fn a_path_that_differs_by_one_character_is_another_place() {
        let pairs = [
            (r"\\?\C:\Work\app", r"C:\Work\apq"),
            (r"\\?\C:\Work\app", r"D:\Work\app"),
            (r"\\?\C:\Work\app", r"C:\Work\app2"),
            (r"\\?\C:\Work\app", r"C:\Work"),
            (r"\\?\UNC\server\share\app", r"\\server\shard\app"),
            (r"\\?\UNC\server\share\app", r"\\servex\share\app"),
            (r"\\?\UNC\server\share\app", r"C:\server\share\app"),
            // A drive's root against the current directory on that drive.
            (r"C:\Work", "C:Work"),
            (r"\Work", "Work"),
            (r"\\?\C:\Work", r"\Work"),
        ];
        for (first, second) in pairs {
            assert!(!same(Windows, first, second), "{first} and {second}");
            assert!(!same(Windows, second, first), "{second} and {first}");
        }
        assert!(!same(Posix, "/work/app", "/work/apq"));
    }

    #[test]
    fn within_is_by_whole_names_and_by_place() {
        assert!(within(
            Windows,
            r"\\?\C:\Work\app\src\main.rs",
            r"c:\work\APP"
        ));
        assert!(within(Windows, r"C:\Work\app", r"\\?\C:\Work\app"));
        assert!(within(
            Windows,
            r"\\?\UNC\server\share\a\b",
            r"\\SERVER\share\a"
        ));
        assert!(!within(Windows, r"\\?\C:\Work\application", r"C:\Work\app"));
        assert!(!within(Windows, r"C:\Work", r"\\?\C:\Work\app"));
        assert!(!within(Windows, r"D:\Work\app", r"C:\Work"));
        assert!(within(Posix, "/work/app/src", "/work/app/"));
        assert!(!within(Posix, "/work/application", "/work/app"));
        assert!(!within(Posix, "/Work/app", "/work"));
    }

    #[test]
    fn what_is_left_below_a_base_is_spelled_as_the_path_spells_it() {
        let strip = |style: super::PathStyle, path: &str, base: &str| {
            style
                .strip_location_prefix(Path::new(path), Path::new(base))
                .map(|rest| rest.to_string_lossy().into_owned())
        };
        assert_eq!(
            strip(
                Windows,
                r"\\?\C:\Profiles\Me\Work\App\x.toml",
                r"c:\Profiles\me"
            )
            .as_deref(),
            Some(r"Work\App\x.toml")
        );
        assert_eq!(
            strip(Windows, r"\\?\C:\Profiles\Me", "C:/Profiles/me/").as_deref(),
            Some("")
        );
        assert_eq!(
            strip(Windows, r"C:\Profiles\Mee\x", r"C:\Profiles\Me"),
            None
        );
        assert_eq!(
            strip(Posix, "/srv/me/work/x", "/srv/me").as_deref(),
            Some("work/x")
        );
        assert_eq!(strip(Posix, "/srv/Me/x", "/srv/me"), None);
    }

    #[test]
    fn what_cannot_be_read_as_a_place_is_compared_exactly() {
        // A volume by its GUID, and a device: neither a drive nor a share.
        let volume = r"\\?\Volume{0b1c2d3e-0000-0000-0000-100000000000}\Work";
        assert!(same(Windows, volume, volume));
        assert!(!same(Windows, volume, r"C:\Work"));
        assert!(!same(Windows, r"\\?\C:Work", r"C:Work"));
        assert!(!same(Windows, r"\\?\UNC\server", r"\\server"));
    }
}
