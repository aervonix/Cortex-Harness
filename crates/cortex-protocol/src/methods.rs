//! Methods: every request one side can make of the other, with its
//! parameters and its result (`docs/architecture/11-rpc-protocol.md`).
//!
//! [`Op`] is what a front end asks of the core, and [`OpResult`] the answer.
//! [`ServerRequest`] is what the core asks of a front end, and
//! [`ServerResponse`] the answer. In-process front ends pass these values
//! over channels. `cortex-rpc` carries the same values as JSON-RPC requests
//! and responses, using [`Op::from_wire`] and its companions, and adds no
//! behaviour of its own.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::event::SessionEvent;
use crate::ids::{ApprovalId, CallId, ItemId, ProjectKey, SessionId, Timestamp, TurnId};
use crate::item::{Delivery, Item, Plan};
use crate::jsonrpc::{self, ErrorObject, Notification, Request, RequestId};
use crate::lease::{Effect, Rule};
use crate::message::Part;
use crate::model::ModelInfo;
use crate::open::{open_enum, open_union};
use crate::project::{EntryChange, PlanEdit, ProjectEntry};
use crate::session::{SandboxStatus, SessionSettings, SessionState, SessionSummary};
use crate::usage::{SessionUsage, UsageGroup, UsageRange, UsageReport};

/// The protocol version this build speaks. It changes only for a breaking
/// change. Everything else is added under a capability.
pub const PROTOCOL_VERSION: u32 = 1;

/// Every protocol version this build can speak, oldest first.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[1];

/// The method of the one notification the server sends: a [`SessionEvent`].
pub const SESSION_EVENT: &str = "session/event";

/// The prefix of experimental methods, which may change without a version
/// bump. A client opts in to each by name in `initialize`.
pub const EXPERIMENTAL_PREFIX: &str = "x/";

/// A result with nothing in it. Written as `{}`, so fields can be added later.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Empty {}

// ---------------------------------------------------------------- handshake

open_enum! {
    /// What a client is to the sessions it opens.
    pub enum ClientRole {
        /// A person's front end. It receives approval and question requests,
        /// and its answer is final.
        User => "user",
        /// Another session's orchestration layer, driving this session. It
        /// receives requests first when no user has answered, and may answer
        /// only within its own grant. A session has at most one.
        Supervisor => "supervisor",
        /// Something that only watches. It is never asked anything.
        Observer => "observer",
    }
}

/// The name and version of one side of a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PeerInfo {
    /// The program's name.
    pub name: String,
    /// Its version.
    pub version: String,
}

/// What a client can handle. A capability that is absent is `false`: the
/// server never sends what the client did not ask for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ClientCapabilities {
    /// It answers `approval/request`.
    #[serde(default)]
    pub approvals: bool,
    /// It answers `question/ask`.
    #[serde(default)]
    pub questions: bool,
    /// It answers `trust/request`.
    #[serde(default)]
    pub trust: bool,
    /// It wants `item.delta` events.
    #[serde(default)]
    pub deltas: bool,
    /// The experimental features it opts in to, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub experimental: Vec<String>,
}

/// What a server offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ServerCapabilities {
    /// It stores sessions.
    #[serde(default)]
    pub sessions: bool,
    /// It was built with the MCP client.
    #[serde(default)]
    pub mcp: bool,
    /// It was built with extensions.
    #[serde(default)]
    pub extensions: bool,
    /// Whether its sandbox confines commands.
    pub sandbox: SandboxStatus,
    /// The experimental features it turned on for this connection: the ones
    /// the client asked for that it has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub experimental: Vec<String>,
}

/// The first request on a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InitializeParams {
    /// The newest protocol version the client speaks.
    pub protocol_version: u32,
    /// The oldest protocol version the client speaks. Absent: only
    /// `protocol_version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_protocol_version: Option<u32>,
    /// The client.
    pub client: PeerInfo,
    /// The client's role.
    pub role: ClientRole,
    /// What the client can handle.
    #[serde(default)]
    pub capabilities: ClientCapabilities,
}

/// The server's answer to `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InitializeResult {
    /// The version the connection uses: the newest one both sides speak.
    pub protocol_version: u32,
    /// The server.
    pub server: PeerInfo,
    /// What the server offers.
    pub capabilities: ServerCapabilities,
}

/// The newest version in `supported` that lies within the client's range.
/// `None` means the two sides have no version in common, and `initialize`
/// fails with the code `unsupported_protocol_version`.
#[must_use]
pub fn negotiate_version(params: &InitializeParams, supported: &[u32]) -> Option<u32> {
    let newest = params.protocol_version;
    let oldest = params.min_protocol_version.unwrap_or(newest).min(newest);
    supported
        .iter()
        .copied()
        .filter(|version| (oldest..=newest).contains(version))
        .max()
}

// ----------------------------------------------------------------- sessions

/// Names a session: the parameters of every method that takes nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionRef {
    /// The session.
    pub session_id: SessionId,
}

/// A result that is the session's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionResult {
    /// The session's state.
    pub session: SessionState,
}

/// `session/new`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionNewParams {
    /// The workspace root: an absolute path on the server's machine.
    pub workspace: String,
    /// The settings to start with. Anything absent comes from configuration.
    #[serde(default)]
    pub settings: SessionSettings,
    /// A title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Configuration overrides for this session, as the configuration file
    /// would have them. They rank as command-line flags do.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub config: Map<String, Value>,
    /// Whether to keep the session in memory only, with no log.
    #[serde(default)]
    pub ephemeral: bool,
}

/// `session/list`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionListParams {
    /// Only sessions of this workspace. Absent: every workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The most sessions to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Where to continue: the `next_cursor` of the page before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// The result of `session/list`: newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionListResult {
    /// The sessions.
    pub sessions: Vec<SessionSummary>,
    /// Present when there are more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `session/read`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionReadParams {
    /// The session.
    pub session_id: SessionId,
    /// Read only this item. An item in progress comes back with the content
    /// it has so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
    /// The most items to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Where to continue: the `next_cursor` of the page before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// The result of `session/read`: items of the active branch, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionReadResult {
    /// The items.
    pub items: Vec<Item>,
    /// The ones among them that are still in progress.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<ItemId>,
    /// Present when there are more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The `seq` of the last event sent for the session when the items were
    /// read. Events after it are not in this result.
    pub seq: u64,
}

/// `session/fork`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionForkParams {
    /// The session to fork.
    pub session_id: SessionId,
    /// The last item the new session keeps. Absent: all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<ItemId>,
}

/// `session/rewind`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionRewindParams {
    /// The session.
    pub session_id: SessionId,
    /// The last item to keep on the active branch.
    pub to: ItemId,
    /// Whether to put files back as they were at that point, from the
    /// snapshots of the turns that are undone.
    #[serde(default)]
    pub restore_files: bool,
}

/// `session/compact`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionCompactParams {
    /// The session.
    pub session_id: SessionId,
    /// What the summary should keep in particular.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
}

/// `session/set`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionSetParams {
    /// The session.
    pub session_id: SessionId,
    /// What to change. Anything absent stays as it is.
    pub settings: SessionSettings,
}

// -------------------------------------------------------------------- turns

/// `turn/start`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnStartParams {
    /// The session.
    pub session_id: SessionId,
    /// The prompt: text and images.
    pub input: Vec<Part>,
    /// How to deliver it if a turn is running. Required then, and ignored
    /// when the session is idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
}

open_enum! {
    /// What became of a prompt.
    pub enum Disposition {
        /// It started a turn.
        Started => "started",
        /// It will be delivered at the next safe boundary of the running turn.
        Steering => "steering",
        /// It will become a turn after the running one.
        Queued => "queued",
    }
}

/// The result of `turn/start`. It means "accepted": the turn ends with the
/// `turn.completed` event, and the result never carries the model's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnStartResult {
    /// The turn the prompt belongs to: the one it started, the running one
    /// it steers, or the one it will start.
    pub turn_id: TurnId,
    /// What became of the prompt.
    pub disposition: Disposition,
}

/// `turn/interrupt`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnInterruptParams {
    /// The session.
    pub session_id: SessionId,
    /// Whether to stop running commands at once, without the polite
    /// interrupt and the grace that normally come first: what a second
    /// interrupt means. Absent or `false` is the ordinary interrupt, which
    /// stays safe to send twice.
    #[serde(default, skip_serializing_if = "is_false")]
    pub now: bool,
}

impl TurnInterruptParams {
    /// The ordinary interrupt of `session_id`'s running turn.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            now: false,
        }
    }

    /// The interrupt that also kills running commands at once.
    #[must_use]
    pub fn now(session_id: SessionId) -> Self {
        Self {
            session_id,
            now: true,
        }
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "the signature serde's `skip_serializing_if` calls"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// The result of `turn/interrupt`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnInterruptResult {
    /// Whether a turn was running.
    #[serde(default)]
    pub interrupted: bool,
    /// Steering prompts that had not been delivered, handed back so the
    /// front end can put them in its editor. Queued prompts stay queued.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undelivered: Vec<Vec<Part>>,
}

// ---------------------------------------------------- plan, usage, project

/// A result that is the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanResult {
    /// The plan now.
    pub plan: Plan,
}

/// `plan/update`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanUpdateParams {
    /// The session.
    pub session_id: SessionId,
    /// The edits, applied in order.
    pub edits: Vec<PlanEdit>,
}

/// The result of `usage/get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageGetResult {
    /// The session's usage now.
    pub usage: SessionUsage,
}

/// `usage/report`. It reads local data only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageReportParams {
    /// The period.
    pub range: UsageRange,
    /// How to group the rows, outermost first. Empty: by provider, then model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<UsageGroup>,
    /// For the `session` range: the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Only this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectKey>,
}

/// Names a project by a session of it, or by its workspace root. Exactly one
/// is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProjectListParams {
    /// A session of the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The project's workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// A result that is project entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProjectEntriesResult {
    /// The entries: all of them for `project/list`, the changed ones for
    /// `project/update`.
    pub entries: Vec<ProjectEntry>,
}

/// `project/update`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProjectUpdateParams {
    /// A session of the project. Exactly one of this and `workspace` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The project's workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The changes, applied in order.
    pub changes: Vec<EntryChange>,
}

// ------------------------------------------- catalog, configuration, status

/// The result of `models/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelsListResult {
    /// Every model of every configured provider.
    pub models: Vec<ModelInfo>,
}

/// `config/get`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigGetParams {
    /// The session whose effective configuration is wanted. Absent: the
    /// configuration without any project's layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Only these keys, in dotted form. Empty: everything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

/// The result of `config/get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigGetResult {
    /// The effective configuration, as the configuration file would have it.
    /// Credentials are never in it.
    pub config: Map<String, Value>,
}

/// The result of `config/reload`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigReloadResult {
    /// Keys whose new values are in force.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<String>,
    /// Keys whose new values wait for the next context epoch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<String>,
    /// What was wrong with a file. When this is not empty the old
    /// configuration stays in force.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

open_enum! {
    /// How long an approval lasts.
    pub enum ApprovalScope {
        /// Until the session ends. Held in memory.
        Session => "session",
        /// For the project, in the user's per-project state.
        Project => "project",
    }
}

/// An approval the user gave earlier that still applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StoredApproval {
    /// The approval's id.
    pub id: ApprovalId,
    /// The rule it added.
    pub rule: Rule,
    /// How long it lasts.
    pub scope: ApprovalScope,
    /// When it was given.
    pub created_at: Timestamp,
}

/// The result of `permissions/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PermissionsListResult {
    /// The approvals in force for the session.
    pub approvals: Vec<StoredApproval>,
}

/// `permissions/revoke`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PermissionsRevokeParams {
    /// The session.
    pub session_id: SessionId,
    /// The approvals to remove.
    pub ids: Vec<ApprovalId>,
}

open_enum! {
    /// The state of an MCP server or an extension.
    pub enum ComponentState {
        /// Starting.
        Starting => "starting",
        /// Working.
        Ready => "ready",
        /// It failed. `error` says how.
        Failed => "failed",
        /// Turned off in configuration.
        Disabled => "disabled",
        /// Its definition is new or changed, and waits for the user's approval.
        NeedsApproval => "needs_approval",
    }
}

/// The state of one MCP server or one extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ComponentStatus {
    /// Its configured name.
    pub name: String,
    /// Its state.
    pub state: ComponentState,
    /// How many tools it provides.
    #[serde(default)]
    pub tools: u32,
    /// What went wrong, for `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The result of `mcp/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct McpStatusResult {
    /// The servers.
    pub servers: Vec<ComponentStatus>,
}

/// The result of `extensions/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExtensionsStatusResult {
    /// The extensions.
    pub extensions: Vec<ComponentStatus>,
}

// ---------------------------------------------- server → client requests

/// The detail of an approval for a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandDetail {
    /// The full command line. Nothing is shortened.
    pub command: String,
    /// The directory it would run in.
    pub cwd: String,
    /// Whether it would run outside the sandbox.
    #[serde(default)]
    pub outside_sandbox: bool,
}

/// The detail of an approval for a change to a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DiffDetail {
    /// The file.
    pub path: String,
    /// The change, as a unified diff.
    pub diff: String,
}

/// The detail of an approval for access to a path or a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TargetDetail {
    /// The effect.
    pub effect: Effect,
    /// The path or the host.
    pub target: String,
}

/// The detail of an approval for an extension or MCP tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolDetail {
    /// The arguments the tool would be called with.
    pub arguments: Value,
}

open_union! {
    /// Exactly what an approval would let happen.
    pub enum ApprovalDetail [kind] {
        /// A command.
        Command(CommandDetail) => "command",
        /// A change to a file.
        Diff(DiffDetail) => "diff",
        /// Access to a path or a host.
        Target(TargetDetail) => "target",
        /// A call to an extension or MCP tool.
        Tool(ToolDetail) => "tool",
    }
}

open_enum! {
    /// An answer to an approval request. A note may go with any of them, and
    /// with `deny` it reaches the model as the reason.
    pub enum ApprovalChoice {
        /// This call only.
        AllowOnce => "allow_once",
        /// This call, and calls like it until the session ends.
        AllowSession => "allow_session",
        /// This call, and calls like it in this project from now on.
        AllowProject => "allow_project",
        /// Refuse. The call gets an error result.
        Deny => "deny",
    }
}

/// `approval/request`: may this tool call run?
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApprovalRequest {
    /// The session.
    pub session_id: SessionId,
    /// The turn.
    pub turn_id: TurnId,
    /// The tool call.
    pub call_id: CallId,
    /// The tool's name.
    pub tool: String,
    /// The call in one line.
    pub summary: String,
    /// Exactly what would happen.
    pub detail: ApprovalDetail,
    /// The answers on offer.
    pub choices: Vec<ApprovalChoice>,
}

/// The answer to `approval/request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApprovalResponse {
    /// The choice: one of those on offer.
    pub choice: ApprovalChoice,
    /// A note for the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `question/ask`: a question for the user, from the agent or an extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QuestionRequest {
    /// The session.
    pub session_id: SessionId,
    /// The turn, when the question belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    /// The question.
    pub question: String,
    /// Answers to pick from. Empty: free text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// The extension that asks. Absent: the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

/// The answer to `question/ask`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QuestionResponse {
    /// The answer. Absent: the user declined to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

open_enum! {
    /// A kind of project resource that can run code or grant permissions.
    pub enum TrustResourceKind {
        /// Permission rules that allow.
        Rules => "rules",
        /// A hook.
        Hook => "hook",
        /// An extension.
        Extension => "extension",
        /// An MCP server.
        McpServer => "mcp_server",
        /// Learned or shared commands used for the safe list.
        Commands => "commands",
    }
}

/// One thing a project asks to be trusted with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TrustResource {
    /// What kind of thing.
    pub kind: TrustResourceKind,
    /// Its name.
    pub name: String,
    /// What it would do, in full: the command, the rules.
    pub detail: String,
    /// Whether it was trusted before and has changed since.
    #[serde(default)]
    pub changed: bool,
}

/// `trust/request`: may this project's executable resources be used?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TrustRequest {
    /// The session.
    pub session_id: SessionId,
    /// The project's workspace root.
    pub workspace: String,
    /// What it asks to be trusted with.
    pub resources: Vec<TrustResource>,
}

/// The answer to `trust/request`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TrustResponse {
    /// Whether the resources listed may be used.
    #[serde(default)]
    pub trusted: bool,
}

// ------------------------------------------------------- the method tables

/// Why a method call could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodError {
    /// No method has this name.
    UnknownMethod(String),
    /// The parameters or the result do not fit the method.
    Invalid {
        /// The method.
        method: &'static str,
        /// What was wrong, naming the field.
        message: String,
    },
}

impl std::fmt::Display for MethodError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownMethod(method) => write!(f, "unknown method `{method}`"),
            Self::Invalid { method, message } => write!(f, "`{method}`: {message}"),
        }
    }
}

impl std::error::Error for MethodError {}

impl MethodError {
    /// The JSON-RPC error that reports this to the sender of the request.
    #[must_use]
    pub fn to_error_object(&self) -> ErrorObject {
        ErrorObject {
            code: match self {
                Self::UnknownMethod(_) => jsonrpc::code::METHOD_NOT_FOUND,
                Self::Invalid { .. } => jsonrpc::code::INVALID_PARAMS,
            },
            message: self.to_string(),
            data: None,
        }
    }
}

/// The schema of one method, for the generated files.
#[cfg(feature = "schema")]
#[derive(Debug, Clone)]
pub struct MethodSchema {
    /// The method name.
    pub name: &'static str,
    /// What the method is for.
    pub doc: String,
    /// The schema of its parameters.
    pub params: schemars::Schema,
    /// The schema of its result.
    pub result: schemars::Schema,
}

/// Reads parameters or a result. Absent and `null` both mean "an empty
/// object", so a method whose parameters are all optional may be called
/// with none.
fn read<T: serde::de::DeserializeOwned>(
    method: &'static str,
    value: Option<Value>,
) -> Result<T, MethodError> {
    let value = match value {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(value) => value,
    };
    T::deserialize(value).map_err(|error| MethodError::Invalid {
        method,
        message: error.to_string(),
    })
}

/// Defines the request enum and the result enum of one direction, with the
/// mapping between method names and types.
macro_rules! methods {
    (
        $(#[doc = $doc:literal])*
        $request:ident;
        $(#[doc = $rdoc:literal])*
        $response:ident;
        $(
            $(#[doc = $vdoc:literal])*
            $variant:ident($params:ty) -> $result:ty => $name:literal
        ),+ $(,)?
    ) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone, PartialEq)]
        #[allow(
            clippy::large_enum_variant,
            reason = "a value is built once and matched once; boxing each payload would cost an allocation per call"
        )]
        pub enum $request {
            $(
                $(#[doc = $vdoc])*
                #[doc = ""]
                #[doc = concat!("Method `", $name, "`.")]
                $variant($params),
            )+
        }

        $(#[doc = $rdoc])*
        #[derive(Debug, Clone, PartialEq)]
        #[allow(
            clippy::large_enum_variant,
            reason = "a value is built once and matched once; boxing each payload would cost an allocation per call"
        )]
        pub enum $response {
            $(
                #[doc = concat!("The result of `", $name, "`.")]
                $variant($result),
            )+
        }

        impl $request {
            /// Every method name, in the order of the variants.
            pub const METHODS: &'static [&'static str] = &[$($name),+];

            /// The method name.
            #[must_use]
            pub fn method(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $name,)+
                }
            }

            /// Reads a request from its method name and parameters.
            ///
            /// # Errors
            ///
            /// When the method is not known, or the parameters do not fit it.
            pub fn from_wire(method: &str, params: Option<Value>) -> Result<Self, MethodError> {
                match method {
                    $($name => read::<$params>($name, params).map(Self::$variant),)+
                    other => Err(MethodError::UnknownMethod(other.to_owned())),
                }
            }

            /// The parameters as JSON.
            ///
            /// # Errors
            ///
            /// When a value inside cannot be written as JSON, which the
            /// protocol's own types never cause.
            pub fn params(&self) -> Result<Value, serde_json::Error> {
                match self {
                    $(Self::$variant(params) => serde_json::to_value(params),)+
                }
            }

            /// The JSON-RPC request that carries this.
            ///
            /// # Errors
            ///
            /// As [`Self::params`].
            pub fn to_request(&self, id: RequestId) -> Result<Request, serde_json::Error> {
                Ok(Request {
                    id,
                    method: self.method().to_owned(),
                    params: Some(self.params()?),
                })
            }

            /// The schema of every method.
            #[cfg(feature = "schema")]
            #[must_use]
            pub fn schemas(generator: &mut schemars::SchemaGenerator) -> Vec<MethodSchema> {
                vec![
                    $(MethodSchema {
                        name: $name,
                        doc: $crate::open::doc_text(&[$($vdoc),*]),
                        params: generator.subschema_for::<$params>(),
                        result: generator.subschema_for::<$result>(),
                    },)+
                ]
            }
        }

        impl $response {
            /// The method this is the result of.
            #[must_use]
            pub fn method(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $name,)+
                }
            }

            /// Reads a result, given the method of the request it answers.
            ///
            /// # Errors
            ///
            /// When the method is not known, or the result does not fit it.
            pub fn from_wire(method: &str, result: Value) -> Result<Self, MethodError> {
                match method {
                    $($name => read::<$result>($name, Some(result)).map(Self::$variant),)+
                    other => Err(MethodError::UnknownMethod(other.to_owned())),
                }
            }

            /// The result as JSON.
            ///
            /// # Errors
            ///
            /// When a value inside cannot be written as JSON, which the
            /// protocol's own types never cause.
            pub fn result(&self) -> Result<Value, serde_json::Error> {
                match self {
                    $(Self::$variant(result) => serde_json::to_value(result),)+
                }
            }
        }
    };
}

methods! {
    /// An operation: what a front end asks of the core. Each one is a
    /// JSON-RPC method from client to server.
    Op;
    /// The answer to an [`Op`].
    OpResult;

    /// The handshake. It must be the first request on a connection.
    Initialize(InitializeParams) -> InitializeResult => "initialize",
    /// Creates a session: workspace, models, level, overrides.
    SessionNew(SessionNewParams) -> SessionResult => "session/new",
    /// Opens a stored session. Returns its metadata and current state.
    SessionOpen(SessionRef) -> SessionResult => "session/open",
    /// Lists stored sessions.
    SessionList(SessionListParams) -> SessionListResult => "session/list",
    /// Reads items of a session from the log, in pages. Also how a client
    /// catches up after a gap in `seq` or an `item.resync`.
    SessionRead(SessionReadParams) -> SessionReadResult => "session/read",
    /// Makes a new session from a point in this one.
    SessionFork(SessionForkParams) -> SessionResult => "session/fork",
    /// Moves the active branch back to an earlier item.
    SessionRewind(SessionRewindParams) -> SessionResult => "session/rewind",
    /// Stops using a session on this connection. The session is kept.
    SessionClose(SessionRef) -> Empty => "session/close",
    /// Sends a prompt. If a turn is running, `delivery` says whether to
    /// steer it or to queue behind it.
    TurnStart(TurnStartParams) -> TurnStartResult => "turn/start",
    /// Cancels the running turn. It is not an error when none is running.
    /// With `now`, running commands are killed at once.
    TurnInterrupt(TurnInterruptParams) -> TurnInterruptResult => "turn/interrupt",
    /// Compacts now, with optional focus instructions.
    SessionCompact(SessionCompactParams) -> Empty => "session/compact",
    /// Changes a session's settings: models, reasoning level, speed, level.
    SessionSet(SessionSetParams) -> SessionResult => "session/set",
    /// Reads the plan.
    PlanGet(SessionRef) -> PlanResult => "plan/get",
    /// Applies a user's edits to the plan.
    PlanUpdate(PlanUpdateParams) -> PlanResult => "plan/update",
    /// Token, cost and cache figures for the session, live.
    UsageGet(SessionRef) -> UsageGetResult => "usage/get",
    /// Usage and cost for a range, grouped, with limits left.
    UsageReport(UsageReportParams) -> UsageReport => "usage/report",
    /// Reads the project profile and memory.
    ProjectList(ProjectListParams) -> ProjectEntriesResult => "project/list",
    /// Accepts, edits, disables, deletes or promotes entries.
    ProjectUpdate(ProjectUpdateParams) -> ProjectEntriesResult => "project/update",
    /// Runs the project's checks now. The outcome arrives as events.
    VerifyRun(SessionRef) -> Empty => "verify/run",
    /// Reads the model catalog.
    ModelsList(Empty) -> ModelsListResult => "models/list",
    /// Reads the effective configuration.
    ConfigGet(ConfigGetParams) -> ConfigGetResult => "config/get",
    /// Re-reads the configuration files.
    ConfigReload(Empty) -> ConfigReloadResult => "config/reload",
    /// Lists the approvals in force for a session.
    PermissionsList(SessionRef) -> PermissionsListResult => "permissions/list",
    /// Removes stored approvals.
    PermissionsRevoke(PermissionsRevokeParams) -> Empty => "permissions/revoke",
    /// The state of the MCP servers.
    McpStatus(Empty) -> McpStatusResult => "mcp/status",
    /// The state of the extensions.
    ExtensionsStatus(Empty) -> ExtensionsStatusResult => "extensions/status",
    /// Stops the server cleanly.
    Shutdown(Empty) -> Empty => "shutdown",
}

methods! {
    /// A request from the core to a front end. It pauses the turn until it
    /// is answered or times out. A client that did not declare the matching
    /// capability is never sent it.
    ServerRequest;
    /// The answer to a [`ServerRequest`].
    ServerResponse;

    /// Asks whether a tool call may run. Unanswered: deny.
    Approval(ApprovalRequest) -> ApprovalResponse => "approval/request",
    /// Asks the user a question. Unanswered: the asker is told "no answer".
    Question(QuestionRequest) -> QuestionResponse => "question/ask",
    /// Asks whether to trust a project's executable resources. Unanswered:
    /// not trusted.
    Trust(TrustRequest) -> TrustResponse => "trust/request",
}

/// A notification from the server.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerNotification {
    /// `session/event`
    Event(Box<SessionEvent>),
    /// A notification method this version does not know. Receivers ignore it.
    Unknown(Notification),
}

impl ServerNotification {
    /// Reads a notification. An unknown method is not an error.
    ///
    /// # Errors
    ///
    /// When the method is `session/event` and its parameters are not an event.
    pub fn from_wire(notification: Notification) -> Result<Self, MethodError> {
        if notification.method == SESSION_EVENT {
            read::<SessionEvent>(SESSION_EVENT, notification.params)
                .map(|event| Self::Event(Box::new(event)))
        } else {
            Ok(Self::Unknown(notification))
        }
    }
}

impl SessionEvent {
    /// The `session/event` notification that carries this event.
    ///
    /// # Errors
    ///
    /// When a value inside cannot be written as JSON, which the protocol's
    /// own types never cause.
    pub fn to_notification(&self) -> Result<Notification, serde_json::Error> {
        Ok(Notification {
            method: SESSION_EVENT.to_owned(),
            params: Some(serde_json::to_value(self)?),
        })
    }
}
