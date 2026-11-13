//! The workspace's starting state (`docs/architecture/07-sessions-and-context.md`,
//! "The log", `base`; `docs/architecture/16-run-engine.md`, "Replay and
//! evaluation").
//!
//! Taken on a thread of its own after the session starts, so it never
//! delays the first prompt, and appended as a `base` record when it is
//! ready.
//!
//! **In a git repository:** the commit, the branch, the uncommitted changes
//! to tracked files as a binary patch (an artifact), and the untracked files
//! that are not ignored, kept in the checkpoint store under caps. A link is
//! recorded by what it points to and never followed.
//!
//! **Elsewhere:** a list of the files with their sizes and hashes, under
//! caps. Their content is not kept, so a replay needs the files to still
//! match.
//!
//! **Project trust.** A repository's own configuration can make git run
//! programs: a clean filter runs on `git diff` and `git status`, and an
//! untrusted repository can carry that configuration. So for a project that
//! is not trusted, only the commit and the branch are read, and the record
//! is marked incomplete. A capture after the project is trusted appends a
//! second, complete record.
//!
//! **Likely secrets are never kept**, trusted or not: files named like
//! environment files, keys and certificates, and `.netrc`. They are listed
//! as omitted, with the reason.
//!
//! git is run with [`GIT_HARDENING`], which turns off what would otherwise
//! let it write to the repository or start a file-system monitor.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use cortex_base::{AbsPath, Clock, ContentHash, PathStyle, SharedClock};
use cortex_protocol::item::ArtifactRef;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::snapshot::hash_file;
use crate::store::DiskStore;

/// The flags every git command of the base state runs with:
///
/// - `--no-optional-locks`: git writes nothing it does not have to (a
///   status or a diff otherwise refreshes the index);
/// - `core.fsmonitor=false`: no file-system monitor is started, which a
///   repository's configuration could name as any program;
/// - `core.quotepath=off`: names come out as they are, not quoted;
/// - `diff.noprefix=false` and `diff.mnemonicPrefix=false`: the patch has
///   its usual `a/` and `b/` prefixes whatever the user's configuration
///   says, so that it applies.
///
/// A diff also runs with `--no-ext-diff`, `--no-textconv`, `--no-color`,
/// `--no-renames`, `--binary` and `--full-index` ([`DIFF_FLAGS`]). Later
/// steps that run git reuse both lists.
pub const GIT_HARDENING: [&str; 9] = [
    "--no-optional-locks",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.quotepath=off",
    "-c",
    "diff.noprefix=false",
    "-c",
    "diff.mnemonicPrefix=false",
];

/// The flags of a diff that must apply, whatever the user's configuration.
pub const DIFF_FLAGS: [&str; 6] = [
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--no-renames",
    "--binary",
    "--full-index",
];

/// The limits on what a base state keeps and how long it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseLimits {
    /// The largest untracked file kept.
    pub file_bytes: u64,
    /// The most untracked files kept.
    pub files: usize,
    /// The most bytes of untracked files kept, in all.
    pub total_bytes: u64,
    /// The largest patch of uncommitted changes kept.
    pub patch_bytes: usize,
    /// Outside git: the most files listed.
    pub listed_files: usize,
    /// Outside git: the largest file hashed. A larger one is listed with its
    /// size only.
    pub hashed_bytes: u64,
    /// Outside git: how long the listing may take.
    pub listing_time: Duration,
}

impl Default for BaseLimits {
    /// 1 MiB a file, 1,000 files and 50 MiB in all for untracked files; a
    /// 50 MiB patch; outside git, 20,000 files listed in at most 2 seconds,
    /// hashing those up to 64 MiB.
    fn default() -> Self {
        Self {
            file_bytes: 1024 * 1024,
            files: 1000,
            total_bytes: 50 * 1024 * 1024,
            patch_bytes: 50 * 1024 * 1024,
            listed_files: 20_000,
            hashed_bytes: 64 * 1024 * 1024,
            listing_time: Duration::from_secs(2),
        }
    }
}

/// The workspace's starting state, as a `base` record holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseState {
    /// Whether everything the state should hold was taken. An incomplete
    /// state says why in `note`.
    pub complete: bool,
    /// Whether the project was trusted when it was taken.
    #[serde(default)]
    pub trusted: bool,
    /// In a git repository: what git says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitBase>,
    /// Outside git: the files and their hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<ListedFile>>,
    /// What was left out, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<Omitted>,
    /// Why the state is incomplete, or what went wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// How long it took, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub took_ms: Option<u64>,
}

/// The state of a git repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBase {
    /// The workspace's place in the repository, relative to its top with
    /// `/` between names. Empty when the workspace is the top.
    #[serde(default)]
    pub root: String,
    /// The commit checked out. `None` in a repository with no commits yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The branch checked out. `None` when the head is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The uncommitted changes to tracked files, as a binary patch against
    /// the commit (in a repository with no commits, the staged changes and
    /// then the unstaged ones). `None` when there are none, or they were
    /// not taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<ArtifactRef>,
    /// Untracked files that are not ignored, kept in the checkpoint store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untracked: Vec<StoredFile>,
    /// Untracked links that are not ignored, by what they point to. A link
    /// is never followed: what it points to may be outside the workspace,
    /// or a device that never ends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<StoredLink>,
}

/// A file whose content is in the checkpoint store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    /// The file, relative to the repository's top.
    pub path: String,
    /// Its content's hash.
    pub hash: ContentHash,
    /// Its size.
    pub bytes: u64,
}

/// A link, recorded as the text it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredLink {
    /// The link, relative to the repository's top.
    pub path: String,
    /// What it points to, as written in the link.
    pub target: String,
}

/// A file in the listing of a workspace that is not a git repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedFile {
    /// The file, relative to the workspace root.
    pub path: String,
    /// Its size.
    pub bytes: u64,
    /// Its content's hash. `None` for a file too large to hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<ContentHash>,
}

/// Something the state left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Omitted {
    /// The file, or `*` for everything past a limit.
    pub path: String,
    /// Why.
    pub reason: OmitReason,
}

/// Why something was left out of a base state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OmitReason {
    /// It is likely to hold a secret.
    Secret,
    /// It is larger than the limit for one file.
    TooLarge,
    /// The limit on how many files was reached.
    TooMany,
    /// The limit on bytes in all was reached.
    TotalLimit,
    /// The listing ran out of time.
    OutOfTime,
    /// It could not be read.
    Unreadable,
    /// It is not a file or a link: a directory (a repository of its own), a
    /// device or a FIFO. Never read.
    NotAFile,
    /// A reason this build does not know.
    Other(String),
}

impl OmitReason {
    fn as_str(&self) -> &str {
        match self {
            Self::Secret => "secret",
            Self::TooLarge => "too_large",
            Self::TooMany => "too_many",
            Self::TotalLimit => "total_limit",
            Self::OutOfTime => "out_of_time",
            Self::Unreadable => "unreadable",
            Self::NotAFile => "not_a_file",
            Self::Other(other) => other,
        }
    }
}

impl Serialize for OmitReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for OmitReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(match text.as_str() {
            "secret" => Self::Secret,
            "too_large" => Self::TooLarge,
            "too_many" => Self::TooMany,
            "total_limit" => Self::TotalLimit,
            "out_of_time" => Self::OutOfTime,
            "unreadable" => Self::Unreadable,
            "not_a_file" => Self::NotAFile,
            _ => Self::Other(text),
        })
    }
}

/// Whether a file's name says it is likely to hold a secret: environment
/// files (`.env`, `.env.local`), private keys and certificates (`*.pem`,
/// `*.key`, `*.p12`, `*.pfx`, `id_*` but not `id_*.pub`) and `.netrc`.
/// The permission rules of a later step replace this list.
#[must_use]
pub fn is_likely_secret(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    name.starts_with(".env")
        || name == ".netrc"
        || name == "_netrc"
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        || (name.starts_with("id_") && !name.ends_with(".pub"))
}

/// What a git command printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// Whether it exited with success.
    pub success: bool,
    /// Its standard output, up to the limit it was given.
    pub stdout: Vec<u8>,
    /// Whether there was more than the limit, which was not read.
    pub truncated: bool,
}

/// Runs git. The base state goes through this, so a test can stand in for
/// it, and hold it back to show that the first prompt does not wait.
pub trait Git: Send + Sync + fmt::Debug {
    /// Runs git in `dir` with [`GIT_HARDENING`] and `args`, reading at most
    /// `limit` bytes of its output.
    ///
    /// # Errors
    ///
    /// When git cannot be started (it is not installed).
    fn run(&self, dir: &Path, args: &[&str], limit: usize) -> io::Result<GitOutput>;
}

/// The `git` program on the `PATH`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGit;

impl Git for SystemGit {
    fn run(&self, dir: &Path, args: &[&str], limit: usize) -> io::Result<GitOutput> {
        let mut child = Command::new("git")
            .args(GIT_HARDENING)
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = Vec::new();
        let mut truncated = false;
        if let Some(mut pipe) = child.stdout.take() {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let read = match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error);
                    }
                };
                if stdout.len() + read > limit {
                    truncated = true;
                    let _ = child.kill();
                    break;
                }
                stdout.extend_from_slice(&buffer[..read]);
            }
        }
        let status = child.wait()?;
        Ok(GitOutput {
            success: status.success() && !truncated,
            stdout,
            truncated,
        })
    }
}

/// A base state being taken on its own thread.
#[derive(Debug)]
pub struct BaseCapture {
    receiver: Receiver<BaseState>,
}

impl BaseCapture {
    /// Starts taking the base state of the workspace at `root`, on a thread
    /// of its own, and returns at once.
    ///
    /// # Errors
    ///
    /// When the thread cannot be started.
    pub fn start(
        root: AbsPath,
        trusted: bool,
        store: Arc<DiskStore>,
        git: Arc<dyn Git>,
        clock: SharedClock,
        limits: BaseLimits,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("cortex-base-state".to_owned())
            .spawn(move || {
                let state = capture(&root, trusted, &store, &*git, &*clock, &limits);
                // The session may have ended first; then nobody wants it.
                let _ = sender.send(state);
            })?;
        Ok(Self { receiver })
    }

    /// The state, if it is ready. Never waits.
    pub fn try_take(&mut self) -> Option<BaseState> {
        self.receiver.try_recv().ok()
    }

    /// Waits for the state. `None` if the thread ended without one.
    #[must_use]
    pub fn wait(self) -> Option<BaseState> {
        self.receiver.recv().ok()
    }

    /// Waits for the state for at most `limit`.
    #[must_use]
    pub fn wait_for(&self, limit: Duration) -> Option<BaseState> {
        self.receiver.recv_timeout(limit).ok()
    }
}

/// Takes the base state of the workspace at `root`. Blocks. Never fails: a
/// problem is recorded in the state's `note`.
pub fn capture(
    root: &AbsPath,
    trusted: bool,
    store: &DiskStore,
    git: &dyn Git,
    clock: &dyn Clock,
    limits: &BaseLimits,
) -> BaseState {
    let started = clock.now();
    let directory = root.to_fs_path();
    let top = git
        .run(&directory, &["rev-parse", "--show-toplevel"], 64 * 1024)
        .ok()
        .filter(|output| output.success)
        .and_then(|output| first_line(&output.stdout));
    let mut state = match top {
        Some(top) => from_git(root, &top, trusted, store, git, limits),
        None => from_listing(root, clock, limits),
    };
    state.trusted = trusted;
    state.took_ms = clock
        .now()
        .duration_since(started)
        .map(|took| u64::try_from(took.as_millis()).unwrap_or(u64::MAX));
    state
}

fn first_line(output: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(output).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

fn from_git(
    root: &AbsPath,
    top: &str,
    trusted: bool,
    store: &DiskStore,
    git: &dyn Git,
    limits: &BaseLimits,
) -> BaseState {
    let top_dir = PathBuf::from(top);
    let small = 64 * 1024;
    let run = |args: &[&str], limit: usize| git.run(&top_dir, args, limit);
    let commit = run(&["rev-parse", "--verify", "--quiet", "HEAD"], small)
        .ok()
        .filter(|output| output.success)
        .and_then(|output| first_line(&output.stdout));
    let branch = run(&["symbolic-ref", "--quiet", "--short", "HEAD"], small)
        .ok()
        .filter(|output| output.success)
        .and_then(|output| first_line(&output.stdout));
    let workspace_root = AbsPath::parse(top, PathStyle::NATIVE)
        .ok()
        .and_then(|top| root.strip_prefix(&top))
        .map(|relative| relative.as_str().to_owned())
        .unwrap_or_default();
    let mut base = GitBase {
        root: workspace_root,
        commit,
        branch,
        patch: None,
        untracked: Vec::new(),
        links: Vec::new(),
    };
    if !trusted {
        return BaseState {
            complete: false,
            git: Some(base),
            note: Some(
                "the project is not trusted, so only the commit and the branch were read; \
                 the uncommitted changes are recorded once it is"
                    .to_owned(),
            ),
            ..BaseState::default()
        };
    }

    let mut omitted = Vec::new();
    let mut problems = Vec::new();

    // Tracked changes, leaving out files that are likely secrets.
    let against: &[&str] = if base.commit.is_some() {
        &["HEAD"]
    } else {
        &["--cached"]
    };
    let mut names = vec!["diff", "--name-only", "-z", "--no-renames", "--no-ext-diff"];
    names.extend_from_slice(against);
    // The secrets are left out of the patch by name, so a patch is taken
    // only with the whole list of changed files in hand.
    let not_listed = "so no patch was taken: without the whole list, likely secrets could not \
                      be left out of it";
    let changed = match run(&names, limits.patch_bytes) {
        Ok(output) if output.success && !output.truncated => Some(split_nul(&output.stdout)),
        Ok(output) if output.truncated => {
            problems.push(format!(
                "the list of changed files is too long, {not_listed}"
            ));
            None
        }
        Ok(_) => {
            problems.push(format!(
                "git could not list the changed files, {not_listed}"
            ));
            None
        }
        Err(error) => {
            problems.push(format!(
                "git could not list the changed files ({error}), {not_listed}"
            ));
            None
        }
    };
    let patch = changed.map(|changed| {
        let mut exclusions = Vec::new();
        for path in changed {
            if is_likely_secret(&path) {
                exclusions.push(format!(":(exclude,literal){path}"));
                omitted.push(Omitted {
                    path,
                    reason: OmitReason::Secret,
                });
            }
        }
        if base.commit.is_some() {
            diff(&run, &["HEAD"], &exclusions, limits.patch_bytes)
        } else {
            // No commits yet: the staged changes, then the unstaged ones.
            diff(&run, &["--cached"], &exclusions, limits.patch_bytes).and_then(|mut staged| {
                let unstaged = diff(&run, &[], &exclusions, limits.patch_bytes)?;
                staged.extend(unstaged);
                Ok(staged)
            })
        }
    });
    match patch {
        None => {}
        Some(Ok(patch)) if patch.is_empty() => {}
        Some(Ok(patch)) => match store.put_artifact(
            "uncommitted changes when the session started",
            "text/x-diff",
            &patch,
        ) {
            Ok(reference) => base.patch = Some(reference),
            Err(error) => problems.push(format!("the patch could not be stored: {error}")),
        },
        Some(Err(DiffFailure::TooLarge)) => omitted.push(Omitted {
            path: "*".to_owned(),
            reason: OmitReason::TooLarge,
        }),
        Some(Err(DiffFailure::Failed(reason))) => problems.push(reason),
    }

    // Untracked files that are not ignored.
    match run(
        &["ls-files", "--others", "--exclude-standard", "-z"],
        limits.patch_bytes,
    ) {
        Ok(output) if output.success => {
            untracked(
                &top_dir,
                split_nul(&output.stdout),
                store,
                limits,
                &mut base,
                &mut omitted,
            );
        }
        Ok(output) => problems.push(if output.truncated {
            "the list of untracked files is too long".to_owned()
        } else {
            "git could not list the untracked files".to_owned()
        }),
        Err(error) => problems.push(format!("git could not list the untracked files: {error}")),
    }

    let complete = problems.is_empty()
        && omitted
            .iter()
            .all(|omitted| omitted.reason == OmitReason::Secret);
    BaseState {
        complete,
        git: Some(base),
        omitted,
        note: (!problems.is_empty()).then(|| problems.join("; ")),
        ..BaseState::default()
    }
}

/// Keeps the untracked files git listed, under the limits.
///
/// A file over the limit for one file is listed by name. Past the limit on
/// the number of files or on bytes in all, one `*` entry says so and
/// nothing more is kept, so a workspace with a million untracked files
/// gives a record of a fixed size, not one entry for each.
///
/// Nothing is followed or read but a regular file: a link is recorded by
/// what it points to, and a directory, a device or a FIFO is listed as not
/// a file.
fn untracked(
    top_dir: &Path,
    paths: Vec<String>,
    store: &DiskStore,
    limits: &BaseLimits,
    base: &mut GitBase,
    omitted: &mut Vec<Omitted>,
) {
    let mut total = 0u64;
    for path in paths {
        if is_likely_secret(&path) {
            omitted.push(Omitted {
                path,
                reason: OmitReason::Secret,
            });
            continue;
        }
        if base.untracked.len() + base.links.len() >= limits.files {
            omitted.push(Omitted {
                path: "*".to_owned(),
                reason: OmitReason::TooMany,
            });
            return;
        }
        let file = top_dir.join(&path);
        let reason = match std::fs::symlink_metadata(&file) {
            Err(_) => OmitReason::Unreadable,
            Ok(found) if found.file_type().is_symlink() => {
                // Never opened: recorded by the text it holds.
                match std::fs::read_link(&file).map(|target| target.into_os_string().into_string())
                {
                    Ok(Ok(target)) => {
                        base.links.push(StoredLink { path, target });
                        continue;
                    }
                    _ => OmitReason::Unreadable,
                }
            }
            Ok(found) if !found.is_file() => OmitReason::NotAFile,
            Ok(found) if found.len() > limits.file_bytes => OmitReason::TooLarge,
            Ok(found) if total + found.len() > limits.total_bytes => {
                omitted.push(Omitted {
                    path: "*".to_owned(),
                    reason: OmitReason::TotalLimit,
                });
                return;
            }
            Ok(_) => match store.put_file(&file, limits.file_bytes) {
                Ok(Some(version)) if version.kept => {
                    total += version.bytes;
                    base.untracked.push(StoredFile {
                        path,
                        hash: version.hash,
                        bytes: version.bytes,
                    });
                    continue;
                }
                // It grew past the limit since it was looked at.
                Ok(Some(_)) => OmitReason::TooLarge,
                Err(crate::SessionError::NotARegularFile { .. }) => OmitReason::NotAFile,
                Ok(None) | Err(_) => OmitReason::Unreadable,
            },
        };
        omitted.push(Omitted { path, reason });
    }
}

enum DiffFailure {
    TooLarge,
    Failed(String),
}

fn diff(
    run: &dyn Fn(&[&str], usize) -> io::Result<GitOutput>,
    against: &[&str],
    exclusions: &[String],
    limit: usize,
) -> Result<Vec<u8>, DiffFailure> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(&DIFF_FLAGS);
    args.extend_from_slice(&["--src-prefix=a/", "--dst-prefix=b/"]);
    args.extend_from_slice(against);
    args.extend_from_slice(&["--", "."]);
    args.extend(exclusions.iter().map(String::as_str));
    match run(&args, limit) {
        Ok(output) if output.truncated => Err(DiffFailure::TooLarge),
        Ok(output) if output.success => Ok(output.stdout),
        Ok(_) => Err(DiffFailure::Failed(
            "git could not make the patch".to_owned(),
        )),
        Err(error) => Err(DiffFailure::Failed(format!(
            "git could not make the patch: {error}"
        ))),
    }
}

fn split_nul(output: &[u8]) -> Vec<String> {
    output
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

/// Lists a workspace that is not a git repository.
fn from_listing(root: &AbsPath, clock: &dyn Clock, limits: &BaseLimits) -> BaseState {
    let started = clock.now();
    let mut files = Vec::new();
    let mut omitted = Vec::new();
    let mut complete = true;
    let mut stack = vec![(root.to_fs_path(), String::new())];
    'walk: while let Some((directory, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            omitted.push(Omitted {
                path: if prefix.is_empty() {
                    ".".to_owned()
                } else {
                    prefix
                },
                reason: OmitReason::Unreadable,
            });
            complete = false;
            continue;
        };
        let mut found: Vec<(String, std::fs::DirEntry)> = entries
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_name()
                    .into_string()
                    .ok()
                    .map(|name| (name, entry))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0));
        for (name, entry) in found {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push((entry.path(), path));
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            if clock
                .now()
                .duration_since(started)
                .is_some_and(|took| took > limits.listing_time)
            {
                omitted.push(Omitted {
                    path: "*".to_owned(),
                    reason: OmitReason::OutOfTime,
                });
                complete = false;
                break 'walk;
            }
            if files.len() >= limits.listed_files {
                omitted.push(Omitted {
                    path: "*".to_owned(),
                    reason: OmitReason::TooMany,
                });
                complete = false;
                break 'walk;
            }
            if is_likely_secret(&path) {
                omitted.push(Omitted {
                    path,
                    reason: OmitReason::Secret,
                });
                continue;
            }
            let bytes = entry.metadata().map(|found| found.len()).unwrap_or(0);
            let hash = if bytes <= limits.hashed_bytes {
                match hash_file(&entry.path()) {
                    Ok(Some(version)) => Some(version.hash),
                    _ => {
                        omitted.push(Omitted {
                            path,
                            reason: OmitReason::Unreadable,
                        });
                        complete = false;
                        continue;
                    }
                }
            } else {
                None
            };
            files.push(ListedFile { path, bytes, hash });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    BaseState {
        complete,
        files: Some(files),
        omitted,
        note: (!complete).then(|| "the listing of the workspace is incomplete".to_owned()),
        ..BaseState::default()
    }
}

/// Writes a base state's patch to `out`, for replay: the artifact's content.
///
/// # Errors
///
/// When the artifact cannot be read.
pub fn write_patch(store: &DiskStore, base: &GitBase, out: &mut dyn Write) -> crate::Result<bool> {
    let Some(patch) = &base.patch else {
        return Ok(false);
    };
    let mut file = store.open_artifact(&patch.id)?;
    io::copy(&mut file, out).map_err(crate::SessionError::io(
        "read the patch",
        store.artifact_path(&patch.id)?,
    ))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn likely_secrets_are_recognised_by_name() {
        for secret in [
            ".env",
            ".env.local",
            "config/.ENV.production",
            "certs/server.pem",
            "tls.key",
            "store.p12",
            "a\\b\\cert.pfx",
            ".netrc",
            "_netrc",
            "home/.ssh/id_ed25519",
            "id_rsa",
        ] {
            assert!(is_likely_secret(secret), "{secret}");
        }
        for ordinary in [
            "src/env.rs",
            "environment.md",
            "id_rsa.pub",
            "keys.rs",
            "monkey.txt",
            "src/identity.rs",
            "docs/pem-files.md",
        ] {
            assert!(!is_likely_secret(ordinary), "{ordinary}");
        }
    }

    #[test]
    fn omit_reasons_round_trip_and_tolerate_new_ones() {
        let reasons = [
            OmitReason::Secret,
            OmitReason::TooLarge,
            OmitReason::TooMany,
            OmitReason::TotalLimit,
            OmitReason::OutOfTime,
            OmitReason::Unreadable,
            OmitReason::NotAFile,
            OmitReason::Other("quarantined".to_owned()),
        ];
        for reason in reasons {
            let text = serde_json::to_string(&reason).unwrap();
            assert_eq!(serde_json::from_str::<OmitReason>(&text).unwrap(), reason);
        }
    }
}
