//! The protected paths of a workspace, looked at before and after a
//! sandboxed command (`docs/architecture/06-permissions-and-sandbox.md`,
//! "Protected paths").
//!
//! Landlock only adds rights, so a path inside a writable workspace cannot
//! be kept from changing. What decides what runs later, outside the
//! sandbox (git's hooks and configuration, CI configuration, build and
//! registry settings, the project's own `.cortex`), is therefore looked at
//! before each sandboxed command and again after it. A change is found,
//! not prevented, and the content from before is kept so that it can be put
//! back.
//!
//! What is looked at:
//!
//! - **The fixed list:** the lease's protected paths (the workspace's
//!   `.git/hooks`, `.git/config`, `.cortex`, `.github/workflows`, … and
//!   `sandbox.protect`), each with everything under it.
//! - **Names at any depth,** found by a walk of the workspace: in every
//!   repository the walk meets, nested ones and submodules' included
//!   (`.git/modules/*`), its `config`, `config.worktree`, `hooks` and
//!   `info`; a bare repository (a directory with `HEAD`, `objects` and
//!   `refs`) the same; a `.git` file; `.cargo/config.toml` and
//!   `.cargo/config`; `.npmrc`, `.yarnrc`, `.yarnrc.yml` and `.envrc`.
//!
//! **Bounded.** The walk lists at most [`Limits::directories`] directories
//! and gives up after [`Limits::time`]; past either, the snapshot says it
//! is incomplete, and what lay further was not looked at. Content is kept
//! for files up to [`Limits::file_bytes`], and up to [`Limits::total_bytes`]
//! in all; a file past either is still compared by its size, times and
//! inode, and is not restorable. Links are never followed.

#![allow(
    clippy::disallowed_methods,
    reason = "a look at the file system, documented as blocking: its callers run it on the \
              blocking pool, never on the core thread"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How far a look goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The most directories the walk lists.
    pub directories: usize,
    /// The longest a look may take.
    pub time: Duration,
    /// The largest file whose content is kept.
    pub file_bytes: u64,
    /// The most content kept in all.
    pub total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            directories: 20_000,
            time: Duration::from_millis(250),
            file_bytes: 256 * 1024,
            total_bytes: 4 * 1024 * 1024,
        }
    }
}

/// What to look at in one workspace.
#[derive(Debug, Clone)]
pub struct Watch {
    workspace: PathBuf,
    fixed: Vec<PathBuf>,
    limits: Limits,
}

/// What the protected paths looked like at one moment.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    entries: BTreeMap<PathBuf, Entry>,
    complete: bool,
    /// The paths this look asked for by name, there or not.
    asked: BTreeSet<PathBuf>,
    /// The directories whose every entry this look went through.
    listed: BTreeSet<PathBuf>,
}

/// One protected entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: Kind,
    bytes: u64,
    modified: Option<SystemTime>,
    stamp: Stamp,
    /// The content, for a file small enough to keep.
    content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Link,
    Other,
}

/// What only the system sets: a program can put a file's modification
/// time back, and cannot do that to its change time or its inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Stamp {
    changed: (i64, i64),
    inode: u64,
    mode: u32,
}

/// How one protected path changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    /// It was not there before.
    Created,
    /// It was there, and is different.
    Modified,
    /// It was there, and is gone.
    Deleted,
}

impl Change {
    /// The word for it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
        }
    }
}

/// One protected path that a command changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// The path.
    pub path: PathBuf,
    /// How it changed.
    pub change: Change,
    /// What it was before, so that it can be put back.
    pub before: Before,
}

/// What a changed path was before the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Before {
    /// Nothing: putting it back is removing it.
    Absent,
    /// A file with this content.
    File(Vec<u8>),
    /// A directory. Putting it back is making it again; what was in it is
    /// listed on its own.
    Directory,
    /// Something whose content was not kept: a link, a special file, or a
    /// file too large. It cannot be put back.
    Unknown,
}

impl Watch {
    /// A watch on `workspace` and the protected paths in `fixed`, which
    /// may lie inside it or outside it.
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>, fixed: Vec<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            fixed,
            limits: Limits::default(),
        }
    }

    /// The same watch with other limits.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Looks at every protected path now. It blocks, for at most the time
    /// the limits give.
    #[must_use]
    pub fn take(&self) -> Snapshot {
        let mut look = Look {
            root: self.workspace.clone(),
            limits: self.limits,
            started: Instant::now(),
            listed: 0,
            kept: 0,
            complete: true,
            entries: BTreeMap::new(),
            asked: BTreeSet::new(),
            through: BTreeSet::new(),
        };
        for path in &self.fixed {
            // A protected path reached through a link in the workspace is
            // not looked through: the link is what is recorded, wherever
            // it leads (found by the review of step 26).
            if look.through_a_link(path) {
                look.asked.insert(path.clone());
                continue;
            }
            look.record_tree(path);
        }
        look.walk(&self.workspace);
        Snapshot {
            entries: look.entries,
            complete: look.complete,
            asked: look.asked,
            listed: look.through,
        }
    }

    /// What changed among the protected paths since `before`, from a look
    /// taken now. A path one of the two looks did not reach is not
    /// reported: it may have been there, or gone, all along.
    #[must_use]
    pub fn changes_since(&self, before: &Snapshot) -> (Vec<Changed>, Snapshot) {
        let after = self.take();
        (before.compare(&after), after)
    }
}

impl Snapshot {
    /// Whether every protected path was looked at, within the limits.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// How many entries it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What differs between this look and a later one. A path that one
    /// look holds and the other does not is reported only where the other
    /// look reached: a look that stopped at its bound says nothing of what
    /// lay beyond it.
    #[must_use]
    pub fn compare(&self, after: &Self) -> Vec<Changed> {
        let mut changed = Vec::new();
        for (path, old) in &self.entries {
            match after.entries.get(path) {
                None if after.covers(path) => changed.push(Changed {
                    path: path.clone(),
                    change: Change::Deleted,
                    before: old.before(),
                }),
                None => {}
                Some(new) => {
                    if let Some(before) = new.change_from(old) {
                        changed.push(Changed {
                            path: path.clone(),
                            change: Change::Modified,
                            before,
                        });
                    }
                }
            }
        }
        for path in after.entries.keys() {
            if !self.entries.contains_key(path) && self.covers(path) {
                changed.push(Changed {
                    path: path.clone(),
                    change: Change::Created,
                    before: Before::Absent,
                });
            }
        }
        changed.sort_by(|a, b| a.path.cmp(&b.path));
        changed
    }

    /// Whether this look would have seen `path` had it been there, or seen
    /// it gone: always for a complete look; for an incomplete one, only
    /// when it asked for the path by name, or went through every entry of
    /// its directory.
    fn covers(&self, path: &Path) -> bool {
        self.complete
            || self.asked.contains(path)
            || path
                .parent()
                .is_some_and(|parent| self.listed.contains(parent))
    }
}

impl Entry {
    /// How this entry changed from `old`, and what it was before: `None`
    /// when it did not.
    fn change_from(&self, old: &Self) -> Option<Before> {
        match (old.kind, self.kind) {
            // A directory's own times change when an entry is added or
            // removed, and those entries are compared one by one. The
            // directory itself has changed when it was replaced, or its
            // mode was.
            (Kind::Directory, Kind::Directory) => (self.stamp.inode != old.stamp.inode
                || self.stamp.mode != old.stamp.mode)
                .then_some(Before::Directory),
            (old_kind, new_kind) if old_kind != new_kind => Some(old.before()),
            // A change of mode (a hook made executable) is not put back by
            // writing the content again: it is put back by hand.
            _ if self.stamp.mode != old.stamp.mode => Some(Before::Unknown),
            // Both contents are known: they decide. The same bytes written
            // again are no change.
            (Kind::File, Kind::File) if self.content.is_some() && old.content.is_some() => {
                (self.content != old.content).then(|| old.before())
            }
            // Otherwise what the system sets, which a program cannot put
            // back, and the size and the time.
            _ => (self.stamp != old.stamp
                || self.bytes != old.bytes
                || self.modified != old.modified)
                .then(|| old.before()),
        }
    }

    fn before(&self) -> Before {
        match (self.kind, &self.content) {
            (Kind::File, Some(content)) => Before::File(content.clone()),
            (Kind::Directory, _) => Before::Directory,
            _ => Before::Unknown,
        }
    }
}

/// One look in progress.
struct Look {
    /// The workspace: links below it are not looked through.
    root: PathBuf,
    limits: Limits,
    started: Instant,
    listed: usize,
    kept: u64,
    complete: bool,
    entries: BTreeMap<PathBuf, Entry>,
    /// The paths asked for by name, there or not.
    asked: BTreeSet<PathBuf>,
    /// The directories whose every entry was gone through.
    through: BTreeSet<PathBuf>,
}

/// The names a walk looks for in every directory, and what each holds that
/// is protected.
const BUILD_FILES: &[&str] = &[".npmrc", ".yarnrc", ".yarnrc.yml", ".envrc"];
const CARGO_FILES: &[&str] = &["config.toml", "config"];
const GIT_PARTS: &[&str] = &["config", "config.worktree", "hooks", "info"];
const WORKTREE_PARTS: &[&str] = &["config.worktree", "commondir", "gitdir"];

impl Look {
    fn out_of_time(&mut self) -> bool {
        if self.started.elapsed() > self.limits.time {
            self.complete = false;
        }
        !self.complete
    }

    /// Whether a directory between the workspace and `path` is a link.
    fn through_a_link(&self, path: &Path) -> bool {
        let Ok(below) = path.strip_prefix(&self.root) else {
            return false;
        };
        let mut walked = self.root.clone();
        let parts: Vec<_> = below.components().collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            walked.push(part);
            if fs::symlink_metadata(&walked).is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return true;
            }
        }
        false
    }

    /// Records `path` and, for a directory, everything under it.
    fn record_tree(&mut self, path: &Path) {
        let Some(kind) = self.record(path) else {
            return;
        };
        if kind != Kind::Directory || self.out_of_time() {
            return;
        }
        let Some(children) = self.list(path) else {
            return;
        };
        for (child, _) in &children {
            self.record_tree(child);
            if self.out_of_time() {
                return;
            }
        }
        self.through.insert(path.to_owned());
    }

    /// Records one entry, without following a link. `None` when it is not
    /// there.
    fn record(&mut self, path: &Path) -> Option<Kind> {
        self.asked.insert(path.to_owned());
        let metadata = fs::symlink_metadata(path).ok()?;
        let kind = if metadata.file_type().is_symlink() {
            Kind::Link
        } else if metadata.is_dir() {
            Kind::Directory
        } else if metadata.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        let bytes = metadata.len();
        let content = (kind == Kind::File
            && bytes <= self.limits.file_bytes
            && self.kept + bytes <= self.limits.total_bytes)
            .then(|| fs::read(path).ok())
            .flatten();
        if let Some(content) = &content {
            self.kept += u64::try_from(content.len()).unwrap_or(u64::MAX);
        }
        self.entries.insert(
            path.to_owned(),
            Entry {
                kind,
                bytes,
                modified: metadata.modified().ok(),
                stamp: stamp(&metadata),
                content,
            },
        );
        Some(kind)
    }

    /// The entries of a directory, counted against the limit. `None` past
    /// the limit, or when the directory cannot be read: then what is in it
    /// is not known.
    ///
    /// Each entry comes with whether it is a directory, not a link to one,
    /// as the listing itself says: no entry is asked for again, which is
    /// most of a walk's time.
    fn list(&mut self, directory: &Path) -> Option<Vec<(PathBuf, bool)>> {
        if self.listed >= self.limits.directories {
            self.complete = false;
            return None;
        }
        self.listed += 1;
        fs::read_dir(directory).ok().map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
                    (entry.path(), is_dir)
                })
                .collect()
        })
    }

    /// Walks the workspace for protected names at any depth.
    fn walk(&mut self, directory: &Path) {
        if self.out_of_time() {
            return;
        }
        let Some(children) = self.list(directory) else {
            return;
        };
        let names: Vec<&str> = children
            .iter()
            .filter_map(|(child, _)| child.file_name().and_then(|name| name.to_str()))
            .collect();
        // A bare repository made of plain files.
        if ["HEAD", "objects", "refs"]
            .iter()
            .all(|name| names.contains(name))
        {
            self.record_git(directory);
        }
        for (child, is_dir) in &children {
            let is_dir = *is_dir;
            let Some(name) = child.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            match name {
                ".git" if is_dir => self.record_git(child),
                ".git" => {
                    self.record(child);
                }
                ".cargo" if is_dir => {
                    for file in CARGO_FILES {
                        self.record(&child.join(file));
                    }
                }
                name if BUILD_FILES.contains(&name) => {
                    self.record(child);
                }
                _ if is_dir => self.walk(child),
                _ => {}
            }
            if self.out_of_time() {
                return;
            }
        }
        self.through.insert(directory.to_owned());
    }

    /// Records the protected parts of a git directory, and of every
    /// repository kept inside it (submodules, worktrees).
    fn record_git(&mut self, git: &Path) {
        for part in GIT_PARTS {
            self.record_tree(&git.join(part));
        }
        let modules = git.join("modules");
        if fs::symlink_metadata(&modules).is_ok_and(|metadata| metadata.is_dir()) {
            for (module, _) in self.list(&modules).unwrap_or_default() {
                self.record_git(&module);
            }
        }
        let worktrees = git.join("worktrees");
        if fs::symlink_metadata(&worktrees).is_ok_and(|metadata| metadata.is_dir()) {
            for (worktree, _) in self.list(&worktrees).unwrap_or_default() {
                // Where the worktree's repository is, and its own settings.
                for part in WORKTREE_PARTS {
                    self.record(&worktree.join(part));
                }
            }
        }
    }
}

#[cfg(unix)]
fn stamp(metadata: &fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    Stamp {
        changed: (metadata.ctime(), metadata.ctime_nsec()),
        inode: metadata.ino(),
        mode: metadata.mode(),
    }
}

#[cfg(not(unix))]
fn stamp(metadata: &fs::Metadata) -> Stamp {
    Stamp {
        mode: u32::from(metadata.permissions().readonly()),
        ..Stamp::default()
    }
}

#[cfg(test)]
mod tests;
