//! The directory walk that `list` and `search` share.
//!
//! It is the walker of the ripgrep libraries
//! (`docs/decisions/0014-search-in-process.md`), so ignore files and hidden
//! files are treated the way people expect, and it runs on several threads.
//! Around it, this module adds what the harness requires of every path:
//!
//! - **Each entry is named through `cortex-base`.** A name the resolver's
//!   rules refuse (one that is not valid Unicode, and on Windows a device
//!   name or a name that ends with a dot) is stepped past and counted, with
//!   the resolver's reason. The walk never stops for one bad name.
//! - **Each entry is put to the [`ReadGate`]**, so that a rule can protect
//!   one file inside a tree whose walk was allowed.
//! - **Links are not followed.** The walk stays under the directory the plan
//!   declared. A link is reported as a link, and the caller decides.
//! - **It stops when cancelled.**

use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use cortex_base::{AbsPath, CancelToken, ResolvedPath};
use globset::{GlobBuilder, GlobMatcher};
use ignore::{WalkBuilder, WalkState};

use crate::context::ReadGate;
use crate::output::count;
use crate::result::ToolError;

/// What to walk, and how.
pub(crate) struct WalkSpec<'a> {
    /// The directory to walk: an existing one, resolved.
    pub(crate) root: &'a ResolvedPath,
    /// How deep to go. 1 is the directory's own entries.
    pub(crate) max_depth: Option<usize>,
    /// Whether to include what ignore files and the hidden-file rule leave
    /// out.
    pub(crate) include_ignored: bool,
    /// Only entries that match this.
    pub(crate) glob: Option<&'a GlobFilter>,
    /// How many threads walk. 0 lets the walker choose.
    pub(crate) threads: usize,
}

/// A glob over the paths of a walk, relative to the directory walked.
///
/// A pattern with no `/` in it matches a name at any depth (`*.rs`). A
/// pattern with one matches the whole relative path (`src/**/*.rs`). `*`
/// never crosses a `/`; `**` does.
#[derive(Debug, Clone)]
pub(crate) struct GlobFilter {
    matcher: GlobMatcher,
    name_only: bool,
}

impl GlobFilter {
    /// Reads a glob. A pattern that is not a glob is refused, with the
    /// reason.
    pub(crate) fn new(pattern: &str) -> Result<Self, ToolError> {
        // On Windows a model may write the separator either way.
        let pattern = if cfg!(windows) {
            pattern.replace('\\', "/")
        } else {
            pattern.to_owned()
        };
        let pattern = pattern.strip_prefix("./").unwrap_or(&pattern);
        if pattern.is_empty() {
            return Err(ToolError::invalid("`glob` must not be empty."));
        }
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .case_insensitive(cfg!(windows))
            .build()
            .map_err(|error| {
                ToolError::invalid(format!(
                    "`glob` is not a valid glob: {}. Use `*` for part of a name, `**` for any \
                     directories, `{{a,b}}` for alternatives.",
                    error.kind()
                ))
            })?;
        Ok(Self {
            matcher: glob.compile_matcher(),
            name_only: !pattern.contains('/'),
        })
    }

    /// Whether an entry matches: `relative` is its path under the directory
    /// walked, with `/` between names.
    pub(crate) fn matches(&self, relative: &str) -> bool {
        let candidate = if self.name_only {
            relative.rsplit('/').next().unwrap_or(relative)
        } else {
            relative
        };
        self.matcher.is_match(candidate)
    }
}

/// What an entry of a walk is. A link is not looked through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Dir,
    Link,
}

/// One entry of a walk, handed to the caller.
pub(crate) struct Found<'a> {
    /// Its path under the directory walked, with `/` between names.
    pub(crate) relative: &'a str,
    /// Its path for the file functions.
    pub(crate) fs_path: &'a Path,
    /// What it is.
    pub(crate) kind: EntryKind,
}

/// What a walk stepped past, for the result to say.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Skipped {
    /// Entries whose names the path rules refuse.
    pub(crate) refused: usize,
    /// The reason the first of them was refused.
    pub(crate) first_refusal: Option<String>,
    /// Entries the permission rules do not let the call read.
    pub(crate) denied: usize,
    /// Entries the file system would not give.
    pub(crate) unreadable: usize,
    /// What it said about the first of them.
    pub(crate) first_error: Option<String>,
    /// Links the caller did not follow.
    pub(crate) links: usize,
}

impl Skipped {
    /// One line for each kind of thing stepped past, for the end of a
    /// result. Nothing when nothing was.
    pub(crate) fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        let entries = |number: usize| if number == 1 { "entry" } else { "entries" };
        if self.refused > 0 {
            notes.push(format!(
                "[Skipped {} {} whose path cannot be used. The first: {}]",
                count(self.refused),
                entries(self.refused),
                self.first_refusal.as_deref().unwrap_or("no reason given")
            ));
        }
        if self.denied > 0 {
            notes.push(format!(
                "[Skipped {} {} that the permission rules do not let this call read.]",
                count(self.denied),
                entries(self.denied)
            ));
        }
        if self.unreadable > 0 {
            notes.push(format!(
                "[Skipped {} {} that could not be read. The first: {}]",
                count(self.unreadable),
                entries(self.unreadable),
                self.first_error.as_deref().unwrap_or("no reason given")
            ));
        }
        if self.links > 0 {
            notes.push(format!(
                "[Skipped {} symbolic {}: a walk does not follow links. Name one as `path` to \
                 use what it points to.]",
                count(self.links),
                if self.links == 1 { "link" } else { "links" }
            ));
        }
        notes
    }

    fn refuse(&mut self, reason: String) {
        self.refused += 1;
        self.first_refusal.get_or_insert(reason);
    }

    fn unreadable(&mut self, reason: &str) {
        self.unreadable += 1;
        if self.first_error.is_none() {
            // One line, and not a long one: it goes into a result.
            let line = reason.lines().next().unwrap_or("");
            self.first_error = Some(line.chars().take(200).collect());
        }
    }
}

/// The directory a thread's last entry was in, so that the entries of one
/// directory, which arrive together, build its path once.
struct Parent {
    fs_path: PathBuf,
    abs: AbsPath,
    relative: String,
}

/// What a walk calls for each entry on one of its threads.
pub(crate) type Visitor<'v> = Box<dyn FnMut(&Found<'_>) + Send + 'v>;

/// Walks a directory on several threads. Each thread gets a visitor of its
/// own from `visitor`, and calls it for each entry that may be used, in no
/// particular order.
///
/// Returns what was stepped past. When `cancel` fires the walk stops, and
/// what was visited so far is all there is: the caller checks the token.
pub(crate) fn walk<'v>(
    spec: &WalkSpec<'_>,
    gate: &dyn ReadGate,
    cancel: &CancelToken,
    visitor: &mut dyn FnMut() -> Visitor<'v>,
) -> Skipped {
    let root_fs = spec.root.to_fs_path();
    let root_abs = spec.root.abs();
    let skipped = Mutex::new(Skipped::default());
    let note = |change: &dyn Fn(&mut Skipped)| {
        change(&mut skipped.lock().unwrap_or_else(PoisonError::into_inner));
    };

    let standard = !spec.include_ignored;
    let mut builder = WalkBuilder::new(&root_fs);
    builder
        .follow_links(false)
        .max_depth(spec.max_depth)
        .threads(spec.threads)
        .hidden(standard)
        .ignore(standard)
        .git_ignore(standard)
        .git_exclude(standard)
        .parents(standard)
        // A `.gitignore` is honoured whether or not the tree is a git
        // repository: an unpacked project has one and no `.git`.
        .require_git(false)
        // The user's global git ignore file is theirs, not the project's,
        // and would make a search depend on the machine it runs on.
        .git_global(false);

    builder.build_parallel().run(|| {
        let mut visit = visitor();
        let mut parent: Option<Parent> = None;
        let root_fs = &root_fs;
        let note = &note;
        Box::new(move |entry| {
            if cancel.is_cancelled() {
                return WalkState::Quit;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    note(&|skipped| skipped.unreadable(&error.to_string()));
                    return WalkState::Continue;
                }
            };
            if entry.depth() == 0 {
                return WalkState::Continue;
            }
            let Some(file_type) = entry.file_type() else {
                return WalkState::Continue;
            };
            let is_dir = file_type.is_dir();
            // Past a directory that cannot be used, nothing is used.
            let step_past = if is_dir {
                WalkState::Skip
            } else {
                WalkState::Continue
            };

            let Some(fs_parent) = entry.path().parent() else {
                return step_past;
            };
            if parent
                .as_ref()
                .is_none_or(|known| known.fs_path != fs_parent)
            {
                match parent_of(root_fs, root_abs, fs_parent) {
                    Ok(found) => parent = Some(found),
                    Err(reason) => {
                        note(&|skipped| skipped.refuse(reason.clone()));
                        return step_past;
                    }
                }
            }
            let Some(known) = parent.as_ref() else {
                return step_past;
            };
            let abs = match known.abs.child(entry.file_name()) {
                Ok(abs) => abs,
                Err(error) => {
                    note(&|skipped| skipped.refuse(error.to_string()));
                    return step_past;
                }
            };
            if !gate.may_read(&abs) {
                note(&|skipped| skipped.denied += 1);
                return step_past;
            }

            let Some(name) = abs.file_name() else {
                return step_past;
            };
            let relative = if known.relative.is_empty() {
                name.to_owned()
            } else {
                format!("{}/{name}", known.relative)
            };
            if spec.glob.is_none_or(|glob| glob.matches(&relative)) {
                let kind = if is_dir {
                    EntryKind::Dir
                } else if file_type.is_symlink() {
                    EntryKind::Link
                } else {
                    EntryKind::File
                };
                visit(&Found {
                    relative: &relative,
                    fs_path: entry.path(),
                    kind,
                });
            }
            WalkState::Continue
        })
    });

    skipped.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// Builds the path of a directory inside the walk from the root's, one name
/// at a time, each checked as any name is.
fn parent_of(root_fs: &Path, root_abs: &AbsPath, fs_parent: &Path) -> Result<Parent, String> {
    let below = fs_parent.strip_prefix(root_fs).map_err(|_| {
        format!(
            "`{}` is not under the directory walked",
            fs_parent.display()
        )
    })?;
    let mut abs = root_abs.clone();
    let mut relative = String::new();
    for component in below.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        abs = abs.child(name).map_err(|error| error.to_string())?;
        if let Some(name) = abs.file_name() {
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(name);
        }
    }
    Ok(Parent {
        fs_path: fs_parent.to_owned(),
        abs,
        relative,
    })
}

/// How a path under a walked directory is shown to the model: under the
/// directory's own shown path.
pub(crate) fn shown_path(root_shown: &str, relative: &str) -> String {
    if root_shown == "." {
        relative.to_owned()
    } else if root_shown.ends_with('/') {
        format!("{root_shown}{relative}")
    } else {
        format!("{root_shown}/{relative}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        GlobFilter::new(pattern)
            .unwrap_or_else(|error| panic!("{pattern}: {error}"))
            .matches(path)
    }

    #[test]
    fn a_glob_without_a_separator_matches_a_name_at_any_depth() {
        assert!(matches("*.rs", "main.rs"));
        assert!(matches("*.rs", "src/deep/down/lib.rs"));
        assert!(!matches("*.rs", "src/main.rs.bak"));
        assert!(matches("Cargo.toml", "crates/a/Cargo.toml"));
        assert!(matches("*.{rs,toml}", "crates/a/Cargo.toml"));
        assert!(matches("test_?.py", "tests/test_a.py"));
        assert!(!matches("test_?.py", "tests/test_ab.py"));
    }

    #[test]
    fn a_glob_with_a_separator_matches_the_whole_path() {
        assert!(matches("src/*.rs", "src/main.rs"));
        assert!(
            !matches("src/*.rs", "src/deep/lib.rs"),
            "`*` stops at a separator"
        );
        assert!(!matches("src/*.rs", "other/src/main.rs"));
        assert!(matches("src/**/*.rs", "src/deep/down/lib.rs"));
        assert!(matches("src/**/*.rs", "src/main.rs"));
        assert!(matches("**/*.rs", "main.rs"));
        assert!(matches("**/tests/**", "crates/a/tests/it/main.rs"));
        assert!(matches("./src/*.rs", "src/main.rs"));
    }

    #[test]
    fn a_bad_glob_is_refused_with_the_reason() {
        for pattern in ["", "src/[", "{a,b"] {
            let error = GlobFilter::new(pattern).expect_err(pattern);
            assert!(matches!(error, ToolError::InvalidArguments(_)));
            assert!(error.to_string().starts_with("`glob` "), "{error}");
        }
    }

    #[test]
    fn what_was_skipped_is_said_once_per_kind() {
        assert!(Skipped::default().notes().is_empty());
        let mut skipped = Skipped::default();
        skipped.refuse("the path `a/\u{fffd}` is not valid Unicode".to_owned());
        skipped.refuse("another".to_owned());
        skipped.denied = 1;
        skipped.unreadable("Permission denied (os error 13)\nsecond line");
        skipped.links = 1200;
        assert_eq!(
            skipped.notes(),
            [
                "[Skipped 2 entries whose path cannot be used. The first: the path `a/\u{fffd}` \
                 is not valid Unicode]",
                "[Skipped 1 entry that the permission rules do not let this call read.]",
                "[Skipped 1 entry that could not be read. The first: Permission denied (os \
                 error 13)]",
                "[Skipped 1,200 symbolic links: a walk does not follow links. Name one as \
                 `path` to use what it points to.]",
            ]
        );
    }

    #[test]
    fn a_path_is_shown_under_the_directory_walked() {
        assert_eq!(shown_path(".", "src/main.rs"), "src/main.rs");
        assert_eq!(shown_path("src", "main.rs"), "src/main.rs");
        assert_eq!(shown_path("/var/log", "syslog"), "/var/log/syslog");
        assert_eq!(shown_path("/", "etc"), "/etc");
        assert_eq!(shown_path("C:/", "Work"), "C:/Work");
    }
}
