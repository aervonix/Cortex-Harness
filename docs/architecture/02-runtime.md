# Runtime model: concurrency, errors, cancellation, logging

## Async and concurrency

**Runtime.** Tokio, current-thread flavour, running on one dedicated core thread. The choice and the numbers behind it are in [decision 0003](../decisions/0003-async-runtime.md).

- The agent's work is I/O-bound: one or a few HTTP streams, a handful of child processes, timers. One thread handles it with no lock contention and no work stealing.
- All session state is owned by tasks on the core thread, so state needs no `Mutex`. Shared handles that cross threads (the event channel, the log queue, cancellation tokens) are the only synchronised objects.
- Tasks are spawned with `tokio::spawn` and keep `Send` bounds, so moving to a multi-thread runtime later is a one-line change.

**Blocking work never runs on the core thread.** File I/O, directory walks, regex search, diffing, hashing and syntax highlighting go to the bounded blocking pool (default 8 threads) through one helper that takes a cancellation token. Clippy's disallowed-methods list bans the blocking standard-library calls in the async crates, and a debug-build watchdog logs a warning when the core thread goes more than 20 ms between polls.

**The UI thread is separate.** The TUI runs on the main thread with its own event loop (see [09-tui.md](09-tui.md)). It talks to the core only through two channels:

| Channel | Direction | Kind | Backpressure |
|---|---|---|---|
| Ops | front end → core | bounded MPSC (64) | The front end waits. Ops are small and rare. |
| Events | core → front end | bounded MPSC (1024) with delta coalescing | See below. |

**Event backpressure.** A slow consumer must not make the core hold unbounded memory or stall a model stream.

- Text, reasoning and tool-output deltas for the same item are merged in the channel when the consumer is behind. The consumer then sees fewer, larger deltas.
- If the channel is still full, the core drops deltas for that item and queues one `item.resync` event in their place. The consumer then asks for the item's current content. Completed-item events are never dropped.
- The in-process TUI drains the channel every frame and in practice never falls behind. The rule exists for RPC clients.

**As built in step 13a** (`cortex-core`, `events`). The "channel" is one queue for each connected client, which the session's sink gives a copy of every event.

- **Merging.** A delta is added to the delta at the end of the queue when both are of the same kind for the same item; anything queued between two deltas keeps them apart, so order is never changed. The merged delta carries the later `seq`.
- **Dropping.** A delta that finds the queue full is dropped, and so is every later delta of that item until the item completes. One `item.resync` is queued in place of the first, with its `seq`. (It was first built to wait for room. A resync that waits is queued behind whole events that came after it, and the client then reads `seq` going backwards, so it is queued at once: it is one small event an item, as an item's other whole events are.)
- **What a client reads is in the session's order.** Every event keeps the `seq` the session gave it, a merged delta has its last part's, and nothing is queued out of order. A jump in `seq` is a delta the client was not sent: one it did not ask for, one merged into its neighbour, or one dropped, and only the last is a loss, which `item.resync` says.
- **Whole events are queued even when the queue is full.** They are few and each is bounded (a tool result is bounded when it is made), so the queue's size is a limit on deltas, not a hard cap. Closing a connection that cannot take them in time is the RPC server's rule (step 27).
- **A client that did not declare `deltas` is sent none.**
- **What an item in progress holds** is kept by the same sink, from the same deltas, and is what `session/read` returns for an item that has not completed.
- **The ops channel** is not in the core: `Core::handle` is called directly, by whatever front end owns the core thread. The bounded channel of 64 is the front end's, between its own thread and the core's (step 13b for headless, step 23 for the TUI).

**Timers and idleness.** When a session is idle the process has no periodic timers. Spinners and clocks tick only while a turn is running. The idle CPU budget in [../performance-budgets.md](../performance-budgets.md) depends on this.

## Error handling

**Typed errors in libraries.** Each library crate defines its own error enum with `thiserror`. No `anyhow` and no boxed string errors in library crates. `cortex-cli` formats the final error for people.

**Every error has a kind.** `cortex-base` defines `ErrorKind`, a closed set that drives retry decisions, exit codes and RPC error codes:

| Kind | Meaning | Retried automatically | Headless exit code |
|---|---|---|---|
| `Usage` | Bad flags or arguments | no | 2 |
| `Config` | Invalid configuration | no | 2 |
| `Auth` | Missing or rejected credentials | no | 3 |
| `Provider` | The provider refused the request for a reason that will not change on retry (invalid request, not found, billing) | no | 4 |
| `Transient` | Network failure, timeout, overload, retryable rate limit | yes | 4 after retries are exhausted |
| `ModelStop` | The model stopped in a way that is not success (refusal, content filter, output limit) | no | 5 |
| `Permission` | A required approval was denied or could not be asked | no | 6 |
| `Limit` | A configured budget was reached (requests, cost, time) | no | 7 |
| `ContextOverflow` | The context cannot be made to fit | after one compaction | 8 |
| `Tool` | A tool failed | no (reported to the model) | n/a |
| `Interrupted` | Cancelled by the user or a signal | no | 130 |
| `Internal` | A bug | no | 1 |

**Failures are data.** A provider failure ends the assistant item with a stop reason of `error` and the error attached. A tool failure is a tool result with `is_error`. Neither unwinds through the loop. This keeps partial output, makes failures visible to every front end in the same way, and makes recovery a normal code path.

**Error classification is structural.** Retry decisions use the transport error type, the HTTP status, the provider's machine-readable error code and response headers. Matching on message text is allowed only as a last resort for a named provider quirk, with a test fixture. Pi and Opencode both classify retries by pattern-matching error text, and both carry long, growing pattern lists (see [../reference-notes.md](../reference-notes.md)).

**Panics.** A panic is a bug, but it must not wreck the user's terminal or their session.

- A process-wide panic hook restores the terminal (leave raw mode, leave the alternate screen, show the cursor), writes a crash report with a backtrace to the log directory, and prints a one-line pointer to it.
- Tool executions, extension calls and provider stream tasks run as separate tasks. A panic in one is caught at the join point and becomes an `Internal` error result for that item. The session continues.
- A panic anywhere else in a turn ends that turn with an `Internal` error, and the session takes the next prompt ([03-agent-loop.md](03-agent-loop.md), "Interruption"). Built in step 7.
- The session log is flushed at item boundaries, so a crash loses at most the item in progress. Resume repairs the tail (see [07-sessions-and-context.md](07-sessions-and-context.md)).

## Cancellation

One mechanism: a tree of cancellation tokens.

```
process ─► session ─► turn ─┬─► model request
                            ├─► tool call A
                            └─► tool call B ─► child process group
```

Cancelling a node cancels everything below it. Every await in the loop that can take time is raced against its token.

**What the user sees.**

| Situation | First interrupt (Ctrl-C or Esc in the TUI) | Second interrupt |
|---|---|---|
| Model is streaming | The request is dropped. Text received so far is kept and marked interrupted. | n/a |
| Tools are running | Every running tool is cancelled. Each started call still gets a result, marked interrupted, with its partial output. | Within 2 s: escalate to a hard kill of child processes immediately. |
| An approval is pending | The approval is answered "deny" and the turn stops. | n/a |
| Idle, editor has text | The editor is cleared. | n/a |
| Idle, editor empty | A hint says to press again to quit. | Within 1 s: quit. |

**Mid-stream.** Dropping the response body closes the connection, which is the only cancellation signal every provider honours. The partial assistant message is recorded with stop reason `interrupted`. Partial tool calls in it are discarded, because their arguments are incomplete, and they are never run.

**Mid-tool.** Tools receive the token and must stop promptly.

- File tools write atomically (temporary file, then rename), so cancellation never leaves a half-written file.
- The shell tool interrupts the whole process tree in stages: a polite interrupt, then termination after a grace period (default 2 s), then a hard kill. Details per OS are in [05-tools.md](05-tools.md).
- A cancelled call records a result (`interrupted`, with output so far). Providers require every tool call to have a result, so the history stays valid and the conversation can continue.

**The second interrupt, "kill now"** (built in step 13a). A tool call is given two tokens: the one that says stop, and a second, `RunCx::kill`, that says stop now. The process runner takes the second as "skip the graces": a command that is being stopped in stages is killed at once, and one that had not been asked yet is killed without being asked. The loop's handle has `interrupt_now`, which fires both, and the protocol carries it as `turn/interrupt` with `now: true` ([11-rpc-protocol.md](11-rpc-protocol.md)). The ordinary interrupt stays safe to send twice: a client that retries one never kills. Counting "a second Ctrl-C within two seconds" is the front end's, which then sends the second kind.

**Signals.** In headless and RPC modes, SIGINT and SIGTERM (Ctrl-C and Ctrl-Break events on Windows) cancel the session token, let the final event and the session log flush, and exit with 130 or 143. A second signal exits immediately.

**Cancellation safety.** Code that must not be interrupted halfway (appending a log entry, renaming a file into place, recording a tool result) runs to completion before the token is observed. These sections are short and never wait on the network.

## Logging and tracing

**Facade.** The `tracing` crate, with a small subscriber of our own. Its measured cost is in [../measurements/dependency-costs.md](../measurements/dependency-costs.md).

**Spans.** `session`, `turn`, `request` (one provider HTTP request, with provider request id and attempt number), `tool` (one tool call), `ext` (one extension call). Span timings are the source for the latency numbers in [../performance-budgets.md](../performance-budgets.md): time from prompt to request sent, to first byte, to first event, to first frame.

**Sinks.**

| Sink | Default | Content |
|---|---|---|
| Log file | on at `info` | JSON lines in the state directory, rotated by size (5 files of 5 MB). Written by the log-writer thread. |
| stderr | off in the TUI, `warn` in headless and RPC modes | Human-readable. Never stdout, which belongs to output and to the protocol. |
| Debug bundle | on request (`cortex debug bundle`) | Recent log files plus a redacted configuration dump. |

**Levels.** `error`, `warn`, `info`, `debug`, `trace`. Set with `--log-level`, the `CORTEX_LOG` environment variable, or configuration, optionally per target prefix (`CORTEX_LOG=info,cortex_llm=debug`). The filter is a small prefix match, not a regex engine. `trace` is compiled out of release builds.

**As built in step 13a** (`cortex_base::log`). The subscriber is `cortex-base`'s, because it has no other owner and every crate can reach it.

- **The filter** is a level and levels for target prefixes, the longest prefix winning, read from the text `--log-level`, `CORTEX_LOG` and `log.level` take (`info,cortex_llm=debug`). It can be replaced while the process runs.
- **The file** is `cortex.log` in the state directory's `logs/`, one JSON object a line (`ts`, `level`, `target`, `msg`, `fields`, and `spans`, outermost first, each with its fields). A thread of its own writes it, from a bounded queue: a line that finds the queue full is dropped and counted, and the count is written when there is room. It rotates at 5 MB to `cortex.log.1` … `cortex.log.4`. Every file is created owner-only.
- **The stderr sink** is off unless it is given a level, which headless and RPC modes do (step 13b turns it on at `warn`). It takes its own level, whatever the file's filter says, and writes one readable line an event.
- **Span timings.** For every span that closes: when it opened and closed on the monotonic clock, how long it was entered, its parent and its fields. The last 1,024 are kept for the performance probes, and each close is also a log line at the span's level. A session's loop runs in a `session` span since step 13a; the `turn`, `request` and `tool` spans are the loop's to open, and are not opened yet ([../outstanding/spans-in-the-agent-loop.md](../outstanding/spans-in-the-agent-loop.md)).
- **Everything passes the secret filter** ([../security.md](../security.md), section 4) before it reaches a sink, and a value longer than 8 KiB is cut. A provider key the core finds after logging has started is added to the filter (`LogHandle::add_secret`, which the core's `key_found` option calls).
- **Not built:** the bridge for dependencies that log through the `log` crate ([../outstanding/log-crate-bridge.md](../outstanding/log-crate-bridge.md)).

**What is never logged.**

- Credentials. They are `Secret` values, whose `Debug` and `Display` print `[redacted]`, and which do not implement `Serialize`.
- `Authorization`, `x-api-key` and similar headers. The request logger has an allow-list of headers it prints.
- Prompt and file content at `info` and below. Bodies are logged only at `debug`, truncated, and after the redaction filter in [../security.md](../security.md).

**No telemetry.** The process makes no network request that the user did not cause. There is no usage reporting and no automatic update check, which the owner confirmed on 2026-11-12. `cortex update check` asks on request.
