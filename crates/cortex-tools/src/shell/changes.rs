//! Which files a command changed
//! (`docs/architecture/07-sessions-and-context.md`, "Changes made by shell
//! commands").
//!
//! A file tool knows what it writes. A command does not say, so the harness
//! looks: once before the command and once after it.
//!
//! - **In a git repository**, it asks `git status` both times and compares
//!   the two answers, with the size and modification time of every file
//!   git named. That finds a file the command changed, created or deleted,
//!   whether or not the file had been changed before. When the command
//!   moved `HEAD` (a checkout, a pull, a commit), git is also asked which
//!   files differ between the two commits.
//! - **Anywhere else**, it walks the workspace afterwards and takes every
//!   file modified since the command started. That cannot tell a new file
//!   from a changed one, and cannot see a deleted one.
//!
//! In a repository it also says which changed files git can still give
//! back as they were, and from which commit ([`Changes::restorable`]).
//!
//! **Asking git runs nothing of the repository's.** A repository's own
//! configuration can name a program for git to run when it is asked what
//! has changed. The harness asks with that turned off.
//!
//! Both are best-effort, and say so: [`Changes::complete`] is `false` when
//! the look ran out of its time cap or of its room, and then the lists are
//! what was seen and no more. Files that ignore rules leave out are left
//! out here too: a build's output directory is not a change to report.
//!
//! Everything here blocks, and is called off the core thread.

#![allow(
    clippy::disallowed_methods,
    reason = "runs on the blocking pool: it reads file metadata and walks the workspace"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cortex_base::{SharedClock, SystemClock, UtcTime};
use ignore::WalkBuilder;

use crate::contract::Json;
use crate::process::brief;

/// How long one look at the workspace may take, unless told otherwise.
pub const DEFAULT_CAP: Duration = Duration::from_millis(500);

/// The most files one look keeps track of.
const MOST_FILES: usize = 20_000;

/// How far before the command's start a modification time still counts as
/// the command's, when there is no git to ask. A file system stamps a file
/// with a clock that runs a tick or two behind the one the harness reads.
const CLOCK_SLACK: Duration = Duration::from_millis(20);

/// How the changes were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// Two answers of `git status`, compared.
    Git,
    /// Modification times under the workspace.
    ModificationTimes,
    /// Nothing was looked at: the first look did not finish.
    None,
}

impl Method {
    /// The name front ends are given: `git`, `mtime` or `none`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::ModificationTimes => "mtime",
            Self::None => "none",
        }
    }
}

/// What a command changed, as far as the harness could see. Every path is
/// relative to the workspace root, with `/`, and each list is sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    /// How they were found.
    pub method: Method,
    /// Files whose content changed. Without git, new files are here too.
    pub modified: Vec<String>,
    /// Files that were not there before. Empty without git.
    pub created: Vec<String>,
    /// Files that are no longer there. Empty without git.
    pub deleted: Vec<String>,
    /// The files of `modified` and `deleted` whose content before the
    /// command can be read from git, at `base_commit`: those the command
    /// found unchanged since that commit. A file that had already been
    /// changed before the command is not here: what it held is gone, and
    /// an undo must say that it cannot restore it. Empty without git.
    pub restorable: Vec<String>,
    /// The commit `HEAD` was at before the command, which the files in
    /// `restorable` are read from.
    pub base_commit: Option<String>,
    /// Whether the command moved git's `HEAD` to another commit.
    pub head_moved: bool,
    /// Whether the look finished. When it did not, the lists are what was
    /// seen, and there may be more.
    pub complete: bool,
}

impl Changes {
    fn nothing(method: Method, complete: bool) -> Self {
        Self {
            method,
            modified: Vec::new(),
            created: Vec::new(),
            deleted: Vec::new(),
            restorable: Vec::new(),
            base_commit: None,
            head_moved: false,
            complete,
        }
    }

    /// Whether no changed file was seen.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modified.is_empty() && self.created.is_empty() && self.deleted.is_empty()
    }

    /// The changes as the structured payload of a result carries them.
    #[must_use]
    pub fn to_json(&self) -> Json {
        serde_json::json!({
            "method": self.method.as_str(),
            "modified": self.modified,
            "created": self.created,
            "deleted": self.deleted,
            "restorable": self.restorable,
            "base_commit": self.base_commit,
            "head_moved": self.head_moved,
            "complete": self.complete,
        })
    }
}

/// What the workspace looked like before a command: enough to tell
/// afterwards what the command changed.
#[derive(Debug, Clone)]
pub struct Before {
    started: UtcTime,
    git: Option<GitLook>,
    /// Whether git was asked and did not answer in time.
    git_gave_up: bool,
}

/// One answer of `git status`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitLook {
    /// The commit `HEAD` is at, if it is at one.
    head: Option<String>,
    /// Every path git named, relative to the workspace, with what the file
    /// looked like on disk.
    files: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// Whether git called it untracked or newly added.
    new_to_git: bool,
    stamp: Stamp,
}

/// What a file looked like on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stamp {
    Missing,
    There {
        bytes: u64,
        modified: Option<UtcTime>,
    },
}

impl Stamp {
    fn of(path: &Path) -> Self {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => Self::There {
                bytes: metadata.len(),
                modified: metadata.modified().ok().and_then(UtcTime::from_system),
            },
            Err(_) => Self::Missing,
        }
    }
}

/// Finds what commands change in one workspace.
#[derive(Debug)]
pub struct ChangeDetector {
    root: PathBuf,
    cap: Duration,
    clock: SharedClock,
    /// Set when `git` could not be started at all, so that it is not tried
    /// before every command.
    no_git: AtomicBool,
}

impl ChangeDetector {
    /// A detector for the workspace at `root`, on the system clock, with
    /// [`DEFAULT_CAP`] for each look.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cap: DEFAULT_CAP,
            clock: std::sync::Arc::new(SystemClock::new()),
            no_git: AtomicBool::new(false),
        }
    }

    /// Sets how long one look may take.
    #[must_use]
    pub fn with_cap(mut self, cap: Duration) -> Self {
        self.cap = cap;
        self
    }

    /// Sets the clock that says when a command started.
    #[must_use]
    pub fn with_clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// Looks at the workspace before a command runs. In a git repository
    /// this starts `git status` and waits for it, up to the cap.
    #[must_use]
    pub fn before(&self) -> Before {
        let started = self.clock.now();
        let (git, git_gave_up) = match self.repository() {
            Some(repository) => match self.git_look(&repository) {
                Some(look) => (Some(look), false),
                None => (None, !self.no_git.load(Ordering::Relaxed)),
            },
            None => (None, false),
        };
        Before {
            started,
            git,
            git_gave_up,
        }
    }

    /// Looks again after the command, and says what changed.
    #[must_use]
    pub fn after(&self, before: &Before) -> Changes {
        if before.git_gave_up {
            // Git is there and was too slow. Walking the same tree would
            // be slower still.
            return Changes::nothing(Method::None, false);
        }
        match (&before.git, self.repository()) {
            (Some(was), Some(repository)) => match self.git_look(&repository) {
                Some(now) => self.compare(&repository, was, &now),
                None => Changes::nothing(Method::Git, false),
            },
            // The command made the repository, or took it away: there is
            // nothing to compare an answer of git with.
            (Some(_), None) | (None, Some(_)) | (None, None) => self.modified_since(before.started),
        }
    }

    /// The repository the workspace is in: its top directory, and the path
    /// from there to the workspace root, with `/` and a `/` at its end, or
    /// empty.
    fn repository(&self) -> Option<Repository> {
        if self.no_git.load(Ordering::Relaxed) {
            return None;
        }
        // A directory, or for a worktree a file, named `.git`.
        let top = self
            .root
            .ancestors()
            .find(|directory| std::fs::symlink_metadata(directory.join(".git")).is_ok())?;
        let below = self.root.strip_prefix(top).ok()?;
        let mut prefix = String::new();
        for name in below.components() {
            prefix.push_str(name.as_os_str().to_str()?);
            prefix.push('/');
        }
        Some(Repository { prefix })
    }

    fn git(&self, arguments: &[&str]) -> Option<Vec<u8>> {
        // A repository's own configuration can name a program for git to
        // run when it is asked what has changed (`core.fsmonitor`). Nobody
        // approved that program, so git is told not to use one, and where
        // hooks are looked for is nowhere.
        let mut hardened = vec![
            "-c",
            "core.fsmonitor=false",
            "-c",
            if cfg!(windows) {
                "core.hooksPath=NUL"
            } else {
                "core.hooksPath=/dev/null"
            },
        ];
        hardened.extend_from_slice(arguments);
        let arguments = &hardened[..];
        let answer = brief::run(
            "git",
            arguments,
            Some(&self.root),
            // Asking must not change anything: no refreshed index written
            // behind the user's own git, and no lock taken.
            &[("GIT_OPTIONAL_LOCKS", "0")],
            self.cap,
        );
        if answer == Err(brief::Unanswered::NotStarted) {
            self.no_git.store(true, Ordering::Relaxed);
        }
        answer.ok()
    }

    fn git_look(&self, repository: &Repository) -> Option<GitLook> {
        let answer = self.git(&[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--",
            ".",
        ])?;
        let status = parse_status(&answer)?;
        let mut files = BTreeMap::new();
        for (path, new_to_git) in status.paths {
            // Git names paths from the top of the repository.
            let Some(relative) = path.strip_prefix(&repository.prefix) else {
                continue;
            };
            if files.len() >= MOST_FILES {
                return None;
            }
            files.insert(
                relative.to_owned(),
                Entry {
                    new_to_git,
                    stamp: Stamp::of(&self.root.join(relative)),
                },
            );
        }
        Some(GitLook {
            head: status.head,
            files,
        })
    }

    fn compare(&self, repository: &Repository, was: &GitLook, now: &GitLook) -> Changes {
        let mut changes = Changes::nothing(Method::Git, true);
        // What git did not name before the command was as its commit has
        // it, so the commit still holds what the command replaced.
        changes.base_commit.clone_from(&was.head);
        let has_base = was.head.is_some();
        for (path, entry) in &now.files {
            match (was.files.get(path), entry.stamp) {
                // Git names it now and did not before.
                (None, Stamp::Missing) => {
                    changes.deleted.push(path.clone());
                    changes.restorable.push(path.clone());
                }
                (None, Stamp::There { .. }) if entry.new_to_git => {
                    changes.created.push(path.clone());
                }
                (None, Stamp::There { .. }) => {
                    changes.modified.push(path.clone());
                    changes.restorable.push(path.clone());
                }
                // Git named it both times: the file itself says.
                (Some(before), now) if before.stamp == now => {}
                (Some(before), Stamp::Missing) if before.stamp != Stamp::Missing => {
                    changes.deleted.push(path.clone());
                }
                (Some(before), Stamp::There { .. }) if before.stamp == Stamp::Missing => {
                    changes.created.push(path.clone());
                }
                (Some(_), _) => changes.modified.push(path.clone()),
            }
        }
        for (path, before) in &was.files {
            if now.files.contains_key(path) {
                continue;
            }
            // Git named it before and does not now: it was committed, put
            // back as it was in the commit, deleted, or is now ignored.
            match (before.stamp, Stamp::of(&self.root.join(path))) {
                (then, current) if then == current => {}
                (Stamp::There { .. }, Stamp::Missing) => changes.deleted.push(path.clone()),
                (Stamp::Missing, Stamp::There { .. }) => changes.created.push(path.clone()),
                _ => changes.modified.push(path.clone()),
            }
        }
        if was.head != now.head {
            changes.head_moved = true;
            // Files that were clean before and are clean now can still
            // differ: git says which, between the two commits.
            if let (Some(old), Some(new)) = (&was.head, &now.head) {
                match self.git(&[
                    "diff",
                    "--name-status",
                    "-z",
                    "--no-renames",
                    old,
                    new,
                    "--",
                    ".",
                ]) {
                    Some(answer) => {
                        for (status, path) in parse_name_status(&answer) {
                            let Some(relative) = path.strip_prefix(&repository.prefix) else {
                                continue;
                            };
                            // A file that had been changed before the
                            // command was judged above, by what is on
                            // disk: the commits differ in it whether or
                            // not the command touched it.
                            if was.files.contains_key(relative) {
                                continue;
                            }
                            let list = match status {
                                b'A' => &mut changes.created,
                                b'D' => &mut changes.deleted,
                                _ => &mut changes.modified,
                            };
                            list.push(relative.to_owned());
                            // It was as the old commit has it.
                            if status != b'A' {
                                changes.restorable.push(relative.to_owned());
                            }
                        }
                    }
                    None => changes.complete = false,
                }
            } else {
                changes.complete = false;
            }
        }
        for list in [
            &mut changes.modified,
            &mut changes.created,
            &mut changes.deleted,
            &mut changes.restorable,
        ] {
            list.sort();
            list.dedup();
        }
        // One file, one list: what the files themselves showed comes
        // before what the commits say.
        let (created, deleted) = (changes.created.clone(), changes.deleted.clone());
        changes
            .modified
            .retain(|path| !created.contains(path) && !deleted.contains(path));
        // Only what is reported as changed or gone, and only with a commit
        // to read it from.
        let (modified, deleted) = (&changes.modified, &changes.deleted);
        changes
            .restorable
            .retain(|path| has_base && (modified.contains(path) || deleted.contains(path)));
        changes
    }

    /// The files under the workspace that were modified at or after
    /// `started`, less the slack between the two clocks.
    fn modified_since(&self, started: UtcTime) -> Changes {
        let since = started.checked_sub(CLOCK_SLACK).unwrap_or(started);
        let begun = self.clock.monotonic();
        let mut changes = Changes::nothing(Method::ModificationTimes, true);
        let mut seen = 0usize;
        let walk = WalkBuilder::new(&self.root)
            .hidden(false)
            .parents(true)
            .git_global(false)
            .require_git(false)
            .follow_links(false)
            .filter_entry(|entry| entry.file_name() != ".git")
            .build();
        for entry in walk {
            let Ok(entry) = entry else { continue };
            seen += 1;
            // The clock is asked once in a while, not for every file.
            if seen > MOST_FILES
                || (seen.is_multiple_of(64)
                    && self.clock.monotonic().saturating_sub(begun) > self.cap)
            {
                changes.complete = false;
                break;
            }
            if entry.file_type().is_none_or(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let modified = metadata.modified().ok().and_then(UtcTime::from_system);
            if modified.is_none_or(|modified| modified < since) {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&self.root) else {
                continue;
            };
            let names: Option<Vec<&str>> = relative
                .components()
                .map(|name| name.as_os_str().to_str())
                .collect();
            if let Some(names) = names {
                changes.modified.push(names.join("/"));
            }
        }
        changes.modified.sort();
        changes
    }
}

/// Where the workspace is in its repository.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Repository {
    prefix: String,
}

/// What `git status --porcelain=v2 --branch -z` said.
#[derive(Debug, PartialEq, Eq)]
struct Status {
    head: Option<String>,
    /// Each path, and whether git called it untracked or newly added.
    paths: Vec<(String, bool)>,
}

/// Reads git's answer. `None` when it is not what the format promises, or
/// names a path that is not Unicode: half an answer is not compared.
fn parse_status(answer: &[u8]) -> Option<Status> {
    let mut status = Status {
        head: None,
        paths: Vec::new(),
    };
    let mut records = answer.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        let record = std::str::from_utf8(record).ok()?;
        let (kind, rest) = record.split_once(' ')?;
        // The fields before the path, by the kind of line. A path may hold
        // spaces, and is everything after them.
        let (fields_before_path, has_original) = match kind {
            "#" => {
                if let Some(commit) = rest.strip_prefix("branch.oid ") {
                    // `(initial)` before the first commit.
                    status.head = (!commit.starts_with('(')).then(|| commit.to_owned());
                }
                continue;
            }
            "?" => {
                status.paths.push((rest.to_owned(), true));
                continue;
            }
            "!" => continue,
            "1" => (7, false),
            "2" => (8, true),
            "u" => (9, false),
            _ => return None,
        };
        let mut fields = rest.splitn(fields_before_path + 1, ' ');
        let state = fields.next()?;
        let path = fields.nth(fields_before_path - 1)?;
        status.paths.push((path.to_owned(), state.starts_with('A')));
        if has_original {
            // A rename's old path is a record of its own.
            let original = records.next().filter(|record| !record.is_empty())?;
            status
                .paths
                .push((std::str::from_utf8(original).ok()?.to_owned(), false));
        }
    }
    Some(status)
}

/// Reads `git diff --name-status -z`: a status letter and a path, each
/// ended by a NUL.
fn parse_name_status(answer: &[u8]) -> Vec<(u8, String)> {
    let mut found = Vec::new();
    let mut records = answer
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let (Some(status), Some(path)) = (records.next(), records.next()) {
        if let (Some(letter), Ok(path)) = (status.first(), std::str::from_utf8(path)) {
            found.push((*letter, path.to_owned()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_answer_is_read_line_by_line() {
        let answer = b"# branch.oid 0123456789abcdef0123456789abcdef01234567\0\
            # branch.head main\0\
            1 .M N... 100644 100644 100644 aaaa bbbb src/changed file.rs\0\
            1 A. N... 000000 100644 100644 0000 cccc added.rs\0\
            1 .D N... 100644 100644 000000 dddd dddd gone.rs\0\
            2 R. N... 100644 100644 100644 eeee ffff R100 new name.rs\0old name.rs\0\
            u UU N... 100644 100644 100644 100644 1111 2222 3333 conflict.rs\0\
            ? untracked dir/new.txt\0\
            ! target/ignored.o\0";
        let status = parse_status(answer).expect("a well-formed answer");
        assert_eq!(
            status.head.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(
            status.paths,
            [
                ("src/changed file.rs".to_owned(), false),
                ("added.rs".to_owned(), true),
                ("gone.rs".to_owned(), false),
                ("new name.rs".to_owned(), false),
                ("old name.rs".to_owned(), false),
                ("conflict.rs".to_owned(), false),
                ("untracked dir/new.txt".to_owned(), true),
            ]
        );
    }

    #[test]
    fn a_repository_with_no_commit_has_no_head() {
        let status = parse_status(b"# branch.oid (initial)\0# branch.head main\0? a.txt\0")
            .expect("a well-formed answer");
        assert_eq!(status.head, None);
        assert_eq!(status.paths, [("a.txt".to_owned(), true)]);
        assert_eq!(
            parse_status(b""),
            Some(Status {
                head: None,
                paths: Vec::new()
            })
        );
    }

    #[test]
    fn an_answer_that_is_not_the_format_is_not_half_used() {
        // A kind of line this does not know.
        assert_eq!(parse_status(b"? a.txt\0X what\0"), None);
        // A line with too few fields.
        assert_eq!(parse_status(b"1 .M N... 100644 path\0"), None);
        // A rename with its second record missing.
        assert_eq!(
            parse_status(b"2 R. N... 100644 100644 100644 eeee ffff R100 new.rs\0"),
            None
        );
        // A path that is not Unicode.
        assert_eq!(parse_status(b"? caf\xe9.txt\0"), None);
    }

    #[test]
    fn a_name_status_answer_is_pairs() {
        assert_eq!(
            parse_name_status(b"M\0src/a b.rs\0A\0new.rs\0D\0old.rs\0"),
            [
                (b'M', "src/a b.rs".to_owned()),
                (b'A', "new.rs".to_owned()),
                (b'D', "old.rs".to_owned())
            ]
        );
        assert!(parse_name_status(b"").is_empty());
        assert_eq!(
            parse_name_status(b"M\0only\0D\0"),
            [(b'M', "only".to_owned())]
        );
    }

    #[test]
    fn changes_are_a_payload_for_front_ends() {
        let mut changes = Changes::nothing(Method::Git, true);
        assert!(changes.is_empty());
        changes.modified.push("src/a.rs".to_owned());
        changes.head_moved = true;
        assert!(!changes.is_empty());
        assert_eq!(
            changes.to_json(),
            serde_json::json!({
                "method": "git",
                "modified": ["src/a.rs"],
                "created": [],
                "deleted": [],
                "restorable": [],
                "base_commit": null,
                "head_moved": true,
                "complete": true,
            })
        );
        assert_eq!(Method::ModificationTimes.as_str(), "mtime");
        assert_eq!(Method::None.as_str(), "none");
    }
}
