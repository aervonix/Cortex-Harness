//! The patterns of permission rules
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Rules").
//!
//! Two matchers, both our own and both small. The search libraries have a
//! glob matcher, and only `cortex-tools` may link them
//! (`docs/architecture/01-workspace.md`, rule 5).
//!
//! - [`NamePattern`] matches one run of text: a file name, one word of a
//!   command, a host, the name of a tool.
//! - [`PathGlob`] matches an absolute path, name by name.
//!
//! # The syntax
//!
//! | Written | Matches |
//! |---|---|
//! | `*` | Any run of characters, within one name. |
//! | `?` | Any one character, within one name. |
//! | `[abc]`, `[a-z]`, `[!a-z]` | One character that is, or is not, in the set. |
//! | `{a,b}` | Either alternative. In a path glob only. |
//! | `**` | As a whole name of a path: any number of names, none included. |
//!
//! There is no escape character. A character that would otherwise be
//! special is written as a set of one: `[*]`. On Windows a `\` in a path
//! glob is a separator, like `/`.
//!
//! # Where a path glob is anchored
//!
//! | Written | Read as |
//! |---|---|
//! | `/etc/**`, `C:/Tools/**`, `//server/share/**` | An absolute path. |
//! | `~/notes/**` | Under the home directory. |
//! | `**/.env` | Anywhere, in the workspace or outside it. |
//! | `src/**`, `docs/*.md` | A pattern with a `/`: under the workspace root. |
//! | `*.lock`, `Cargo.toml` | A pattern with no `/`: that name at any depth in the workspace. |
//!
//! A pattern that ends in `/**` (or in `/`) matches the directory itself as
//! well as everything in it, so that a rule on a tree covers a listing of
//! its top.
//!
//! # Case
//!
//! A glob is matched exactly or folded, and the caller says which. Folded
//! is how a rule that denies or asks is matched on Windows: two spellings of
//! one name match alike, and where that matches too much (in a directory
//! marked case-sensitive, `App` and `app` are two folders) it is on the
//! safe side. A rule that allows is matched exactly, against the path as
//! the file system spells it, so that an allow for `App` never grants
//! `app`. On Linux the two are the same.

use cortex_base::{AbsPath, PathStyle};

/// The most alternatives one pattern's braces may expand to.
const MAX_ALTERNATIVES: usize = 64;

/// The most `**` one alternative may have. Each is a place where the
/// matcher tries every split of the remaining names.
const MAX_ANY_DEPTH: usize = 4;

/// Why a pattern was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the pattern `{pattern}` is not valid: {problem}")]
pub struct GlobError {
    /// The pattern, as written.
    pub pattern: String,
    /// What is wrong with it, and what to write in its place.
    pub problem: String,
}

impl GlobError {
    fn new(pattern: &str, problem: impl Into<String>) -> Self {
        Self {
            pattern: pattern.to_owned(),
            problem: problem.into(),
        }
    }
}

/// One piece of a pattern, inside one name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// This character.
    Literal(char),
    /// `?`: any one character.
    AnyOne,
    /// `*`: any run of characters.
    AnyRun,
    /// `[a-z]`, `[!a-z]`: one character in, or not in, the ranges.
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

impl Piece {
    fn takes(&self, character: char) -> bool {
        match self {
            Self::Literal(literal) => *literal == character,
            Self::AnyOne => true,
            // A run is handled by the matcher's loop, never asked here.
            Self::AnyRun => false,
            Self::Class { negated, ranges } => {
                let inside = ranges
                    .iter()
                    .any(|(low, high)| (*low..=*high).contains(&character));
                inside != *negated
            }
        }
    }
}

/// A pattern for one run of text with no path separator in it: a file name,
/// a word of a command, a host, the name of a tool.
///
/// `*` matches any run of characters (every character: there is no
/// separator inside one run), `?` any one character, and `[…]` a set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamePattern {
    pieces: Vec<Piece>,
}

impl NamePattern {
    /// Reads a pattern.
    ///
    /// # Errors
    ///
    /// When a `[` is not closed, or a set is empty or runs backwards.
    pub fn parse(text: &str) -> Result<Self, GlobError> {
        let mut pieces = Vec::new();
        let mut chars = text.chars().peekable();
        while let Some(character) = chars.next() {
            let piece = match character {
                '?' => Piece::AnyOne,
                '*' => {
                    // `**` inside a name means what `*` means.
                    if pieces.last() == Some(&Piece::AnyRun) {
                        continue;
                    }
                    Piece::AnyRun
                }
                '[' => {
                    let negated = matches!(chars.peek(), Some('!' | '^'));
                    if negated {
                        chars.next();
                    }
                    let mut members = Vec::new();
                    let mut closed = false;
                    for member in chars.by_ref() {
                        // A `]` right after the `[` is a member, not the end.
                        if member == ']' && !members.is_empty() {
                            closed = true;
                            break;
                        }
                        members.push(member);
                    }
                    if !closed {
                        return Err(GlobError::new(
                            text,
                            "a `[` is not closed. Write `[[]` for a `[` itself",
                        ));
                    }
                    Piece::Class {
                        negated,
                        ranges: ranges_of(&members, text)?,
                    }
                }
                literal => Piece::Literal(literal),
            };
            pieces.push(piece);
        }
        Ok(Self { pieces })
    }

    /// A pattern that matches exactly `text`, whatever characters it holds.
    #[must_use]
    pub fn literal(text: &str) -> Self {
        Self {
            pieces: text.chars().map(Piece::Literal).collect(),
        }
    }

    /// Whether the pattern has no wildcard: it matches one text only.
    #[must_use]
    pub fn is_literal(&self) -> bool {
        self.pieces
            .iter()
            .all(|piece| matches!(piece, Piece::Literal(_)))
    }

    /// Whether the pattern is `*` alone: it matches every text.
    #[must_use]
    pub fn is_any(&self) -> bool {
        self.pieces == [Piece::AnyRun]
    }

    /// Whether `text` matches, all of it.
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        let text: Vec<char> = text.chars().collect();
        let (mut piece, mut at) = (0usize, 0usize);
        // The last `*` passed, and how much of the text it has taken.
        let mut run: Option<(usize, usize)> = None;
        while at < text.len() {
            match self.pieces.get(piece) {
                Some(Piece::AnyRun) => {
                    run = Some((piece, at));
                    piece += 1;
                    continue;
                }
                Some(other)
                    if text
                        .get(at)
                        .is_some_and(|&character| other.takes(character)) =>
                {
                    piece += 1;
                    at += 1;
                    continue;
                }
                _ => {}
            }
            // No match here: let the last `*` take one more character.
            let Some((star, taken)) = run else {
                return false;
            };
            piece = star + 1;
            at = taken + 1;
            run = Some((star, taken + 1));
        }
        self.pieces
            .get(piece..)
            .is_some_and(|rest| rest.iter().all(|piece| *piece == Piece::AnyRun))
    }
}

/// The ranges of a set: `a-z` is a range, anything else is itself.
fn ranges_of(members: &[char], pattern: &str) -> Result<Vec<(char, char)>, GlobError> {
    let mut ranges = Vec::new();
    let mut index = 0;
    while let Some(&low) = members.get(index) {
        match (members.get(index + 1), members.get(index + 2)) {
            (Some('-'), Some(&high)) => {
                if high < low {
                    return Err(GlobError::new(
                        pattern,
                        format!("the range `{low}-{high}` runs backwards"),
                    ));
                }
                ranges.push((low, high));
                index += 3;
            }
            _ => {
                ranges.push((low, low));
                index += 1;
            }
        }
    }
    if ranges.is_empty() {
        return Err(GlobError::new(pattern, "a set `[]` is empty"));
    }
    Ok(ranges)
}

/// One step of a path glob.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// One name that matches the pattern.
    Name(NamePattern),
    /// `**`: any number of names, none included.
    AnyDepth,
}

/// Where a path glob starts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Start {
    /// At this root, in the form the style compares roots in: `/`, `C:/`,
    /// `//SERVER/SHARE/`.
    Root(String),
    /// At any root.
    Anywhere,
}

/// One alternative of a path glob: where it starts, and its steps.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alternative {
    start: Start,
    steps: Vec<Step>,
}

/// What a relative pattern and a `~` are relative to.
#[derive(Debug, Clone, Copy)]
pub struct Anchors<'a> {
    /// The workspace root, resolved.
    pub workspace: &'a AbsPath,
    /// The home directory, when there is one.
    pub home: Option<&'a AbsPath>,
}

/// How a glob compares names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// As written. On Windows the path must be in the file system's own
    /// spelling, as a resolved path is.
    Exact,
    /// As the style compares names: without regard to case on Windows.
    Folded,
}

/// A glob over absolute paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathGlob {
    alternatives: Vec<Alternative>,
    case: Case,
    style: PathStyle,
}

impl PathGlob {
    /// Reads a path glob, in the style of the workspace root.
    ///
    /// # Errors
    ///
    /// When the pattern is empty, has a `.` or `..` step, has a `~` and
    /// there is no home directory, has braces that are not closed or expand
    /// to too many alternatives, has too many `**`, or has a name that is
    /// not a valid [`NamePattern`].
    pub fn compile(pattern: &str, anchors: Anchors<'_>, case: Case) -> Result<Self, GlobError> {
        let style = anchors.workspace.style();
        if pattern.trim().is_empty() {
            return Err(GlobError::new(pattern, "it is empty"));
        }
        let mut alternatives = Vec::new();
        for expanded in expand_braces(pattern)? {
            alternatives.push(alternative(&expanded, pattern, anchors, case, style)?);
        }
        Ok(Self {
            alternatives,
            case,
            style,
        })
    }

    /// Whether `path` matches. A path in another style never does.
    #[must_use]
    pub fn matches(&self, path: &AbsPath) -> bool {
        if path.style() != self.style {
            return false;
        }
        let root = self.style.fold(path.root().as_str()).into_owned();
        let names: Vec<std::borrow::Cow<'_, str>> = match self.case {
            Case::Exact => path.names().map(std::borrow::Cow::Borrowed).collect(),
            Case::Folded => path.names().map(|name| self.style.fold(name)).collect(),
        };
        self.alternatives.iter().any(|alternative| {
            let starts = match &alternative.start {
                Start::Anywhere => true,
                Start::Root(wanted) => *wanted == root,
            };
            starts && steps_match(&alternative.steps, &names)
        })
    }
}

/// Whether the steps take all of the names.
fn steps_match(steps: &[Step], names: &[std::borrow::Cow<'_, str>]) -> bool {
    match steps.split_first() {
        None => names.is_empty(),
        Some((Step::AnyDepth, rest)) => {
            (0..=names.len()).any(|skipped| steps_match(rest, names.get(skipped..).unwrap_or(&[])))
        }
        Some((Step::Name(pattern), rest)) => names
            .split_first()
            .is_some_and(|(name, more)| pattern.matches(name) && steps_match(rest, more)),
    }
}

/// Reads one alternative, after its braces have been expanded.
fn alternative(
    expanded: &str,
    written: &str,
    anchors: Anchors<'_>,
    case: Case,
    style: PathStyle,
) -> Result<Alternative, GlobError> {
    let text = if style == PathStyle::Windows {
        expanded.replace('\\', "/")
    } else {
        expanded.to_owned()
    };
    let fold = |name: &str| match case {
        Case::Exact => name.to_owned(),
        Case::Folded => style.fold(name).into_owned(),
    };
    let literal_steps = |path: &AbsPath| -> Vec<Step> {
        path.names()
            .map(|name| Step::Name(NamePattern::literal(&fold(name))))
            .collect()
    };
    let root_of = |path: &AbsPath| Start::Root(style.fold(path.root().as_str()).into_owned());

    // Where the pattern starts, the steps that come from that, and the rest
    // of the pattern.
    let (start, mut steps, rest): (Start, Vec<Step>, &str) = if let Some(rest) = text
        .strip_prefix("~/")
        .or_else(|| (text == "~").then_some(""))
    {
        let Some(home) = anchors.home else {
            return Err(GlobError::new(
                written,
                "it starts with `~` and no home directory is known. Write the path out",
            ));
        };
        (root_of(home), literal_steps(home), rest)
    } else if let Some((root, rest)) = absolute_root(&text, style) {
        (
            Start::Root(style.fold(&root).into_owned()),
            Vec::new(),
            rest,
        )
    } else if text == "**" || text.starts_with("**/") {
        (Start::Anywhere, Vec::new(), text.as_str())
    } else if text.contains('/') {
        let rest = text.strip_prefix("./").unwrap_or(&text);
        (
            root_of(anchors.workspace),
            literal_steps(anchors.workspace),
            rest,
        )
    } else {
        // A bare name: that name at any depth in the workspace.
        let mut steps = literal_steps(anchors.workspace);
        steps.push(Step::AnyDepth);
        (root_of(anchors.workspace), steps, text.as_str())
    };

    let ends_with_separator = text.ends_with('/');
    for name in rest.split('/') {
        match name {
            "" => {}
            "." | ".." => {
                return Err(GlobError::new(
                    written,
                    "it has a `.` or `..` step. Write the path it leads to",
                ));
            }
            "**" => {
                if steps.last() != Some(&Step::AnyDepth) {
                    steps.push(Step::AnyDepth);
                }
            }
            name => steps.push(Step::Name(
                NamePattern::parse(&fold(name))
                    .map_err(|error| GlobError::new(written, error.problem))?,
            )),
        }
    }
    // `dir/` means the tree under `dir`, as `dir/**` does.
    if ends_with_separator && steps.last() != Some(&Step::AnyDepth) {
        steps.push(Step::AnyDepth);
    }
    let depth_wildcards = steps.iter().filter(|step| **step == Step::AnyDepth).count();
    if depth_wildcards > MAX_ANY_DEPTH {
        return Err(GlobError::new(
            written,
            format!("it has more than {MAX_ANY_DEPTH} `**`"),
        ));
    }
    Ok(Alternative { start, steps })
}

/// The root an absolute pattern starts at, and the pattern after it.
fn absolute_root(text: &str, style: PathStyle) -> Option<(String, &str)> {
    match style {
        PathStyle::Posix => text.strip_prefix('/').map(|rest| ("/".to_owned(), rest)),
        PathStyle::Windows => {
            let mut chars = text.chars();
            let (first, second, third) = (chars.next(), chars.next(), chars.next());
            if let (Some(letter), Some(':'), Some('/')) = (first, second, third)
                && letter.is_ascii_alphabetic()
            {
                let root = format!("{}:/", letter.to_ascii_uppercase());
                return Some((root, text.get(3..).unwrap_or("")));
            }
            let share = text.strip_prefix("//")?;
            let mut parts = share.splitn(3, '/');
            let (server, name) = (parts.next()?, parts.next()?);
            if server.is_empty() || name.is_empty() {
                return None;
            }
            let rest = parts.next().unwrap_or("");
            Some((format!("//{server}/{name}/"), rest))
        }
    }
}

/// Expands `{a,b}` into its alternatives. A brace inside a set (`[{]`) is
/// a character, not a group.
fn expand_braces(pattern: &str) -> Result<Vec<String>, GlobError> {
    let mut done = Vec::new();
    let mut pending = vec![pattern.to_owned()];
    while let Some(text) = pending.pop() {
        let Some(group) = first_group(&text, pattern)? else {
            done.push(text);
            if done.len() + pending.len() > MAX_ALTERNATIVES {
                return Err(too_many(pattern));
            }
            continue;
        };
        let before = text.get(..group.open).unwrap_or("");
        let after = text.get(group.close + 1..).unwrap_or("");
        let mut from = group.open + 1;
        for end in group.commas.iter().copied().chain([group.close]) {
            let choice = text.get(from..end).unwrap_or("");
            pending.push(format!("{before}{choice}{after}"));
            if done.len() + pending.len() > MAX_ALTERNATIVES {
                return Err(too_many(pattern));
            }
            from = end + 1;
        }
    }
    done.sort();
    done.dedup();
    Ok(done)
}

/// The first `{…}` group of a pattern, by the byte positions of its braces
/// and of the commas that separate its choices.
struct Group {
    open: usize,
    close: usize,
    commas: Vec<usize>,
}

/// Finds the first group that is not inside a set. Groups nest, and a comma
/// inside a nested group belongs to that group.
fn first_group(text: &str, written: &str) -> Result<Option<Group>, GlobError> {
    let mut in_set = false;
    // Whether the character just read opened the set, after which a `!`,
    // a `^` or a `]` is a member and not the end.
    let mut set_started = false;
    let mut depth = 0usize;
    let mut group: Option<Group> = None;
    for (at, character) in text.char_indices() {
        if in_set {
            let opening = set_started;
            set_started = opening && matches!(character, '!' | '^');
            if character == ']' && !opening {
                in_set = false;
            }
            continue;
        }
        match character {
            '[' => {
                in_set = true;
                set_started = true;
            }
            '{' => {
                depth += 1;
                if depth == 1 {
                    group = Some(Group {
                        open: at,
                        close: at,
                        commas: Vec::new(),
                    });
                }
            }
            '}' => {
                if depth == 0 {
                    return Err(GlobError::new(written, "a `}` has no `{` before it"));
                }
                depth -= 1;
                if depth == 0
                    && let Some(mut found) = group.take()
                {
                    found.close = at;
                    return Ok(Some(found));
                }
            }
            ',' if depth == 1 => {
                if let Some(found) = &mut group {
                    found.commas.push(at);
                }
            }
            _ => {}
        }
    }
    if depth > 0 {
        return Err(GlobError::new(written, "a `{` is not closed"));
    }
    Ok(None)
}

fn too_many(pattern: &str) -> GlobError {
    GlobError::new(
        pattern,
        format!("its braces expand to more than {MAX_ALTERNATIVES} alternatives"),
    )
}

#[cfg(test)]
mod tests {
    use cortex_base::Rng;
    use cortex_testkit::cases::{self, ensure};

    use super::*;

    fn name(pattern: &str) -> NamePattern {
        NamePattern::parse(pattern).unwrap()
    }

    fn posix(path: &str) -> AbsPath {
        AbsPath::parse(path, PathStyle::Posix).unwrap()
    }

    fn windows(path: &str) -> AbsPath {
        AbsPath::parse(path, PathStyle::Windows).unwrap()
    }

    fn glob(pattern: &str) -> PathGlob {
        let workspace = posix("/work/app");
        let home = posix("/users/dev");
        PathGlob::compile(
            pattern,
            Anchors {
                workspace: &workspace,
                home: Some(&home),
            },
            Case::Exact,
        )
        .unwrap()
    }

    #[test]
    fn a_name_pattern_matches_the_whole_text() {
        for (pattern, text, expected) in [
            ("", "", true),
            ("", "a", false),
            ("abc", "abc", true),
            ("abc", "abcd", false),
            ("abc", "ab", false),
            ("*", "", true),
            ("*", "anything at all", true),
            ("*.rs", "main.rs", true),
            ("*.rs", ".rs", true),
            ("*.rs", "main.rs.bak", false),
            (".env*", ".env", true),
            (".env*", ".env.local", true),
            (".env*", "x.env", false),
            ("a*b*c", "abc", true),
            ("a*b*c", "a-b-b-c", true),
            ("a*b*c", "a-b-c-d", false),
            ("a**b", "ab", true),
            ("?", "a", true),
            ("?", "", false),
            ("?", "ab", false),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("[abc]", "b", true),
            ("[abc]", "d", false),
            ("[a-c]x", "bx", true),
            ("[a-c]x", "dx", false),
            ("[!a-c]", "d", true),
            ("[!a-c]", "a", false),
            ("[^a-c]", "d", true),
            ("[]]", "]", true),
            ("[*]", "*", true),
            ("[*]", "a", false),
            ("[[]", "[", true),
            ("id_[rd]sa", "id_rsa", true),
            ("id_[rd]sa", "id_map", false),
            ("é*", "école", true),
        ] {
            assert_eq!(
                name(pattern).matches(text),
                expected,
                "{pattern:?} on {text:?}"
            );
        }
    }

    #[test]
    fn a_name_pattern_that_cannot_be_read_is_refused() {
        for pattern in ["[abc", "a[", "[z-a]", "[!"] {
            let error = NamePattern::parse(pattern).unwrap_err();
            assert_eq!(error.pattern, pattern);
        }
        // `[]` alone is a set that never closed: the `]` is its first member.
        assert!(NamePattern::parse("[]").is_err());
    }

    #[test]
    fn literal_and_any_are_recognised() {
        assert!(name("cargo").is_literal());
        assert!(!name("carg?").is_literal());
        assert!(name("*").is_any());
        assert!(name("**").is_any());
        assert!(!name("a*").is_any());
        assert!(NamePattern::literal("a*b").matches("a*b"));
        assert!(!NamePattern::literal("a*b").matches("axb"));
    }

    #[test]
    fn a_path_glob_is_anchored_by_how_it_is_written() {
        for (pattern, path, expected) in [
            // Absolute.
            ("/etc/**", "/etc/passwd", true),
            ("/etc/**", "/etc", true),
            ("/etc/**", "/etcetera", false),
            ("/etc/*", "/etc/passwd", true),
            ("/etc/*", "/etc/ssh/config", false),
            ("/etc/*", "/etc", false),
            // Home.
            ("~/notes/**", "/users/dev/notes/a/b.md", true),
            ("~/notes/**", "/users/dev/notes", true),
            ("~/notes/**", "/users/other/notes/a.md", false),
            ("~", "/users/dev", true),
            ("~/", "/users/dev/anything", true),
            // Anywhere.
            ("**/.env", "/work/app/.env", true),
            ("**/.env", "/srv/other/deep/.env", true),
            ("**/.env", "/.env", true),
            ("**/.env", "/work/app/.env.local", false),
            ("**/.env*", "/work/app/.env.local", true),
            ("**", "/anything/at/all", true),
            ("**", "/", true),
            // With a slash: under the workspace root.
            ("src/**", "/work/app/src/main.rs", true),
            ("src/**", "/work/app/src", true),
            ("src/**", "/work/app/tests/src/x.rs", false),
            ("src/**", "/work/other/src/main.rs", false),
            ("./src/*.rs", "/work/app/src/main.rs", true),
            ("docs/*.md", "/work/app/docs/a.md", true),
            ("docs/*.md", "/work/app/docs/sub/a.md", false),
            ("src/", "/work/app/src/deep/x.rs", true),
            ("src/**/test_*.rs", "/work/app/src/test_a.rs", true),
            ("src/**/test_*.rs", "/work/app/src/a/b/test_a.rs", true),
            ("src/**/test_*.rs", "/work/app/src/a/b/a.rs", false),
            // A bare name: at any depth in the workspace, and not outside.
            ("*.lock", "/work/app/Cargo.lock", true),
            ("*.lock", "/work/app/vendor/x/Cargo.lock", true),
            ("*.lock", "/srv/elsewhere/Cargo.lock", false),
            ("Cargo.toml", "/work/app/crates/a/Cargo.toml", true),
            ("Cargo.toml", "/work/app/crates/a/Cargo.toml.orig", false),
            // Braces.
            ("{src,tests}/**", "/work/app/tests/it/main.rs", true),
            ("{src,tests}/**", "/work/app/docs/a.md", false),
            ("**/*.{pem,key}", "/any/where/tls.key", true),
            ("**/*.{pem,key}", "/any/where/tls.crt", false),
            (
                "**/id_{rsa,ed25519{,_sk}}",
                "/users/dev/.ssh/id_ed25519_sk",
                true,
            ),
            (
                "**/id_{rsa,ed25519{,_sk}}",
                "/users/dev/.ssh/id_ed25519",
                true,
            ),
            (
                "**/id_{rsa,ed25519{,_sk}}",
                "/work/app/src/id_map.rs",
                false,
            ),
        ] {
            assert_eq!(
                glob(pattern).matches(&posix(path)),
                expected,
                "{pattern:?} on {path:?}"
            );
        }
    }

    #[test]
    fn a_brace_inside_a_set_is_a_character() {
        // How a literal path with braces in it is written as a pattern.
        let literal = glob("/data/[{]a,b[}]/x");
        assert!(literal.matches(&posix("/data/{a,b}/x")));
        assert!(!literal.matches(&posix("/data/a/x")));
        assert!(glob("/data/[!{]x").matches(&posix("/data/ax")));
        assert!(!glob("/data/[!{]x").matches(&posix("/data/{x")));
        assert!(glob("/data/[]{]x").matches(&posix("/data/{x")));
        // Groups still nest, and a set may stand inside one.
        let nested = glob("/data/{a,b{c,d},[{]}");
        for (path, expected) in [
            ("/data/a", true),
            ("/data/bc", true),
            ("/data/bd", true),
            ("/data/{", true),
            ("/data/b", false),
            ("/data/c", false),
        ] {
            assert_eq!(nested.matches(&posix(path)), expected, "{path}");
        }
    }

    #[test]
    fn the_names_of_the_workspace_are_literal_even_when_they_look_like_patterns() {
        let workspace = posix("/work/a*b/[x]");
        let anchors = Anchors {
            workspace: &workspace,
            home: None,
        };
        let glob = PathGlob::compile("src/**", anchors, Case::Exact).unwrap();
        assert!(glob.matches(&posix("/work/a*b/[x]/src/main.rs")));
        assert!(!glob.matches(&posix("/work/aXb/x/src/main.rs")));
    }

    #[test]
    fn a_path_glob_that_cannot_be_read_is_refused() {
        let workspace = posix("/work/app");
        let no_home = Anchors {
            workspace: &workspace,
            home: None,
        };
        for pattern in [
            "",
            "  ",
            "../outside/**",
            "src/../x",
            "src/./x",
            "~/notes/**",
            "{a,b",
            "a}b",
            "src/[abc",
            "**/a/**/b/**/c/**/d/**/e",
        ] {
            let error = PathGlob::compile(pattern, no_home, Case::Exact).unwrap_err();
            assert_eq!(error.pattern, pattern, "{error}");
        }
        // Braces that multiply past the limit.
        let wide = "{a,b,c,d}{a,b,c,d}{a,b,c,d}{a,b,c,d}";
        assert!(PathGlob::compile(wide, no_home, Case::Exact).is_err());
    }

    #[test]
    fn on_windows_a_folded_glob_ignores_case_and_an_exact_one_does_not() {
        let workspace = windows(r"C:\Work\App");
        let home = windows(r"C:\People\Dev");
        let anchors = Anchors {
            workspace: &workspace,
            home: Some(&home),
        };
        let compile = |pattern: &str, case| PathGlob::compile(pattern, anchors, case).unwrap();

        for (pattern, path, exact, folded) in [
            (r"src\**", r"C:\Work\App\src\main.rs", true, true),
            (r"src\**", r"C:\Work\App\SRC\main.rs", false, true),
            ("SRC/**", r"C:\Work\App\src\main.rs", false, true),
            // The workspace root's own names are compared the same way.
            ("src/**", r"C:\work\app\src\main.rs", false, true),
            // A drive letter has no case on disk: always folded.
            (r"c:\Tools\**", r"C:\Tools\x.exe", true, true),
            (r"c:\tools\**", r"C:\Tools\x.exe", false, true),
            (r"D:\Tools\**", r"C:\Tools\x.exe", false, false),
            (r"~\Notes\**", r"C:\People\Dev\Notes\a.md", true, true),
            (r"~\notes\**", r"C:\People\Dev\Notes\a.md", false, true),
            ("**/.ENV", r"C:\Work\App\.env", false, true),
            ("**/.env", r"C:\Work\App\.env", true, true),
            (r"\\Server\Share\**", r"\\SERVER\share\x", true, true),
            (
                r"\\Server\Share\Dir\**",
                r"\\SERVER\share\dir\x",
                false,
                true,
            ),
            // Letters outside ASCII fold too.
            ("**/É*", r"C:\Work\App\école", false, true),
        ] {
            let path = windows(path);
            assert_eq!(
                compile(pattern, Case::Exact).matches(&path),
                exact,
                "exact {pattern:?} on {path}"
            );
            assert_eq!(
                compile(pattern, Case::Folded).matches(&path),
                folded,
                "folded {pattern:?} on {path}"
            );
        }
        // A path in the other style never matches.
        assert!(!compile("**", Case::Folded).matches(&posix("/work/app")));
    }

    /// A reference matcher for name patterns without sets: plain recursion.
    fn reference(pattern: &[char], text: &[char]) -> bool {
        match pattern.split_first() {
            None => text.is_empty(),
            Some(('*', rest)) => (0..=text.len()).any(|taken| reference(rest, &text[taken..])),
            Some(('?', rest)) => !text.is_empty() && reference(rest, &text[1..]),
            Some((literal, rest)) => text.first() == Some(literal) && reference(rest, &text[1..]),
        }
    }

    #[test]
    fn a_name_pattern_agrees_with_a_plain_recursive_matcher() {
        cases::check(
            "name_pattern_matches_reference",
            2000,
            |rng| {
                let alphabet = ['a', 'b', '*', '?'];
                let letters = ['a', 'b'];
                let pattern: String = (0..cases::small(rng, 8))
                    .map(|_| *cases::pick(rng, &alphabet))
                    .collect();
                let text: String = (0..cases::small(rng, 8))
                    .map(|_| *cases::pick(rng, &letters))
                    .collect();
                (pattern, text)
            },
            |(pattern, text)| {
                let expected = reference(
                    &pattern.chars().collect::<Vec<_>>(),
                    &text.chars().collect::<Vec<_>>(),
                );
                let got = NamePattern::parse(pattern)?.matches(text);
                ensure!(got == expected, "got {got}, the reference says {expected}");
                Ok(())
            },
        );
    }

    /// Folding is not always the wider match: a set that excludes letters
    /// excludes more of them once both sides are in upper case. That is why
    /// a rule that denies or asks is matched folded **and** exactly
    /// (`rules`), never folded alone.
    #[test]
    fn a_negated_set_can_match_exactly_and_not_folded() {
        let workspace = windows(r"C:\Work\App");
        let anchors = Anchors {
            workspace: &workspace,
            home: None,
        };
        let path = windows(r"C:\Work\App\Src");
        let exact = PathGlob::compile("**/[!a-z]rc", anchors, Case::Exact).unwrap();
        let folded = PathGlob::compile("**/[!a-z]rc", anchors, Case::Folded).unwrap();
        assert!(exact.matches(&path));
        assert!(!folded.matches(&path));
    }

    /// Without a negated set, folding only ever adds matches: whatever an
    /// exact glob matches, the folded glob of the same pattern matches too.
    #[test]
    fn a_folded_glob_matches_everything_the_exact_glob_matches() {
        let workspace = windows(r"C:\Work\App");
        let anchors = Anchors {
            workspace: &workspace,
            home: None,
        };
        let names = [
            "src", "Src", "SRC", "main.rs", "Main.RS", ".env", ".ENV", "é", "É",
        ];
        let patterns = [
            "src", "Src", "*", "**", "*.rs", "*.RS", ".env", "?", "É", "[a-z]*",
        ];
        cases::check(
            "folded_glob_is_a_superset",
            1500,
            |rng| {
                let pattern: Vec<&str> = (0..=rng.below(3))
                    .map(|_| *cases::pick(rng, &patterns))
                    .collect();
                let path: Vec<&str> = (0..=rng.below(3))
                    .map(|_| *cases::pick(rng, &names))
                    .collect();
                let anchored = match rng.below(3) {
                    0 => format!("**/{}", pattern.join("/")),
                    1 => format!("C:/Work/App/{}", pattern.join("/")),
                    _ => format!("{}/", pattern.join("/")),
                };
                (anchored, format!(r"C:\Work\App\{}", path.join(r"\")))
            },
            |(pattern, path)| {
                let path = windows(path);
                let exact = PathGlob::compile(pattern, anchors, Case::Exact)?.matches(&path);
                let folded = PathGlob::compile(pattern, anchors, Case::Folded)?.matches(&path);
                ensure!(!exact || folded, "exact matches and folded does not");
                Ok(())
            },
        );
    }

    /// The cases `globset` documents for its syntax, where this matcher has
    /// the same syntax: `globset` with `literal_separator` on, which is how
    /// `cortex-tools` uses it for `list` and `search`. This crate may not
    /// link `globset` (`docs/architecture/01-workspace.md`, rule 5), so the
    /// cases are written out here.
    #[test]
    fn the_cases_globset_documents_hold_here_too() {
        for (pattern, path, expected) in [
            // `?` matches any single character, and not a separator.
            ("/a?c", "/abc", true),
            ("/a?c", "/ac", false),
            ("/a?c", "/a/c", false),
            // `*` matches zero or more characters, and not a separator.
            ("/*.rs", "/main.rs", true),
            ("/*.rs", "/.rs", true),
            ("/*.rs", "/src/main.rs", false),
            ("/a*", "/a", true),
            ("/a*b", "/ab", true),
            ("/a*b", "/axxb", true),
            ("/a*b", "/a/b", false),
            ("/*", "/a/b", false),
            // `**/` at the start matches in any directory, the top included.
            ("**/foo", "/foo", true),
            ("**/foo", "/a/foo", true),
            ("**/foo", "/a/b/foo", true),
            ("**/foo", "/foo/bar", false),
            ("**/foo", "/afoo", false),
            // `/**/` matches zero or more directories.
            ("/a/**/b", "/a/b", true),
            ("/a/**/b", "/a/x/b", true),
            ("/a/**/b", "/a/x/y/b", true),
            ("/a/**/b", "/a/xb", false),
            ("/a/**/b", "/ab", false),
            // `/**` at the end matches everything inside.
            ("/foo/**", "/foo/a", true),
            ("/foo/**", "/foo/a/b", true),
            ("/foo/**", "/foobar/a", false),
            // Sets and their negation, and ranges.
            ("/[ab]c", "/ac", true),
            ("/[ab]c", "/bc", true),
            ("/[ab]c", "/cc", false),
            ("/[!ab]c", "/cc", true),
            ("/[!ab]c", "/ac", false),
            ("/[0-9]x", "/5x", true),
            ("/[0-9]x", "/ax", false),
            ("/[a-cx-z]", "/y", true),
            ("/[a-cx-z]", "/m", false),
            // A set of one writes a special character itself.
            ("/[*]", "/*", true),
            ("/[*]", "/a", false),
            ("/[?]", "/?", true),
            ("/[[]", "/[", true),
            ("/[]]", "/]", true),
            ("/a[]]b", "/a]b", true),
            // Alternatives, the empty one included.
            ("/{a,b}", "/a", true),
            ("/{a,b}", "/b", true),
            ("/{a,b}", "/c", false),
            ("/*.{rs,toml}", "/Cargo.toml", true),
            ("/*.{rs,toml}", "/Cargo.lock", false),
            ("/a{,b}", "/a", true),
            ("/a{,b}", "/ab", true),
            ("/{a/b,c}/d", "/a/b/d", true),
            ("/{a/b,c}/d", "/c/d", true),
        ] {
            assert_eq!(
                glob(pattern).matches(&posix(path)),
                expected,
                "{pattern:?} on {path:?}"
            );
        }
        // Where this matcher differs from `globset`, on purpose:
        // - `dir/**` matches `dir` itself here, so that a rule on a tree
        //   covers a listing of its top;
        // - there is no backslash escape: a set of one does that;
        // - alternatives may nest.
        assert!(glob("/foo/**").matches(&posix("/foo")));
        assert!(glob("/{a,{b,c}d}").matches(&posix("/cd")));
    }

    /// A matcher written as directly as the rules read, character by
    /// character over the whole path, with nothing in common with the one
    /// under test but the rules.
    fn path_reference(pattern: &[char], text: &[char], at_boundary: bool) -> bool {
        // `**` as a whole name.
        if at_boundary && pattern.starts_with(&['*', '*']) {
            match pattern.get(2) {
                // At the end: everything that is left.
                None => return true,
                Some('/') => {
                    let rest = &pattern[3..];
                    // No directory at all, or any number of them.
                    if path_reference(rest, text, true) {
                        return true;
                    }
                    return (0..text.len())
                        .filter(|&index| text[index] == '/')
                        .any(|index| path_reference(rest, &text[index + 1..], true));
                }
                Some(_) => {}
            }
        }
        // `/**` at the end also matches the directory itself, however many
        // times it is written.
        if text.is_empty()
            && !pattern.is_empty()
            && pattern.chunks(3).all(|chunk| chunk == ['/', '*', '*'])
        {
            return true;
        }
        let Some((&first, rest)) = pattern.split_first() else {
            return text.is_empty();
        };
        // A name is never empty: `/*` does not match `/`.
        if at_boundary && text.first().is_none_or(|&c| c == '/') {
            return false;
        }
        match first {
            '*' => {
                path_reference(rest, text, false)
                    || (text.first().is_some_and(|&c| c != '/')
                        && path_reference(pattern, &text[1..], false))
            }
            '?' => {
                text.first().is_some_and(|&c| c != '/') && path_reference(rest, &text[1..], false)
            }
            '[' => {
                let close = rest.iter().position(|&c| c == ']').expect("a closed set");
                let (members, after) = (&rest[..close], &rest[close + 1..]);
                let (negated, members) = match members.split_first() {
                    Some(('!', others)) => (true, others),
                    _ => (false, members),
                };
                text.first()
                    .is_some_and(|&c| c != '/' && members.contains(&c) != negated)
                    && path_reference(after, &text[1..], false)
            }
            literal => {
                text.first() == Some(&literal) && path_reference(rest, &text[1..], literal == '/')
            }
        }
    }

    #[test]
    fn a_path_glob_agrees_with_a_matcher_written_from_the_rules() {
        let names = [
            "a", "b", "ab", "*", "?", "**", "[ab]", "[!a]", "a*", "*b", "?b", "a?",
        ];
        let files = ["a", "b", "ab", "ba", "aab", "c"];
        cases::check(
            "path_glob_matches_reference",
            4000,
            |rng| {
                let pattern: Vec<&str> = (0..1 + rng.below(4))
                    .map(|_| *cases::pick(rng, &names))
                    .collect();
                let path: Vec<&str> = (0..rng.below(5))
                    .map(|_| *cases::pick(rng, &files))
                    .collect();
                (
                    format!("/{}", pattern.join("/")),
                    format!("/{}", path.join("/")),
                )
            },
            |(pattern, path)| {
                let workspace = posix("/w");
                let anchors = Anchors {
                    workspace: &workspace,
                    home: None,
                };
                let Ok(compiled) = PathGlob::compile(pattern, anchors, Case::Exact) else {
                    // More `**` than the matcher takes.
                    return Ok(());
                };
                let got = compiled.matches(&posix(path));
                let pattern: Vec<char> = pattern.chars().skip(1).collect();
                let text: Vec<char> = path.chars().skip(1).collect();
                let expected = path_reference(&pattern, &text, true);
                ensure!(got == expected, "got {got}, the reference says {expected}");
                Ok(())
            },
        );
    }
}
