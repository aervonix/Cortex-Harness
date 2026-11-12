# The agent loop

The loop lives in `cortex-agent`. It is generic over three traits (`ModelClient`, `ToolSet`, `SessionStore`), so every behaviour on this page is tested with a scripted provider and fake tools, with no network and no real clock.

## Vocabulary

- A **turn** starts with a user prompt and ends when the session is idle again. It can contain many model requests.
- A **request** is one call to the model and its streamed response.
- A **tool round** is the set of tool calls from one response, and their results.
- An **item** is what front ends display and what the log stores: a user message, an assistant message, a tool call with its result, or a notice.

## Turn state machine

The loop is an explicit state machine. There are no boolean flags such as "is retrying" or "is compacting": the state says it.

```
            ┌──────────────────────────────────────────────────────────────────┐
            ▼                                                                  │
 Idle ─► Preparing ─► Requesting ─► Streaming ─► Deciding ─┬─► ToolRound ──────┤
  ▲          │             │            │           │      ├─► Verifying ──────┘ (on failure)
  │          │             └── Backoff ◄┘           │      └─► Finishing ─► Idle
  │          └─► Compacting ─► Preparing            │
  └──────────────────── Interrupted ◄───────────────┴── (from any state)
```

| State | What happens | Leaves when |
|---|---|---|
| `Idle` | Nothing. No timers. | A prompt arrives, or a queued prompt exists. |
| `Preparing` | Take steering messages from the queue. Assemble the request. Estimate tokens. | Ready → `Requesting`. Over budget → `Compacting`. Request or cost limit reached → `Finishing` with a `Limit` notice. |
| `Compacting` | Summarise and start a new context epoch (see [07-sessions-and-context.md](07-sessions-and-context.md)). | Done → `Preparing`. |
| `Requesting` | Send the request. Wait for the response to start. | First event → `Streaming`. Retryable failure → `Backoff`. Other failure → `Finishing`. |
| `Streaming` | Decode events, emit deltas, build the assistant message. | Stream ends → `Deciding`. Retryable failure → `Backoff`. |
| `Backoff` | Sleep until the next attempt. Emit retry notices. | Timer → `Requesting`. Attempts exhausted → `Finishing` with the error. |
| `Deciding` | Record the assistant item. Look at the stop reason. | Tool calls → `ToolRound`. No tool calls and files changed in this turn → `Verifying`. Otherwise → `Finishing`, or `Preparing` if a continuation is required. |
| `ToolRound` | Plan, approve, run and record every call. Calls that started early are joined here. | All results recorded → `Preparing`. |
| `Verifying` | Run the project's checks ([15-efficiency.md](15-efficiency.md)). After the first release, a check that speculative verification already ran against exactly this state is used instead of being run again. | Passed, skipped or round limit reached → `Finishing`. Failed → append the failures as a notice → `Preparing`. |
| `Finishing` | Flush the log. Emit `turn.completed`. | Queued prompt → `Preparing` as a new turn. Otherwise → `Idle`. |
| `Interrupted` | Cancel children, record what exists, emit `turn.completed` with outcome `interrupted`. | Always → `Idle`. Queued prompts stay queued and the front end is told. |

**A completed turn means settled.** `turn.completed` is emitted only when no automatic work remains for that prompt: no retry pending, no compaction running, no tool running. Clients wait on that one event. Pi needed a separate "settled" event because its run-end event could be followed by retries and queued work (see [../reference-notes.md](../reference-notes.md)); this design avoids the second event by making the turn the unit that includes them.

## Streaming

The provider adapter yields neutral model events (see [04-providers.md](04-providers.md)). For each, the loop does three things: update the assistant message being built, emit an `Event` for front ends, and nothing else. Persistence happens once, when the item completes.

| Model event | Loop action | Event to front ends |
|---|---|---|
| Response start | Create the assistant item. | `item.started` |
| Text delta | Append. | `item.delta` (text) |
| Reasoning delta | Append to the reasoning part. | `item.delta` (reasoning) |
| Tool call start | Create a tool-call part. | `item.delta` (tool call started, with name) |
| Tool call arguments delta | Append the JSON fragment. Do not parse yet. | `item.delta` (arguments, coalesced) |
| Block end | Attach the provider's raw block. For a tool call, parse and validate the arguments. | none |
| Usage | Update running usage. | `usage.updated`, throttled to once per frame |
| Response end | Finalise. Append the item to the log. | `item.completed` with the full message |

`item.completed` carries the authoritative content. Deltas are a display convenience and may be coalesced or dropped under backpressure.

**Tool arguments are untrusted partial JSON until the block ends.** They are parsed once, at block end, and validated against the tool's schema. Invalid arguments never reach a tool: the call gets an error result that tells the model what was wrong, and the round continues.

**Calls start as soon as their arguments are complete.** When a tool call's block ends, the loop plans it at once, while the model is still streaming the rest of the response.

- A call whose plan has no side effects (no write, exec or network effect) starts running immediately.
- A call that needs approval puts its question to the user immediately.
- A call with side effects that is already allowed starts when the response ends.

The split exists because a response can fail halfway and be retried. A read that ran for nothing costs nothing. A side effect that ran for a message that no longer exists would be wrong. The reasoning, and the later option to start side-effecting calls early too, are in [15-efficiency.md](15-efficiency.md).

## Tool rounds

A round has four phases. The split exists so that policy and scheduling work from declared effects before anything runs. Planning and approval begin during streaming, as described above; by the time the response ends, read-only calls may already be finished.

**1. Plan.** For each call, in the model's order, the tool's `plan` function validates arguments and returns an invocation plan with no side effects: the paths it will read or write, the command it will run, whether it needs the network, and its concurrency class.

**2. Approve.** The permission engine decides each plan: allow, deny, or ask (see [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). Questions go to the front end one at a time, in call order. Calls that are already allowed start at once while a question for a later call is open. A denied call gets an error result that carries the user's reason if they gave one.

**3. Run.** The scheduler runs the allowed calls.

| Concurrency class | Examples | Rule |
|---|---|---|
| Read-only | read, search, list | Run in parallel, up to the limit (default 8). |
| Mutating, keyed | write and edit, keyed by canonical path | Calls on the same path run one at a time in call order. Calls on different paths run in parallel. |
| Exclusive | shell (default), tools from extensions unless they declare otherwise | Runs alone: it waits for running calls to finish, and later calls wait for it. |

Conflicting calls always run in the model's order. Calls that do not conflict run together. The class is part of each tool's contract ([05-tools.md](05-tools.md)), and a tool may refine it per call (for example a shell command that the policy has proven read-only).

**4. Record.** Results are appended to the log, and sent to the model, in the model's call order whatever order they finished in. Front ends see each result as it completes.

Every started call ends with exactly one result: success, error, denied or interrupted. This is checked by a debug assertion and by tests, because every provider rejects a history where a call has no result.

**Hooks.** Extensions can observe or gate a call before it runs and can annotate its result afterwards ([12-extensions.md](12-extensions.md)). Hooks run in the Approve phase and after Run. They have deadlines, and they cannot reorder results.

## Steering and queued prompts

The user can type while a turn runs. There are two delivery modes, chosen by the front end per message.

| Mode | When the message is delivered | Use |
|---|---|---|
| `steer` | At the next safe boundary of the running turn: after the current tool round's results are recorded, before the next request. If the model is streaming a final answer with no tool calls, the message becomes a new turn instead. | Correct the agent's course without stopping it. |
| `queue` | As a new turn, after the current turn completes. One at a time. | Line up the next task. |

**Edits to the plan** made by the user while the agent works ([05-tools.md](05-tools.md)) are delivered the same way as a steering message: as a notice at the next safe boundary.

A safe boundary is the only place a user message can be appended without breaking a tool round. An RPC `Op::Prompt` sent while a turn is running must say which mode it wants; the core rejects it otherwise instead of guessing. Interrupting a turn leaves queued prompts in the queue and returns unsent steering text to the front end.

## Retries

Retries are a separate layer between the loop and the provider client, with its own unit tests against a fake clock.

- **What is retried.** Errors of kind `Transient`: connection failures, timeouts, HTTP 408, 409, 429 (when the provider's error code says it is a rate limit and not a spending limit), 5xx, provider overload codes, and mid-stream failures. The per-protocol classification table is in [04-providers.md](04-providers.md).
- **Schedule.** Exponential backoff with full jitter. Base 1 s, factor 2, cap 30 s, at most 5 attempts per request. All four are configurable.
- **Server hints win.** A `retry-after` header (seconds or date) or a provider reset header sets the wait. If the hint is longer than `retry.max_wait` (default 2 minutes), the loop does not sleep: it finishes the turn with the error and the hint, so a person or a script can decide.
- **Mid-stream failure.** Only side-effect-free calls start before a response completes, so a failed stream has had no side effects. The partial response is discarded from the context, kept in the log as an `attempt` entry for debugging, and the same request is sent again. Results of read-only calls that already ran are kept in a short-lived cache and reused if the new response asks for the same call. Front ends receive `item.reset` so they can clear the partial text.
- **Visibility.** Each wait emits a `retry` notice with the attempt number, the reason and the time of the next attempt.
- **Idle timeout.** If no bytes arrive on a stream for 90 s (pings count as bytes), the request fails as `Transient`. A separate limit applies to the wait for the first event. Both are configurable per provider.

## Rate limits

- Rate-limit headers from each response are parsed into a per-provider state (remaining requests and tokens, reset times) and exposed in `usage.updated`, so a front end can show it.
- The loop does not pace itself ahead of limits. It reacts to 429 and overload responses through the retry layer. Pacing can be added later behind the same state without changing the loop.
- Non-retryable 429 responses exist on every provider (spending caps, exhausted credit). They are classified as `Provider`, not `Transient`, and end the turn with a clear message.
- **A speed tier can have a limit of its own.** When a request at a speed other than standard is refused for that tier's limit, the retry layer may wait, or send the request at standard speed and put the tier in a cooldown. The loop emits a `speed_fallback` notice for the request and `speed_cooldown` notices when the cooldown starts and ends, and the session state shows the speed in effect. The rules, and why the harness does not switch back and forth, are in [04-providers.md](04-providers.md), "Speed tiers".

## Stop reasons

The loop works with a neutral union. Each protocol adapter maps its own values onto it and keeps the raw value for the log.

| Stop reason | Loop behaviour |
|---|---|
| `end` | Finish the turn. |
| `tool_calls` | Run a tool round. |
| `max_output` | If the last block is an unfinished tool call, resend once with a higher output limit when the model allows it. Otherwise finish, mark the item truncated, and let the user continue. |
| `context_overflow` | Compact once and retry. If it still does not fit, finish with `ContextOverflow`. |
| `paused` | The provider paused a server-side loop. Append the response unchanged and send the next request. |
| `refusal` | Finish with `ModelStop`. Show the category if the provider gave one. Never retried on the same model automatically. |
| `content_filter` | Finish with `ModelStop`. |
| `malformed_tool_call` | Treat as a failed tool call: record an error result that describes the problem and continue. Counts towards the loop guard below. |
| `error` | Handled by the retry layer or finishes the turn. |
| `interrupted` | Set by the loop, never by a provider. |

## Loop guards and limits

- **Request limit per turn.** Default 200. Reaching it finishes the turn with a `Limit` notice. Headless mode exposes it as a flag.
- **Cost and token limits.** Optional per turn and per session, from configuration or flags. Checked in `Preparing`. A limit is an account in a **ledger**, and several sessions can draw on one account, which is how a group of supervised sessions shares one hard cap ([16-run-engine.md](16-run-engine.md)).
- **Repetition guard.** If the model issues the same call with the same arguments and gets the same result three times in a row, the loop appends a notice telling the model the call is repeating without effect. After five, it stops the turn and asks the user. The thresholds are configurable and the guard can be turned off.
- **Wall-clock limit.** Optional, for headless runs.

## Interruption

See [02-runtime.md](02-runtime.md) for the mechanism. From the loop's point of view, cancellation is an input like any other: the state machine moves to `Interrupted`, records what exists, and returns to `Idle`. After an interruption the history is always valid for the next request, which tests check by sending one.

## Several sessions at once

One `Core` runs any number of sessions, each with its own workspace root, state machine and cancellation tree, all on the core thread. The RPC server uses this for several clients. The run engine uses it for parallel attempts ([16-run-engine.md](16-run-engine.md)). Sessions share only read-only data (the catalog, configuration) and the bounded blocking pool.

## What is recorded for adaptation

At the end of each request, tool call and turn, the loop emits a statistics event ([14-adaptation.md](14-adaptation.md)): the model and variants in effect, the outcome, durations and usage. Recording is a queue push and never blocks the loop.

## What the loop does not do

- It does not know which provider it is talking to, how tools are implemented, or where the log is stored.
- It does not render anything or read a terminal.
- It does not edit history. Compaction and model changes start a new context epoch; they never rewrite the current one.
