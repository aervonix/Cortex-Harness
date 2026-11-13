//! The statistics events the loop emits
//! (`docs/architecture/03-agent-loop.md`, "What is recorded for
//! adaptation").
//!
//! At the end of each request, each tool call and each turn the loop hands
//! one small event to a [`Stats`]. `cortex-stats` records them; the core
//! connects the two. An event holds no prompt and no file content.

use std::time::Duration;

use cortex_base::UtcTime;
use cortex_llm::Variants;
use cortex_protocol::event::{TurnOutcome, Verification};
use cortex_protocol::usage::RequestUsage;
use cortex_protocol::{ErrorData, ModelId, ProviderId, SessionId, StopReason, TurnId};

/// One request to a model ended.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestStat {
    /// The session.
    pub session: SessionId,
    /// The turn.
    pub turn: TurnId,
    /// When the request ended.
    pub at: UtcTime,
    /// The provider.
    pub provider: ProviderId,
    /// The model.
    pub model: ModelId,
    /// The per-model variants in effect.
    pub variants: Variants,
    /// What the request used, at which speed, and what it cost. `None` when
    /// it failed before any usage was reported.
    pub usage: Option<RequestUsage>,
    /// Why the response ended.
    pub stop: StopReason,
    /// The failure, when the request failed.
    pub error: Option<ErrorData>,
    /// How many attempts were given up before this one.
    pub retries: u32,
}

/// How a tool call ended, for the success rates of tools and schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolStatOutcome {
    /// The tool ran and succeeded.
    Ok,
    /// The arguments did not fit the tool, or were not JSON.
    InvalidArguments,
    /// The model named a tool that does not exist.
    UnknownTool,
    /// The tool ran and failed.
    Failed,
    /// The call was not allowed to run.
    Denied,
    /// The call was cancelled.
    Interrupted,
}

/// One tool call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallStat {
    /// The session.
    pub session: SessionId,
    /// The turn.
    pub turn: TurnId,
    /// The tool's name.
    pub tool: String,
    /// The model that made the call.
    pub model: ModelId,
    /// The per-model variants in effect.
    pub variants: Variants,
    /// How the call ended.
    pub outcome: ToolStatOutcome,
    /// How long it ran. Zero for a call that never ran.
    pub duration: Duration,
}

/// One turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStat {
    /// The session.
    pub session: SessionId,
    /// The turn.
    pub turn: TurnId,
    /// The main model.
    pub model: ModelId,
    /// How many requests the turn made.
    pub requests: u64,
    /// How many tool calls it recorded a result for.
    pub tool_calls: u64,
    /// How it ended.
    pub outcome: TurnOutcome,
    /// What the project's checks said.
    pub verification: Option<Verification>,
    /// How long it took.
    pub duration: Duration,
}

/// One statistics event.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
#[expect(
    clippy::large_enum_variant,
    reason = "an event is made once and handed over; a box per request would buy nothing"
)]
pub enum StatEvent {
    /// A request ended.
    Request(RequestStat),
    /// A tool call ended.
    ToolCall(ToolCallStat),
    /// A turn ended.
    Turn(TurnStat),
}

/// Takes the loop's statistics events.
///
/// `record` is a queue push: it never blocks and never fails the loop.
pub trait Stats: Send + Sync {
    /// One event.
    fn record(&self, event: StatEvent);
}

/// A [`Stats`] that keeps nothing: statistics turned off.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStats;

impl Stats for NoStats {
    fn record(&self, _event: StatEvent) {}
}
