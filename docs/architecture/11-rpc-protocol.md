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

- `cortex-protocol` defines the types once, in Rust, with serde.
- `cortex-rpc` maps each JSON-RPC method to an `Op` and each `Event` to a notification. It adds no behaviour of its own.
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

Errors use the JSON-RPC error object. `code` follows the JSON-RPC ranges for protocol errors. For application errors, `data` carries `{ "kind", "code", "message" }` with `kind` from the error table in [02-runtime.md](02-runtime.md), so a client can branch on a stable string.

## Handshake and versioning

The first request on a connection must be `initialize`.

```json
→ {"jsonrpc":"2.0","id":1,"method":"initialize","params":{
     "protocol_version":1,
     "client":{"name":"example-editor","version":"0.3.0"},
     "role":"user",
     "capabilities":{"approvals":true,"questions":true,"deltas":true}}}
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

**Versioning rules.

- `protocol_version` is one integer. It changes only for a breaking change. A server may support several versions and picks the highest both sides know. If there is none in common, `initialize` fails with the versions the server supports.
- Everything else evolves by addition, described by **capabilities**. A client states what it can handle (answering approvals, receiving deltas). A server states what it offers.
- Receivers ignore unknown fields, unknown notification methods and unknown event types. Senders never rely on the other side understanding an addition unless a capability says it does.
- Enumerations that may grow (event types, stop reasons, error kinds) are documented as open. Clients must have a default branch.
- Methods under an `x/` prefix are experimental and may change without a version bump. A client opts in per connection.
- A deprecated method or field keeps working for at least two minor releases and is listed in the changelog.

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
| `session/new` | Create a session: workspace, model, permission mode, overrides. |
| `session/open` | Open a stored session by id. Returns its metadata and current state. |
| `session/list` | List stored sessions for a workspace. |
| `session/read` | Read items of a session, paged, from the log. Also used to resynchronise. |
| `session/fork`, `session/rewind` | Branching, as in [07-sessions-and-context.md](07-sessions-and-context.md). |
| `session/close` | Stop using a session on this connection. |
| `turn/start` | Send a prompt. If a turn is running, `delivery` must be `"steer"` or `"queue"`. Returns at once with the turn id and a disposition: `started`, `steering` or `queued`. |
| `turn/interrupt` | Cancel the running turn. Idempotent. |
| `session/compact` | Compact now, with optional focus instructions. |
| `session/set` | Change settings for the session: model, reasoning level, permission mode. |
| `plan/get`, `plan/update` | Read the plan, and apply a user's edit to it (reorder, skip, reword, add, remove). |
| `usage/get` | Token, cost and cache figures for the session. |
| `project/list`, `project/update` | Read the project profile and memory, and accept, edit, disable or delete entries. |
| `verify/run` | Run the project's checks now. |
| `models/list`, `config/get`, `config/reload` | Read the catalog and the effective configuration. Reload configuration. |
| `permissions/list`, `permissions/revoke` | Inspect and remove stored approvals. |
| `mcp/status`, `extensions/status` | State of MCP servers and extensions. |
| `shutdown` | Stop the server cleanly. |

`turn/start` returning means "accepted". Completion is the `turn.completed` event. The response never carries the model's answer.

After the first release, the orchestration layer adds methods under `run/*` ([16-run-engine.md](16-run-engine.md)) and the Coordinator adds methods under `team/*` ([17-coordinator.md](17-coordinator.md)): the team's status, the escalation queue and answers to it, the decision log, and attaching to an Employee. A run and an Employee are ordinary sessions, so the methods and events on this page are how they are driven:

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
| `approval/request` | Ask whether a tool call may run. Params: the tool, the plan summary, the full detail (command, diff or path), and the choices on offer. Result: the choice and an optional note. | Deny. |
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
| `usage.updated` | Token and cost totals, cache hit rate, and rate-limit state. |
| `plan.updated` | The plan changed, by the agent or by a user. |
| `project.updated` | Project profile or memory entries were proposed or changed. |
| `notice` | Retry scheduled, compaction started or finished, limit reached, sandbox report, instruction files changed. |
| `request.resolved` | A server request was answered by another client or timed out. |
| `turn.completed` | The turn is over and nothing automatic remains. Carries the outcome, stop reason, usage and error. |
| `session.idle` | The session has no running turn and no queued prompt. With `turn.completed`, this is what a supervisor waits on. |
| `session.updated` | Session metadata changed: title, model, permission mode, queue length. |

**Item kinds:** `user_message`, `assistant_message` (parts: text, reasoning, tool calls), `tool_result` (model-visible content, structured payload such as a diff or exit status, artifact references), `notice`, `summary`.

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
- `xtask schema` generates JSON Schema for every method, event and item into `docs/protocol/schema/`, and TypeScript declarations into `docs/protocol/ts/`. Both are checked in.
- A CI test regenerates them and fails if the checked-in files differ, so a protocol change always shows up in review as a schema diff.
- A second test replays recorded message transcripts from earlier versions against the current types, to catch accidental breaking changes.
- A TypeScript client needs only the generated declarations and a line reader. No Rust code is involved, which is what makes the "TypeScript front end later" option real.

## What is deliberately left out

- **No HTTP REST surface.** One bidirectional message protocol covers requests, events and server requests. A REST façade can be layered on later if a client needs it.
- **No remote file or terminal access methods.** A client that needs files reads them itself, or asks the agent.
- **No UI-control methods** (driving the TUI from outside). The TUI is one client among others.
- **Agent Client Protocol.** Editors that speak that protocol can be served by a thin adapter that translates it to this one. It is a separate step after the first release, because that protocol's own API has been changing quickly.
