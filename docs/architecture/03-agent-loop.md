# The agent loop

The loop lives in `cortex-agent`. It owns no I/O: everything it touches it reaches through a trait, so every behaviour on this page is tested with a scripted provider and fake tools, with no network and no real clock.

| Trait | What the loop asks of it | In `cortex-agent` until the real one exists |
|---|---|---|
| `ModelClient` (`cortex-llm`) | One request in, a stream of events out. | The scripted provider of the test kit, in tests. |
| `ToolSet` | The definitions to declare, and the tool a call names. | Implemented for `Toolbox` of `cortex-tools`. |
| `Policy` | Allow, deny or ask for a planned call, and put the question. | `AllowAll`. The engine is `cortex-policy` (step 12). |
| `SessionStore` | Read the session once, append records, end a turn. | `MemorySessionStore`. The log is `cortex-session` (step 8), adapted in step 13. |
| `Context` | What a request contains, whether to compact first, and the compaction. | `StubContext`: everything, never compacted. Step 15 builds the real one. |
| `Verifier` | One round of the project's checks. | `NoVerifier`: skipped. Step 19 builds the real one. |
| `EventSink` | Takes the session's events, without blocking. | The core's channel (step 13). |
| `Stats` | Takes the statistics events. | `NoStats`. `cortex-stats` (step 16). |

They are held as trait objects (`Arc<dyn ModelClient>`, `Box<dyn SessionStore>`), not as type parameters. The first draft of this page said "generic over". Objects give the same testability with no type-parameter list on every item and less to compile, and a call through one costs nothing next to a model request. As built in step 7.

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
  └──────────────────── Interrupted ◄───────────────┴── (from every state that waits)
```

The picture leaves out three kinds of arrow that the table has: `Compacting` is also entered from `Requesting`, `Streaming` and `Deciding`, when the provider says a request did not fit; `Streaming` goes back to `Requesting` when a speed tier refuses part-way and the request is sent again at standard speed; and `Preparing`, `Compacting`, `Requesting`, `Streaming` and `ToolRound` each go to `Finishing` when the turn cannot go on.

| State | What happens | Leaves when |
|---|---|---|
| `Idle` | Nothing. No timers. | A prompt arrives. |
| `Preparing` | Take steering messages from the queue. Assemble the request. Estimate tokens. | Ready → `Requesting`. Over budget → `Compacting`. Request or cost limit reached → `Finishing` with a `Limit` notice. |
| `Compacting` | Summarise and start a new context epoch (see [07-sessions-and-context.md](07-sessions-and-context.md)). | Done → `Preparing`. |
| `Requesting` | Send the request. Wait for the response to start. | Response starts → `Streaming`. Retryable failure → `Backoff`. The provider says the request does not fit → `Compacting`, once. Other failure → `Finishing`. |
| `Streaming` | Decode events, emit deltas, build the assistant message. Plan each tool call as its block ends, and start the ones with no side effects. | Stream ends → `Deciding`. Retryable failure → `Backoff`. A speed tier refuses part-way and the request is sent again at standard speed → `Requesting`. The request does not fit → `Compacting`, once. Failure for good → `Finishing`, with what arrived kept on the item and the error attached. |
| `Backoff` | The client waits before the next attempt. The retry notice is emitted on the way in. | The next event of the stream → `Requesting`: whatever it is, the wait is over and the request was sent again. (If that event is the final error, the turn then goes from `Requesting` to `Finishing`.) |
| `Deciding` | Record the assistant item. Look at the stop reason. | Tool calls → `ToolRound`. No tool calls and files changed in this turn → `Verifying`. A stop reason that says the request did not fit → `Compacting`, once. Otherwise → `Finishing`, or `Preparing` if a continuation is required. |
| `ToolRound` | Plan, approve, run and record every call. Calls that started early are joined here. | All results recorded → `Preparing`. The repetition guard stops the turn → `Finishing`. |
| `Verifying` | Run the project's checks ([15-efficiency.md](15-efficiency.md)). After the first release, a check that speculative verification already ran against exactly this state is used instead of being run again. | Passed, skipped or round limit reached → `Finishing`. Failed → append the failures as a notice → `Preparing`. |
| `Finishing` | Flush the log. Emit `turn.completed`. | The turn completed and a prompt is waiting → `Preparing` as a new turn. Otherwise → `Idle`. After a turn that failed or hit a limit, prompts that were waiting are held ("Steering and queued prompts"). |
| `Interrupted` | Cancel children, record what exists, emit `turn.completed` with outcome `interrupted`. | Always → `Idle`. Queued prompts are held and the front end is told. |

**The table of transitions is code.** Every change of state goes through one function, which checks it against `TRANSITIONS` (`crates/cortex-agent/src/state.rs`) in debug builds. Each transition has a scripted scenario (`crates/cortex-agent/tests/it/transitions.rs`), and one test fails when the scenarios and the table disagree in either direction: a transition with no scenario, or a scenario that makes a transition the table does not have.

**What a request has already been through travels with it,** from state to state, as data: that the context was compacted for it, that the provider said it did not fit and a compaction answered that, that it was sent again with a higher output limit. Each is allowed once, and a new request starts clean. None of it is a flag on the session.

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
- A call that changes the session itself (`plan`, `remember`) has no such effect and still waits for the response to end: its plan says that it changes the session ([05-tools.md](05-tools.md), "The tool trait").
- A call that needs approval puts its question to the user immediately.
- A call with side effects that is already allowed starts when the response ends.

The split exists because a response can fail halfway and be retried. A read that ran for nothing costs nothing. A side effect that ran for a message that no longer exists would be wrong. The reasoning, and the later option to start side-effecting calls early too, are in [15-efficiency.md](15-efficiency.md).

**A call starts when its block ends, not before.** [15-efficiency.md](15-efficiency.md) once said a `read` could start as soon as its `path` argument was complete. That would mean parsing arguments twice and running a call whose other arguments are not known yet, against the rule above. As built in step 7, the block's end is the earliest moment.

## Tool rounds

A round has four phases. The split exists so that policy and scheduling work from declared effects before anything runs. Planning and approval begin during streaming, as described above; by the time the response ends, read-only calls may already be finished.

**1. Plan.** For each call, in the model's order, the tool's `plan` function validates arguments and returns an invocation plan with no side effects: the paths it will read or write, the command it will run, whether it needs the network, and its concurrency class.

**2. Approve.** The permission engine decides each plan: allow, deny, or ask (see [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). Questions go to the front end one at a time, in call order. Calls that are already allowed start at once while a question for a later call is open. A denied call gets an error result that carries the user's reason if they gave one.

**3. Run.** The scheduler runs the allowed calls.

| Concurrency class | Examples | Rule |
|---|---|---|
| Read-only | read, search, list | Run in parallel, up to the limit (default 8, and it counts every running call of the round, whatever its class). A read-only call still waits its turn behind a keyed call it conflicts with (below). |
| Mutating, keyed | write and edit, keyed by canonical path | Calls on the same path run one at a time in call order, and so do calls where one path is inside the other. Calls on unrelated paths run in parallel. |
| Exclusive | shell (default), tools from extensions unless they declare otherwise | Runs alone: it waits for running calls to finish, and later calls wait for it. |

**A read conflicts with a write to what it reads.** A keyed call conflicts with a read-only call that reads one of its paths, lists or walks a directory that holds one, or reads something inside one. `edit a.rs` followed by `read a.rs` in one response therefore sees the edit, and `write src/new.rs` followed by `list src` sees the file. Two read-only calls never conflict. The rule is `Plan::conflicts_with` in `cortex-tools` ([05-tools.md](05-tools.md), "The tool trait"), which works from each plan's effects and paths; it was settled in step 6. A tool from an extension or an MCP server whose spec says exclusive is exclusive whatever its plan says.

Conflicting calls always run in the model's order. Calls that do not conflict run together. The class is part of each tool's contract ([05-tools.md](05-tools.md)), and a tool may refine it per call (for example a shell command that the policy has proven read-only).

As built, one rule decides what may start: a call waits for every earlier call it conflicts with, and for every earlier call whose plan is not known yet, which could turn out to conflict. Planning, a question and a tool run are each a task that reports to the loop, so the decision is made in one place. A task that panics reports an error result in its place, and that holds for a question too: the call gets the error and the next question is put.

- **"The model's order" is the order of the blocks' indexes,** not the order the blocks happened to end in. While the response is arriving, nothing after a tool call that is still being written starts or is asked about.
- **A call that is not scheduled as read-only is taken to have side effects,** whatever effects its plan declares, and so waits for the response to end. A tool from outside whose author declared nothing is exclusive ([05-tools.md](05-tools.md)), and must not start early because its list of effects is empty.
- **So is a call whose plan says that it changes the session** (`Plan::changes_session`, since step 10). `plan` and `remember` declare no effect and are scheduled as read-only, because they touch nothing outside the session. Started early, they would replace the plan or propose a memory for a response that might then fail and be thrown away. Two such calls also conflict with each other, so they run one at a time in the model's order: of two `plan` calls in one response the second stands. Beside every other call they run as read-only calls do.
- **A call that repeats the id of another call of the response is dropped,** from the message and from the round. A result names its call by id, so two calls with one id cannot both be answered, and every provider rejects them.

**4. Record.** Results are appended to the log, and sent to the model, in the model's call order whatever order they finished in. Front ends see each result as it completes.

Every started call ends with exactly one result: success, error, denied or interrupted. This is checked by a debug assertion and by tests, because every provider rejects a history where a call has no result. "Started" means its block ended: a call that could not be planned, was denied, named no tool or never got its turn before an interruption has a result like any other, made by the loop and passed through the same output policy.

**Hooks.** Extensions can observe or gate a call before it runs and can annotate its result afterwards ([12-extensions.md](12-extensions.md)). Hooks run in the Approve phase and after Run. They have deadlines, and they cannot reorder results.

## Steering and queued prompts

The user can type while a turn runs. There are two delivery modes, chosen by the front end per message.

| Mode | When the message is delivered | Use |
|---|---|---|
| `steer` | At the next safe boundary of the running turn: after the current tool round's results are recorded, before the next request. If the model is streaming a final answer with no tool calls, the message becomes a new turn instead. | Correct the agent's course without stopping it. |
| `queue` | As a new turn, after the current turn completes. One at a time. | Line up the next task. |

**Edits to the plan** made by the user while the agent works ([05-tools.md](05-tools.md)) are delivered the same way as a steering message: as a notice at the next safe boundary. A notice never starts a turn. One that arrives while the session is idle waits, and is delivered ahead of the next prompt.

The text of the notice is written by the plan itself (`PlanBoard::apply` in `cortex-tools`, step 10): what the user changed, item by item, and the plan as it now is. A `plan` call the model wrote before the notice reached it replaces nothing: the user's edit wins, and the call's result gives the model the current plan ([05-tools.md](05-tools.md), "`plan`"). The result is recorded with its round, and the notice after it, at the boundary.

A safe boundary is the only place a user message can be appended without breaking a tool round. It is the `Preparing` state. An RPC `Op::Prompt` sent while a turn is running must say which mode it wants; the loop rejects it otherwise (`turn_running`) instead of guessing. A prompt is answered at once with the turn it belongs to: the one it started, the running one it steers, or the one it will start, whose id is given when it is queued.

**Only a turn that completed starts the next prompt by itself.** The owner decided this on 2026-11-13, in step 7; the first draft of this page said two things at once.

| The turn ended | What happens to the prompts that were waiting |
|---|---|
| `completed` | The next one starts as a new turn: a steering message that found no boundary first, then the queue, one at a time. When none is left the session is idle. |
| `interrupted` | Steering messages that were never delivered are handed back to the front end, for its editor, in the answer to the interrupt. Queued prompts are **held**, and so is a steering message that arrives after the interrupt, while the turn is still ending. |
| `failed` or `limit` | Steering messages that were never delivered, and the queue, are **held**, in that order. |

- **Held prompts are kept, and nothing runs by itself.** An interrupt means stop. And queued prompts are usually steps that assume the one before them worked ("then commit"): after an authentication error or a spent budget each would fail in turn.
- **The front end is told.** `session.updated` carries the number of prompts waiting, and `session.idle` is not sent: by its definition the session has a queued prompt.
- **The next prompt the user sends runs first,** at once, and the held prompts follow it in order.
- **An interrupt that arrives as a turn is finishing** does not change how that turn ended: it had completed. It still means stop, so what was waiting is held all the same. Closing a session does the same.
- **A steering message is answered with the id of the running turn.** If it finds no boundary and becomes a turn of its own, that turn has a new id, which `turn.started` carries.

## Retries

Retries are a separate layer between the loop and the provider's protocol and transport, with its own tests against a fake clock. As built in step 5 it is inside the model client (`ProviderClient` in `cortex-llm`): the client makes the attempts and does the waiting, and the loop sees each failed attempt as an event in the response stream, `Retry` with the error, the wait and the time of the next attempt. The loop's `Backoff` state is the time between that event and the next response's start. The rules themselves are one function with no clock and no I/O, tested row by row.

- **What is retried.** Errors of kind `Transient`: connection failures, timeouts, HTTP 408, 409, 429 (when the provider's error code says it is a rate limit and not a spending limit), 5xx, provider overload codes, and mid-stream failures. The per-protocol classification table is in [04-providers.md](04-providers.md).
- **Schedule.** Exponential backoff with full jitter. Base 1 s, factor 2, cap 30 s, at most 5 attempts per request. All four are configurable.
- **Server hints win.** A `retry-after` header (seconds or date) or a provider reset header sets the wait. If the hint is longer than `retry.max_wait` (default 2 minutes), the loop does not sleep: it finishes the turn with the error and the hint, so a person or a script can decide.
- **Mid-stream failure.** Only side-effect-free calls start before a response completes, so a failed stream has had no side effects. The partial response is discarded from the context, kept in the log as an `attempt` entry for debugging, and the same request is sent again. Results of read-only calls that already ran are kept in a short-lived cache and reused if the new response asks for the same call. Front ends receive `item.reset` so they can clear the partial text.
- **The cache of early results, as built in step 7.**
  - It lives for one request: from its first attempt until its response ends or the turn does. A call is the same call when the tool and the arguments are the same; the new response gives it a new id, and the result is recorded under that one.
  - **A kept result is used when the call's turn to start comes, not when its block ends.** It is held back by the same rules as a new run. If the new response puts a write to the file before the read, the kept read is discarded and the read runs again after the write.
  - A run that is still going when its response fails is kept too, and the new response joins it.
  - Front ends get `item.reset` for the tool result of a call that is thrown away, and see it begin again if it is reused.
  - **A result that is not asked for again is discarded, and forgotten.** The model never saw it. But `read` has already noted in the table of seen files which lines it showed, and a later read would be answered "unchanged since you read it". So the loop makes the table forget every file a discarded call read, before anything else that touches the same file may run. The price is small and on the safe side: the next read of such a file is in full.
  - An open question for a call of the failed response is withdrawn. An answer already given is not kept: the call is asked about again if it comes back.
- **What a failed attempt used is counted.** If the provider reported usage before the attempt failed, it is added to the session's usage and charged to the ledger: it was used.
- **Visibility.** Each wait emits a `retry` notice with the attempt number, the reason and the time of the next attempt.
- **Idle timeout.** If no bytes arrive on a stream for 90 s (pings count as bytes), the request fails as `Transient`. A separate limit applies to the wait for the first event: 60 s from sending the request, which is right for a hosted provider. Both are in the retry settings and both can be set per provider. A local server can send nothing until it has read a long prompt, which can take longer than that on modest hardware; the step that adds OpenAI-compatible servers revisits the default for them.
- **A stream that ends early is a failure too.** A response whose stream ends before the provider's terminal event, or cannot be decoded, is `Transient` and is sent again.

## Rate limits

- Rate-limit headers from each response are parsed into a per-provider state (remaining requests and tokens, reset times) and exposed in `usage.updated`, so a front end can show it.
- The loop does not pace itself ahead of limits. It reacts to 429 and overload responses through the retry layer. Pacing can be added later behind the same state without changing the loop.
- Non-retryable 429 responses exist on every provider (spending caps, exhausted credit). They are classified as `Provider`, not `Transient`, and end the turn with a clear message.
- **A speed tier can have a limit of its own.** When a request at a speed other than standard is refused for that tier's limit, the retry layer may wait, or send the request at standard speed and put the tier in a cooldown. The response stream says which (`SpeedFallback`, `SpeedCooldown`), and the loop turns that into a `speed_fallback` notice for the request and `speed_cooldown` notices when the cooldown starts and ends. The session state shows the speed in effect, which the loop reads from the same table of cooldowns the client obeys. The rules, and why the harness does not switch back and forth, are in [04-providers.md](04-providers.md), "Speed tiers".
- **What the loop sends, as built in step 7.** Each request asks for the speed its role is **set to**: the role's own, and without one the session's for `main` and `standard` for a utility role. The client sends `standard` in its place while the tier cools down or when the model has no such tier. The request still says what was asked for, so the usage record carries both speeds, and the client can notice the end of a cooldown. A change of the setting applies from the next request.
- **A fallback is shown when it is news.** The stream carries a `SpeedFallback` for every request sent at `standard` during a cooldown, and one for every request a provider downgrades. Shown each time, they would be one notice per request for ten minutes. So the loop remembers the last fallback it showed for each role and shows one only when it differs: the first of a cooldown (the refusal that started it and the requests sent at `standard` after it are one piece of news), the first for a tier the model does not offer, the first of a run of requests the provider served at another speed. It is news again after a request of that role ran at the speed that was asked for. Nothing is hidden by this: every request's usage record says the speed it ran at and the speed that was asked for.
- **`speed_cooldown` is always shown,** when a cooldown starts and when it ends.
- **The session state is told when it changes.** After each of these events, and before each request, the loop works out what every role's next request will ask for, and sends `session.updated` when that differs from what front ends were last told. Reading the state never waits for that: it reads the table of cooldowns when it is asked.

## Stop reasons

The loop works with a neutral union. Each protocol adapter maps its own values onto it and keeps the raw value for the log.

| Stop reason | Loop behaviour |
|---|---|
| `end` | Finish the turn. |
| `tool_calls` | Run a tool round. |
| `max_output` | If the last block is an unfinished tool call, resend once with a higher output limit when the model allows it: the cut-off response becomes an `attempt` entry, and the request is sent with the model's own maximum in place of the configured limit. Otherwise mark the item truncated and drop the unfinished call. If the response has tool calls that are complete, they run and the turn goes on. If it has none, the turn ends (`model_stop`), and the user can continue. |
| `context_overflow` | Compact once and retry. If it still does not fit, finish with `ContextOverflow`. |
| `paused` | The provider paused a server-side loop. Append the response unchanged and send the next request. |
| `refusal` | Finish with `ModelStop`. Show the category if the provider gave one. Never retried on the same model automatically. |
| `content_filter` | Finish with `ModelStop`. |
| `malformed_tool_call` | Treat as a failed tool call: record an error result that describes the problem and continue. When the response has no call a result could answer, the model is told in a notice. Counts towards the loop guard below. |
| `error` | Handled by the retry layer or finishes the turn. |
| `interrupted` | Set by the loop, never by a provider. |

A stop reason this build does not know is treated as `end`. A response that ends the turn as a failure (`refusal`, `content_filter`, `error`, and `context_overflow` the second time) keeps its text and loses its tool calls: none of them runs, so none may stay in a message that needs a result for each.

## Loop guards and limits

- **Request limit per turn.** Default 200. Reaching it finishes the turn with a `Limit` notice. Headless mode exposes it as a flag.
- **Cost and token limits.** Optional per turn and per session, from configuration or flags. Checked in `Preparing`. A limit is an account in a **ledger**, and several sessions can draw on one account, which is how a group of supervised sessions shares one hard cap ([16-run-engine.md](16-run-engine.md)).
- **Repetition guard.** If the model issues the same call with the same arguments and gets the same result three times in a row, the loop appends a notice telling the model the call is repeating without effect. After five, it stops the turn and asks the user. The thresholds are configurable and the guard can be turned off.
- **Wall-clock limit.** Optional, for headless runs.

As built in step 7 (`crates/cortex-agent/src/ledger.rs`, `guard.rs`):

- **An account is a budget and what has been spent of it:** money, tokens, requests and wall-clock time, the four fields of a lease's budget ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). A session's ledger is the accounts it draws on. The first is its lease's. Another may be shared: every clone of an account is the same account, so sessions given one spend from one budget. Each turn also has an account of its own, which holds the limits one turn may not pass.
- **Every request is charged before it is sent.** In `Preparing` each account is asked for one request. An account that is spent refuses, nothing is charged to any, and the turn ends with the outcome `limit`. What the request then uses, in tokens and money, is charged when its response ends. A cap on tokens or money can therefore be passed by the one request that was in flight when it was reached, and by no more.
- **Money counts every cost recorded,** whatever its basis. A request whose model has no known price costs tokens only.
- **The wall-clock limit is checked at the same place,** before a request, on the monotonic clock. A request or a tool that is running is not cut short by it: tools have their own time limits.
- **A limit that is reached** is a `limit` notice for front ends, and a line in the conversation that says why the turn stopped, so the model knows when the user carries on.
- **The repetition guard compares whole rounds:** the calls, their arguments and what came back. For a round of one call that is the rule above. Comparing rounds also catches a model that alternates between two calls. A response whose tool call could not be read counts as a round. When the guard stops a turn its outcome is `limit`, with the error code `repetition`.

## Interruption

See [02-runtime.md](02-runtime.md) for the mechanism. From the loop's point of view, cancellation is an input like any other: the state machine moves to `Interrupted`, records what exists, and returns to `Idle`. After an interruption the history is always valid for the next request, which tests check by sending one.

What "records what exists" means, as built in step 7:

| Interrupted | What is recorded |
|---|---|
| Before the response starts | The prompt. No assistant item. |
| While the response is arriving | The assistant item with what arrived, marked `interrupted`. A tool call whose arguments were complete stays in it and gets a result: its own, if it had started and finished; what it returns when it is cancelled, if it was running; "interrupted", if it never ran. A call that was still being written is left out. An item that had begun and has nothing in it is recorded empty, and is not sent to a model. |
| During a tool round | One result for every call, as above. The call whose question was open is denied. |
| Anywhere else | Nothing more: the log already holds whole items. |

**When the log cannot be written.** A failed append ends the turn ([07-sessions-and-context.md](07-sessions-and-context.md)). The loop then reads the session again from its store at the start of the next turn, so that memory holds what the log holds and a store that repairs on open closes a round that was half recorded. If that read fails, the turn fails and the next one tries again: no request goes out on a history that may have a hole.

**When the loop itself has a bug.** A panic in a tool call, a plan or a question is an error result for that call ([02-runtime.md](02-runtime.md)). A panic anywhere else in a turn (in the context, the verifier, the store, the loop's own code) ends the turn: front ends get `turn.completed` with the outcome `failed` and the code `loop_panicked`, what was waiting is held, and the session is read again from its store before the next turn.

## Several sessions at once

One `Core` runs any number of sessions, each with its own workspace root, state machine and cancellation tree, all on the core thread. The RPC server uses this for several clients. The run engine uses it for parallel attempts ([16-run-engine.md](16-run-engine.md)). Sessions share only read-only data (the catalog, configuration) and the bounded blocking pool.

As built in step 7, a session is two things. `Session` is the loop: one future, for one task, which owns everything it works on. `SessionHandle` is how everything else reaches it: send a prompt, interrupt, give a notice, change a speed, read the state. The two share one small piece of state under a lock (which turn is running, what is waiting to be delivered, the settings a request reads), and whether a turn is running is decided under that lock, so two prompts can never both find the session idle. Sessions share nothing they were not given to share: the model clients, the table of speed-tier cooldowns, and any account of the ledger.

## What is recorded for adaptation

At the end of each request, tool call and turn, the loop emits a statistics event ([14-adaptation.md](14-adaptation.md)): the model and variants in effect, the outcome, durations and usage. Recording is a queue push and never blocks the loop. The events go to a `Stats` trait in `cortex-agent`; `cortex-stats` (step 16) records them, and the core connects the two.

## What the loop does not do

- It does not know which provider it is talking to, how tools are implemented, or where the log is stored.
- It does not render anything or read a terminal.
- It does not edit history. Compaction and model changes start a new context epoch; they never rewrite the current one.
