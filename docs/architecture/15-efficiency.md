# Efficiency features

The second pillar of the [vision](../vision.md): full-featured, and lean in tokens, wall-clock time, footprint and the user's attention. Footprint is covered by [../performance-budgets.md](../performance-budgets.md). Cache-friendly prompt layout is covered by [07-sessions-and-context.md](07-sessions-and-context.md). This page specifies the rest.

Each feature here spends something to save something. Where it spends against a speed budget, the conflict is listed in the open questions of [../build-plan.md](../build-plan.md) with a recommended resolution.

## Repository map

**Goal.** Give the model the shape of the repository, so it can find its way without reading whole files.

**What it is.** `cortex-index` produces two things.

| Output | Content | Where it is used |
|---|---|---|
| **Outline of a file** | Its top-level and nested definitions with line numbers: functions, types, classes, methods, constants, sections. A few hundred bytes for a typical file. | The `outline` tool. The start of a `read` result for a large file. |
| **Map of the repository** | A compact tree: directories with what they hold, the most relevant files, and their main definitions, cut to a token budget (default 1,500). | The first user message of a context epoch. |

**How outlines are made.** By small per-language extractors written in Rust: line-oriented scanners that recognise definition syntax for about fifteen common languages, with a generic indentation-based fallback for the rest. They are approximate by design. They are fast (megabytes per second per core), have no external dependencies, and need no grammar files.

A parser-based approach would be more accurate. It was not chosen for the first release because each language's grammar is a C library that adds build time and around a megabyte or more of binary; fifteen of them would roughly double the binary. See [decision 0022](../decisions/0022-repository-map.md).

**How relevance is ranked.** Files touched in this session, then files touched in recent sessions (from the statistics), then recently changed files (from the file system or git), then by position in the tree. No model call is involved.

**Cost control.**

| Concern | Design |
|---|---|
| Start-up | Indexing never runs on the start-up path. It starts in the background, on the blocking pool, after the first frame. |
| First prompt | The map is included if it is ready within a short wait (default 150 ms), which it is for a cached repository. Otherwise the first request goes without it and the map arrives as an appended notice. |
| Repeated work | Outlines are cached on disk per project, keyed by path, size and modification time. A warm start re-scans only changed files. |
| Huge repositories | Hard caps: files scanned (default 20,000), bytes per file (1 MiB), total time per pass (2 s, then it continues in later idle slices). Ignore rules are honoured. Beyond the caps the map covers the top of the tree and says it is partial. |
| Memory | The index is not held in memory. The map is built from the cache when needed and dropped. |
| Stability | The map is fixed for a context epoch, like the rest of the prefix. |

**The `outline` tool.**

| Argument | Meaning |
|---|---|
| `path` | A file or a directory. |
| `depth` | For directories: how deep to list. |

Returns the outline of a file, or for a directory the outlines of its files up to the output limit. Read-only.

## Changed-only re-reads

The `read` tool remembers what it returned ([05-tools.md](05-tools.md)). When the model asks for a range it has already seen in this context epoch:

| Situation | Result |
|---|---|
| The file has not changed | A one-line result: unchanged since it was read at such-and-such point, with the range. No content is resent. |
| The file changed, and the difference is smaller than the content | A unified diff against the version the model last saw, with a note saying so. |
| The file changed a lot | The content, as usual. |

- The model can force a full read with `fresh: true`.
- The comparison version is the snapshot kept for the staleness guard, stored in the session's content store.
- After compaction the model no longer has the earlier content, so the first read in a new epoch is always full.

## Compressed tool output

Every tool result passes the output policy in [05-tools.md](05-tools.md). The vision asks for more than truncation.

**Stage 1, structural, always on.** Before the size limits are applied:

- terminal control sequences and colour codes are removed;
- progress redraws (lines rewritten with carriage returns) collapse to their final state;
- runs of identical or near-identical lines collapse to one line and a count;
- for commands the profile knows as tests or builds, the lines that matter (failures, errors, warnings and their context) are kept ahead of the routine ones when the output must be cut.

**Stage 2, model-assisted, later.** For very long outputs, a summary written by the fast model with pointers into the full output. It costs tokens and time, so it waits for routing data that shows it pays.

**The full output is always one key press away.** It is an artifact. `Ctrl+O` opens it in the viewer ([09-tui.md](09-tui.md)), and the model can read or search it by path.

## Starting tools early

**Goal.** A tool should start when its arguments are complete, not when the whole response is.

| Kind of call | When it starts | Why |
|---|---|---|
| Read-only (its plan has no write, exec or network effect) | As soon as its block ends in the stream. | It cannot do harm, and reads and searches are most calls. |
| Needs approval | The approval prompt appears as soon as its block ends. It runs when approved. | The user's thinking time overlaps the model's. |
| Has side effects and is already allowed | When the response ends. | See below. |

**Why side effects wait.** If a response fails halfway and is retried, a side-effecting call that already ran would belong to a message that no longer exists. Read-only calls have no such problem: their results are kept in a short-lived cache and reused if the retried response asks for the same thing, or dropped. Starting side-effecting calls early is possible later, with recovery that keeps the completed part of a failed response. It is listed in the open questions.

**Parallelism** follows the scheduler in [03-agent-loop.md](03-agent-loop.md): independent calls run together.

## Prefetch

"Likely reads are fetched ahead", in three steps of increasing ambition.

| Step | What | When |
|---|---|---|
| 1 | Start a `read` as soon as its `path` argument is complete in the stream, before the block ends. Warm the file cache for files the user names with `@`. | First release. |
| 2 | Keep a small result cache for read-only calls, keyed by tool, arguments and file state, so a repeated or predicted call is instant. | First release. |
| 3 | Predict context from history: which files are read together, which test file goes with which source file. Attach outlines or excerpts before the model asks, inside a token budget. | Later. It needs statistics from real sessions, and a wrong prediction costs tokens. |

Local reads are fast, so steps 1 and 2 save milliseconds. Step 3 is the one that saves whole round trips, which is why it must be driven by evidence.

## Background compaction

Specified in [07-sessions-and-context.md](07-sessions-and-context.md). In short: the summary is prepared in the background, by the fast model, when the context passes a lower threshold, and swapped in at the next safe boundary. The session does not stop to compact unless the hard limit arrives first.

## Automatic verification

**Goal.** After the agent changes files, check the work and feed failures back, so the user does not have to.

**How it runs.** A stage of the agent loop ([03-agent-loop.md](03-agent-loop.md)).

1. When the model ends its turn and files were changed in it, the loop picks the checks to run.
2. It runs them in the sandbox, with a time limit.
3. If they pass, the turn completes, and the result shows in the turn summary.
4. If they fail, the failures (compressed as above) are appended as a notice, and the model continues the same turn. This repeats up to a limit (default 2 rounds), after which the turn completes and reports the failing state.

**Which checks.**

| Source | First release |
|---|---|
| `verify.commands` in configuration | Yes. |
| `command` entries of class check and test in the project profile, accepted by the user | Yes. |
| **Affected-test selection:** only the tests that the changed files can affect | A simple form: the profile can hold a command template that takes changed paths or package names. Per-ecosystem test-impact analysis comes later. |

**Limits and control.**

- Off until at least one check is known. Onboarding offers to detect and confirm the project's commands.
- A time limit per round (default 120 s) and per turn. A check that usually takes longer than the limit, according to the statistics, is skipped with a note.
- Known flaky tests from the profile are retried once before being reported, and are reported as flaky.
- `verify.mode` is `auto`, `ask` (offer after each turn) or `off`.
- The user can interrupt verification like any other tool.

**Cost.** Verification trades wall-clock time and CPU for fewer broken turns. It runs at the end of a turn, not after every edit, to keep that trade favourable. Speculative verification, which starts earlier, is in [../ideas.md](../ideas.md).

## Live usage view

Always visible in the status line ([09-tui.md](09-tui.md)): the context gauge, tokens, and session cost. `/usage` opens a panel with:

- tokens by class for the session and the last request: fresh input, cache read, cache write, output, reasoning;
- **cache hit rate:** cache-read tokens as a share of all input tokens, for the last request and the session;
- cost by model and role;
- time to first token and tokens per second;
- rate-limit state where the provider reports it.

The same numbers are in `usage.updated` events for other clients ([11-rpc-protocol.md](11-rpc-protocol.md)). A fuller "where did the tokens go, and why did the cache miss" view is proposed in [../ideas.md](../ideas.md).

## Smart permissions

Specified in [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md). The part that saves attention: reads and in-project edits do not ask; commands on the built-in read-only list, the approved list or the learned list do not ask; everything else asks once and can be remembered at the right scope.

## First release and later

| Feature | First usable release | Later | Why later |
|---|---|---|---|
| Repository map and `outline` | Yes, with approximate extractors. | Parser-based outlines behind a feature. | Binary size and build time. See decision 0022. |
| Changed-only re-reads | Yes. | | |
| Output compression | Stage 1. | Stage 2. | Costs tokens; wait for data. |
| Early tool start | Read-only calls and approval prompts. | Side-effecting calls. | Needs partial-response recovery. |
| Parallel tools | Yes. | | |
| Prefetch | Steps 1 and 2. | Step 3. | Needs statistics. |
| Background compaction | Yes. | | |
| Automatic verification | Configured and learned commands, at the end of a turn. | Test-impact analysis. Speculative runs. | Per-ecosystem work; the second is an idea, not yet agreed. |
| Live usage view | Yes. | The context profiler. | An idea, not yet agreed. |
