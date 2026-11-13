# Efficiency features

The second pillar of the [vision](../vision.md): full-featured, and lean in tokens, wall-clock time, footprint and the user's attention. Footprint is covered by [../performance-budgets.md](../performance-budgets.md). Cache-friendly prompt layout is covered by [07-sessions-and-context.md](07-sessions-and-context.md). This page specifies the rest.

Each feature here spends something to save something. Where it spends against a speed budget, the conflict is listed in [../build-plan.md](../build-plan.md) with the resolution the owner accepted on 2026-11-12.

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

Built in step 6. What "already seen" means, exactly:

- **The table of seen files keeps, per file, the lines the model has been shown** of the version it last saw, as ranges. A read of lines that are all among them, of an unchanged file, is the one-line result. A read that reaches past them returns the content and adds its lines.
- **After a diff, the model has seen** the lines it had seen that are still there, at their new numbers, every added line, and the unchanged lines the diff shows around each change. Nothing else counts as seen: a later "unchanged" must never be said of a line the model was not shown.
- **A diff answers a read only when, with the diff, the model has seen every line the read would have returned.** The lines are counted in the file as it is now. A model that saw lines 100 to 150 and asks for 200 to 250 of the changed file gets those lines, not a diff. So does one that saw lines 1 to 10 and asks for them again after the first five were deleted: five lines it never saw have moved into the range, and a diff would not show them.
- **A line shown shortened has not been seen.** A line over the line limit is cut with a marker, so it is left out of the lines the model has, and a read that includes it is answered with the content again, never with "unchanged". `read` cannot return more of such a line than the limit allows; the rest of it is reached with `search` or a command.
- **The table is written when the result is delivered.** A read that is cancelled at the last moment returns "interrupted" and leaves the table as it was.
- "Smaller than the content" compares the diff, with its one-line preface, against the result the content would have been. A change of line endings or of the final line break alone is said in one line.
- A file's version is kept for diffing only up to 1 MiB, and two versions are diffed only up to 50,000 lines between them. Beyond either, a changed file is sent as content.
- Whether a file changed is decided by its content hash, never by its modification time.

## Compressed tool output

Every tool result passes the output policy in [05-tools.md](05-tools.md). The vision asks for more than truncation.

**Stage 1, structural, always on.** Before the size limits are applied:

- terminal control sequences and colour codes are removed;
- progress redraws (lines rewritten with carriage returns) collapse to their final state;
- runs of identical or near-identical lines collapse to one line and a count;
- for commands the profile knows as tests or builds, the lines that matter (failures, errors, warnings and their context) are kept ahead of the routine ones when the output must be cut.

The mechanics were built in step 6, in the output policy of `cortex-tools`. Three details were settled there:

- **It applies to output that was produced, not to content that was asked for.** The output of a command or of an extension or MCP tool is compressed. A file read is returned exactly, because the model edits against it.
- **Identical lines always collapse; near-identical lines only when the output is over the limits.** A run of four or more identical lines becomes the line and a count, and nothing is lost. Lines that differ only in their numbers (`Downloading 12/100`) become the first and the last of the run with a count between, which loses their text: so it is done only to output that would be cut anyway, and the full output is then an artifact. A line with fewer than three letters is data, not progress, and is never folded.
- **"The lines that matter" is a way of choosing what to keep,** which the caller asks for: lines that report a problem, each with the line before it and the two after, then the end and the beginning of the output. The shell tool and the verification stage ask for it when the profile says a command is a build or a test.

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

Built in step 7, in the loop ([03-agent-loop.md](03-agent-loop.md), "Streaming" and "Retries"). Three things were settled there:

- **"No side effects" is read from the plan:** a call may start early when its plan has no effect but reads (`Plan::has_side_effects`) **and** is scheduled as read-only. The second half is there so that a tool whose author declared no effects, and which therefore runs alone, does not start early on the strength of an empty list. **A tool that changes the session itself must still not look like a read.** `plan` and `remember` touch nothing outside the session and are scheduled as read-only, so with an empty list of effects they would start early, and a response that was then thrown away would leave a replaced plan or a proposed memory behind it. Step 10, which builds them, settles how such a call says so.
- **A dropped result is also forgotten.** `read` notes what it showed the model in the table of seen files. When an early result is dropped, the loop makes the table forget the files that call read, so that a later read is not answered "unchanged" about lines the model never saw.
- **An approval is not a reason to start early.** A call with side effects that is approved while the model is still writing waits for the response to end like any other.

**Parallelism** follows the scheduler in [03-agent-loop.md](03-agent-loop.md): independent calls run together.

## Prefetch

"Likely reads are fetched ahead", in three steps of increasing ambition. The owner settled the first two on 2026-11-13, in step 7, as that step had built them.

| Step | What | When |
|---|---|---|
| 1 | Start a call that only reads as soon as its block ends in the stream, while the model is still writing the rest of the response ("Starting tools early", above). | First release. Built in step 7. |
| 2 | Keep what ran early when a response fails and is retried, and use it again if the retried response asks for the same call. | First release. Built in step 7. |
| 3 | Warm the file cache for files the user names with `@`. | First release, with the editor's attachments (step 24), which is where a named file is known before the prompt is sent. |
| 4 | Predict context from history: which files are read together, which test file goes with which source file. Attach outlines or excerpts before the model asks, inside a token budget. | Later. It needs statistics from real sessions, and a wrong prediction costs tokens. |

Local reads are fast, so steps 1 to 3 save milliseconds. Step 4 is the one that saves whole round trips, which is why it must be driven by evidence.

**Two things an earlier draft of this table promised are not built, by decision:**

- **A call does not start before its block ends.** The draft had a `read` start as soon as its `path` argument was complete. Arguments are untrusted partial JSON until the block ends, and are parsed once ([03-agent-loop.md](03-agent-loop.md)). Starting on a half-read object would mean a second, tolerant parser, and a run that has not seen `offset`, `limit` or `fresh` yet and may have to be thrown away. A `read` call's arguments are a few dozen bytes, so the gap between "path complete" and "block ended" is a few milliseconds of stream.
- **There is no result cache across requests.** The draft had a cache for read-only calls keyed by tool, arguments and file state. A repeated `read` is already cheap, and cheaper in tokens than a cached copy: the tool answers "unchanged" or sends a diff ("Changed-only re-reads", above). A cache in the loop that returned the earlier full result would undo that. The cache of step 2 lives for one request and no longer. A cache for predicted calls belongs with step 4, which predicts them.

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

**Cost.** Verification trades wall-clock time and CPU for fewer broken turns. It runs at the end of a turn, not after every edit, to keep that trade favourable. The owner accepted this resolution on 2026-11-12 (conflict C7 in the [build plan](../build-plan.md)).

### Speculative verification

Adopted by the owner on 2026-11-12 ([../ideas.md](../ideas.md)), with one condition: it must work together with end-of-turn verification, not replace it or argue with it. This is how.

**End-of-turn verification stays the only gate.** What is checked, when failures are fed back to the model, the time limits and the limit of two rounds are exactly as above. Speculative verification changes one thing: **when the work of a check starts.** It is a head start, and its result is a cache entry.

| | End-of-turn verification (C7) | Speculative verification |
|---|---|---|
| Decides whether the turn passed | Yes | No |
| Feeds failures back to the model unasked | Yes, at the end of the turn, up to two rounds | **Never** |
| Starts | When the model ends its turn | When edits have landed and gone quiet, while the model is still working |
| Result | Shown in the turn summary | Stored under a key, and used only if the key still matches |

**The key.** Every speculative run is tied to the exact state it ran against: the check's command, and a hash of the contents of the files the turn has changed so far together with the base state. A result is valid only for that key.

**How the two meet.**

1. An edit lands. After a short quiet period (default 750 ms with no further edit), the harness starts the check the end-of-turn stage would run, for the current key.
2. If another edit lands, the key changes. The running check is cancelled and, after the quiet period, restarted.
3. When the model ends its turn, the verification stage computes the key and looks it up:
   - **Finished, same key:** the result is used at once. The turn waits for nothing.
   - **Running, same key:** the stage waits for that run, which has a head start, under the stage's own time limit.
   - **No run, or a different key:** the stage runs the check normally.
4. From there on the stage behaves as it always does. If the check failed, the failure is fed back, the model edits, and the cycle starts again with a new key. A feedback round counts as a round whether its result came from the cache or not.

**When the model runs the check itself.** If the model calls the shell tool with a command that is exactly a known check, and a result exists for the current key, that result is returned with a note that it was already run against this state. Nothing is injected into the conversation that the model did not ask for, so the reason for C7 (no extra tokens and no interruptions after every edit) still holds.

**Not interfering with the model's next step.**

| Kind of check | Where it runs |
|---|---|
| Reads the tree only (a type-check with no outputs, a linter) | In place. |
| Writes build outputs or caches | In an isolated copy of the workspace from the orchestration layer's workspace manager ([16-run-engine.md](16-run-engine.md)), with its own build directory. Or in place, if the project profile marks the command as safe to run alongside edits. |
| Unknown | Not run speculatively. |

**Limits.**

- One speculative run at a time. Cancelled promptly, with the whole process tree.
- It runs in the sandbox under the same policy as verification commands.
- Not when the statistics say the check usually takes longer than a turn does, since it would rarely finish in time to be used.
- `verify.speculative` is `off`, `auto` or `on`. The default is `off` until the local statistics show that its results are used often enough to pay for the CPU: every run is recorded as used, waited-on or wasted, and `auto` turns it on per project above a hit rate.
- It needs verification (first release) and, for checks that write, the workspace manager (after the release). It is built after both.

## Live usage view

Always visible in the status line ([09-tui.md](09-tui.md)): the context gauge, tokens, and session cost. `/usage` opens a panel. Its history side (usage and cost per provider, model and project over time, limits left, and other tools' records) is specified in [18-usage.md](18-usage.md). Its session side shows:

- tokens by class for the session and the last request: fresh input, cache read, cache write, output, reasoning;
- **cache hit rate:** cache-read tokens as a share of all input tokens, for the last request and the session;
- cost by model and role;
- time to first token and tokens per second;
- rate-limit state where the provider reports it.

The same numbers are in `usage.updated` events for other clients ([11-rpc-protocol.md](11-rpc-protocol.md)).

## Context profiler

Adopted by the owner on 2026-11-12 ([../ideas.md](../ideas.md)). The usage view says how much was used. The profiler says why, for one session.

**Three questions.**

| Question | Answered from | Example |
|---|---|---|
| Where did the tokens go? | The context record the statistics keep for each request ([14-adaptation.md](14-adaptation.md)): sizes by source and by item. | "Tool results are 61% of this context. One file, read four times, is 9,000 tokens of it." |
| Why did the cache miss? | The prefix check that already runs on every request ([07-sessions-and-context.md](07-sessions-and-context.md)), which knows the first segment that differed. | "Request 14 missed the cache: the tool list changed when an MCP server reconnected." |
| What would be cheaper? | Small rules over the same record, each with an estimated saving. | "This server's tool definitions cost 6% of every request. Switching it to search would save about 4,000 tokens a request." |

**Rules, not a model.** Each suggestion is a tested rule with a threshold. A rule that cannot estimate its saving does not speak. The first set: a file read whole several times where its outline would have served; a tool's output that was mostly discarded; an extension or MCP server whose definitions are large and rarely used; an instruction file that dominates the prompt; a cache miss with a nameable cause; a session that would be cheaper after compaction.

**Cost.** The record is a few numbers per item, written with the request event. The view is computed when opened. Nothing runs during a turn.

**Where.** `/context` opens it. In the first release `/context` shows the breakdown by category, and the per-request record is already being kept. The full view, the cache-miss explanation and the suggestions are one build step after the release.

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
| Prefetch | Steps 1 to 3: calls that only read start early, what ran early survives a retry, and `@` files are warmed. | Step 4: prediction from history. | Needs statistics. |
| Background compaction | Yes. | | |
| Automatic verification | Configured and learned commands, at the end of a turn. | Test-impact analysis. Speculative verification. | Per-ecosystem work. Speculative runs need the workspace manager and statistics that show they pay. |
| Live usage view | Yes, with usage history for our own numbers. | Other tools' usage records. | Separate reader work. |
| Context profiler | The by-category breakdown, and the per-request record. | The full view, cache-miss explanations, suggestions. | Presentation over data the first release already keeps. |
