# Architecture overview

Cortex-Harness is a terminal agent harness written in Rust. It runs an LLM agent loop that reads and edits code, runs commands, and talks to several model providers. This page gives the shape of the whole system. The numbered pages after it specify each part.

## What it is for

The [product vision](../vision.md) in one sentence: an agent harness that learns your project, your models and your habits, and stays fast while it does. Its three pillars shape this architecture.

| Pillar | What the architecture provides |
|---|---|
| **It adapts** | Local statistics, a project profile and memory, per-model variants, model roles, profiles, reloading without a restart, and extensions the harness can write for the user. [14-adaptation.md](14-adaptation.md), [12-extensions.md](12-extensions.md). |
| **It is lean without being bare** | An append-only, cache-friendly context, a repository map, changed-only re-reads, compressed output, tools that start early and run in parallel, background compaction, automatic verification, and budgets for start-up, memory and rendering that CI checks. [15-efficiency.md](15-efficiency.md), [../performance-budgets.md](../performance-budgets.md). |
| **It is ahead** | One orchestration layer that runs several agent sessions and judges the results: parallel attempts, replay and evaluation, and the Coordinator, where a Manager session runs a team. [16-run-engine.md](16-run-engine.md), [17-coordinator.md](17-coordinator.md). |

## Engineering goals, in priority order

1. **Speed and efficiency.** Fast start, low memory, low latency added to the model's own latency, smooth rendering, small binary. The same priority applies to the development loop: build times, test times and CI.
2. **Maintainability over years.** Every line is ours. Boundaries are crates with narrow, typed contracts. Behaviour that matters is covered by deterministic tests.
3. **Four interfaces over one core.** Interactive TUI, headless mode, an RPC server, and an extension system all drive the same core through the same protocol.
4. **Multi-provider.** The provider abstraction is designed for Anthropic, OpenAI-compatible APIs and Google. Anthropic is implemented first.
5. **Linux and native Windows are equal targets.** macOS is not a target now, and nothing in the design rules it out.
6. **Usable by anyone.** Sensible defaults, a guided first run, and reading the set-up people already have from other agents.

Where a product feature pulls against goal 1, the conflict is stated in the open questions of [../build-plan.md](../build-plan.md) with a recommended resolution. Features are not cut quietly and budgets are not loosened quietly.

## The shape

```
                 ┌──────────────────────────── one process: `cortex` ───────────────────────────┐
 terminal ◄────► │  TUI (main thread)        headless printer         RPC server (stdio/socket)  │ ◄──► editors, GUIs,
                 │        │                        │                          │                  │      other agents
                 │        └────────── Op ──────────┴────────── Op ────────────┘                  │
                 │                               ▼            ▲ Event                            │
                 │                    ┌────────────────────────────┐                             │
                 │                    │  Core (one "core" thread)  │                             │
                 │                    │  session ─ agent loop      │                             │
                 │                    │  context ─ permissions     │   knowledge · statistics    │
                 │                    │  verification ─ scripts    │   repository index          │
                 │                    └───┬────────┬─────────┬─────┘                             │
                 │            providers   │  tools │         │ session store                     │
                 │        (HTTP + SSE) ◄──┘        ▼         └──► JSONL log, artifacts, snapshots│
                 └───────────────────────── child processes ─────────────────────────────────────┘
                              shell commands · verification · hook commands · MCP servers
                                   (launched through the sandbox launcher)
```

- **One binary, one process.** There is no daemon and no second runtime. The TUI, the headless printer and the RPC server are thin front ends inside the same process as the core.
- **One protocol.** Front ends send `Op` values to the core and receive `Event` values back. In-process front ends use typed channels with no serialisation. The RPC server serialises the same types as JSON-RPC 2.0. Because the built-in TUI uses the same operations and events as an external client, anything the TUI can do, a TypeScript GUI or editor client can do too. See [11-rpc-protocol.md](11-rpc-protocol.md).
- **The core is a library.** The agent loop, providers, tools, sessions and permissions are library crates with no knowledge of terminals. See [01-workspace.md](01-workspace.md).
- **Children are isolated.** Shell commands, verification commands, hook commands and MCP servers are separate processes. Each is started in its own process group (Linux) or Job Object (Windows), through a launcher that applies the sandbox policy. See [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md).
- **Extension scripts run inside, in a sandbox of their own.** An embedded script runtime gives extensions no access to anything except host functions that are checked one by one. This works the same on Windows as on Linux, and it is what lets the harness write and load an extension on the spot. See [12-extensions.md](12-extensions.md).

## Threads

| Thread | Job | Notes |
|---|---|---|
| Main | TUI: input, layout, rendering | Never waits on the network or the disk. In headless and RPC modes the main thread runs the printer or the server loop instead. |
| Core | A single-threaded async runtime: agent loop, provider streams, child-process I/O, timers | All session state lives here, so it needs no locks. |
| Blocking pool | File reads and writes, search, diffing, hashing, indexing | Bounded. Work is handed over with a cancellation token. |
| Script host | Runs extension scripts | One thread. Each extension has its own runtime with memory and time limits. Started on first use. |
| Terminal input | Blocking read of terminal events, forwarded to the main thread | TUI only. |
| Log writer | Drains the log queue to disk | Off the hot path. |

See [02-runtime.md](02-runtime.md) for the concurrency, error, cancellation and logging models.

## Data flow of one turn

1. The front end sends `Op::Prompt`. The core appends the user message to the session log.
2. The core assembles the request: the frozen system prompt and tool list for the current context epoch, plus the append-only message history. It checks the token budget and compacts first if needed. See [07-sessions-and-context.md](07-sessions-and-context.md).
3. The provider adapter builds the HTTP request and streams the response. The stream decoder turns provider events into neutral model events. See [04-providers.md](04-providers.md).
4. The agent loop forwards deltas as events and records the finished assistant message. Each tool call is planned as soon as its arguments have streamed; calls without side effects start at once, and the rest are decided by the permission engine (asking the user through the front end when needed) and run when the response ends. Non-conflicting calls run in parallel. See [03-agent-loop.md](03-agent-loop.md) and [05-tools.md](05-tools.md).
5. Tool results are appended in the model's call order, and the loop goes back to step 2 until the model stops calling tools.
6. If the turn changed files, the project's checks run, and failures go back to the model ([15-efficiency.md](15-efficiency.md)).
7. The turn completes only when no automatic work remains: no retries pending, no queued steering messages, no verification or compaction in progress. Its file changes form one snapshot that can be undone.

## Design rules that hold everywhere

These are referred to from the other pages as "the invariants".

1. **History is append-only.** Within a context epoch, nothing already sent to a model is edited, reordered or removed. Changes are appended. This keeps provider prompt caches warm and keeps provider reasoning blocks valid (two providers now reject or drop reasoning when earlier history changes). See [07-sessions-and-context.md](07-sessions-and-context.md).
2. **Failures are data.** Provider errors, tool errors and interruptions are values in the event stream and the session log. They are not panics, and they do not unwind through the loop.
3. **Everything cancellable is cancelled through one token tree.** Session → turn → model stream and each tool call.
4. **Pure mapping at the edges.** Building a provider request, decoding a provider stream, classifying an error, rendering lines: each is a pure function that can be tested from fixtures.
5. **Nothing blocks the UI thread, and nothing blocks the core thread.** File and CPU work goes to the blocking pool.
6. **Output is bounded.** Every tool result passes through one truncation policy before it enters the context. Full output goes to an artifact file.
7. **Secrets are typed.** Credentials live in a `Secret` type that cannot be printed or serialised by accident.
8. **Determinism where a model can see it.** Anything that reaches a prompt is ordered deterministically (sorted maps, stable tool order), and carries no timestamps in the cached prefix.
9. **The same behaviour on Linux and Windows unless a page says otherwise**, with the difference tested on both.

## Where each topic lives

| Topic | Page |
|---|---|
| Crates and dependency direction | [01-workspace.md](01-workspace.md) |
| Async model, errors, cancellation, logging | [02-runtime.md](02-runtime.md) |
| Agent loop | [03-agent-loop.md](03-agent-loop.md) |
| Providers | [04-providers.md](04-providers.md) |
| Built-in tools | [05-tools.md](05-tools.md) |
| Permissions and sandboxing | [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md) |
| Sessions and context management | [07-sessions-and-context.md](07-sessions-and-context.md) |
| Configuration | [08-configuration.md](08-configuration.md) |
| TUI | [09-tui.md](09-tui.md) |
| Headless mode | [10-headless.md](10-headless.md) |
| RPC protocol | [11-rpc-protocol.md](11-rpc-protocol.md) |
| Extensions | [12-extensions.md](12-extensions.md) |
| MCP | [13-mcp.md](13-mcp.md) |
| Statistics, project profile and memory, per-model variants, roles | [14-adaptation.md](14-adaptation.md) |
| Repository map, re-reads, output compression, early tool start, verification | [15-efficiency.md](15-efficiency.md) |
| The orchestration layer, parallel attempts, replay and evaluation | [16-run-engine.md](16-run-engine.md) |
| The Coordinator: a Manager session running a team of Employee sessions | [17-coordinator.md](17-coordinator.md) |
| Product vision, and further ideas | [../vision.md](../vision.md), [../ideas.md](../ideas.md) |
| Performance budgets | [../performance-budgets.md](../performance-budgets.md) |
| Testing and CI | [../testing-and-ci.md](../testing-and-ci.md) |
| Security | [../security.md](../security.md) |
| Decisions and their reasons | [../decisions/README.md](../decisions/README.md) |
| Build plan and open questions | [../build-plan.md](../build-plan.md) |

## Names used in these docs

| Name | Meaning |
|---|---|
| Cortex-Harness | The project. |
| `cortex` | The command (provisional; see the open questions in the build plan). |
| Session | One conversation with its log, artifacts and checkpoints. |
| Turn | Everything from one user prompt until the session is idle again: one or more model requests, tool rounds, retries and compaction. |
| Item | One unit inside a turn: a user message, an assistant message, a tool call with its result, or a notice. |
| Context epoch | A span of a session with a frozen system prompt and tool list and an append-only history. Compaction or a model change starts a new epoch. |
| Protocol (provider) | A wire dialect, for example Anthropic Messages or OpenAI Chat Completions. |
| Provider | An endpoint that speaks a protocol: a base URL, credentials and capability flags. |
| Workspace | The project directory the session works in. |
| Artifact | A file in the session directory that holds output too large for the context. |
| Snapshot | The saved previous contents of every file a turn changed. What undo restores. |
| Project profile | What the harness knows about a repository: commands, conventions, flaky tests, facts. Reviewed and editable by the user. |
| Settings profile | A named set of configuration overrides chosen with one flag. |
| Variant | One choice at a point where models differ: an edit format, a tool-schema style, a prompt style. |
| Role | What a model is used for: `main`, `fast` or `judge`. |
| Run | One unattended session in an isolated workspace, started by the orchestration layer. |
| Grant | Everything a session is allowed to do, as one value: mode, rules, sandbox policy, models, budget, hard limits. A child's grant is always a subset of its parent's. |
| Manager, Employee | Under the Coordinator: the session that plans, assigns and reviews, and the sessions that do the pieces of work. |
