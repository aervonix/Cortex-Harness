# RPC protocol

The RPC protocol lets another program drive Cortex-Harness: an editor extension, a GUI, a web UI, or another agent. It is complete by construction, because the built-in TUI uses the same operations and events in-process. Anything the TUI does, an external client can do.

The requirement from the TUI language decision applies here: a TypeScript client must be buildable on this protocol later with no change to the core.

## One contract, two carriers

```
            cortex-protocol types (Op, Event, Item, …)
                 │                          │
   in-process: typed channels        out of process: JSON-RPC 2.0
   (TUI, headless printer)           (cortex-rpc over stdio or a local socket)
```

- `cortex-protocol` defines the types once, in Rust, with serde. It is types and their JSON form: no I/O and no behaviour. In a normal build it depends on serde and serde_json and nothing else, which `cargo xtask rules` checks.
- `cortex-rpc` maps each JSON-RPC method to an `Op` and each `Event` to a notification. It adds no behaviour of its own. The mapping between a method name and its parameter and result types is part of `cortex-protocol` (`Op::from_wire`, `OpResult::from_wire` and their counterparts for the server's requests), so a client written in Rust and the server use the same table.
- A conformance test runs the same scripted scenarios through both carriers and requires identical event sequences.

## Transports

| Transport | Start | Framing | Clients | Authentication |
|---|---|---|---|---|
| Standard I/O | `cortex serve --stdio` | One JSON object per line, UTF-8, `\n` terminated. A trailing `\r` is tolerated on input. | One. The parent process. | None needed: the parent already controls the process. |
| Local socket | `cortex serve --socket [PATH]` | The same. | Several at once. | Unix: socket file with owner-only permissions in the runtime directory, and a peer-credential check that the caller is the same user. Windows: a named pipe whose access-control list admits only the current user. |
| WebSocket (after the first release) | `cortex serve --listen 127.0.0.1:PORT` | One JSON object per text frame. | Browsers and remote tools. | See "Network listening" below. |

In `--stdio` mode standard output carries only protocol messages. Logs go to standard error and the log file. Any code path that could print to standard output in this mode is a bug, and a test guards it.

Newline-delimited JSON was chosen over length-prefixed framing because every language can produce and consume it with a line reader, and because it is the same framing MCP uses for stdio, so the extension host, the MCP client and this server share one reader and writer.

## Messages

JSON-RPC 2.0. Three kinds of message, in both directions.

| Kind | Has `id` | Example |
|---|---|---|
| Request | yes | client → server `turn/start`; server → client `approval/request` |
| Response | the request's `id` | `{"jsonrpc":"2.0","id":7,"result":{…}}` or `{"jsonrpc":"2.0","id":7,"error":{…}}` |
| Notification | no | server → client `session/event` |

Errors use the JSON-RPC error object.

| JSON-RPC `code` | When |
|---|---|
| -32700, -32600 | The line was not JSON, or not a JSON-RPC message. |
| -32601 | The method does not exist. |
| -32602 | The parameters do not fit the method. The message names the field. |
| -32603 | The server failed in a way that says nothing about the request. |
| -32000 | **Every application error.** A client never branches on this number. |

For an application error, `data` is an error object of our own: `{ "kind", "code", "message" }`, and where they apply `request_id` (the provider's id for the failed request), `retry_at` (when the provider said to try again) and `details` (structured detail whose shape depends on `code`).

- `kind` is from the error table in [02-runtime.md](02-runtime.md): `usage`, `config`, `auth`, `provider`, `transient`, `model_stop`, `permission`, `limit`, `context_overflow`, `tool`, `interrupted`, `internal`. It says what sort of failure this is and what a client should offer.
- `code` is a stable name for the exact failure, in `snake_case`: one the protocol defines (`not_initialized`, `already_initialized`, `unsupported_protocol_version`, `role_not_allowed`, `unknown_session`, `session_not_open`, `turn_running`, `supervisor_present`, `unknown_value`, `sandbox_required`), a provider's own code, or one a later version adds.

The same error object appears in `turn.completed` and on an assistant item whose request failed, so a failure looks the same whichever way it reaches a client.

## Conventions

These hold for every message, and the generated schema follows them.

| Matter | Rule |
|---|---|
| Names | Field names and enumeration values are `snake_case`. The one exception is the level `read-only`, spelled as the command line spells it. |
| Tagged unions | Events are told apart by `type`, with dotted names (`item.delta`). Every other union (items, parts, deltas, notices, plan edits, approval details, stop conditions) is told apart by `kind`. Two names, so that an event can carry a `kind` of its own beside its `type`. |
| Optional fields | A field with nothing in it is left out. A receiver reads `null` as absent. |
| Empty results | A method with nothing to return answers `{}`, not `null`, so that fields can be added. |
| Times | RFC 3339 strings in UTC: `2026-11-12T10:15:02Z`. |
| Durations | Whole milliseconds, in fields that end in `_ms`. |
| Money | Whole nano-dollars (1e-9 US dollars) in fields named `usd_nanos`, so that sums are exact. A cost is an amount with its basis (`metered`, `estimate`, `recorded`), and amounts with different bases are never added. Prices are nano-dollars per million tokens. |
| Identifiers | Opaque strings. A client compares and returns them and reads nothing into their text. |
| Provider JSON | Kept as a string that holds the JSON text (`raw`), never as a nested value, so that no parser reorders or re-escapes it ([04-providers.md](04-providers.md)). |
| Paths | A workspace is an absolute path on the server's machine. Paths inside a workspace are relative to its root. |

## Handshake and versioning

The first request on a connection must be `initialize`.

```json
→ {"jsonrpc":"2.0","id":1,"method":"initialize","params":{
     "protocol_version":1,
     "client":{"name":"example-editor","version":"0.3.0"},
     "role":"user",
     "capabilities":{"approvals":true,"questions":true,"trust":true,"deltas":true}}}
← {"jsonrpc":"2.0","id":1,"result":{
     "protocol_version":1,
     "server":{"name":"cortex-harness","version":"0.1.0"},
     "capabilities":{"sessions":true,"mcp":true,"extensions":true,"sandbox":"enforcing"}}}
```

**Client roles.** A client states its role in `initialize`.

| Role | Who | Receives approval and question requests |
|---|---|---|
| `user` | A person's front end: the TUI, an editor, a GUI. | Yes. A user's answer is final. |
| `supervisor` | Another session's orchestration layer, driving this session as a run or an Employee ([16-run-engine.md](16-run-engine.md), [17-coordinator.md](17-coordinator.md)). | First, when no user has answered. It may answer only within its own grant. Anything else it must pass up. |
| `observer` | Something that only watches. | No. |

A session has at most one supervisor. A user client can attach to a supervised session at any time, see everything, send prompts, and answer pending requests; the supervisor is told when a user intervenes.

**Capabilities.** A capability that is absent is `false`: the server never sends what a client did not ask for.

| Client capability | Meaning |
|---|---|
| `approvals` | It answers `approval/request`. |
| `questions` | It answers `question/ask`. |
| `trust` | It answers `trust/request`. |
| `deltas` | It wants `item.delta` events. |
| `experimental` | A list of the experimental features it opts in to, by name. |

The server answers with what it offers: `sessions`, `mcp`, `extensions`, the state of its `sandbox` (`enforcing` or `off`), and the experimental features it turned on for the connection.

**Versioning rules.**

- `protocol_version` is one integer. It is 1. It changes only for a breaking change. A client sends the newest version it speaks, and `min_protocol_version` if it also speaks older ones. The server picks the newest version both sides speak. If there is none, `initialize` fails with the code `unsupported_protocol_version`, and `details.supported` lists the server's versions.
- Everything else evolves by addition, described by **capabilities**. A client states what it can handle (answering approvals, receiving deltas). A server states what it offers.
- **Receivers ignore unknown fields**, in every object, and unknown notification methods. Senders never rely on the other side understanding an addition unless a capability says it does.
- **Every enumeration is open**, not only the ones expected to grow. A string enumeration (a stop reason, an error kind, a level, a speed) may arrive with a value this version does not know. A tagged union (an event, an item, a part, a notice) may arrive with a `type` or `kind` this version does not know. Clients must have a default branch.
- **An unknown value is kept, not dropped.** The Rust types hold it as `Unknown` and write it back exactly as it arrived, so a client that stores or relays messages loses nothing. The generated TypeScript types say the same thing.
- **A known variant that is malformed is still an error.** A `text` part with no `text` is refused with a message that names the part and the field. It is never mistaken for an unknown variant.
- **Open does not mean accepted.** A server that receives a level, a role or a delivery mode it does not know refuses the request, with the code `unknown_value`. The types keep the value so that the refusal can name it.
- **Every notice has a `message`** in words, so a client can show a kind of notice it has never seen.
- Methods under an `x/` prefix are experimental and may change without a version bump. A client opts in to each by name, in `capabilities.experimental`.
- A deprecated method or field keeps working for at least two minor releases and is listed in the changelog.

**What is a breaking change**, and so needs version 2: removing or renaming a field, a method, an event type or an enumeration value; changing a field's type or meaning; making an optional field required; making the server send, by default, something a version 1 client would mishandle. **What is not:** a new method, a new optional field, a new event type, item kind, part kind or notice kind, a new enumeration value, a new capability, a new error code.

## Object model

| Object | Identity | Lifetime |
|---|---|---|
| Session | `session_id` | Stored. Opened by a connection. Several connections may open the same session. |
| Turn | `turn_id` | From `turn/start` until `turn.completed`. |
| Item | `item_id` | Created inside a turn. Immutable once completed. |
| Approval | the JSON-RPC request id | From the request until it is answered or times out. |

## Client → server methods

The first-release set. Each maps to one `Op`.

| Method | Purpose |
|---|---|
| `initialize` | Handshake. |
| `session/new` | Create a session: workspace, settings (models per role, reasoning level, speed, level), title, configuration overrides. |
| `session/open` | Open a stored session by id. Returns its metadata and current state. |
| `session/list` | List stored sessions for a workspace. |
| `session/read` | Read items of a session, paged, from the log, or one item by id. Also used to resynchronise: an item in progress comes back with the content it has so far, and the result says which items are incomplete. |
| `session/fork`, `session/rewind` | Branching, as in [07-sessions-and-context.md](07-sessions-and-context.md). |
| `session/close` | Stop using a session on this connection. |
| `turn/start` | Send a prompt. If a turn is running, `delivery` must be `"steer"` or `"queue"`. Returns at once with the turn id and a disposition: `started`, `steering` or `queued`. |
| `turn/interrupt` | Cancel the running turn. Idempotent. The result hands back steering prompts that had not been delivered. |
| `session/compact` | Compact now, with optional focus instructions. |
| `session/set` | Change settings for the session: the model and reasoning level of a role, the speed tier (for the session, and per role), the level (permission mode). Anything left out stays as it is. Raising the level needs the `user` role. |
| `plan/get`, `plan/update` | Read the plan, and apply a user's edit to it (reorder, skip, reword, add, remove). |
| `usage/get` | Token, cost and cache figures for the session, by provider, model, role and speed. |
| `usage/report` | Usage and cost for a range (session, today, week, month, all time), grouped by provider, model, project, source or speed, with limits left. Reads local data only ([18-usage.md](18-usage.md)). |
| `project/list`, `project/update` | Read the project profile and memory, and accept, edit, disable, delete or promote entries. |
| `verify/run` | Run the project's checks now. |
| `models/list`, `config/get`, `config/reload` | Read the catalog and the effective configuration. Reload configuration. A model's entry includes its reasoning control and the speed tiers it offers, each with its own prices. |
| `permissions/list`, `permissions/revoke` | Inspect and remove stored approvals. |
| `mcp/status`, `extensions/status` | State of MCP servers and extensions. |
| `shutdown` | Stop the server cleanly. |

`turn/start` returning means "accepted". Completion is the `turn.completed` event. The response never carries the model's answer.

**Settings and state.** A session's settings are a level, a speed, and per model role (`main`, `fast`, `judge`) a model, a reasoning level and optionally a speed of its own. `session/new` and `session/set` take the same settings object, in which everything is optional. The session state that `session/open`, `session/set` and `session.updated` carry says, per role, what is set and what is in effect: the reasoning level as set and the model's own level it maps to; the speed as set, the speed the next request will ask for, and the end of a cooldown when one is running ([04-providers.md](04-providers.md), "Speed tiers").

After the first release, task leases add `lease/grant`, `lease/get`, `lease/extend` and `lease/revoke`, which only a `user` client may call, and the usage readers add `usage/sources`. The orchestration layer adds methods under `run/*` ([16-run-engine.md](16-run-engine.md)) and the Coordinator adds methods under `team/*` ([17-coordinator.md](17-coordinator.md)): the team's status, the escalation queue and answers to it, the decision log, and attaching to an Employee. A run and an Employee are ordinary sessions, so the methods and events on this page are how they are driven:

| The Coordinator needs | Provided by |
|---|---|
| Sending prompts and follow-ups to an Employee | `turn/start` with `delivery` |
| Streaming status and knowing when an Employee is idle | `session/event`, `turn.completed`, `session.idle` |
| An Employee's approval requests reaching its Manager | `approval/request` routed to the `supervisor` client |
| The user attaching to an Employee directly | A second client with role `user` on the same session |

## Server → client requests

These pause the turn until answered. A client that did not declare the matching capability is never sent them; the server then applies the non-interactive default.

| Method | Purpose | If unanswered within the timeout |
|---|---|---|
| `approval/request` | Ask whether a tool call may run. Params: the tool, the plan summary, the full detail (a command with its directory, a diff, a path or host, or a tool's arguments), and the choices on offer (`allow_once`, `allow_session`, `allow_project`, `deny`). Result: the choice and an optional note, which with `deny` reaches the model as the reason. | Deny. |
| `question/ask` | A question for the user from the agent or an extension, with optional choices. | The tool that asked receives "no answer". |
| `trust/request` | Ask whether to trust a project's executable resources, listing them. | Not trusted. |

When several clients are attached to a session, a request goes to all that declared the capability. The first answer wins, and the others receive `request.resolved`.

## Events

All events arrive as one notification method.

```json
{"jsonrpc":"2.0","method":"session/event","params":{
   "session_id":"s_01JCX…","seq":412,"turn_id":"t_19","type":"item.delta",
   "item_id":"i_88","delta":{"kind":"text","text":"The build fails because"}}}
```

`seq` increases by one per event per session. It is how a client detects a gap.

| Type | Meaning |
|---|---|
| `turn.started` | A turn began. |
| `item.started` | An item began: its kind and, for tool calls, the tool name. |
| `item.delta` | Streaming content for an item: text, reasoning, tool arguments, tool output. Sent only to clients that declared `deltas`. |
| `item.reset` | The partial content of an item was discarded (a retried request). |
| `item.completed` | The final item, in full. Authoritative. |
| `item.resync` | Deltas were dropped for a slow client. Fetch the item with `session/read`. |
| `usage.updated` | Token and cost totals, by provider, model, role and speed; the last request with the speed it ran at; cache hit rate; and rate-limit state, including a speed tier's own limit. |
| `plan.updated` | The plan changed, by the agent or by a user. |
| `project.updated` | Project profile or memory entries were proposed or changed. |
| `notice` | Something transient to show. Kinds: `retry`, `compaction`, `limit`, `sandbox`, `instructions`, `speed_fallback` (a request ran at a different speed from the one asked for), `speed_cooldown` (a speed tier's cooldown began or ended), `info`. Each has a `message` in words. |
| `request.resolved` | A server request was answered by another client or timed out. |
| `turn.completed` | The turn is over and nothing automatic remains. Carries the outcome, stop reason, usage and error. |
| `session.idle` | The session has no running turn and no queued prompt. With `turn.completed`, this is what a supervisor waits on. |
| `session.updated` | Session metadata changed: title, models, reasoning level, speed (set and in effect), level, queue length. |

**Item kinds:** `user_message`, `assistant_message` (parts: text, reasoning, tool calls, provider blocks; stop reason; the request's usage), `tool_result` (model-visible content, how the call ended, a structured payload such as a diff or exit status, artifact references), `notice`, `summary`.

**Two things are called a notice.** A `notice` *item* is a message the harness added to the conversation: the model sees it, and the log keeps it. A `notice` *event* tells front ends about something transient and is never sent to the model. A retry is an event. "The user edited the plan" is an item.

**Deltas** have a `kind`: `text`, `reasoning`, `tool_call` (the model began a call), `tool_args` (a fragment of the call's JSON arguments, not yet valid JSON) and `tool_output` (output from a running tool).

## Reconnecting and several clients

- Events are not stored separately. A client that connects late, or notices a gap in `seq`, calls `session/read` to load items from the log, then continues with live events. `session/open` returns the current `seq` and whether a turn is running, so there is no race between reading and subscribing.
- Deltas for an item in progress are not replayed. A late client sees the item when it completes, or reads its partial content with `session/read`.
- Several clients may watch and drive one session. Operations are applied in the order the core receives them. A second `turn/start` without `delivery` while a turn runs is an error.
- **Persistent sessions.** A server started with `--socket` keeps its sessions running when a client disconnects. Another terminal (`cortex attach`), an editor, or later a web or phone client over the network transport can pick the session up. This is not a headline feature, and the protocol needs nothing more to allow it.

## Backpressure

Each connection has a bounded outbound queue.

1. When it grows, deltas for the same item are merged.
2. If it is still full, deltas for that item are dropped and one `item.resync` is queued.
3. Completed items, turn events and server requests are never dropped. If they cannot be queued within a time limit, the connection is closed with an error. The session and the turn carry on.

A slow or stuck client therefore never stalls a model stream or grows memory without bound.

## Network listening

Off by default, and not in the first release. When added:

- Binds to the loopback address unless `--allow-remote` is given.
- Requires a bearer token on every connection. A random token is generated per launch, written to a file with owner-only permissions, and printed once to standard error. There is no unauthenticated mode.
- Checks the `Origin` header against an allow-list, so a web page in the user's browser cannot connect to a local server.
- Offers no TLS of its own. Remote use goes through an SSH tunnel or a TLS-terminating proxy.
- `--allow-remote` without a token is refused.

The threats are in [../security.md](../security.md).

## Schema and generated types

- The Rust types in `cortex-protocol` are the source of truth.
- `cargo xtask schema` generates two files, both checked in ([../protocol/README.md](../protocol/README.md)):
  - `docs/protocol/schema/cortex-protocol.schema.json`: one JSON Schema document with every type under `$defs`, a root that accepts any JSON-RPC message, and a table (`x-cortex-methods`) that says, for each method and notification, which types its parameters and result are.
  - `docs/protocol/ts/cortex-protocol.d.ts`: TypeScript declarations for every type, with `ClientMethods`, `ServerMethods` and `ServerNotifications` maps from method name to types. It is translated from the schema, so the two cannot disagree.
- **A stale file fails the build twice over:** `cargo xtask check` has a `generated` step, and a test regenerates the files and compares, on both OSes. A protocol change therefore always shows up in review as a schema diff. A pull request that touches these files is never treated as documentation-only.
- **The golden transcript** (`crates/cortex-protocol/tests/golden/v1-session.jsonl`) is a session written by hand in protocol version 1: every method, every event type, and every kind of item, part, delta, notice and plan edit. Tests read each line into its typed form and write it back, and require the same JSON. A second test adds a field no version knows to every object and requires the same result. A third validates every line against the generated schema, which is what shows that the schema describes the wire form the types really have. The transcript is the record of version 1: a change that breaks it is a mistake or a new version, and the file is not edited to make a test pass. Later versions add transcripts beside it.
- A TypeScript client needs only the generated declarations and a line reader. No Rust code is involved, which is what makes the "TypeScript front end later" option real.

## What is deliberately left out

- **No HTTP REST surface.** One bidirectional message protocol covers requests, events and server requests. A REST façade can be layered on later if a client needs it.
- **No remote file or terminal access methods.** A client that needs files reads them itself, or asks the agent.
- **No UI-control methods** (driving the TUI from outside). The TUI is one client among others.
- **Agent Client Protocol.** Editors that speak that protocol can be served by a thin adapter that translates it to this one. It is a separate step after the first release, because that protocol's own API has been changing quickly.
