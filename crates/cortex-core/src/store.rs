//! Where a session is kept, as the loop reaches it
//! (`docs/architecture/03-agent-loop.md`; `docs/architecture/07-sessions-and-context.md`).
//!
//! The loop sees a session as its own small `SessionStore` trait: read it
//! once, append records, end a turn. The real log is `cortex-session`'s,
//! which blocks. [`LogStore`] joins them, running each call on the blocking
//! pool, and is also where the things that happen *because* something was
//! recorded are done:
//!
//! - **The round's checkpoint is written before the round's results.**
//!   Whatever the round's tools changed is in the checkpoint by the time
//!   the loop records a result ([`crate::checkpoints`]), so the entry goes
//!   in ahead of the first record that follows.
//! - **A message with a secret in a provider's own block is stored as its
//!   neutral projection** and marked, and the user is told
//!   ([`crate::redact`]). The loop's copy is untouched.
//! - **A resumed session's reasoning is dropped where the log was
//!   altered**, and the user is told how many messages that touched.
//! - **The plan is kept with the conversation.** A `plan` call that
//!   replaced the plan, and a user's edit, each leave a `plan` entry, and a
//!   notice of an edit that the loop has recorded is reported to the plan
//!   board, which then lets the model's next `plan` call through.
//! - **The base state is appended when its capture is ready**, and the user
//!   is told if it could not be recorded.
//!
//! A session with no log (`ephemeral`) is kept in memory through the same
//! adapter, so everything above but the disk behaves the same.

#![allow(
    clippy::disallowed_types,
    reason = "the session store blocks, and is called on the blocking pool"
)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use cortex_agent::{
    EpochReason, EpochStart, Opened, Record, SessionHandle, SessionStore, StoreError,
};
use cortex_llm::ToolDefinition;
use cortex_protocol::event::{Actor, Event, InfoNotice, Notice, PlanUpdated, TurnCompleted};
use cortex_protocol::item::NoticeSource;
use cortex_protocol::{Item, Plan, TurnId};
use cortex_session::entry::{Attempt, Epoch, InstructionMark, Outcome, PlanRecord, UsageRecord};
use cortex_session::{BaseCapture, Body, Entry};
use cortex_tools::{BoxFuture, PlanBoard};

use crate::checkpoints::Rounds;
use crate::redact;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Each change behind these locks is whole before the lock is let go.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A session kept on disk.
pub(crate) struct Disk {
    pub(crate) session: cortex_session::Session,
    pub(crate) rounds: Arc<Rounds>,
    pub(crate) filter: crate::provider::SharedFilter,
    /// The capture of the workspace's starting state, until it is ready.
    pub(crate) base: Option<BaseCapture>,
}

/// A session kept in memory only.
#[derive(Debug, Default)]
pub(crate) struct Memory {
    items: Vec<Item>,
    plan: Option<Plan>,
    epochs: Vec<(usize, EpochStart)>,
    carried: Option<serde_json::Value>,
    last_turn: u64,
    last_item: u64,
}

/// An epoch as the log's entry holds it.
fn epoch_body(epoch: EpochStart) -> Body {
    let tools: Vec<serde_json::Value> = epoch
        .tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.schema,
            })
        })
        .collect();
    Body::Epoch(Epoch {
        provider: epoch.provider,
        protocol: epoch.protocol,
        model: epoch.model,
        effort: epoch.effort,
        hash: Epoch::hash_of(&epoch.system, &tools),
        system: epoch.system,
        tools,
        reason: epoch.reason.map(|reason| reason.as_str().to_owned()),
        base: epoch.base,
        instructions: epoch
            .instructions
            .into_iter()
            .map(|(path, hash)| InstructionMark { path, hash })
            .collect(),
    })
}

/// An epoch as the loop is given it, from the log's entry. `redacted` is
/// the entry's mark: the filter masked something in it.
fn epoch_start(epoch: &Epoch, redacted: bool) -> EpochStart {
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    EpochStart {
        provider: epoch.provider.clone(),
        protocol: epoch.protocol.clone(),
        model: epoch.model.clone(),
        effort: epoch.effort.clone(),
        system: epoch.system.clone(),
        tools: epoch
            .tools
            .iter()
            .map(|tool| ToolDefinition {
                name: text(tool, "name"),
                description: text(tool, "description"),
                schema: tool.get("parameters").cloned().unwrap_or_default(),
            })
            .collect(),
        reason: epoch.reason.as_deref().and_then(EpochReason::from_name),
        base: epoch.base,
        instructions: epoch
            .instructions
            .iter()
            .map(|mark| (mark.path.clone(), mark.hash))
            .collect(),
        // The hash was taken of the prompt and the tools as they were sent,
        // before the filter: one that no longer matches is of a prompt the
        // log holds masked.
        altered: redacted || Epoch::hash_of(&epoch.system, &epoch.tools) != epoch.hash,
    }
}

/// The epochs of a branch's entries, each with the number of items before
/// it, and what the last summary carried.
fn epochs_of(entries: &[Entry]) -> (Vec<(usize, EpochStart)>, Option<serde_json::Value>) {
    let mut epochs = Vec::new();
    let mut carried = None;
    let mut items = 0;
    for entry in entries {
        match &entry.body {
            Body::Epoch(epoch) => epochs.push((items, epoch_start(epoch, entry.redacted))),
            Body::Summary { carried: value, .. } => carried = Some(value.clone()),
            _ => {}
        }
        if entry.body.as_item().is_some() {
            items += 1;
        }
    }
    (epochs, carried)
}

/// Where a session is kept.
#[expect(
    clippy::large_enum_variant,
    reason = "one per session, made once and kept behind a lock"
)]
pub(crate) enum Kept {
    Disk(Disk),
    Memory(Memory),
}

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disk(disk) => f
                .debug_struct("Disk")
                .field("session", disk.session.id())
                .finish_non_exhaustive(),
            Self::Memory(memory) => f
                .debug_struct("Memory")
                .field("items", &memory.items.len())
                .finish_non_exhaustive(),
        }
    }
}

/// What one append did besides appending.
#[derive(Debug, Default)]
struct Appended {
    /// The item was stored as its neutral projection.
    altered: bool,
    /// What could not be recorded of the base state.
    base_problem: Option<String>,
    /// The session is written without its lock.
    unlocked: bool,
}

fn number_after(id: &str, prefix: &str) -> u64 {
    id.strip_prefix(prefix)
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

impl Kept {
    /// A session in memory, empty.
    pub(crate) fn memory() -> Self {
        Self::Memory(Memory::default())
    }

    /// The session as it stands, and how many of its messages were
    /// projected because the log before them was altered.
    fn open(&mut self) -> Result<(Opened, usize), String> {
        match self {
            Self::Disk(disk) => {
                let entries = disk
                    .session
                    .read_branch()
                    .map_err(|error| error.to_string())?;
                let resumed = redact::resumed(&entries);
                let (epochs, carried) = epochs_of(&entries);
                let (last_turn, last_item) = disk.session.last_ids();
                Ok((
                    Opened {
                        items: resumed.items,
                        last_turn,
                        last_item,
                        epochs,
                        carried,
                    },
                    resumed.projected,
                ))
            }
            Self::Memory(memory) => Ok((
                Opened {
                    items: memory.items.clone(),
                    last_turn: memory.last_turn,
                    last_item: memory.last_item,
                    epochs: memory.epochs.clone(),
                    carried: memory.carried.clone(),
                },
                0,
            )),
        }
    }

    /// Writes the checkpoint of the round that has just run, if it changed
    /// a file, and the base state, if it is ready.
    fn settle(&mut self, turn: &TurnId, appended: &mut Appended) -> Result<(), String> {
        let Self::Disk(disk) = self else {
            return Ok(());
        };
        match disk.rounds.take() {
            Ok(Some(checkpoint)) => {
                disk.session
                    .append(Some(turn), checkpoint)
                    .map_err(|error| error.to_string())?;
            }
            Ok(None) => {}
            // A file the round changed cannot be read to say what it is
            // now. The turn goes on: the round's results are worth more
            // than its checkpoint, and an undo will say what it cannot do.
            Err(error) => {
                tracing::warn!(%error, "a round's checkpoint could not be taken");
            }
        }
        if let Some(state) = disk.base.as_mut().and_then(BaseCapture::try_take) {
            disk.base = None;
            if let Err(error) = disk.session.append(None, Body::Base(state)) {
                appended.base_problem = Some(error.to_string());
            }
        }
        Ok(())
    }

    fn append(&mut self, turn: &TurnId, record: Record) -> Result<Appended, String> {
        let mut appended = Appended::default();
        self.settle(turn, &mut appended)?;
        match self {
            Self::Disk(disk) => {
                match record {
                    Record::Item(item) => match redact::for_disk(
                        &disk
                            .filter
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                        &item,
                    ) {
                        Some(stored) => {
                            let body = Body::from_item(stored)
                                .ok_or("an item of a kind this version does not know")?;
                            disk.session
                                .append_altered(Some(turn), body)
                                .map_err(|error| error.to_string())?;
                            appended.altered = true;
                        }
                        None => {
                            disk.session
                                .append_item(Some(turn), item)
                                .map_err(|error| error.to_string())?;
                        }
                    },
                    Record::Epoch(epoch) => {
                        disk.session
                            .append(Some(turn), epoch_body(epoch))
                            .map_err(|error| error.to_string())?;
                    }
                    Record::Summary(summary) => {
                        disk.session
                            .append(
                                Some(turn),
                                Body::Summary {
                                    item: summary.item,
                                    carried: summary.carried,
                                },
                            )
                            .map_err(|error| error.to_string())?;
                    }
                    Record::Usage { purpose, usage } => {
                        disk.session
                            .append(Some(turn), Body::Usage(UsageRecord { purpose, usage }))
                            .map_err(|error| error.to_string())?;
                    }
                    Record::Attempt(attempt) => {
                        let body = Body::Attempt(Attempt {
                            item: Some(attempt.item_id),
                            attempt: attempt.attempt,
                            error: Some(attempt.error),
                            // What had arrived is kept for debugging, and
                            // never replayed: its raw blocks are not
                            // needed, and are not written.
                            parts: attempt
                                .partial
                                .iter()
                                .filter_map(redact::neutral_part)
                                .collect(),
                        });
                        disk.session
                            .append(Some(turn), body)
                            .map_err(|error| error.to_string())?;
                    }
                    _ => return Err("a record of a kind this version does not know".to_owned()),
                }
                appended.unlocked = disk.session.is_unlocked();
            }
            Self::Memory(memory) => {
                memory.last_turn = memory.last_turn.max(number_after(turn.as_str(), "t_"));
                match record {
                    Record::Item(item) => {
                        if let Some(id) = item.id() {
                            memory.last_item = memory.last_item.max(number_after(id, "i_"));
                        }
                        memory.items.push(item);
                    }
                    Record::Attempt(attempt) => {
                        memory.last_item = memory
                            .last_item
                            .max(number_after(attempt.item_id.as_str(), "i_"));
                    }
                    Record::Epoch(epoch) => memory.epochs.push((memory.items.len(), epoch)),
                    Record::Summary(summary) => {
                        memory.last_item = memory
                            .last_item
                            .max(number_after(summary.item.id.as_str(), "i_"));
                        memory.carried = Some(summary.carried);
                        memory.items.push(Item::Summary(summary.item));
                    }
                    Record::Usage { .. } => {}
                    _ => return Err("a record of a kind this version does not know".to_owned()),
                }
            }
        }
        Ok(appended)
    }

    fn end_turn(&mut self, turn: &TurnId, outcome: &TurnCompleted) -> Result<Appended, String> {
        let mut appended = Appended::default();
        self.settle(turn, &mut appended)?;
        if let Self::Disk(disk) = self {
            // Nothing is written for a session that never got a prompt on
            // disk: an outcome alone would not create its log.
            if disk.session.is_born() {
                disk.session
                    .append(Some(turn), Body::Outcome(Outcome::completed(outcome)))
                    .map_err(|error| error.to_string())?;
            }
            disk.session.end_turn().map_err(|error| error.to_string())?;
            appended.unlocked = disk.session.is_unlocked();
        }
        Ok(appended)
    }

    /// Records the plan as it now is, on the active branch.
    pub(crate) fn record_plan(&mut self, turn: Option<&TurnId>, plan: &Plan) -> Result<(), String> {
        match self {
            Self::Disk(disk) => disk
                .session
                .append(turn, Body::Plan(PlanRecord { plan: plan.clone() }))
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Self::Memory(memory) => {
                memory.plan = Some(plan.clone());
                Ok(())
            }
        }
    }

    /// The items of the active branch, as the log holds them.
    pub(crate) fn items(&self) -> Result<Vec<Item>, String> {
        match self {
            Self::Disk(disk) => disk.session.items().map_err(|error| error.to_string()),
            Self::Memory(memory) => Ok(memory.items.clone()),
        }
    }

    /// The disk session, when there is one.
    pub(crate) fn disk(&mut self) -> Option<&mut Disk> {
        match self {
            Self::Disk(disk) => Some(disk),
            Self::Memory(_) => None,
        }
    }
}

/// The plan of a branch: the last one recorded on it.
///
/// This is the one place that says where a session's plan is kept between
/// runs: in `plan` entries of the conversation tree, one after each change.
/// A change by the model's `plan` call is written **before the call's
/// result**, because the plan changed when the call ran: a branch that
/// holds the result holds the plan it made, so a rewind or a fork at the
/// result keeps it, and one at the response that made the call does not.
/// A change by the user is written when it is made. **A compaction carries
/// the plan in its `summary` entry** (`carried.plan`), which is read here
/// as the plan of the point where the new epoch begins: a summary with no
/// plan in it says nothing, and the search goes on before it.
pub(crate) fn plan_of(entries: &[Entry]) -> Option<Plan> {
    entries.iter().rev().find_map(|entry| match &entry.body {
        Body::Plan(record) => Some(record.plan.clone()),
        Body::Summary { carried, .. } => carried
            .get("plan")
            .filter(|plan| !plan.is_null())
            .and_then(|plan| serde_json::from_value(plan.clone()).ok()),
        _ => None,
    })
}

/// A session's log, shared by the loop's store and the core.
pub(crate) type SharedLog = Arc<Mutex<Kept>>;

/// What the store does once something is recorded, and how it reaches the
/// session's front ends.
pub(crate) struct Hooks {
    handle: OnceLock<SessionHandle>,
    /// What to tell front ends once there is a handle to tell them through.
    waiting: Mutex<Vec<Event>>,
    pub(crate) board: Arc<PlanBoard>,
    /// The revisions of the plan whose notices were handed to the loop and
    /// have not been recorded yet, oldest first.
    handed: Mutex<VecDeque<u64>>,
    warned_unlocked: AtomicBool,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

impl Hooks {
    pub(crate) fn new(board: Arc<PlanBoard>) -> Self {
        Self {
            handle: OnceLock::new(),
            waiting: Mutex::new(Vec::new()),
            board,
            handed: Mutex::new(VecDeque::new()),
            warned_unlocked: AtomicBool::new(false),
        }
    }

    /// Gives the hooks the session's handle, and sends what was waiting
    /// for it.
    pub(crate) fn bind(&self, handle: SessionHandle) {
        let _ = self.handle.set(handle);
        let waiting = std::mem::take(&mut *lock(&self.waiting));
        if let Some(handle) = self.handle.get() {
            for event in waiting {
                handle.emit(event);
            }
        }
    }

    /// Sends front ends an event, now or as soon as the session is open.
    pub(crate) fn emit(&self, event: Event) {
        match self.handle.get() {
            Some(handle) => handle.emit(event),
            None => lock(&self.waiting).push(event),
        }
    }

    /// Tells the user something in a line.
    pub(crate) fn tell(&self, message: impl Into<String>) {
        self.emit(Event::Notice(Notice::Info(InfoNotice {
            message: message.into(),
        })));
    }

    /// A notice of the plan at `revision` was handed to the loop.
    pub(crate) fn handed_plan_notice(&self, revision: u64) {
        lock(&self.handed).push_back(revision);
    }

    /// Forgets the notices that were handed over and never recorded: the
    /// session moved to another point.
    pub(crate) fn forget_plan_notices(&self) {
        lock(&self.handed).clear();
    }

    fn after(&self, appended: &Appended) {
        if appended.altered {
            self.tell(
                "A provider block of the last response held what looks like a credential. The \
                 response was stored without its provider blocks, so nothing of it is on disk. \
                 This session goes on as before; if it is resumed later, the reasoning from that \
                 response onwards will not be sent back to the model.",
            );
        }
        if let Some(problem) = &appended.base_problem {
            self.tell(format!(
                "The workspace's starting state could not be recorded, so this session cannot be \
                 replayed from its start: {problem}"
            ));
        }
        if appended.unlocked && !self.warned_unlocked.swap(true, Ordering::Relaxed) {
            self.tell(
                "The file system the session is stored on cannot lock files. Nothing stops a \
                 second Cortex-Harness from writing this same session: open it in one place at \
                 a time.",
            );
        }
    }
}

/// The loop's store over a [`SharedLog`].
#[derive(Debug)]
pub(crate) struct LogStore {
    log: SharedLog,
    hooks: Arc<Hooks>,
}

impl LogStore {
    pub(crate) fn new(log: SharedLog, hooks: Arc<Hooks>) -> Self {
        Self { log, hooks }
    }

    /// Runs `work` on the session's log, on the blocking pool.
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Kept) -> Result<T, String> + Send + 'static,
    ) -> Result<T, StoreError> {
        let log = Arc::clone(&self.log);
        match tokio::task::spawn_blocking(move || work(&mut lock(&log))).await {
            Ok(done) => done.map_err(StoreError::new),
            Err(_) => Err(StoreError::new("the write to the session was abandoned")),
        }
    }
}

/// What of an item the hooks act on, read before the item is handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Nothing,
    /// A notice of the plan was recorded.
    PlanNotice,
    /// A `plan` call replaced the plan.
    PlanReplaced,
}

fn seen(record: &Record) -> Seen {
    match record {
        Record::Item(Item::Notice(notice)) if notice.source == NoticeSource::Plan => {
            Seen::PlanNotice
        }
        Record::Item(Item::ToolResult(result))
            if result.tool == "plan"
                && result
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("replaced"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true) =>
        {
            Seen::PlanReplaced
        }
        _ => Seen::Nothing,
    }
}

impl SessionStore for LogStore {
    fn open(&mut self) -> BoxFuture<'_, Result<Opened, StoreError>> {
        Box::pin(async move {
            let (opened, projected) = self.blocking(Kept::open).await?;
            if projected > 0 {
                self.hooks.tell(format!(
                    "This session's log was changed when it was stored (a credential was \
                     masked), so the reasoning of {projected} earlier response{} is not sent \
                     back to the model. The text and the tool calls are.",
                    if projected == 1 { "" } else { "s" }
                ));
            }
            Ok(opened)
        })
    }

    fn append<'a>(
        &'a mut self,
        turn: &'a TurnId,
        record: Record,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let seen = seen(&record);
            // A `plan` call changed the plan when it ran, so the plan it
            // made is written before its result: the branch that holds the
            // result holds the plan, and a rewind or a fork at the result
            // keeps it (`plan_of`).
            let replaced = if seen == Seen::PlanReplaced {
                let plan = self.hooks.board.get();
                let recorded = plan.clone();
                let turn_id = turn.clone();
                // The plan is the session's working state: a turn does not
                // fail because it could not be written down.
                if let Err(error) = self
                    .blocking(move |kept| kept.record_plan(Some(&turn_id), &recorded))
                    .await
                {
                    tracing::warn!(%error, "the plan could not be recorded");
                }
                Some(plan)
            } else {
                None
            };
            let turn_id = turn.clone();
            let appended = self
                .blocking(move |kept| kept.append(&turn_id, record))
                .await?;
            self.hooks.after(&appended);
            if seen == Seen::PlanNotice {
                // The model will see what the user changed with the next
                // request: its next `plan` call goes through.
                let delivered = lock(&self.hooks.handed).pop_front();
                if let Some(revision) = delivered {
                    self.hooks.board.delivered(revision);
                }
            }
            if let Some(plan) = replaced {
                self.hooks.emit(Event::PlanUpdated(PlanUpdated {
                    plan,
                    by: Actor::Agent,
                }));
            }
            Ok(())
        })
    }

    fn end_turn<'a>(
        &'a mut self,
        turn: &'a TurnId,
        outcome: &'a TurnCompleted,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let turn_id = turn.clone();
            let outcome = outcome.clone();
            let appended = self
                .blocking(move |kept| kept.end_turn(&turn_id, &outcome))
                .await?;
            self.hooks.after(&appended);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::Timestamp;
    use cortex_protocol::item::{PlanItem, PlanStatus, SummaryItem};
    use cortex_protocol::{ItemId, PlanItemId};
    use cortex_session::EntryId;

    use super::*;

    fn entry(body: Body) -> Entry {
        Entry {
            id: EntryId::new("e_0001"),
            parent: None,
            ts: Timestamp::from("2026-11-14T00:00:00Z"),
            turn: None,
            redacted: false,
            body,
        }
    }

    fn plan(text: &str) -> Plan {
        Plan {
            items: vec![PlanItem {
                id: PlanItemId::new("p1"),
                text: text.to_owned(),
                status: PlanStatus::Active,
            }],
            revision: 1,
        }
    }

    fn summary(carried: serde_json::Value) -> Entry {
        entry(Body::Summary {
            item: SummaryItem {
                id: ItemId::new("i_9"),
                text: "The summary.".to_owned(),
                covers_through: None,
                usage: None,
                created_at: None,
            },
            carried,
        })
    }

    #[test]
    fn the_plan_a_summary_carries_is_the_plan_where_its_epoch_begins() {
        let before = entry(Body::Plan(PlanRecord {
            plan: plan("before"),
        }));
        let carried = summary(serde_json::json!({"plan": plan("carried")}));
        assert_eq!(
            plan_of(&[before.clone(), carried.clone()]),
            Some(plan("carried"))
        );
        // A later change of the plan is the plan.
        let after = entry(Body::Plan(PlanRecord {
            plan: plan("after"),
        }));
        assert_eq!(
            plan_of(&[before.clone(), carried, after]),
            Some(plan("after"))
        );
        // A summary that carries no plan says nothing about it.
        let none = summary(serde_json::json!({"plan": null}));
        assert_eq!(plan_of(&[before, none]), Some(plan("before")));
    }

    #[test]
    fn the_epochs_of_a_branch_are_placed_among_its_items() {
        let epoch = EpochStart {
            provider: cortex_protocol::ProviderId::new("p"),
            protocol: "test".to_owned(),
            model: cortex_protocol::ModelId::new("m"),
            effort: Some("high".to_owned()),
            system: "be brief".to_owned(),
            tools: vec![ToolDefinition {
                name: "read".to_owned(),
                description: "Reads.".to_owned(),
                schema: serde_json::json!({"type": "object"}),
            }],
            reason: Some(EpochReason::Compaction),
            base: Some(cortex_base::ContentHash::of(b"base")),
            instructions: vec![("AGENTS.md".to_owned(), cortex_base::ContentHash::of(b"x"))],
            altered: false,
        };
        let entries = [
            entry(epoch_body(epoch.clone())),
            summary(serde_json::json!({"files": []})),
            entry(epoch_body(epoch.clone())),
        ];
        let (epochs, carried) = epochs_of(&entries);
        assert_eq!(epochs, [(0, epoch.clone()), (1, epoch.clone())]);
        assert_eq!(carried, Some(serde_json::json!({"files": []})));

        // A prompt the filter masked something in is not the one that was
        // sent: the hash says so, and so does the mark.
        let Body::Epoch(mut masked) = epoch_body(epoch.clone()) else {
            unreachable!()
        };
        masked.system = "be [redacted]".to_owned();
        let (epochs, _) = epochs_of(&[entry(Body::Epoch(masked))]);
        assert!(epochs[0].1.altered);
        let mut marked = entry(epoch_body(epoch));
        marked.redacted = true;
        assert!(epochs_of(&[marked]).0[0].1.altered);
    }
}
