// The Cortex-Harness protocol, as TypeScript declarations.
//
// Generated from the Rust types in crates/cortex-protocol by `cargo xtask schema`.
// Do not edit: change the types and run the command again.
//
// Every enumeration and every tagged union is open. A string enumeration
// lists the values this version knows and also accepts any other string.
// A tagged union lists the variants this version knows; a receiver must
// also tolerate an object whose `type` or `kind` is not listed, and skip
// it or show it generically. Objects may carry fields that are not listed.

/** The protocol version these declarations describe. */
export type ProtocolVersion = 1;

/** Requests a client sends to the server, by method name. */
export interface ClientMethods {
  /** Reads the effective configuration. */
  "config/get": { params: ConfigGetParams; result: ConfigGetResult };
  /** Re-reads the configuration files. */
  "config/reload": { params: Empty; result: ConfigReloadResult };
  /** The state of the extensions. */
  "extensions/status": { params: Empty; result: ExtensionsStatusResult };
  /** The handshake. It must be the first request on a connection. */
  initialize: { params: InitializeParams; result: InitializeResult };
  /** The state of the MCP servers. */
  "mcp/status": { params: Empty; result: McpStatusResult };
  /** Reads the model catalog. */
  "models/list": { params: Empty; result: ModelsListResult };
  /** Lists the approvals in force for a session. */
  "permissions/list": { params: SessionRef; result: PermissionsListResult };
  /** Removes stored approvals. */
  "permissions/revoke": { params: PermissionsRevokeParams; result: Empty };
  /** Reads the plan. */
  "plan/get": { params: SessionRef; result: PlanResult };
  /** Applies a user's edits to the plan. */
  "plan/update": { params: PlanUpdateParams; result: PlanResult };
  /** Reads the project profile and memory. */
  "project/list": { params: ProjectListParams; result: ProjectEntriesResult };
  /** Accepts, edits, disables, deletes or promotes entries. */
  "project/update": { params: ProjectUpdateParams; result: ProjectEntriesResult };
  /** Stops using a session on this connection. The session is kept. */
  "session/close": { params: SessionRef; result: Empty };
  /** Compacts now, with optional focus instructions. */
  "session/compact": { params: SessionCompactParams; result: Empty };
  /** Makes a new session from a point in this one. */
  "session/fork": { params: SessionForkParams; result: SessionResult };
  /** Lists stored sessions. */
  "session/list": { params: SessionListParams; result: SessionListResult };
  /** Creates a session: workspace, models, level, overrides. */
  "session/new": { params: SessionNewParams; result: SessionResult };
  /** Opens a stored session. Returns its metadata and current state. */
  "session/open": { params: SessionRef; result: SessionResult };
  /** Reads items of a session from the log, in pages. Also how a client catches up after a gap in `seq` or an `item.resync`. */
  "session/read": { params: SessionReadParams; result: SessionReadResult };
  /** Moves the active branch back to an earlier item. */
  "session/rewind": { params: SessionRewindParams; result: SessionResult };
  /** Changes a session's settings: models, reasoning level, speed, level. */
  "session/set": { params: SessionSetParams; result: SessionResult };
  /** Stops the server cleanly. */
  shutdown: { params: Empty; result: Empty };
  /** Cancels the running turn. It is not an error when none is running. */
  "turn/interrupt": { params: SessionRef; result: TurnInterruptResult };
  /** Sends a prompt. If a turn is running, `delivery` says whether to steer it or to queue behind it. */
  "turn/start": { params: TurnStartParams; result: TurnStartResult };
  /** Token, cost and cache figures for the session, live. */
  "usage/get": { params: SessionRef; result: UsageGetResult };
  /** Usage and cost for a range, grouped, with limits left. */
  "usage/report": { params: UsageReportParams; result: UsageReport };
  /** Runs the project's checks now. The outcome arrives as events. */
  "verify/run": { params: SessionRef; result: Empty };
}

/** Requests the server sends to a client, by method name. Each pauses the turn until it is answered or times out. */
export interface ServerMethods {
  /** Asks whether a tool call may run. Unanswered: deny. */
  "approval/request": { params: ApprovalRequest; result: ApprovalResponse };
  /** Asks the user a question. Unanswered: the asker is told "no answer". */
  "question/ask": { params: QuestionRequest; result: QuestionResponse };
  /** Asks whether to trust a project's executable resources. Unanswered: not trusted. */
  "trust/request": { params: TrustRequest; result: TrustResponse };
}

/** Notifications the server sends, by method name, with their parameters. A client ignores a method it does not know. */
export interface ServerNotifications {
  /** One event of a session. The only notification the server sends. */
  "session/event": SessionEvent;
}

/** Who made a change. */
export type Actor = "agent" | "user" | "harness" | (string & {});

/** An answer to an approval request. A note may go with any of them, and with `deny` it reaches the model as the reason. */
export type ApprovalChoice = "allow_once" | "allow_session" | "allow_project" | "deny" | (string & {});

/** Exactly what an approval would let happen. */
export type ApprovalDetail = ({
    kind: "command";
  } & CommandDetail) | ({
    kind: "diff";
  } & DiffDetail) | ({
    kind: "target";
  } & TargetDetail) | ({
    kind: "tool";
  } & ToolDetail);

/** One stored approval. */
export type ApprovalId = string;

/** `approval/request`: may this tool call run? */
export interface ApprovalRequest {
  /** The tool call. */
  call_id: CallId;
  /** The answers on offer. */
  choices: Array<ApprovalChoice>;
  /** Exactly what would happen. */
  detail: ApprovalDetail;
  /** The session. */
  session_id: SessionId;
  /** The call in one line. */
  summary: string;
  /** The tool's name. */
  tool: string;
  /** The turn. */
  turn_id: TurnId;
}

/** The answer to `approval/request`. */
export interface ApprovalResponse {
  /** The choice: one of those on offer. */
  choice: ApprovalChoice;
  /** A note for the model. */
  note?: string | null;
}

/** How long an approval lasts. */
export type ApprovalScope = "session" | "project" | (string & {});

/** A stored artifact, such as the full output of a command or an attached image. */
export type ArtifactId = string;

/** A reference to a stored artifact, such as the full output of a command. */
export interface ArtifactRef {
  /** Its size in bytes. */
  bytes?: number | null;
  /** The artifact's id. */
  id: ArtifactId;
  /** What it holds, in a few words. */
  label: string;
}

/** One response from the model: text, reasoning and tool calls. */
export interface AssistantMessageItem {
  /** When it was recorded. */
  created_at?: Timestamp | null;
  /** The failure, when `stop_reason` is `error`. */
  error?: ErrorData | null;
  /** The item's id. */
  id: ItemId;
  /** What produced the message. */
  origin?: Origin | null;
  /** The content, in the model's order. */
  parts: Array<Part>;
  /** The provider's own stop value. */
  raw_stop?: string | null;
  /** More about the stop where the provider gave it, such as the category of a refusal. */
  stop_detail?: string | null;
  /** Why the response ended. Absent only on an item that is still streaming. */
  stop_reason?: StopReason | null;
  /** Whether the response was cut off by the output limit. */
  truncated?: boolean;
  /** What the request used and cost. */
  usage?: RequestUsage | null;
}

/** How the usage is paid for. */
export type Billing = "metered" | "subscription" | "unknown" | (string & {});

/** What a lease may spend. A field that is absent is not limited by this lease. */
export interface Budget {
  /** Requests to a model. */
  requests?: number | null;
  /** Tokens, input and output together. */
  tokens?: number | null;
  /** Money, in nano-dollars. */
  usd_nanos?: number | null;
  /** Wall-clock time, in milliseconds. */
  wall_clock_ms?: number | null;
}

/** The price of writing to the prompt cache, for one lifetime class. */
export interface CacheWritePrice {
  /** The provider's name for the lifetime class, for example `5m` or `1h`. */
  class: string;
  /** Nano-dollars per million tokens. */
  price: number;
}

/** Input tokens written to the cache, for one lifetime class. */
export interface CacheWriteTokens {
  /** The provider's name for the lifetime class, for example `5m` or `1h`. */
  class: string;
  /** The tokens written in that class. */
  tokens: number;
}

/** One tool call. The provider chose it, so it is unique only within its session. */
export type CallId = string;

/** What a client can handle. A capability that is absent is `false`: the server never sends what the client did not ask for. */
export interface ClientCapabilities {
  /** It answers `approval/request`. */
  approvals?: boolean;
  /** It wants `item.delta` events. */
  deltas?: boolean;
  /** The experimental features it opts in to, by name. */
  experimental?: Array<string>;
  /** It answers `question/ask`. */
  questions?: boolean;
  /** It answers `trust/request`. */
  trust?: boolean;
}

/** What a client is to the sessions it opens. */
export type ClientRole = "user" | "supervisor" | "observer" | (string & {});

/** The detail of an approval for a command. */
export interface CommandDetail {
  /** The full command line. Nothing is shortened. */
  command: string;
  /** The directory it would run in. */
  cwd: string;
  /** Whether it would run outside the sandbox. */
  outside_sandbox?: boolean;
}

/** What a command entry is for. */
export type CommandPurpose = "build" | "test" | "check" | "format" | "run" | (string & {});

/** Notice: a compaction started, finished or failed. */
export interface CompactionNotice {
  /** Whether it runs in the background while the turn carries on. */
  background?: boolean;
  /** The notice in words. */
  message: string;
  /** Where it stands. */
  state: CompactionState;
}

/** Where a compaction stands. */
export type CompactionState = "started" | "finished" | "failed" | (string & {});

/** The state of an MCP server or an extension. */
export type ComponentState = "starting" | "ready" | "failed" | "disabled" | "needs_approval" | (string & {});

/** The state of one MCP server or one extension. */
export interface ComponentStatus {
  /** What went wrong, for `failed`. */
  error?: string | null;
  /** Its configured name. */
  name: string;
  /** Its state. */
  state: ComponentState;
  /** How many tools it provides. */
  tools?: number;
}

/** `config/get` */
export interface ConfigGetParams {
  /** Only these keys, in dotted form. Empty: everything. */
  keys?: Array<string>;
  /** The session whose effective configuration is wanted. Absent: the configuration without any project's layers. */
  session_id?: SessionId | null;
}

/** The result of `config/get`. */
export interface ConfigGetResult {
  /** The effective configuration, as the configuration file would have it. Credentials are never in it. */
  config: { [key: string]: unknown };
}

/** The result of `config/reload`. */
export interface ConfigReloadResult {
  /** Keys whose new values are in force. */
  applied?: Array<string>;
  /** What was wrong with a file. When this is not empty the old configuration stays in force. */
  errors?: Array<string>;
  /** Keys whose new values wait for the next context epoch. */
  pending?: Array<string>;
}

/** Whether a cooldown began or ended. */
export type CooldownState = "started" | "ended" | (string & {});

/** An amount of money with its basis. Amounts with different bases are never added together. */
export interface Cost {
  /** What kind of number it is. */
  basis: CostBasis;
  /** US dollars, as a whole number of nano-dollars (1e-9). Whole numbers make sums exact: `184_200_000` is $0.1842. */
  usd_nanos: number;
}

/** What kind of number a cost is. */
export type CostBasis = "metered" | "estimate" | "recorded" | (string & {});

/** When a prompt sent during a running turn is delivered. */
export type Delivery = "steer" | "queue" | (string & {});

/** Streaming content for an item. A display convenience: deltas may be merged or dropped for a slow client, and `item.completed` is what counts. */
export type Delta = ({
    kind: "text";
  } & TextDelta) | ({
    kind: "reasoning";
  } & TextDelta) | ({
    kind: "tool_call";
  } & ToolCallDelta) | ({
    kind: "tool_args";
  } & ToolArgsDelta) | ({
    kind: "tool_output";
  } & TextDelta);

/** The detail of an approval for a change to a file. */
export interface DiffDetail {
  /** The change, as a unified diff. */
  diff: string;
  /** The file. */
  path: string;
}

/** What became of a prompt. */
export type Disposition = "started" | "steering" | "queued" | (string & {});

/** What a planned tool call does. */
export type Effect = "read" | "write" | "exec" | "network" | "ext" | (string & {});

/** A result with nothing in it. Written as `{}`, so fields can be added later. */
export interface Empty {
  [key: string]: unknown;
}

/** What a user does to an entry. */
export type EntryAction = "accept" | "edit" | "disable" | "delete" | "promote" | (string & {});

/** How the harness came to hold an entry. */
export type EntryBasis = "observed" | "proposed" | (string & {});

/** One change to a project entry. */
export interface EntryChange {
  /** What to do to it. */
  action: EntryAction;
  /** The entry. */
  id: EntryId;
  /** For `edit`: the new text. */
  text?: string | null;
}

/** A cheap, read-only test that shows an entry still holds. */
export interface EntryCheck {
  /** For `command`: the command line. */
  command?: string | null;
  /** The form of the check. */
  kind: EntryCheckKind;
  /** For `exists` and `contains`: the path, relative to the workspace root. */
  path?: string | null;
  /** For `contains`: the pattern. */
  pattern?: string | null;
}

/** The form of a check. */
export type EntryCheckKind = "exists" | "contains" | "command" | (string & {});

/** The command of a `command` entry. */
export interface EntryCommand {
  /** The command line. */
  command: string;
  /** The directory it runs in, relative to the workspace root. */
  cwd?: string | null;
  /** What it is for. */
  purpose: CommandPurpose;
  /** Whether it only reads. */
  read_only?: boolean;
  /** How long it usually takes, in milliseconds. */
  usual_duration_ms?: number | null;
}

/** One entry of the project profile or memory. */
export type EntryId = string;

/** What an entry is. */
export type EntryKind = "command" | "convention" | "flaky" | "fact" | (string & {});

/** Which file an entry lives in. */
export type EntryScope = "shared" | "private" | (string & {});

/** Where an entry came from. */
export interface EntrySource {
  /** For `learned`: when. */
  at?: Timestamp | null;
  /** For `learned`: the item. */
  item_id?: ItemId | null;
  /** Who it came from. */
  kind: EntrySourceKind;
  /** For `learned`: the session. */
  session_id?: SessionId | null;
}

/** Who an entry came from. */
export type EntrySourceKind = "user" | "learned" | (string & {});

/** Whether an entry is in use. */
export type EntryStatus = "proposed" | "active" | "disabled" | (string & {});

/** An error, as every front end sees it: in `turn.completed`, on an assistant item whose request failed, and as the `data` of a JSON-RPC application error. */
export interface ErrorData {
  /** A stable, machine-readable name for the exact failure, in `snake_case`: one of [`code`](crate::error::code), a provider's own error code, or a code a later version adds. Clients may branch on the ones they know and must tolerate the rest. */
  code: string;
  /** Structured detail whose shape depends on `code`. For `unsupported_protocol_version`: `{"supported": [1]}`. */
  details?: { [key: string]: unknown } | null;
  /** What sort of failure this is. Clients branch on it. */
  kind: ErrorKind;
  /** One or two sentences for a person. */
  message: string;
  /** The provider's id for the failed request, for its support channel. */
  request_id?: string | null;
  /** When the provider said the request may be tried again, if it said so and the harness did not wait itself. */
  retry_at?: Timestamp | null;
}

/** What sort of failure an error is. It decides whether the harness retries, which exit code a headless run ends with, and what a client should offer the user. */
export type ErrorKind = "usage" | "config" | "auth" | "provider" | "transient" | "model_stop" | "permission" | "limit" | "context_overflow" | "tool" | "interrupted" | "internal" | (string & {});

/** One event of a session. */
export type Event = ({
    type: "turn.started";
  } & TurnStarted) | ({
    type: "item.started";
  } & ItemStarted) | ({
    type: "item.delta";
  } & ItemDelta) | ({
    type: "item.reset";
  } & ItemRef) | ({
    type: "item.completed";
  } & ItemCompleted) | ({
    type: "item.resync";
  } & ItemRef) | ({
    type: "usage.updated";
  } & UsageUpdated) | ({
    type: "plan.updated";
  } & PlanUpdated) | ({
    type: "project.updated";
  } & ProjectUpdated) | ({
    type: "notice";
  } & Notice) | ({
    type: "request.resolved";
  } & RequestResolved) | ({
    type: "turn.completed";
  } & TurnCompleted) | ({
    type: "session.idle";
  } & SessionIdle) | ({
    type: "session.updated";
  } & SessionUpdated);

/** When a lease ends. */
export interface Expiry {
  /** For `at`: the moment. */
  at?: Timestamp | null;
  /** Which kind of end. */
  kind: ExpiryKind;
}

/** When a lease ends. */
export type ExpiryKind = "never" | "at" | "end_of_task" | "end_of_session";

/** The result of `extensions/status`. */
export interface ExtensionsStatusResult {
  /** The extensions. */
  extensions: Array<ComponentStatus>;
}

/** What a lease allows. */
export interface Grant {
  /** What may be spent. */
  budget?: Budget;
  /** The level. A level that is not listed is refused. */
  level: "read-only" | "supervised" | "guided" | "autonomous" | "unrestricted";
  /** What no level relaxes. */
  limits?: HardLimits;
  /** The models that may be used. Empty means the configured ones. */
  models?: Array<ModelId>;
  /** The rules. */
  rules?: Array<Rule>;
  /** The sandbox policy. */
  sandbox: SandboxPolicy;
}

/** Who granted a lease. */
export interface Grantor {
  /** Which kind of grantor. */
  kind: GrantorKind;
  /** For `parent`: the lease this one was narrowed from. */
  lease_id?: LeaseId | null;
}

/** Who granted a lease. */
export type GrantorKind = "user" | "parent";

/** What no level relaxes. Set by the user in the grant and enforced by the harness. */
export interface HardLimits {
  /** Whether work may be merged into the repository's main branch. */
  merge_to_main?: boolean;
  /** The most sessions a team may run at once. */
  team_size?: number | null;
}

/** An image. Input only. */
export interface ImagePart {
  /** The stored artifact that holds the image. */
  artifact?: ArtifactId | null;
  /** The image bytes in base64. Exactly one of `data` and `artifact` is set. */
  data?: string | null;
  /** The media type, for example `image/png`. */
  media_type: string;
}

/** Notice: anything else worth telling the user. */
export interface InfoNotice {
  /** The notice in words. */
  message: string;
}

/** The first request on a connection. */
export interface InitializeParams {
  /** What the client can handle. */
  capabilities?: ClientCapabilities;
  /** The client. */
  client: PeerInfo;
  /** The oldest protocol version the client speaks. Absent: only `protocol_version`. */
  min_protocol_version?: number | null;
  /** The newest protocol version the client speaks. */
  protocol_version: number;
  /** The client's role. */
  role: ClientRole;
}

/** The server's answer to `initialize`. */
export interface InitializeResult {
  /** What the server offers. */
  capabilities: ServerCapabilities;
  /** The version the connection uses: the newest one both sides speak. */
  protocol_version: number;
  /** The server. */
  server: PeerInfo;
}

/** Notice: instruction files changed and will be re-read. */
export interface InstructionsNotice {
  /** The files, relative to the workspace root where they are inside it. */
  files?: Array<string>;
  /** The notice in words. */
  message: string;
}

/** One item of a session. */
export type Item = ({
    kind: "user_message";
  } & UserMessageItem) | ({
    kind: "assistant_message";
  } & AssistantMessageItem) | ({
    kind: "tool_result";
  } & ToolResultItem) | ({
    kind: "notice";
  } & NoticeItem) | ({
    kind: "summary";
  } & SummaryItem);

/** An item is complete. This is the authoritative content. */
export interface ItemCompleted {
  /** The item, in full. */
  item: Item;
}

/** Streaming content for an item. Sent only to clients that declared `deltas`. */
export interface ItemDelta {
  /** The content. */
  delta: Delta;
  /** The item. */
  item_id: ItemId;
}

/** One item of a session: a message, a tool result, a notice or a summary. */
export type ItemId = string;

/** The kind of an item, where only the kind is needed. */
export type ItemKind = "user_message" | "assistant_message" | "tool_result" | "notice" | "summary" | (string & {});

/** Names an item: the payload of `item.reset` and `item.resync`. */
export interface ItemRef {
  /** The item. */
  item_id: ItemId;
}

/** An item began. Its content follows as deltas, and then in full. */
export interface ItemStarted {
  /** For a tool result: the call it answers. */
  call_id?: CallId | null;
  /** The item. */
  item_id: ItemId;
  /** Its kind. */
  kind: ItemKind;
  /** For a tool result: the tool that is running. */
  tool?: string | null;
}

/** The error member of a failed response. */
export interface JsonRpcError {
  /** A JSON-RPC error code. -32000 for every application error. */
  code: number;
  /** For an application error (code -32000), an ErrorData. Otherwise optional detail. */
  data?: ErrorData | unknown;
  /** One sentence for a person. */
  message: string;
}

/** Any JSON-RPC message: one JSON object per line. */
export type JsonRpcMessage = JsonRpcRequest | JsonRpcNotification | JsonRpcResponse;

/** A JSON-RPC notification: a message that gets no answer. */
export interface JsonRpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

/** A JSON-RPC request: the receiver must answer it with a response that has the same id. */
export interface JsonRpcRequest {
  id: RequestId;
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

/** The answer to a JSON-RPC request. It has `result` or `error`, never both. */
export interface JsonRpcResponse {
  error?: JsonRpcError;
  id: RequestId | null;
  jsonrpc: "2.0";
  result?: unknown;
}

/** Everything a session is allowed to do, as one value. */
export interface Lease {
  /** When the lease ends. */
  expires: Expiry;
  /** What is allowed. */
  grant: Grant;
  /** Who granted it. */
  granted_by: Grantor;
  /** The lease's id. */
  id: LeaseId;
  /** When the holder must stop and report. */
  stop_when?: Array<StopCondition>;
}

/** A lease: what a session is allowed to do. */
export type LeaseId = string;

/** How much a session may do without asking: who decides when no rule has settled the question. The command line calls a session's level its permission mode. */
export type Level = "read-only" | "supervised" | "guided" | "autonomous" | "unrestricted" | (string & {});

/** Which limit was reached. */
export type LimitKind = "requests" | "cost" | "tokens" | "time" | (string & {});

/** What a limit counts. */
export type LimitMeasure = "requests" | "tokens" | "input_tokens" | "output_tokens" | (string & {});

/** Notice: a configured limit was reached, or is near. */
export interface LimitNotice {
  /** Which limit. */
  limit: LimitKind;
  /** The notice in words. */
  message: string;
  /** Whether it was reached. `false` is a warning that it is near. */
  reached: boolean;
}

/** Where a limit figure came from. */
export type LimitSource = "headers" | "refresh" | (string & {});

/** One limit a provider reported: how much of it is left and when it resets. */
export interface LimitWindow {
  /** The limit. */
  limit?: number | null;
  /** What the limit counts. */
  measure: LimitMeasure;
  /** What is left of it. Absent when not known, which includes "the reset time has passed and no request has been made since". */
  remaining?: number | null;
  /** When it resets. */
  resets_at?: Timestamp | null;
  /** The speed tier the limit belongs to, when the provider keeps a separate limit for that tier. Absent for the model's ordinary limit. */
  speed?: Speed | null;
}

/** The result of `mcp/status`. */
export interface McpStatusResult {
  /** The servers. */
  servers: Array<ComponentStatus>;
}

/** One message of a conversation. */
export interface Message {
  /** For an assistant message: what produced it. */
  origin?: Origin | null;
  /** Its content, in order. */
  parts: Array<Part>;
  /** Who it is from. */
  role: Role;
}

/** A kind of input or output a model handles. */
export type Modality = "text" | "image" | (string & {});

/** Capability flags of a model. A flag that is absent is `false`. */
export interface ModelCapabilities {
  /** System messages may appear in the middle of a conversation. */
  mid_conversation_system?: boolean;
  /** The smallest prefix the provider will cache, in tokens. */
  min_cacheable_tokens?: number | null;
  /** The provider checks tool arguments against the schema. */
  strict_tools?: boolean;
  /** The provider can hold the model to an output schema. */
  structured_output?: boolean;
}

/** A model, as the catalog names it. Unique within its provider. */
export type ModelId = string;

/** A model, as the catalog describes it. */
export interface ModelInfo {
  /** Capability flags. */
  capabilities?: ModelCapabilities;
  /** The context window, in tokens. */
  context_window: number;
  /** The name to show. */
  display_name: string;
  /** The model's id at its provider. */
  id: ModelId;
  /** What it accepts as input. */
  input?: Array<Modality>;
  /** The most output tokens one response may have. */
  max_output: number;
  /** The rate card at `standard` speed. Absent when no price is known. */
  prices?: Prices | null;
  /** The wire dialect used to reach it, for example `anthropic-messages`. */
  protocol: string;
  /** The provider that serves it. */
  provider: ProviderId;
  /** The models whose opaque reasoning this model can read back. Its own id is not listed. */
  reads_reasoning_from?: Array<ModelId>;
  /** How reasoning depth is controlled. */
  reasoning: ReasoningControl;
  /** The speed tiers it offers besides `standard`. Empty for most models. A front end shows a speed control only when this is not empty. */
  speeds?: Array<SpeedOffer>;
}

/** The job a model is doing. Configuration names one model per role (`docs/architecture/14-adaptation.md`, "Model roles and routing"). */
export type ModelRole = "main" | "fast" | "judge" | (string & {});

/** A session's usage for one provider, model, role and speed. */
export interface ModelUsage {
  /** The model. */
  model: ModelId;
  /** The provider. */
  provider: ProviderId;
  /** The role. */
  role: ModelRole;
  /** The speed the requests ran at. */
  speed?: Speed;
  /** What was used. */
  totals: UsageTotals;
}

/** The result of `models/list`. */
export interface ModelsListResult {
  /** Every model of every configured provider. */
  models: Array<ModelInfo>;
}

/** Whether sandboxed processes may use the network. There is no per-host list. */
export type NetworkAccess = "off" | "full";

/** Something transient that front ends should show. It is never sent to the model and is not an item. Every notice has a `message` in words, so a client can show a kind of notice it does not know. */
export type Notice = ({
    kind: "retry";
  } & RetryNotice) | ({
    kind: "compaction";
  } & CompactionNotice) | ({
    kind: "limit";
  } & LimitNotice) | ({
    kind: "sandbox";
  } & SandboxNotice) | ({
    kind: "instructions";
  } & InstructionsNotice) | ({
    kind: "speed_fallback";
  } & SpeedFallback) | ({
    kind: "speed_cooldown";
  } & SpeedCooldown) | ({
    kind: "info";
  } & InfoNotice);

/** A message the harness added to the conversation. The model sees it. This is not the `notice` event ([`Notice`](crate::event::Notice)), which tells front ends about something transient and is never sent to the model. */
export interface NoticeItem {
  /** When it was recorded. */
  created_at?: Timestamp | null;
  /** The item's id. */
  id: ItemId;
  /** Why it was added. */
  source: NoticeSource;
  /** What the model is told. */
  text: string;
}

/** Why the harness added a notice to the conversation. */
export type NoticeSource = "instructions" | "plan" | "repetition" | "verification" | "limit" | "harness" | (string & {});

/** What produced an assistant message. Raw blocks are valid only for replay to the same protocol, and to a model that can read them. */
export interface Origin {
  /** The account or organisation the credential belonged to, where the provider ties opaque reasoning to it. Never the credential. */
  account_scope?: string | null;
  /** The model. */
  model: ModelId;
  /** The wire dialect, for example `anthropic-messages`. */
  protocol: string;
  /** The provider. */
  provider: ProviderId;
}

/** One part of a message. */
export type Part = ({
    kind: "text";
  } & TextPart) | ({
    kind: "image";
  } & ImagePart) | ({
    kind: "tool_call";
  } & ToolCallPart) | ({
    kind: "tool_result";
  } & ToolResultPart) | ({
    kind: "reasoning";
  } & ReasoningPart) | ({
    kind: "provider_block";
  } & ProviderBlockPart);

/** A set of paths: everything, or the trees under some roots. */
export interface PathSet {
  /** Every path. When set, `roots` is ignored. */
  all?: boolean;
  /** The roots of the trees in the set. */
  roots?: Array<string>;
}

/** The name and version of one side of a connection. */
export interface PeerInfo {
  /** The program's name. */
  name: string;
  /** Its version. */
  version: string;
}

/** The result of `permissions/list`. */
export interface PermissionsListResult {
  /** The approvals in force for the session. */
  approvals: Array<StoredApproval>;
}

/** `permissions/revoke` */
export interface PermissionsRevokeParams {
  /** The approvals to remove. */
  ids: Array<ApprovalId>;
  /** The session. */
  session_id: SessionId;
}

/** The plan for the current work: a task list that the agent keeps and the user can see and change. */
export interface Plan {
  /** The tasks, in order. */
  items?: Array<PlanItem>;
  /** Goes up by one with every change, so a client can tell which of two plans is newer. */
  revision?: number;
}

/** Adds a plan item. */
export interface PlanAdd {
  /** The item it goes after. Absent: at the end. */
  after?: PlanItemId | null;
  /** The task. */
  text: string;
}

/** One edit a user makes to the plan. It takes effect at once, and reaches the model as a notice at the next safe boundary. */
export type PlanEdit = ({
    kind: "reorder";
  } & PlanReorder) | ({
    kind: "skip";
  } & PlanItemRef) | ({
    kind: "reword";
  } & PlanReword) | ({
    kind: "add";
  } & PlanAdd) | ({
    kind: "remove";
  } & PlanItemRef);

/** One task of a plan. */
export interface PlanItem {
  /** The task's id, stable across edits. */
  id: PlanItemId;
  /** Where it stands. */
  status: PlanStatus;
  /** The task. */
  text: string;
}

/** One item of a plan. */
export type PlanItemId = string;

/** Names one plan item: the payload of `skip` and `remove`. */
export interface PlanItemRef {
  /** The item. */
  id: PlanItemId;
}

/** Puts the plan's items in a new order. */
export interface PlanReorder {
  /** Every item's id, in the new order. */
  order: Array<PlanItemId>;
}

/** A result that is the plan. */
export interface PlanResult {
  /** The plan now. */
  plan: Plan;
}

/** Replaces a plan item's text. */
export interface PlanReword {
  /** The item. */
  id: PlanItemId;
  /** The new text. */
  text: string;
}

/** Where a plan item stands. */
export type PlanStatus = "pending" | "active" | "done" | "skipped" | (string & {});

/** `plan/update` */
export interface PlanUpdateParams {
  /** The edits, applied in order. */
  edits: Array<PlanEdit>;
  /** The session. */
  session_id: SessionId;
}

/** The plan changed. */
export interface PlanUpdated {
  /** Who changed it. */
  by: Actor;
  /** The plan now. */
  plan: Plan;
}

/** The prices of a request whose input is larger than a threshold. */
export interface PriceTier {
  /** The tier applies when the request's input tokens exceed this. */
  above_input_tokens: number;
  /** Cache reads, nano-dollars per million. */
  cache_read?: number | null;
  /** Cache writes per lifetime class. */
  cache_write?: Array<CacheWritePrice>;
  /** Input tokens, nano-dollars per million. */
  input: number;
  /** Output tokens, nano-dollars per million. */
  output: number;
}

/** One rate card: what a million tokens of each class cost. Every price is a whole number of nano-dollars (1e-9 US dollars) per million tokens, so `4_000_000_000` is $4.00 per million. */
export interface Prices {
  /** Input tokens read from the cache. Absent when the model has no cache price. */
  cache_read?: number | null;
  /** Input tokens written to the cache, per lifetime class. */
  cache_write?: Array<CacheWritePrice>;
  /** Input tokens that were not read from or written to the cache. */
  input: number;
  /** Output tokens, reasoning included. */
  output: number;
  /** Rate cards that replace this one for larger requests, by ascending threshold. The last tier whose threshold the request passes applies. */
  tiers?: Array<PriceTier>;
}

/** A result that is project entries. */
export interface ProjectEntriesResult {
  /** The entries: all of them for `project/list`, the changed ones for `project/update`. */
  entries: Array<ProjectEntry>;
}

/** One entry of the project profile or memory. */
export interface ProjectEntry {
  /** How the harness came to hold it. Absent for an entry the user wrote. */
  basis?: EntryBasis | null;
  /** A test that shows it still holds. */
  check?: EntryCheck | null;
  /** For a `command` entry: the command. */
  command?: EntryCommand | null;
  /** When it was last seen to hold. */
  confirmed_at?: Timestamp | null;
  /** Where it was learned: a path and line, a command, or the user's words. */
  evidence?: string | null;
  /** When it stops being used unless confirmed again. Entries the user wrote do not expire. */
  expires_at?: Timestamp | null;
  /** The entry's id. */
  id: EntryId;
  /** What it is. */
  kind: EntryKind;
  /** Which file it lives in. */
  scope: EntryScope;
  /** Where it came from. */
  source: EntrySource;
  /** Whether it is in use. */
  status: EntryStatus;
  /** The entry in words: the rule, the fact, the test's name, or what the command is. */
  text: string;
}

/** The key that names a project in the state directory. Never a path. */
export type ProjectKey = string;

/** Names a project by a session of it, or by its workspace root. Exactly one is set. */
export interface ProjectListParams {
  /** A session of the project. */
  session_id?: SessionId | null;
  /** The project's workspace root. */
  workspace?: string | null;
}

/** `project/update` */
export interface ProjectUpdateParams {
  /** The changes, applied in order. */
  changes: Array<EntryChange>;
  /** A session of the project. Exactly one of this and `workspace` is set. */
  session_id?: SessionId | null;
  /** The project's workspace root. */
  workspace?: string | null;
}

/** Project profile or memory entries were proposed or changed. */
export interface ProjectUpdated {
  /** The entries that are new or changed, as they are now. */
  entries?: Array<ProjectEntry>;
  /** The entries that were deleted. */
  removed?: Array<EntryId>;
}

/** Something the provider produced that the neutral model does not name: server-side tool use and its results, compaction blocks, fallback markers. */
export interface ProviderBlockPart {
  /** The provider's name for the block type, for display. */
  block_type?: string | null;
  /** The provider's own block. */
  raw: RawJson;
}

/** A provider: an endpoint that speaks a protocol, by its configured name. */
export type ProviderId = string;

/** The limits one provider last reported. A provider that reports none has no entry, and a front end shows "not available". */
export interface ProviderLimits {
  /** The provider. */
  provider: ProviderId;
  /** When the figures were seen. */
  seen_at: Timestamp;
  /** Where they came from. */
  source: LimitSource;
  /** The limits. */
  windows: Array<LimitWindow>;
}

/** `question/ask`: a question for the user, from the agent or an extension. */
export interface QuestionRequest {
  /** Answers to pick from. Empty: free text. */
  choices?: Array<string>;
  /** The extension that asks. Absent: the agent. */
  extension?: string | null;
  /** The question. */
  question: string;
  /** The session. */
  session_id: SessionId;
  /** The turn, when the question belongs to one. */
  turn_id?: TurnId | null;
}

/** The answer to `question/ask`. */
export interface QuestionResponse {
  /** The answer. Absent: the user declined to answer. */
  answer?: string | null;
}

/** JSON text kept exactly as a provider sent it, carried as a string so that no parser on the way can reorder keys, change escapes or round numbers. Replay sends it back unchanged. Clients treat it as opaque. */
export type RawJson = string;

/** A model's reasoning control. Depth is a per-model set, never a number on a shared scale. */
export interface ReasoningControl {
  /** For `levels`: the level used when none is set. */
  default?: string | null;
  /** Which kind of control this is. */
  kind: ReasoningControlKind;
  /** For `levels`: the model's own level names, from least to most. */
  levels?: Array<string>;
  /** For `budget`: the largest budget, in tokens. */
  max_tokens?: number | null;
  /** For `budget`: the smallest budget, in tokens. */
  min_tokens?: number | null;
}

/** How a model's reasoning depth is controlled. */
export type ReasoningControlKind = "none" | "levels" | "budget" | (string & {});

/** Reasoning by the model. */
export interface ReasoningPart {
  /** The provider's own block, which holds the opaque payload (a signature, encrypted content). It is replayed unchanged to a model that can read it and dropped for any other. */
  raw?: RawJson | null;
  /** The visible reasoning or its summary. Often empty. */
  text?: string;
}

/** The id that ties a response to its request: a number or a string, chosen by the side that sends the request. */
export type RequestId = number | string;

/** A request from the server (an approval, a question, a trust prompt) was settled without this client. The client withdraws its prompt. */
export interface RequestResolved {
  /** The JSON-RPC id of the request, as this client received it. */
  request_id: RequestId;
  /** How it was settled. */
  resolution: Resolution;
}

/** The usage of one request to a model: what was used, at which speed, and what it cost. */
export interface RequestUsage {
  /** How the usage is paid for. */
  billing: Billing;
  /** The version of the catalog whose prices were used. */
  catalog_version?: string | null;
  /** The cost, fixed when the request was recorded. Absent when the model has no known price. */
  cost?: Cost | null;
  /** Milliseconds the request took. */
  duration_ms?: number | null;
  /** The model. */
  model: ModelId;
  /** The provider. */
  provider: ProviderId;
  /** The provider's own usage object. */
  raw?: RawJson | null;
  /** The role the request was made in. */
  role: ModelRole;
  /** The speed the request **ran at**: the provider's own report where it gives one, otherwise the speed that was sent. The cost is priced at this speed's rate. */
  speed?: Speed;
  /** The speed that was asked for, when the request ran at another one. */
  speed_requested?: Speed | null;
  /** Milliseconds from sending the request to the first token. */
  time_to_first_token_ms?: number | null;
  /** Tokens by class. */
  tokens: TokenUsage;
}

/** How a request from the server to its clients was settled. */
export type Resolution = "answered" | "timed_out" | "cancelled" | (string & {});

/** Notice: a request failed and will be tried again. */
export interface RetryNotice {
  /** Which attempt comes next, counting from 1. */
  attempt: number;
  /** Why the last attempt failed. */
  error: ErrorData;
  /** The most attempts a request gets. */
  max_attempts: number;
  /** The notice in words. */
  message: string;
  /** When the next attempt starts. */
  next_attempt_at: Timestamp;
}

/** Who a message is from. */
export type Role = "user" | "assistant" | "tool" | (string & {});

/** A change to the settings of one model role. A field that is absent is left as it is. */
export interface RoleSettings {
  /** The reasoning level: one of the model's own levels, or a portable alias (`minimal`, `low`, `medium`, `high`, `max`). */
  effort?: string | null;
  /** The model. */
  model?: ModelId | null;
  /** The provider that serves the model. Needed only when two providers have a model with the same id. */
  provider?: ProviderId | null;
  /** The speed for this role. For `main` it overrides the session's speed. The utility roles (`fast`, `judge`) do not follow the session's speed: they run at `standard` unless this is set. */
  speed?: Speed | null;
}

/** What one model role of a session is set to, and what is in effect. */
export interface RoleState {
  /** The reasoning level as it was set. */
  effort?: string | null;
  /** The model's own level that the setting maps to. */
  effort_in_effect?: string | null;
  /** The model. */
  model: ModelId;
  /** The provider. */
  provider: ProviderId;
  /** The speed this role is set to: its own setting; without one, the session's speed for `main` and `standard` for a utility role. Required, so that a reader never has to guess a default. */
  speed: Speed;
  /** When a cooldown of `speed` ends, while one is running. */
  speed_cooldown_until?: Timestamp | null;
  /** The speed its next request will ask for. It differs from `speed` when the model does not offer that speed, or while the speed is in a cooldown. Required. */
  speed_in_effect: Speed;
}

/** A permission rule. `deny` beats `ask` beats `allow`, whatever the order. */
export interface Rule {
  /** What it decides. */
  action: RuleAction;
  /** The effect the rule is about. An effect that is not listed is refused. */
  effect: "read" | "write" | "exec" | "network" | "ext";
  /** What it matches: a path glob, a command pattern, a host, or a `server/tool` name. */
  match: string;
}

/** What a rule decides. */
export type RuleAction = "allow" | "ask" | "deny";

/** Notice: something about the sandbox the user should know: it is not enforcing, a command ran outside it, a protected path changed. */
export interface SandboxNotice {
  /** The notice in words. */
  message: string;
  /** Whether the sandbox is enforcing. */
  status: SandboxStatus;
}

/** What a child process may touch, whatever it runs. */
export interface SandboxPolicy {
  /** Whether the network may be used. */
  network: NetworkAccess;
  /** Paths inside `write` that must not be changed. */
  protect?: PathSet;
  /** What may be read. */
  read: PathSet;
  /** What may be written. */
  write: PathSet;
}

/** Whether the OS sandbox confines child processes on this machine. */
export type SandboxStatus = "enforcing" | "off" | (string & {});

/** What a server offers. */
export interface ServerCapabilities {
  /** The experimental features it turned on for this connection: the ones the client asked for that it has. */
  experimental?: Array<string>;
  /** It was built with extensions. */
  extensions?: boolean;
  /** It was built with the MCP client. */
  mcp?: boolean;
  /** Whether its sandbox confines commands. */
  sandbox: SandboxStatus;
  /** It stores sessions. */
  sessions?: boolean;
}

/** `session/compact` */
export interface SessionCompactParams {
  /** What the summary should keep in particular. */
  focus?: string | null;
  /** The session. */
  session_id: SessionId;
}

/** An event with its envelope: which session, where in its sequence, and which turn. The event's own fields sit beside the envelope's. */
export type SessionEvent = {
    /** Goes up by one per event per session. A gap means events were missed. */
    seq: number;
    session_id: SessionId;
    turn_id?: TurnId;
  } & Event;

/** `session/fork` */
export interface SessionForkParams {
  /** The last item the new session keeps. Absent: all of it. */
  at?: ItemId | null;
  /** The session to fork. */
  session_id: SessionId;
}

/** A stored session. */
export type SessionId = string;

/** The session has no running turn and no queued prompt. */
export interface SessionIdle {
  [key: string]: unknown;
}

/** `session/list` */
export interface SessionListParams {
  /** Where to continue: the `next_cursor` of the page before. */
  cursor?: string | null;
  /** The most sessions to return. */
  limit?: number | null;
  /** Only sessions of this workspace. Absent: every workspace. */
  workspace?: string | null;
}

/** The result of `session/list`: newest first. */
export interface SessionListResult {
  /** Present when there are more. */
  next_cursor?: string | null;
  /** The sessions. */
  sessions: Array<SessionSummary>;
}

/** `session/new` */
export interface SessionNewParams {
  /** Configuration overrides for this session, as the configuration file would have them. They rank as command-line flags do. */
  config?: { [key: string]: unknown };
  /** Whether to keep the session in memory only, with no log. */
  ephemeral?: boolean;
  /** The settings to start with. Anything absent comes from configuration. */
  settings?: SessionSettings;
  /** A title. */
  title?: string | null;
  /** The workspace root: an absolute path on the server's machine. */
  workspace: string;
}

/** `session/read` */
export interface SessionReadParams {
  /** Where to continue: the `next_cursor` of the page before. */
  cursor?: string | null;
  /** Read only this item. An item in progress comes back with the content it has so far. */
  item_id?: ItemId | null;
  /** The most items to return. */
  limit?: number | null;
  /** The session. */
  session_id: SessionId;
}

/** The result of `session/read`: items of the active branch, oldest first. */
export interface SessionReadResult {
  /** The ones among them that are still in progress. */
  incomplete?: Array<ItemId>;
  /** The items. */
  items: Array<Item>;
  /** Present when there are more. */
  next_cursor?: string | null;
  /** The `seq` of the last event sent for the session when the items were read. Events after it are not in this result. */
  seq: number;
}

/** Names a session: the parameters of every method that takes nothing else. */
export interface SessionRef {
  /** The session. */
  session_id: SessionId;
}

/** A result that is the session's state. */
export interface SessionResult {
  /** The session's state. */
  session: SessionState;
}

/** `session/rewind` */
export interface SessionRewindParams {
  /** Whether to put files back as they were at that point, from the snapshots of the turns that are undone. */
  restore_files?: boolean;
  /** The session. */
  session_id: SessionId;
  /** The last item to keep on the active branch. */
  to: ItemId;
}

/** `session/set` */
export interface SessionSetParams {
  /** The session. */
  session_id: SessionId;
  /** What to change. Anything absent stays as it is. */
  settings: SessionSettings;
}

/** A change to a session's settings: the parameters of `session/set`, and the settings a new session starts with. A field that is absent is left as it is. */
export interface SessionSettings {
  /** The level (permission mode). Raising it needs the `user` role. */
  level?: Level | null;
  /** Settings per model role. */
  roles?: { [key: string]: RoleSettings };
  /** The session's speed: what the `main` role runs at unless it sets its own. The utility roles do not inherit it. */
  speed?: Speed | null;
}

/** A session's metadata and current state. */
export interface SessionState {
  /** When it was created. */
  created_at: Timestamp;
  /** The lease in force. `lease/get` returns its terms. */
  lease_id?: LeaseId | null;
  /** The level of the lease in force. */
  level: Level;
  /** The session it was forked from. */
  parent?: SessionId | null;
  /** How many prompts are queued behind the running turn. */
  queue_length?: number;
  /** The model roles. */
  roles: { [key: string]: RoleState };
  /** The turn that is running, if one is. */
  running_turn?: TurnId | null;
  /** Whether the sandbox confines the session's commands. */
  sandbox: SandboxStatus;
  /** The `seq` of the last event sent for this session. Events after it are live. */
  seq: number;
  /** The session. */
  session_id: SessionId;
  /** The session's speed setting: what the `main` role runs at unless it sets its own. Required. */
  speed: Speed;
  /** The session's title. */
  title?: string | null;
  /** The workspace root. An absolute path on the server's machine. */
  workspace: string;
}

/** A stored session, in a list. */
export interface SessionSummary {
  /** When it was created. */
  created_at: Timestamp;
  /** The main model it last used. */
  model?: ModelId | null;
  /** The session it was forked from. */
  parent?: SessionId | null;
  /** The session. */
  session_id: SessionId;
  /** The session's title. */
  title?: string | null;
  /** How many turns it has. */
  turns?: number;
  /** When it was last added to. */
  updated_at: Timestamp;
  /** The workspace root. */
  workspace: string;
}

/** Session metadata changed: title, models, speed, level, queue length. */
export interface SessionUpdated {
  /** The session's state now. */
  session: SessionState;
}

/** The live usage of a session: what `usage/get` returns and `usage.updated` carries. */
export interface SessionUsage {
  /** The same, per provider, model, role and speed. */
  by_model?: Array<ModelUsage>;
  /** Cache reads as a share of all input tokens sent, from 0 to 1. */
  cache_hit_rate?: number | null;
  /** The tokens in the context of the most recent request to the main model. */
  context_tokens?: number | null;
  /** The most recent request. */
  last_request?: RequestUsage | null;
  /** The latest limits each provider reported. */
  limits?: Array<ProviderLimits>;
  /** Output tokens per second of the most recent request. */
  tokens_per_second?: number | null;
  /** Everything the session has used, out-of-band requests included (compaction, titles). */
  totals?: UsageTotals;
}

/** A speed tier, by name. Which ones a model offers is in its [`ModelInfo`](crate::model::ModelInfo). */
export type Speed = "standard" | "fast" | (string & {});

/** Notice: a speed tier's cooldown began or ended. During a cooldown the harness sends requests at `standard` and does not flip back and forth, because each change of speed can cost a prompt-cache rewrite. */
export interface SpeedCooldown {
  /** The notice in words. */
  message: string;
  /** The model. */
  model: ModelId;
  /** The provider. */
  provider: ProviderId;
  /** The tier that is cooling down. */
  speed: Speed;
  /** Whether the cooldown began or ended. */
  state: CooldownState;
  /** When it ends, for one that began. */
  until?: Timestamp | null;
}

/** What a speed tier buys, compared with `standard`. */
export type SpeedEffect = "faster_output" | "priority" | "deferred" | (string & {});

/** Notice: a request ran, or will run, at a different speed from the one the session asked for. The usage record of the request says the same thing in numbers: it is priced at the speed it ran at. */
export interface SpeedFallback {
  /** Who moved it. */
  by: SpeedFallbackBy;
  /** When the harness will ask for the requested speed again, if it has put the tier in a cooldown. */
  cooldown_until?: Timestamp | null;
  /** The notice in words, so that a client that does not know this kind of notice can still show it. */
  message: string;
  /** The model. */
  model: ModelId;
  /** The provider. */
  provider: ProviderId;
  /** Why. */
  reason: SpeedFallbackReason;
  /** The speed the session asked for. */
  requested: Speed;
  /** The role whose request it was. */
  role: ModelRole;
  /** The speed the request ran at, or will run at. */
  served: Speed;
}

/** Who moved a request to another speed. */
export type SpeedFallbackBy = "harness" | "provider" | (string & {});

/** Why a request did not run at the speed that was asked for. */
export type SpeedFallbackReason = "rate_limited" | "overloaded" | "cooldown" | "not_offered" | "unspecified" | (string & {});

/** One speed tier a model offers besides `standard`, with everything that makes it different from another provider's tier of the same name. */
export interface SpeedOffer {
  /** What the tier buys. */
  effect: SpeedEffect;
  /** What the provider calls it, for display: "Fast mode", "Flex processing". */
  label: string;
  /** Whether the provider marks the tier as a preview that may change or need access to be granted. */
  preview?: boolean;
  /** The tier's own full rate card. Absent when its price is not known, in which case its usage shows tokens only. */
  prices?: Prices | null;
  /** Whether the tier has a rate limit of its own, apart from the model's standard limit. */
  separate_limit?: boolean;
  /** Whether a prompt-cache prefix written at `standard` can be read at this tier and the other way round. `false` means a change of speed is a cache miss. Absent when the provider does not say. */
  shares_cache?: boolean | null;
  /** The tier's name: what a setting names and what a usage record carries. */
  speed: Speed;
  /** What the provider does when it cannot serve the tier. */
  when_unavailable: SpeedUnavailable;
}

/** What the provider does when it cannot serve a request at the tier that was asked for. */
export type SpeedUnavailable = "error" | "downgrade" | (string & {});

/** A condition on which the holder of a lease must stop and report. The harness checks it. It is data, not an instruction to the model. */
export type StopCondition = {
    kind: "verification_failed";
    /** The number of failed rounds in a row. */
    rounds: number;
  } | {
    kind: "out_of_scope";
  } | {
    kind: "spending";
    /** The threshold, in nano-dollars. */
    usd_nanos: number;
  } | {
    /** The moment. */
    at: Timestamp;
    kind: "time";
  };

/** Why a response from the model ended. Each provider's own value is mapped onto this set, and kept beside it as `raw_stop`. */
export type StopReason = "end" | "tool_calls" | "max_output" | "context_overflow" | "paused" | "refusal" | "content_filter" | "malformed_tool_call" | "error" | "interrupted" | (string & {});

/** An approval the user gave earlier that still applies. */
export interface StoredApproval {
  /** When it was given. */
  created_at: Timestamp;
  /** The approval's id. */
  id: ApprovalId;
  /** The rule it added. */
  rule: Rule;
  /** How long it lasts. */
  scope: ApprovalScope;
}

/** A compaction summary. It starts a context epoch's history. */
export interface SummaryItem {
  /** The last item the summary covers. */
  covers_through?: ItemId | null;
  /** When it was recorded. */
  created_at?: Timestamp | null;
  /** The item's id. */
  id: ItemId;
  /** The summary. */
  text: string;
  /** What the summarising request used and cost. */
  usage?: RequestUsage | null;
}

/** The detail of an approval for access to a path or a host. */
export interface TargetDetail {
  /** The effect. */
  effect: Effect;
  /** The path or the host. */
  target: string;
}

/** More text of the item. */
export interface TextDelta {
  /** The text to append. */
  text: string;
}

/** A piece of text. */
export interface TextPart {
  /** The provider's own block, on assistant messages. */
  raw?: RawJson | null;
  /** The text. */
  text: string;
}

/** A moment in time, in RFC 3339 form in UTC: `2026-11-12T10:15:02Z`. Fractional seconds are allowed. */
export type Timestamp = string;

/** Token counts by class. The classes do not overlap: the tokens sent are `input + cache_read + cache_write`. */
export interface TokenUsage {
  /** Input tokens read from the cache. */
  cache_read?: number;
  /** Input tokens written to the cache, all lifetime classes together. */
  cache_write?: number;
  /** `cache_write` split by lifetime class, where the provider has classes. */
  cache_write_classes?: Array<CacheWriteTokens>;
  /** Input tokens billed at the normal rate: not read from the cache and not written to it. */
  input?: number;
  /** Output tokens, reasoning included. */
  output?: number;
  /** The part of `output` that was reasoning, when the provider reports it. */
  reasoning?: number | null;
}

/** More of a tool call's arguments: a fragment of JSON text that is not valid JSON until the call is complete. */
export interface ToolArgsDelta {
  /** The call. */
  call_id: CallId;
  /** The fragment to append. */
  json: string;
}

/** The model began a tool call. */
export interface ToolCallDelta {
  /** The call. */
  call_id: CallId;
  /** The tool's name. */
  name: string;
}

/** A request by the model to run a tool. */
export interface ToolCallPart {
  /** The arguments, as the JSON value the model wrote. When the model's text was not valid JSON this is that text as a JSON string, and the call's result is an error that says so. */
  arguments: unknown;
  /** The provider's id for the call. Its result names it. */
  id: CallId;
  /** The tool's name. */
  name: string;
  /** The provider's own block. A protocol with a second id for the call keeps it here. */
  raw?: RawJson | null;
}

/** The detail of an approval for an extension or MCP tool. */
export interface ToolDetail {
  /** The arguments the tool would be called with. */
  arguments: unknown;
}

/** How a tool call ended. Every started call ends with exactly one result. */
export type ToolOutcome = "success" | "error" | "denied" | "interrupted" | (string & {});

/** The result of one tool call. */
export interface ToolResultItem {
  /** Where the full output is, when the content was shortened. */
  artifacts?: Array<ArtifactRef>;
  /** The call this answers. */
  call_id: CallId;
  /** What the model is shown, after the output policy. */
  content: Array<Part>;
  /** When it was recorded. */
  created_at?: Timestamp | null;
  /** Milliseconds the call took. */
  duration_ms?: number | null;
  /** The item's id. */
  id: ItemId;
  /** How the call ended. */
  outcome: ToolOutcome;
  /** A structured payload for front ends, whose shape belongs to the tool: a diff, an exit status, match counts. */
  payload?: unknown;
  /** The tool's name. */
  tool: string;
}

/** The result of one tool call, as the model sees it. */
export interface ToolResultPart {
  /** The call this answers. */
  call_id: CallId;
  /** What the model is shown: text and images. */
  content: Array<Part>;
  /** Whether the call failed. A failure is a result, written so the model can correct itself. */
  is_error?: boolean;
}

/** `trust/request`: may this project's executable resources be used? */
export interface TrustRequest {
  /** What it asks to be trusted with. */
  resources: Array<TrustResource>;
  /** The session. */
  session_id: SessionId;
  /** The project's workspace root. */
  workspace: string;
}

/** One thing a project asks to be trusted with. */
export interface TrustResource {
  /** Whether it was trusted before and has changed since. */
  changed?: boolean;
  /** What it would do, in full: the command, the rules. */
  detail: string;
  /** What kind of thing. */
  kind: TrustResourceKind;
  /** Its name. */
  name: string;
}

/** A kind of project resource that can run code or grant permissions. */
export type TrustResourceKind = "rules" | "hook" | "extension" | "mcp_server" | "commands" | (string & {});

/** The answer to `trust/request`. */
export interface TrustResponse {
  /** Whether the resources listed may be used. */
  trusted?: boolean;
}

/** The turn is over and nothing automatic remains: no retry pending, no compaction running, no tool running. Clients wait on this one event. */
export interface TurnCompleted {
  /** Milliseconds the turn took. */
  duration_ms?: number | null;
  /** The failure, when there was one. */
  error?: ErrorData | null;
  /** How it ended. */
  outcome: TurnOutcome;
  /** Why the last response ended. */
  stop_reason?: StopReason | null;
  /** What the turn used and cost. */
  usage?: UsageTotals;
  /** What the project's checks said. */
  verification?: Verification | null;
}

/** One turn of a session: from a prompt until the session is settled again. */
export type TurnId = string;

/** The result of `turn/interrupt`. */
export interface TurnInterruptResult {
  /** Whether a turn was running. */
  interrupted?: boolean;
  /** Steering prompts that had not been delivered, handed back so the front end can put them in its editor. Queued prompts stay queued. */
  undelivered?: Array<Array<Part>>;
}

/** How a turn ended. It and a headless run's exit code always agree. */
export type TurnOutcome = "completed" | "failed" | "interrupted" | "limit" | "denied" | (string & {});

/** `turn/start` */
export interface TurnStartParams {
  /** How to deliver it if a turn is running. Required then, and ignored when the session is idle. */
  delivery?: Delivery | null;
  /** The prompt: text and images. */
  input: Array<Part>;
  /** The session. */
  session_id: SessionId;
}

/** The result of `turn/start`. It means "accepted": the turn ends with the `turn.completed` event, and the result never carries the model's answer. */
export interface TurnStartResult {
  /** What became of the prompt. */
  disposition: Disposition;
  /** The turn the prompt belongs to: the one it started, the running one it steers, or the one it will start. */
  turn_id: TurnId;
}

/** A turn began. */
export interface TurnStarted {
  [key: string]: unknown;
}

/** The result of `usage/get`. */
export interface UsageGetResult {
  /** The session's usage now. */
  usage: SessionUsage;
}

/** A way to group the rows of a usage report. */
export type UsageGroup = "provider" | "model" | "project" | "source" | "speed" | (string & {});

/** The period a usage report covers. */
export type UsageRange = "session" | "today" | "week" | "month" | "all" | (string & {});

/** Usage and cost over a range: the result of `usage/report`. */
export interface UsageReport {
  /** The start of the range. Absent for all time. */
  from?: Timestamp | null;
  /** How the rows are grouped, outermost first. */
  group_by: Array<UsageGroup>;
  /** The latest limits each provider reported. */
  limits?: Array<ProviderLimits>;
  /** The range covered. */
  range: UsageRange;
  /** Whether usage history is being recorded. When statistics are off, only the session range has figures. */
  recording: boolean;
  /** The rows. */
  rows: Array<UsageRow>;
  /** The sources included: `cortex-harness`, and other tools the user turned on. */
  sources?: Array<string>;
  /** The end of the range. */
  to?: Timestamp | null;
  /** All rows together. */
  totals: UsageTotals;
}

/** `usage/report`. It reads local data only. */
export interface UsageReportParams {
  /** How to group the rows, outermost first. Empty: by provider, then model. */
  group_by?: Array<UsageGroup>;
  /** Only this project. */
  project?: ProjectKey | null;
  /** The period. */
  range: UsageRange;
  /** For the `session` range: the session. */
  session_id?: SessionId | null;
}

/** One row of a usage report. The fields that are set are the ones the report is grouped by. */
export interface UsageRow {
  /** The model. */
  model?: ModelId | null;
  /** The project's key. */
  project?: ProjectKey | null;
  /** The project's name or shortened path, for display. */
  project_label?: string | null;
  /** The provider. */
  provider?: ProviderId | null;
  /** The tool that recorded the usage: `cortex-harness`, or another tool's id. */
  source?: string | null;
  /** The speed the requests ran at. */
  speed?: Speed | null;
  /** What was used. */
  totals: UsageTotals;
}

/** Usage added up over some requests: one row of a breakdown. */
export interface UsageTotals {
  /** The cost, one amount per basis. Empty when no price is known. */
  costs?: Array<Cost>;
  /** How many requests. */
  requests?: number;
  /** Tokens by class. */
  tokens?: TokenUsage;
}

/** The session's usage changed. */
export interface UsageUpdated {
  /** The session's usage now. */
  usage: SessionUsage;
}

/** A message from the user. */
export interface UserMessageItem {
  /** When it was recorded. */
  created_at?: Timestamp | null;
  /** How it was delivered, when it was sent during a running turn. */
  delivery?: Delivery | null;
  /** The item's id. */
  id: ItemId;
  /** What the user sent: text and images. */
  parts: Array<Part>;
}

/** What the project's checks said at the end of a turn. */
export type Verification = "passed" | "failed" | "skipped" | (string & {});
