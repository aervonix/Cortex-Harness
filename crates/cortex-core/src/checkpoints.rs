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
use cortex_sandbox::protect::{Before as Was, Changed, Snapshot};
use cortex_session::entry::FileVersion;
use cortex_session::{Before, Body, DiskStore, Git, RoundCheckpoint, SessionError};
use cortex_tools::{
    BoxFuture, CheckpointError, Checkpoints, Effect, FileSnapshot, Json, Plan, PlanCx, Preview,
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
    /// The paths the harness's own tools have changed since the last look
    /// after a confined command, by [`AbsPath::key`]: so that a change the
    /// continuity check finds that one of them made is not reported as a
    /// change made from outside (found by the third review of step 26). It
    /// spans rounds — an `edit` in one turn and a confined command in the
    /// next — and is cleared when the next after-look is taken.
    touched: Mutex<std::collections::BTreeSet<String>>,
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
            touched: Mutex::new(std::collections::BTreeSet::new()),
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

    /// Notes that one of the harness's tools changed `file`, so the
    /// continuity check does not report it as a change from outside.
    fn note_touched(&self, file: &AbsPath) {
        self.touched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(file.key().to_owned());
    }

    /// The paths the harness's tools have changed since the last after-look.
    pub(crate) fn touched(&self) -> std::collections::BTreeSet<String> {
        self.touched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Forgets the touched paths: called when the look after a confined
    /// command is taken, which already holds their new state.
    pub(crate) fn clear_touched(&self) {
        self.touched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
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
    ///
    /// Returns the paths undo will not put back: outside the workspace (a
    /// `sandbox.protect` entry), whose earlier state was not kept (a link,
    /// a file too large, a change of mode), or a directory.
    pub(crate) fn record_protected(&self, changed: &[Changed]) -> Vec<std::path::PathBuf> {
        let mut by_hand = Vec::new();
        for changed in changed {
            let Some(file) = changed
                .path
                .strip_prefix(self.root.to_fs_path())
                .ok()
                .and_then(|relative| relative.to_str())
                .and_then(|relative| self.root.join(relative).ok())
            else {
                by_hand.push(changed.path.clone());
                continue;
            };
            let recorded = match &changed.before {
                Was::Absent => self.before(&file, None).is_ok(),
                Was::File(content) => self.before(&file, Some(content)).is_ok(),
                Was::Unknown => {
                    self.lock().seen_changed(&file, Before::Unknown);
                    false
                }
                Was::Directory => false,
            };
            // Undo leaves a path reached through a link, and cannot put
            // back a mode.
            if !recorded || changed.mode_changed || reached_through_a_link(&self.root, &file) {
                by_hand.push(changed.path.clone());
            }
        }
        by_hand
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
        self.note_touched(file);
        self.lock().record_before(file, before);
        Ok(())
    }

    fn after(&self, file: &AbsPath, left: Option<&FileSnapshot>) {
        // `None`: there is no file, which is what a round records for one
        // it removed, or could not create.
        self.note_touched(file);
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

/// Whether a directory between `root` and `file`, or `file` itself, is a
/// link now: undo will not put such a file back
/// (`cortex_session`'s `through_a_link`).
#[allow(
    clippy::disallowed_methods,
    reason = "called while the round is recorded, on the blocking pool"
)]
fn reached_through_a_link(root: &AbsPath, file: &AbsPath) -> bool {
    let top = root.to_fs_path();
    let target = file.to_fs_path();
    let Ok(below) = target.strip_prefix(&top) else {
        return false;
    };
    let mut walked = top.clone();
    below.components().any(|part| {
        walked.push(part);
        std::fs::symlink_metadata(&walked).is_ok_and(|metadata| metadata.file_type().is_symlink())
    })
}

/// Any tool of a session, held to a stop a command made: once a confined
/// command of the turn changed protected paths, no call of the turn that
/// changes anything runs, even one decided or approved before the stop
/// (found by the reviews of step 26). A call that only reads still runs.
pub(crate) struct StopsWithTurn {
    tool: Arc<dyn Tool>,
    policy: Arc<EnginePolicy>,
}

impl StopsWithTurn {
    pub(crate) fn new(tool: Arc<dyn Tool>, policy: Arc<EnginePolicy>) -> Self {
        Self { tool, policy }
    }
}

impl Tool for StopsWithTurn {
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
        if plan.has_side_effects()
            && let Some(reason) = self.policy.stopped_turn(cx.turn)
        {
            return Box::pin(async move {
                ToolResult::denied(format!(
                    "The call was not run: the turn was stopped. {reason}"
                ))
            });
        }
        self.tool.run(plan, cx, out)
    }

    // A question about a change to a file shows the change: the wrapped
    // tool's preview, which a wrapper must not lose.
    fn preview(&self, plan: &Plan, cx: &RunCx) -> Option<Preview> {
        self.tool.preview(plan, cx)
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

    fn preview(&self, plan: &Plan, cx: &RunCx) -> Option<Preview> {
        self.tool.preview(plan, cx)
    }

    fn run<'a>(
        &'a self,
        plan: Plan,
        cx: &'a RunCx,
        out: &'a mut dyn ToolOutput,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            // A turn a command of it stopped runs no more of its calls:
            // [`StopsWithTurn`], around every tool, sees to that.
            //
            // Every confined command has its protected paths looked at
            // before and after it, one the engine marked as only reading
            // too: the program it runs may do otherwise. One the user let
            // leave the sandbox is not looked at.
            let unconfined = plan.effects.contains(&Effect::Unconfined);
            let watch = self.sandbox.watch().filter(|_| !unconfined).cloned();
            let before = match &watch {
                Some(watch) => {
                    let watch = watch.clone();
                    tokio::task::spawn_blocking(move || watch.take()).await.ok()
                }
                None => None,
            };
            // Before the command runs, the look before it is compared with
            // the look after the last confined command: a protected path
            // that changed in between, and that none of the harness's own
            // tools changed, was changed from outside — a process an earlier
            // command left running, or the user's own hand — and stops the
            // turn, whether or not the kill reported an escape (found by the
            // third review of step 26).
            if let Some(before) = &before
                && let Some(stop) = self.continuity(before, cx).await
            {
                return stop;
            }
            // The command itself runs on this thread, the core's: the
            // kernel's death signal is tied to the thread that started it.
            let mut result = self.tool.run(plan, cx, out).await;
            let found = match (watch, before) {
                (Some(watch), Some(before)) => tokio::task::spawn_blocking(move || {
                    let (changed, after) = watch.changes_since(&before);
                    let complete = (before.is_complete(), after.is_complete());
                    (changed, complete, after)
                })
                .await
                .ok(),
                _ => None,
            };
            // The look after this command is kept for the next command's
            // and the close's continuity check, and the harness-touched set
            // is forgotten: the after-look already holds their new state.
            if let Some((_, _, after)) = &found {
                self.sandbox.remember_after(after.clone());
                if let Some(rounds) = &self.rounds {
                    rounds.clear_touched();
                }
            } else if unconfined && self.sandbox.watch().is_some() {
                // A command the user let leave the sandbox is not looked at,
                // so its own approved changes to a protected path are not
                // recorded. The next confined command's continuity check must
                // not compare across it and report those as a change from
                // outside, so the baseline is dropped and the next command
                // starts fresh (found by the independent review of the fixes).
                self.sandbox.forget_after();
                if let Some(rounds) = &self.rounds {
                    rounds.clear_touched();
                }
            }
            let changes = result
                .payload()
                .and_then(|payload| payload.get("changes"))
                .filter(|changes| !changes.is_null())
                .cloned();
            if let Some(changes) = &changes {
                note_changed(changes, cx);
            }
            let mut by_hand = found
                .as_ref()
                .map(|(changed, _, _)| changed.iter().map(|one| one.path.clone()).collect())
                .unwrap_or_default();
            if let Some(rounds) = &self.rounds {
                let rounds = Arc::clone(rounds);
                let protected = found.as_ref().map(|(changed, _, _)| changed.clone());
                // Not cancelled: the command ran, and what it changed is
                // recorded whether or not the turn goes on.
                let recorded = tokio::task::spawn_blocking(move || {
                    let by_hand = protected.map(|protected| rounds.record_protected(&protected));
                    if let Some(changes) = changes {
                        rounds.record_changes(&changes);
                    }
                    by_hand
                })
                .await;
                match recorded {
                    Ok(Some(not_kept)) => by_hand = not_kept,
                    Ok(None) => {}
                    Err(_) => tracing::warn!("what a shell command changed could not be recorded"),
                }
            }
            if let Some((changed, complete, _)) = found {
                let root = cx.paths.root().to_fs_path();
                if let Some(note) = protected_note(&changed, complete, &root, &by_hand) {
                    result = result.with_part(Part::text(note.clone()));
                    // A change, or a look the command itself made stop
                    // short, stops the turn; a workspace already past the
                    // bound only warns.
                    if !changed.is_empty() || (complete.0 && !complete.1) {
                        self.policy.stop_turn(cx.turn, note);
                    }
                }
            }
            // A command that left a process the kill could not clear may
            // still be writing a protected path after the look: the turn
            // stops, so that nothing the model does next builds on a look
            // that may already be stale (found by the third review of
            // step 26).
            let escaped = result
                .payload()
                .and_then(|payload| payload.get("escaped"))
                .and_then(Json::as_bool)
                .unwrap_or(false);
            if escaped && self.sandbox.watch().is_some() {
                let note = "[A process the command started kept forking and could not all be \
                            killed. It may still be running inside the sandbox and may change a \
                            protected path after this check — a git hook (`.git/hooks`), git's \
                            configuration (`.git/config`), a CI file (`.github/workflows`), \
                            `.cortex` or another watched name. The turn stops here, and the \
                            protected paths are looked at again before the next command and at \
                            the session's close.]";
                self.policy.stop_turn(cx.turn, note.to_owned());
            }
            result
        })
    }
}

/// Tells the table of seen files which files a command changed, for what a
/// compaction carries across (`docs/architecture/07-sessions-and-context.md`,
/// "Compaction").
fn note_changed(changes: &Json, cx: &RunCx) {
    let root = cx.paths.root();
    for list in ["created", "modified", "deleted"] {
        let paths = changes.get(list).and_then(Json::as_array);
        for path in paths.into_iter().flatten().filter_map(Json::as_str) {
            if let Ok(file) = root.join(path) {
                cx.files.note_changed(&file);
            }
        }
    }
}

impl RecordingShell {
    /// Compares the look before this command with the look after the last
    /// confined command, and if a protected path changed in between that
    /// none of the harness's own tools changed, records it, stops the turn,
    /// and returns the result that says so — so this command does not run on
    /// a workspace something outside the harness has changed. `None` when
    /// nothing changed from outside.
    async fn continuity(&self, before: &Snapshot, cx: &RunCx) -> Option<ToolResult> {
        let changed = self.sandbox.continuity(before)?;
        // What the harness's own tools changed since the last after-look is
        // not a change from outside.
        let touched = self.rounds.as_ref().map(|rounds| rounds.touched());
        let changed: Vec<Changed> = changed
            .into_iter()
            .filter(|one| {
                touched.as_ref().is_none_or(|touched| {
                    AbsPath::from_os(&one.path)
                        .ok()
                        .is_none_or(|path| !touched.contains(path.key()))
                })
            })
            .collect();
        if changed.is_empty() {
            return None;
        }
        let root = cx.paths.root().to_fs_path();
        let by_hand = match &self.rounds {
            Some(rounds) => {
                let rounds = Arc::clone(rounds);
                let changed = changed.clone();
                tokio::task::spawn_blocking(move || rounds.record_protected(&changed))
                    .await
                    .unwrap_or_default()
            }
            None => changed.iter().map(|one| one.path.clone()).collect(),
        };
        let note = continuity_note(&changed, &root, &by_hand);
        self.policy.stop_turn(cx.turn, note.clone());
        Some(ToolResult::denied(format!(
            "The command was not run. {note}"
        )))
    }
}

/// What the model and the user are told when a protected path changed
/// between two confined commands, from outside the harness.
fn continuity_note(
    changed: &[Changed],
    root: &std::path::Path,
    by_hand: &[std::path::PathBuf],
) -> String {
    let shown: Vec<String> = changed
        .iter()
        .map(|one| {
            let path = one.path.strip_prefix(root).unwrap_or(&one.path);
            format!("`{}` ({})", path.display(), one.change.as_str())
        })
        .collect();
    let undo = if by_hand.len() == changed.len() {
        "Undoing this turn does not put them back: what they were is not kept, so they must be \
         put back by hand."
    } else {
        "Undoing this turn puts them back."
    };
    format!(
        "[A protected path changed since the last command, and none of the harness's own tools \
         changed it: a process an earlier command left running inside the sandbox, or a change \
         made by hand between turns. These decide what runs later outside the sandbox: {}. The \
         turn stops here, so that the user can look. {undo}]",
        shown.join(", ")
    )
}

#[cfg(test)]
mod tests;
