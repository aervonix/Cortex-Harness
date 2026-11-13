//! What headless mode writes: the first and last lines of `--output jsonl`,
//! and the whole of `--output json` (`docs/architecture/10-headless.md`,
//! "Output formats").
//!
//! Between its first and last lines, `jsonl` carries the events of the
//! session in the shape of a `session/event` notification's parameters
//! ([`SessionEvent`](crate::SessionEvent)). The two records here carry a
//! `type` like every event, so a reader dispatches each line on it. An
//! agreed addition of step 13b: optional for every other client, and
//! described by the generated schema like the rest of the protocol.

use serde::{Deserialize, Serialize};

use crate::error::ErrorData;
use crate::event::TurnOutcome;
use crate::ids::{CallId, ModelId, SessionId, TurnId};
use crate::item::StopReason;
use crate::open::open_union;
use crate::usage::{Cost, TokenUsage};

/// The version of the two records' shape. It changes only when a field is
/// removed or changes its meaning; fields are added without a change.
pub const HEADLESS_SCHEMA: u32 = 1;

/// The first line of `jsonl`: which session the run is, on which model, in
/// which workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HeadlessStart {
    /// The version of the records' shape, 1 for this one.
    pub schema: u32,
    /// The session the run is in.
    pub session_id: SessionId,
    /// The main role's model.
    pub model: ModelId,
    /// The workspace root: an absolute path.
    pub workspace: String,
}

/// A tool call that was refused because nothing could approve it: the
/// usual reason a headless run does less than was expected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PermissionDenial {
    /// The turn the call belonged to.
    pub turn_id: TurnId,
    /// The call.
    pub call_id: CallId,
    /// The tool.
    pub tool: String,
    /// The call in one line.
    pub summary: String,
    /// Why it needed an answer.
    pub reason: String,
}

/// How a headless run ended: the whole of `--output json`, and the last
/// line of `jsonl`. It is written on success and on failure alike, and its
/// `outcome` and the process's exit code always agree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HeadlessResult {
    /// The version of the records' shape, 1 for this one.
    pub schema: u32,
    /// The session. Absent when the run failed before one was started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// How the run ended.
    pub outcome: TurnOutcome,
    /// The text of the assistant's last message. Empty when it wrote none.
    pub result: String,
    /// Why the last response ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// The failure, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorData>,
    /// Tokens, by class.
    pub usage: TokenUsage,
    /// What the run cost, one amount per basis, in whole nano-dollars:
    /// `usd_nanos / 1e9` is US dollars. Empty when no price is known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub costs: Vec<Cost>,
    /// How many requests were made to a model.
    pub requests: u64,
    /// How many tool calls were made.
    pub tool_calls: u64,
    /// Milliseconds from the start of the process to this record.
    pub duration_ms: u64,
    /// The files the run created, changed or deleted, relative to the
    /// workspace, sorted.
    pub files_changed: Vec<String>,
    /// The calls that were refused because nothing could approve them.
    pub permission_denials: Vec<PermissionDenial>,
}

open_union! {
    /// A record of headless mode's own, told apart by `type`.
    pub enum HeadlessRecord [type] {
        /// The first line of `jsonl`.
        Session(HeadlessStart) => "session",
        /// The last line of `jsonl`, and the whole of `json`.
        Result(HeadlessResult) => "result",
    }
}
