//! Events: everything the core tells its front ends
//! (`docs/architecture/11-rpc-protocol.md`, "Events").
//!
//! In-process front ends receive [`SessionEvent`] values on a channel. Over
//! RPC each one is the `params` of a `session/event` notification. The two
//! carry the same thing.

use serde::{Deserialize, Serialize};

use crate::error::ErrorData;
use crate::ids::{CallId, EntryId, ItemId, SessionId, Timestamp, TurnId};
use crate::item::{Item, Plan, StopReason};
use crate::jsonrpc::RequestId;
use crate::open::{open_enum, open_union};
use crate::project::ProjectEntry;
use crate::session::{SandboxStatus, SessionState};
use crate::speed::{SpeedCooldown, SpeedFallback};
use crate::usage::{SessionUsage, UsageTotals};

/// A turn began.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnStarted {}

open_enum! {
    /// The kind of an item, where only the kind is needed.
    pub enum ItemKind {
        /// A message from the user.
        UserMessage => "user_message",
        /// A response from the model.
        AssistantMessage => "assistant_message",
        /// The result of a tool call.
        ToolResult => "tool_result",
        /// A message the harness added to the conversation.
        Notice => "notice",
        /// A compaction summary.
        Summary => "summary",
    }
}

/// An item began. Its content follows as deltas, and then in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ItemStarted {
    /// The item.
    pub item_id: ItemId,
    /// Its kind.
    pub kind: ItemKind,
    /// For a tool result: the tool that is running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// For a tool result: the call it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<CallId>,
}

/// More text of the item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextDelta {
    /// The text to append.
    pub text: String,
}

/// The model began a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolCallDelta {
    /// The call.
    pub call_id: CallId,
    /// The tool's name.
    pub name: String,
}

/// More of a tool call's arguments: a fragment of JSON text that is not
/// valid JSON until the call is complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolArgsDelta {
    /// The call.
    pub call_id: CallId,
    /// The fragment to append.
    pub json: String,
}

open_union! {
    /// Streaming content for an item. A display convenience: deltas may be
    /// merged or dropped for a slow client, and `item.completed` is what counts.
    pub enum Delta [kind] {
        /// More of the assistant's text.
        Text(TextDelta) => "text",
        /// More of the assistant's visible reasoning.
        Reasoning(TextDelta) => "reasoning",
        /// The model began a tool call.
        ToolCall(ToolCallDelta) => "tool_call",
        /// More of a tool call's arguments.
        ToolArgs(ToolArgsDelta) => "tool_args",
        /// More output from a running tool.
        ToolOutput(TextDelta) => "tool_output",
    }
}

/// Streaming content for an item. Sent only to clients that declared `deltas`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ItemDelta {
    /// The item.
    pub item_id: ItemId,
    /// The content.
    pub delta: Delta,
}

/// Names an item: the payload of `item.reset` and `item.resync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ItemRef {
    /// The item.
    pub item_id: ItemId,
}

/// An item is complete. This is the authoritative content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ItemCompleted {
    /// The item, in full.
    pub item: Item,
}

/// The session's usage changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageUpdated {
    /// The session's usage now.
    pub usage: SessionUsage,
}

open_enum! {
    /// Who made a change.
    pub enum Actor {
        /// The agent.
        Agent => "agent",
        /// A user.
        User => "user",
        /// The harness itself.
        Harness => "harness",
    }
}

/// The plan changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanUpdated {
    /// The plan now.
    pub plan: Plan,
    /// Who changed it.
    pub by: Actor,
}

/// Project profile or memory entries were proposed or changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProjectUpdated {
    /// The entries that are new or changed, as they are now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ProjectEntry>,
    /// The entries that were deleted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<EntryId>,
}

/// Notice: a request failed and will be tried again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RetryNotice {
    /// The notice in words.
    pub message: String,
    /// Which attempt comes next, counting from 1.
    pub attempt: u32,
    /// The most attempts a request gets.
    pub max_attempts: u32,
    /// Why the last attempt failed.
    pub error: ErrorData,
    /// When the next attempt starts.
    pub next_attempt_at: Timestamp,
}

open_enum! {
    /// Where a compaction stands.
    pub enum CompactionState {
        /// It began.
        Started => "started",
        /// It finished, and a new context epoch began.
        Finished => "finished",
        /// It failed. The session carries on with what it had.
        Failed => "failed",
    }
}

/// Notice: a compaction started, finished or failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CompactionNotice {
    /// The notice in words.
    pub message: String,
    /// Where it stands.
    pub state: CompactionState,
    /// Whether it runs in the background while the turn carries on.
    #[serde(default)]
    pub background: bool,
}

open_enum! {
    /// Which limit was reached.
    pub enum LimitKind {
        /// Requests in one turn.
        Requests => "requests",
        /// Money.
        Cost => "cost",
        /// Tokens.
        Tokens => "tokens",
        /// Wall-clock time.
        Time => "time",
    }
}

/// Notice: a configured limit was reached, or is near.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LimitNotice {
    /// The notice in words.
    pub message: String,
    /// Which limit.
    pub limit: LimitKind,
    /// Whether it was reached. `false` is a warning that it is near.
    pub reached: bool,
}

/// Notice: something about the sandbox the user should know: it is not
/// enforcing, a command ran outside it, a protected path changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SandboxNotice {
    /// The notice in words.
    pub message: String,
    /// Whether the sandbox is enforcing.
    pub status: SandboxStatus,
}

/// Notice: instruction files changed and will be re-read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InstructionsNotice {
    /// The notice in words.
    pub message: String,
    /// The files, relative to the workspace root where they are inside it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// Notice: anything else worth telling the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InfoNotice {
    /// The notice in words.
    pub message: String,
}

open_union! {
    /// Something transient that front ends should show. It is never sent to
    /// the model and is not an item.
    ///
    /// Every notice has a `message` in words, so a client can show a kind of
    /// notice it does not know.
    pub enum Notice [kind] {
        /// A request failed and will be tried again.
        Retry(RetryNotice) => "retry",
        /// A compaction started, finished or failed.
        Compaction(CompactionNotice) => "compaction",
        /// A configured limit was reached or is near.
        Limit(LimitNotice) => "limit",
        /// Something about the sandbox.
        Sandbox(SandboxNotice) => "sandbox",
        /// Instruction files changed.
        Instructions(InstructionsNotice) => "instructions",
        /// A request ran at a different speed from the one asked for.
        SpeedFallback(SpeedFallback) => "speed_fallback",
        /// A speed tier's cooldown began or ended.
        SpeedCooldown(SpeedCooldown) => "speed_cooldown",
        /// Anything else.
        Info(InfoNotice) => "info",
    }
}

impl Notice {
    /// The notice in words. `None` only for a kind this version does not
    /// know that broke the rule and has no `message`.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Retry(notice) => Some(&notice.message),
            Self::Compaction(notice) => Some(&notice.message),
            Self::Limit(notice) => Some(&notice.message),
            Self::Sandbox(notice) => Some(&notice.message),
            Self::Instructions(notice) => Some(&notice.message),
            Self::SpeedFallback(notice) => Some(&notice.message),
            Self::SpeedCooldown(notice) => Some(&notice.message),
            Self::Info(notice) => Some(&notice.message),
            Self::Unknown(unknown) => unknown.message(),
        }
    }
}

open_enum! {
    /// How a request from the server to its clients was settled.
    pub enum Resolution {
        /// Another client answered it.
        Answered => "answered",
        /// Nobody answered in time, and the default was applied.
        TimedOut => "timed_out",
        /// The turn was interrupted before anybody answered.
        Cancelled => "cancelled",
    }
}

/// A request from the server (an approval, a question, a trust prompt) was
/// settled without this client. The client withdraws its prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RequestResolved {
    /// The JSON-RPC id of the request, as this client received it.
    pub request_id: RequestId,
    /// How it was settled.
    pub resolution: Resolution,
}

open_enum! {
    /// How a turn ended. It and a headless run's exit code always agree.
    pub enum TurnOutcome {
        /// The model ended normally.
        Completed => "completed",
        /// It ended with an error.
        Failed => "failed",
        /// The user or a signal interrupted it.
        Interrupted => "interrupted",
        /// A configured limit stopped it.
        Limit => "limit",
        /// A denied permission stopped it.
        Denied => "denied",
    }
}

open_enum! {
    /// What the project's checks said at the end of a turn.
    pub enum Verification {
        /// They passed.
        Passed => "passed",
        /// They still failed when the round limit was reached.
        Failed => "failed",
        /// They did not run: no files changed, no check is known, or the
        /// mode is off.
        Skipped => "skipped",
    }
}

/// The turn is over and nothing automatic remains: no retry pending, no
/// compaction running, no tool running. Clients wait on this one event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnCompleted {
    /// How it ended.
    pub outcome: TurnOutcome,
    /// Why the last response ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// The failure, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorData>,
    /// What the turn used and cost.
    #[serde(default)]
    pub usage: UsageTotals,
    /// What the project's checks said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Milliseconds the turn took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// The session has no running turn and no queued prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionIdle {}

/// Session metadata changed: title, models, speed, level, queue length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionUpdated {
    /// The session's state now.
    pub session: SessionState,
}

open_union! {
    /// One event of a session.
    pub enum Event [type] {
        /// A turn began.
        TurnStarted(TurnStarted) => "turn.started",
        /// An item began.
        ItemStarted(ItemStarted) => "item.started",
        /// Streaming content for an item.
        ItemDelta(ItemDelta) => "item.delta",
        /// The partial content of an item was discarded, because its request
        /// is being retried. The client clears what it has shown.
        ItemReset(ItemRef) => "item.reset",
        /// The final item, in full. Authoritative.
        ItemCompleted(ItemCompleted) => "item.completed",
        /// Deltas were dropped for a slow client. It fetches the item with
        /// `session/read`.
        ItemResync(ItemRef) => "item.resync",
        /// Token and cost totals, cache hit rate and rate-limit state.
        UsageUpdated(UsageUpdated) => "usage.updated",
        /// The plan changed, by the agent or by a user.
        PlanUpdated(PlanUpdated) => "plan.updated",
        /// Project profile or memory entries were proposed or changed.
        ProjectUpdated(ProjectUpdated) => "project.updated",
        /// Something transient to show: a retry, a compaction, a limit, a
        /// sandbox report, changed instructions, a change of speed.
        Notice(Notice) => "notice",
        /// A server request was answered by another client or timed out.
        RequestResolved(RequestResolved) => "request.resolved",
        /// The turn is over and nothing automatic remains.
        TurnCompleted(TurnCompleted) => "turn.completed",
        /// The session has no running turn and no queued prompt.
        SessionIdle(SessionIdle) => "session.idle",
        /// Session metadata changed.
        SessionUpdated(SessionUpdated) => "session.updated",
    }
}

/// An event with its envelope: which session, where in its sequence, and
/// which turn. This is the `params` of a `session/event` notification.
///
/// On the wire the event's own fields sit beside the envelope's:
/// `{"session_id": …, "seq": 412, "turn_id": …, "type": "item.delta", …}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// The session.
    pub session_id: SessionId,
    /// Goes up by one per event per session. A gap means events were missed,
    /// and the client reads the session to catch up.
    pub seq: u64,
    /// The turn the event belongs to. Absent for events outside a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    /// The event.
    #[serde(flatten)]
    pub event: Event,
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for SessionEvent {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SessionEvent".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let session_id = generator.subschema_for::<SessionId>();
        let turn_id = generator.subschema_for::<TurnId>();
        let event = generator.subschema_for::<Event>();
        schemars::json_schema!({
            "description": "An event with its envelope: which session, where in its sequence, and which turn. The event's own fields sit beside the envelope's.",
            "allOf": [
                {
                    "type": "object",
                    "properties": {
                        "session_id": session_id,
                        "seq": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Goes up by one per event per session. A gap means events were missed.",
                        },
                        "turn_id": turn_id,
                    },
                    "required": ["session_id", "seq"],
                },
                event,
            ],
        })
    }
}
