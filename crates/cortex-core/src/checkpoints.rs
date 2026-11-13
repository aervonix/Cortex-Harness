//! A tool round's checkpoint, as the tools fill it
//! (`docs/architecture/07-sessions-and-context.md`, "Snapshots and undo";
//! `docs/architecture/05-tools.md`, "Checkpoints").
//!
//! Undo of a turn puts back every file the turn's tools changed. The tools
//! are in `cortex-tools` and the checkpoints in `cortex-session`, and
//! neither can see the other, so this module is where they meet:
//!
//! - **The editing tools** (`write`, `edit`) hand over what a file held
//!   before they change it and what they left, through `cortex-tools`'
//!   [`Checkpoints`] seam, which [`Rounds`] implements over the round's
//!   `RoundCheckpoint`. The bytes a tool read are the bytes that are kept:
//!   nothing is read a second time.
//! - **The shell tool** cannot say beforehand what a command will change.
//!   It looks afterwards, and puts what it saw in its result's payload,
//!   `changes`. [`RecordingShell`] wraps the tool and turns that payload
//!   into the same round's checkpoint as soon as the command is over: a
//!   file the command created did not exist; a file whose earlier content
//!   git still has is stored from that content; any other changed file is
//!   recorded with its earlier state unknown, which an undo reports and
//!   never removes or overwrites.
//!
//! Both are done by the time the loop records the round's results, so the
//! session store takes the round ([`Rounds::take`]) and writes its
//! `checkpoint` entry before the first of them: a crash between the two
//! leaves a checkpoint for calls whose results a resume then repairs, never
//! results for changes nobody recorded.
//!
//! **Hashing.** A file the shell changed is hashed whole when the round is
//! taken, to record what the round left. That is about 0.4 s a gibibyte,
//! and nothing caps it: a command that rewrites a file of several
//! gibibytes makes the round's end that much slower. Left as it is, and
//! said in the step's report.

#![allow(
    clippy::disallowed_types,
    reason = "checkpoint calls block, and are made on the blocking pool"
)]

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_base::{AbsPath, ContentHash};
use cortex_protocol::Part;
use cortex_sandbox::protect::{Before as Was, Changed};
use cortex_session::entry::FileVersion;
use cortex_session::{Before, Body, DiskStore, Git, RoundCheckpoint, SessionError};
use cortex_tools::{
    BoxFuture, CheckpointError, Checkpoints, Concurrency, Effect, FileSnapshot, Json, Plan, PlanCx,
    RunCx, ShellTool, Tool, ToolError, ToolOutput, ToolResult, ToolSpec,
};

use crate::policy::EnginePolicy;
use crate::sandbox::{SessionSandbox, protected_note};

/// The checkpoint of the tool round that is running, shared by the tools
/// that fill it and the session store that writes it.
pub(crate) struct Rounds {
    store: Arc<DiskStore>,
    root: AbsPath,
    limit: u64,
    git: Arc<dyn Git>,
    round: Mutex<RoundCheckpoint>,
}

impl std::fmt::Debug for Rounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rounds")
            .field("root", &self.root)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

impl Rounds {
    /// The rounds of a session whose workspace is at `root`, keeping the
    /// content of files up to `limit` bytes in `store`.
    pub(crate) fn new(store: Arc<DiskStore>, root: AbsPath, limit: u64, git: Arc<dyn Git>) -> Self {
        Self {
            round: Mutex::new(RoundCheckpoint::with_limit(root.clone(), limit)),
            store,
            root,
            limit,
            git,
        }
    }

    fn lock(&self) -> MutexGuard<'_, RoundCheckpoint> {
        // A round is changed one whole record at a time.
        self.round.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The largest file whose content is kept.
    pub(crate) fn limit(&self) -> u64 {
        self.limit
    }

    /// Ends the round: its `checkpoint` entry, if a file changed, and a
    /// fresh round for what comes next. Blocks: it hashes the files whose
    /// result no tool recorded.
    ///
    /// # Errors
    ///
    /// When such a file cannot be read. The round is gone all the same:
    /// its files would fail the next round's entry too.
    pub(crate) fn take(&self) -> Result<Option<Body>, SessionError> {
        let round = std::mem::replace(
            &mut *self.lock(),
            RoundCheckpoint::with_limit(self.root.clone(), self.limit),
        );
        if round.is_empty() {
            return Ok(None);
        }
        round.finish()
    }

    /// Records what a shell command changed, from the `changes` of its
    /// result's payload. Blocks: it asks git for the earlier content of
    /// the files git still has.
    pub(crate) fn record_changes(&self, changes: &Json) {
        let list = |name: &str| -> Vec<&str> {
            changes
                .get(name)
                .and_then(Json::as_array)
                .map(|paths| paths.iter().filter_map(Json::as_str).collect())
                .unwrap_or_default()
        };
        let commit = changes.get("base_commit").and_then(Json::as_str);
        let restorable = list("restorable");
        let created = list("created");

        let mut seen: Vec<(AbsPath, Before)> = Vec::new();
        for path in &created {
            if let Ok(file) = self.root.join(path) {
                seen.push((file, Before::Absent));
            }
        }
        for path in list("modified").into_iter().chain(list("deleted")) {
            let Ok(file) = self.root.join(path) else {
                continue;
            };
            if created.contains(&path) {
                continue;
            }
            // Only what git can still give back, and only from the commit
            // the tool named, is known. Everything else that changed had
            // already been changed before the command, or git was not
            // asked.
            let before = match commit {
                Some(commit) if restorable.contains(&path) => self
                    .earlier_content(commit, path)
                    .map_or(Before::Unknown, Before::Version),
                _ => Before::Unknown,
            };
            seen.push((file, before));
        }
        let mut round = self.lock();
        for (file, before) in seen {
            round.seen_changed(&file, before);
        }
    }

    /// Records protected paths a command changed, with what they held
    /// before it, so that undoing the round puts them back. It comes before
    /// [`record_changes`](Self::record_changes): only the first record of
    /// a file in a round counts, and this one is exact. Blocks: it stores
    /// the content.
    pub(crate) fn record_protected(&self, changed: &[Changed]) {
        for changed in changed {
            let Some(file) = changed
                .path
                .strip_prefix(self.root.to_fs_path())
                .ok()
                .and_then(|relative| relative.to_str())
                .and_then(|relative| self.root.join(relative).ok())
            else {
                // Outside the workspace (a `sandbox.protect` entry): not
                // the round's to put back.
                continue;
            };
            let _ = match &changed.before {
                Was::Absent => self.before(&file, None),
                Was::File(content) => self.before(&file, Some(content)),
                Was::Unknown => {
                    self.lock().seen_changed(&file, Before::Unknown);
                    Ok(())
                }
                Was::Directory => Ok(()),
            };
        }
    }

    /// The content of `path` at `commit`, stored. `None` when git cannot
    /// give it, or it is not content the store can keep a record of.
    fn earlier_content(&self, commit: &str, path: &str) -> Option<FileVersion> {
        // A commit and a path are data here, never options: a name that
        // begins with a dash is not one git would take as a flag after
        // `--`, and a commit is a hash the tool read from git itself.
        if !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) || commit.is_empty() {
            return None;
        }
        // `./` makes the path relative to the workspace, wherever the
        // workspace is in its repository. `cat-file` gives the bytes as
        // they are stored, through no filter of the repository's.
        let object = format!("{commit}:./{path}");
        let limit = usize::try_from(self.limit).unwrap_or(usize::MAX);
        let output = self
            .git
            .run(
                &self.root.to_fs_path(),
                &["cat-file", "blob", &object],
                limit.saturating_add(1),
            )
            .ok()?;
        if !output.success {
            return None;
        }
        if output.truncated || output.stdout.len() as u64 > self.limit {
            // Too large to keep: there is no content to put back, and its
            // hash is not known either.
            return None;
        }
        self.store
            .put_stream(&mut output.stdout.as_slice(), self.limit)
            .ok()
    }
}

impl Checkpoints for Rounds {
    fn before(&self, file: &AbsPath, content: Option<&[u8]>) -> Result<(), CheckpointError> {
        let before = match content {
            None => Before::Absent,
            // The bytes the tool read are what is kept: no second read of
            // the file, and nothing between the two for it to become.
            Some(content) => {
                let bytes = content.len() as u64;
                let kept = bytes <= self.limit;
                let hash = if kept {
                    self.store
                        .put_blob(content)
                        .map_err(|error| CheckpointError::new(error.to_string()))?
                } else {
                    ContentHash::of(content)
                };
                Before::Version(FileVersion { hash, bytes, kept })
            }
        };
        // Only the first record of a file in a round counts.
        self.lock().record_before(file, before);
        Ok(())
    }

    fn after(&self, file: &AbsPath, left: Option<&FileSnapshot>) {
        // `None`: there is no file, which is what a round records for one
        // it removed, or could not create.
        self.lock().after(
            file,
            left.map(|left| FileVersion {
                hash: left.hash,
                bytes: left.size,
                kept: true,
            }),
        );
    }
}

/// The shell tool, with what each command changed recorded in the round's
/// checkpoint when the command is over, and the protected paths looked at
/// around each confined command (`docs/architecture/06-permissions-and-sandbox.md`,
/// "Protected paths").
#[derive(Debug)]
pub(crate) struct RecordingShell {
    tool: ShellTool,
    /// The session's checkpoints. `None` for a session kept in memory.
    rounds: Option<Arc<Rounds>>,
    sandbox: Arc<SessionSandbox>,
    policy: Arc<EnginePolicy>,
}

impl RecordingShell {
    pub(crate) fn new(
        tool: ShellTool,
        rounds: Option<Arc<Rounds>>,
        sandbox: Arc<SessionSandbox>,
        policy: Arc<EnginePolicy>,
    ) -> Self {
        Self {
            tool,
            rounds,
            sandbox,
            policy,
        }
    }
}

impl Tool for RecordingShell {
    fn spec(&self) -> &ToolSpec {
        self.tool.spec()
    }

    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError> {
        self.tool.plan(args, cx)
    }

    fn run<'a>(
        &'a self,
        plan: Plan,
        cx: &'a RunCx,
        out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            // A confined command that may write has its protected paths
            // looked at before and after it. One the engine vouched only
            // reads is not, nor one the user let leave the sandbox.
            let watch = self
                .sandbox
                .watch()
                .filter(|_| {
                    plan.concurrency != Concurrency::ReadOnly
                        && !plan.effects.contains(&Effect::Unconfined)
                })
                .cloned();
            let before = match &watch {
                Some(watch) => {
                    let watch = watch.clone();
                    tokio::task::spawn_blocking(move || watch.take()).await.ok()
                }
                None => None,
            };
            // The command itself runs on this thread, the core's: the
            // kernel's death signal is tied to the thread that started it.
            let mut result = self.tool.run(plan, cx, out).await;
            let found = match (watch, before) {
                (Some(watch), Some(before)) => tokio::task::spawn_blocking(move || {
                    let (changed, after) = watch.changes_since(&before);
                    (changed, before.is_complete() && after.is_complete())
                })
                .await
                .ok(),
                _ => None,
            };
            let changes = result
                .payload()
                .and_then(|payload| payload.get("changes"))
                .filter(|changes| !changes.is_null())
                .cloned();
            if let Some(rounds) = &self.rounds {
                let rounds = Arc::clone(rounds);
                let protected = found.as_ref().map(|(changed, _)| changed.clone());
                // Not cancelled: the command ran, and what it changed is
                // recorded whether or not the turn goes on.
                let recorded = tokio::task::spawn_blocking(move || {
                    if let Some(protected) = protected {
                        rounds.record_protected(&protected);
                    }
                    if let Some(changes) = changes {
                        rounds.record_changes(&changes);
                    }
                })
                .await;
                if recorded.is_err() {
                    tracing::warn!("what a shell command changed could not be recorded");
                }
            }
            if let Some((changed, complete)) = found {
                let root = cx.paths.root().to_fs_path();
                if let Some(note) = protected_note(&changed, complete, &root, self.rounds.is_some())
                {
                    result = result.with_part(Part::text(note.clone()));
                    if !changed.is_empty() {
                        self.policy.stop_turn(cx.turn, note);
                    }
                }
            }
            result
        })
    }
}

#[cfg(test)]
mod tests;
