//! Reading a path as text: what kind of path it is, and whether each of its
//! names is one the harness accepts. Nothing here looks at the file system.

use super::{PathError, PathStyle};

/// Where an absolute path starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Root {
    /// `/`.
    Posix,
    /// A drive, by its letter in upper case: `C:\`.
    Drive(char),
    /// A network share: `\\server\share\`.
    Share { server: String, share: String },
}

/// One step of a path, after `.` and empty steps are dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part<'a> {
    /// A file or directory name.
    Name(&'a str),
    /// `..`.
    Parent,
}

/// A path read as text: its root, if it is absolute, and its steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed<'a> {
    pub(crate) root: Option<Root>,
    pub(crate) parts: Vec<Part<'a>>,
}

/// The characters Windows does not allow in a file name, beyond the
/// separators and the control characters. A colon is among them because it
/// starts an alternate data stream: `secret.env::$DATA` is `secret.env`.
const WINDOWS_FORBIDDEN: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];

/// The device names Windows reserves in every directory.
const WINDOWS_DEVICES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

fn is_separator(style: PathStyle, character: char) -> bool {
    character == '/' || (style == PathStyle::Windows && character == '\\')
}

/// Reads `input` as a path in the given style.
///
/// Refused, with the reason: an empty path, a NUL character, and on Windows
/// every form that names a device or that Windows would quietly read as a
/// different name. See [`PathError`].
pub(crate) fn parse(input: &str, style: PathStyle) -> Result<Parsed<'_>, PathError> {
    if input.is_empty() {
        return Err(PathError::Empty);
    }
    if input.contains('\0') {
        return Err(PathError::ContainsNul {
            path: input.replace('\0', "\\0"),
        });
    }
    let (root, rest) = match style {
        PathStyle::Posix => match input.strip_prefix('/') {
            Some(rest) => (Some(Root::Posix), rest),
            None => (None, input),
        },
        PathStyle::Windows => windows_root(input)?,
    };
    let mut parts = Vec::new();
    for step in rest.split(|character| is_separator(style, character)) {
        match step {
            "" | "." => {}
            ".." => parts.push(Part::Parent),
            name => {
                check_name(name, style).map_err(|problem| problem.into_error(input, name))?;
                parts.push(Part::Name(name));
            }
        }
    }
    Ok(Parsed { root, parts })
}

/// Splits a Windows path into its root and the rest, refusing the forms
/// that are not a plain relative path, a drive path or a network share.
fn windows_root(input: &str) -> Result<(Option<Root>, &str), PathError> {
    let path = || input.to_owned();
    let separator = |character: char| is_separator(PathStyle::Windows, character);
    let mut chars = input.chars();
    let (first, second, third) = (chars.next(), chars.next(), chars.next());

    match (first, second) {
        // Two separators: a network share, or the device namespace.
        (Some(a), Some(b)) if separator(a) && separator(b) => {
            let rest = &input[2..];
            let mut steps = rest.splitn(3, separator);
            let server = steps.next().unwrap_or("");
            let share = steps.next().unwrap_or("");
            let rest = steps.next().unwrap_or("");
            // `\\.\` is the device namespace and `\\?\` the extended-length
            // form, in which `/`, `.` and `..` mean something else. Neither
            // is accepted from outside: extended-length paths are something
            // the harness builds itself, from a path it has checked.
            if matches!(server, "." | "?") {
                return Err(PathError::DevicePath { path: path() });
            }
            if server.is_empty() || share.is_empty() {
                return Err(PathError::MalformedShare { path: path() });
            }
            for name in [server, share] {
                check_name(name, PathStyle::Windows)
                    .map_err(|problem| problem.into_error(input, name))?;
            }
            let root = Root::Share {
                server: server.to_owned(),
                share: share.to_owned(),
            };
            Ok((Some(root), rest))
        }
        // A drive letter.
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => match third {
            Some(after) if separator(after) => {
                Ok((Some(Root::Drive(letter.to_ascii_uppercase())), &input[3..]))
            }
            // `C:name` is relative to whatever directory the process last
            // had on drive C, which is not something a caller can know.
            _ => Err(PathError::DriveRelative { path: path() }),
        },
        // One separator: the root of whatever drive is current.
        (Some(a), _) if separator(a) => Err(PathError::RootRelative { path: path() }),
        _ => Ok((None, input)),
    }
}

/// What is wrong with one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameProblem {
    Reserved,
    TrailingDotOrSpace,
    Forbidden(char),
}

impl NameProblem {
    pub(crate) fn into_error(self, path: &str, name: &str) -> PathError {
        let (path, name) = (path.to_owned(), name.to_owned());
        match self {
            Self::Reserved => PathError::ReservedName { path, name },
            Self::TrailingDotOrSpace => PathError::TrailingDotOrSpace { path, name },
            Self::Forbidden(character) => PathError::InvalidCharacter {
                path,
                name,
                character,
            },
        }
    }
}

/// Checks one name of a path. On POSIX every name without a NUL or a `/` is
/// an ordinary name, and those two cannot reach here. On Windows a name is
/// refused when Windows would not open the file it seems to name.
pub(crate) fn check_name(name: &str, style: PathStyle) -> Result<(), NameProblem> {
    if style == PathStyle::Posix {
        return Ok(());
    }
    if let Some(character) = name
        .chars()
        .find(|character| WINDOWS_FORBIDDEN.contains(character) || u32::from(*character) < 0x20)
    {
        return Err(NameProblem::Forbidden(character));
    }
    // Windows drops trailing dots and spaces, so `secret.env.` opens
    // `secret.env`. This also covers names made only of dots.
    if name.ends_with(['.', ' ']) {
        return Err(NameProblem::TrailingDotOrSpace);
    }
    if is_windows_device(name) {
        return Err(NameProblem::Reserved);
    }
    Ok(())
}

/// Whether Windows reads `name` as a device, whatever directory it is in.
///
/// The device is named by what comes before the first dot, with spaces
/// before the dot ignored: `NUL`, `nul.txt` and `NUL .tar.gz` are all the
/// null device. `COM` and `LPT` take one digit, which may be one of the
/// superscript digits `¹`, `²` and `³`.
fn is_windows_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    if WINDOWS_DEVICES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
    {
        return true;
    }
    let mut chars = stem.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let numbered = prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT");
    match (chars.next(), chars.next()) {
        (Some(digit), None) => numbered && (digit.is_ascii_digit() || "¹²³".contains(digit)),
        _ => false,
    }
}

/// Turns what `std::fs::canonicalize` returns on Windows into the form
/// [`parse`] reads: `\\?\C:\dir` becomes `C:\dir`, and `\\?\UNC\server\share`
/// becomes `\\server\share`. Any other extended-length path (a volume by its
/// GUID, the object namespace) is refused.
pub(crate) fn from_extended_length(text: &str) -> Result<std::borrow::Cow<'_, str>, PathError> {
    use std::borrow::Cow;
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return Ok(Cow::Borrowed(text));
    };
    let mut chars = rest.chars();
    if let (Some(letter), Some(':'), Some('\\') | None) = (chars.next(), chars.next(), chars.next())
        && letter.is_ascii_alphabetic()
    {
        return Ok(Cow::Borrowed(rest));
    }
    if let Some(share) = rest.strip_prefix(r"UNC\") {
        return Ok(Cow::Owned(format!(r"\\{share}")));
    }
    Err(PathError::DevicePath {
        path: text.to_owned(),
    })
}

/// One character as Windows compares it: in upper case, by the simple
/// one-to-one mappings of the Basic Multilingual Plane.
///
/// NTFS compares names by an upper-case table of its own, one 16-bit unit at
/// a time. This is that rule as closely as the standard library can state
/// it: a character whose upper case is one character in the same plane is
/// mapped, and every other character is left alone (`ß` stays `ß`).
pub(crate) fn fold_char(character: char) -> char {
    if character.is_ascii() {
        return character.to_ascii_uppercase();
    }
    if u32::from(character) > 0xffff {
        return character;
    }
    let mut upper = character.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) if u32::from(single) <= 0xffff => single,
        _ => character,
    }
}

/// Resolves `..` against the names before it, as text.
///
/// Returns how many `..` are left at the front (steps that climb out of
/// where the path started) and the names that remain.
pub(crate) fn collapse<'a>(parts: &[Part<'a>]) -> (usize, Vec<&'a str>) {
    let mut climbs = 0;
    let mut names = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            Part::Name(name) => names.push(*name),
            Part::Parent => {
                if names.pop().is_none() {
                    climbs += 1;
                }
            }
        }
    }
    (climbs, names)
}
