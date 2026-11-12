# Sessions and context management

`cortex-session` stores conversations. `cortex-agent` decides what part of a stored conversation is sent to the model. This page covers both, because one rule shapes them together: **history that has been sent to a model is never edited**.

## Why append-only

Two facts from the providers (sources in the research notes; re-check when building):

- **Prompt caches match on prefixes.** Any byte that changes early in a request makes everything after it a cache miss, which costs money and adds latency.
- **Reasoning is bound to the history that produced it.** Anthropic's current models reject a request, or drop the reasoning, when the system prompt, the tool list or any earlier message has changed since a reasoning block was produced. OpenAI's newer models drop reasoning in the same case. Losing reasoning between tool calls makes an agent worse and more expensive.

So the design never does the things harnesses commonly do to save tokens: trimming old tool results in place, rewriting the system prompt mid-session, injecting a reminder and deleting it later, or swapping the tool list. Each has an append-only replacement here.

| Instead of | Cortex-Harness does |
|---|---|
| Editing the system prompt mid-session | Appends a notice message. |
| Changing the tool list mid-session | Keeps the list fixed for the context epoch. MCP and extension tools are reached through a fixed pair of tools ([05-tools.md](05-tools.md)), so a tool added mid-session works at once without touching the list. |
| A per-turn reminder that is deleted next turn | Appends it and leaves it. |
| Trimming old tool results in place | Bounds every result when it is created ([05-tools.md](05-tools.md)), and reclaims space only at compaction. |
| Summarising old turns while replaying newer ones unchanged | Starts a new context epoch (below). |

## Context epochs

A session is a sequence of epochs.

```
epoch 1                                   epoch 2
┌──────────────────────────────────┐      ┌──────────────────────────────────────┐
│ system prompt + tools (frozen)   │      │ system prompt + tools (rebuilt)      │
│ user, assistant, tool results …  │ ───► │ summary of epoch 1 + carried state   │
│ (append-only)                    │      │ user, assistant, tool results …      │
└──────────────────────────────────┘      └──────────────────────────────────────┘
          compaction, or a change of model, protocol or tool set
```

- Within an epoch, each request's prefix is byte-identical to the previous request's. A debug-build check and tests enforce it ([04-providers.md](04-providers.md)).
- A new epoch is the only place where the system prompt and tool list may change, where project instructions are re-read in full, and where old content is dropped.
- Epoch boundaries never fall inside a tool round.

## On-disk layout

```
<data dir>/sessions/<project key>/
    index.json                       rebuildable cache for listing sessions
    <session id>/
        log.jsonl                    the session: append-only, the source of truth
        artifacts/<id>.<ext>         full tool outputs and other large content
        checkpoints/<hash>           file contents saved before tools changed them
```

- The data directory follows the platform convention (`$XDG_DATA_HOME/cortex-harness` on Linux, `%LOCALAPPDATA%\cortex-harness` on Windows) and is configurable.
- The project key is derived from the canonical workspace path.
- Session ids are time-ordered, so sorting by id sorts by creation time.
- Files are created with owner-only permissions.

JSONL files were chosen over a database. The comparison and the measured cost of bundling SQLite are in [decision 0006](../decisions/0006-session-storage.md).

## The log

One JSON object per line. Every entry has the same envelope.

```json
{"v":1,"id":"e_0007","parent":"e_0006","ts":"2026-11-12T10:15:02Z","kind":"assistant", "...": "kind-specific fields"}
```

`parent` makes the log a tree. The active branch is the path from the newest leaf to the root. Rewinding to an earlier point and continuing adds a sibling; nothing is removed.

| Kind | Content | Sent to the model |
|---|---|---|
| `session` | First line: format version, session id, workspace path, creation time, harness version, parent session if forked. | no |
| `base` | The workspace's starting state: the commit, and a patch of uncommitted changes stored as an artifact. Taken in the background after the session starts. It is what makes a session replayable ([16-run-engine.md](16-run-engine.md)). | no |
| `epoch` | Start of a context epoch: provider, protocol and model, reasoning setting, the system prompt text, the tool definitions, and a hash of both. | yes, as the request prefix |
| `user` | A user message: text, attachments by artifact reference, delivery mode. | yes |
| `assistant` | An assistant message: neutral parts, the provider's raw blocks, stop reason, usage, cost, provider request id. | yes |
| `tool_result` | One tool result: call id, model-visible content, artifact references, error flag, structured payload, timing. | yes |
| `notice` | A message appended by the harness: changed instructions, a steering note, a repetition warning. | yes |
| `summary` | A compaction summary and the carried state. Starts an epoch's history. | yes |
| `attempt` | A failed or abandoned request attempt, with its error and any partial output. | no |
| `usage` | Usage outside the main loop: compaction, titles. | no |
| `checkpoint` | Which files a tool round changed, with checkpoint references. The checkpoints of a turn are that turn's snapshot. | no |
| `outcome` | How a turn ended: completed, interrupted, verified or not, later undone or rewound. | no |
| `meta` | Title, labels, user notes. | no |
| `ext` | State saved by an extension, namespaced by extension id. | no |
| `escalation` | A question waiting for the user, and later its answer. Used by the Coordinator ([17-coordinator.md](17-coordinator.md)). Reserved in the first release. | no |
| `decision` | Something a Manager session decided without asking, with its reason and the grant clause that allowed it. Reserved in the first release. | no |

**Writing.**

- An entry is one `write` call of one complete line. The file is opened in append mode.
- The file is created when the first user message is recorded, so an empty session leaves nothing behind.
- Durability is a setting: flush to disk at the end of every item (default), at the end of every turn, or never. The default costs one flush per item and loses at most the item in progress on a crash.
- If a write fails, the entry is not added to the in-memory session either. Memory never holds entries that are not on disk.

**Reading.**

- The log is parsed as a stream. Unknown kinds and unknown fields are skipped, so an older build can open a newer session read-only.
- A final line that is cut off by a crash is ignored and reported.
- **Repair on resume.** If the active branch ends with tool calls that have no results (a crash during a tool round), an "interrupted" result is appended for each before anything is sent. Tools are never re-run automatically.
- The format has a version number in the first line. A format change ships with a migration that reads the old version.

**Large content is stored by reference.** A tool result entry holds only what the model saw. The full output is an artifact. Session logs therefore stay small, and opening a long session stays fast. Other harnesses report session files of hundreds of megabytes where raw tool output is stored inline (see [../reference-notes.md](../reference-notes.md)).

**The index** is a cache: one record per session with id, title, first prompt, entry count and last-updated time. It is rewritten atomically when a session changes, and rebuilt from the logs if it is missing or stale. Listing sessions reads only the index.

## Resume, branches and forks

| Action | What happens |
|---|---|
| Continue (`cortex --continue`) | Opens the most recent session for this workspace. |
| Resume (`cortex --resume <id>`, or the picker) | Opens a chosen session. A unique id prefix is enough. |
| Rewind (TUI) | Moves the active leaf to an earlier entry. The next prompt starts a sibling branch from there. Optionally restores files from checkpoints to that point. |
| Switch branch (TUI) | Makes another leaf active. |
| Fork (`cortex --fork <id>`, or a command) | Creates a new session whose log begins with a copy of the active branch and records where it came from. The original is untouched. |

A resumed session starts a new context epoch if the provider's cache has certainly expired or the harness version changed the prompt or tools. Otherwise it continues the old epoch and may get a cache hit.

## Snapshots and undo

Edits apply without asking ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). That default is only acceptable because every turn can be undone.

- **Per-turn snapshot.** Before any tool writes a file, its previous content is saved in `checkpoints/` under its hash (or the fact that it did not exist), and the round's `checkpoint` entry lists what changed. Together, a turn's checkpoints are its snapshot. Taking one costs a file copy per changed file and nothing when no file changes.
- **Undo** restores the files changed by the last turn and rewinds the session to before it. **Rewind** to any entry does the same back to that point. Both are available from the TUI and the protocol.
- A restore checks each file against the state the harness left it in, and asks before overwriting something the user has edited since.
- **Changes made by shell commands** are not captured by file checkpoints. The harness detects them: it compares the workspace's changed-file list before and after each command (through git where there is a repository, otherwise by modification times under the workspace, within a time cap), records what it saw, and saves the previous content of any tracked file the command changed when that content is still recoverable from git. An undo that crosses such a command says exactly which changes it can and cannot restore.
- **Whole-workspace snapshots** (a copy-on-write or shadow-repository snapshot per turn) would make every change undoable at a cost in time and disk on large trees. They are not in the first release. The conflict and the recommendation are in the open questions of [../build-plan.md](../build-plan.md).

**Branching time-travel**, forking the conversation and the files together and comparing branches, is not a headline feature. The pieces it needs are here: the log is a tree, each turn has a snapshot, and sessions record their base state.

## What goes into a request

In order, for one request in an epoch:

1. **System prompt** (frozen for the epoch), assembled from fixed sections in a fixed order:
   1. The core instructions: who the agent is and how it works. Short, and ours.
   2. Environment facts that do not change during the session: operating system, shell, workspace root, whether it is a git repository. No date and no clock.
   3. Usage notes contributed by the enabled tools, in tool order.
   4. Project instructions (below).
   5. The project profile: accepted commands, conventions, known flaky tests and facts, inside a token budget ([14-adaptation.md](14-adaptation.md)).
   6. The index of available skills and custom commands, and a one-line summary of each source of extension and MCP tools.
   7. Sections contributed by extensions, ordered by extension id.
   8. The user's appended text, from configuration or a flag.
   A user or project can replace section 1 with their own text. The wording of section 1 can vary by model, as a prompt-style variant ([14-adaptation.md](14-adaptation.md)).
2. **Tool definitions** (frozen for the epoch), in sorted order.
3. **History** for the epoch: the summary if there is one, then every `user`, `assistant`, `tool_result` and `notice` entry on the active branch, unchanged.

The first user message of an epoch also carries what is useful but not stable enough for the system prompt: the current date, and the repository map ([15-efficiency.md](15-efficiency.md)) when it is ready. If the map is not ready in time it follows as an appended notice.

## Project instruction files

- **Names.** `AGENTS.md` and `CLAUDE.md`, so people coming from other agents do not have to rewrite anything. When a directory has both, both are read, `AGENTS.md` first. Other names can be added in configuration (`instructions.names`).
- **Discovery at the start of an epoch.** The user's own file in the configuration directory first, then one file per directory from the workspace root down to the working directory. All that exist are included, outermost first, each under a header that gives its path.
- **Size.** A total cap (default 48 KiB). Beyond it, files are cut from the innermost end with a visible warning in the UI. A warning is never silent truncation.
- **Nested files.** When a tool first touches a file under a directory that has its own instruction file, that file is appended once as a notice.
- **Changes on disk.** Checked at the start of each turn. A changed file is appended as a notice that says it replaces the earlier version. The system prompt itself is not rewritten until the next epoch.
- **Trust.** Instruction files load without project trust, because they are text for the model and cannot grant permissions. Their injection risk is handled in [../security.md](../security.md).

## Budgeting

Before each request the loop estimates the request size.

- **Estimate.** The input and output token counts the provider reported for the last request, plus an estimate for everything appended since (bytes divided by a per-model ratio from the catalog). The provider's counting endpoint is called only when the estimate is within 10% of the limit.
- **Limit.** The model's context window, minus the output reserve (the configured maximum output, capped), minus a safety margin.
- **Trigger.** When the estimate exceeds 85% of the limit, the loop compacts before sending. The ratio is configurable, per model if needed.
- **Fallback.** If a provider still reports a context overflow, the loop compacts and retries once.

## Compaction

Compaction ends the current epoch and starts a new one. It runs between tool rounds, never inside one.

1. **Summarise.** The whole epoch is sent to the model (the session's model, or a configured cheaper one) with the summary instructions. The instructions ask for a fixed outline: the user's goal and constraints, what has been done, decisions and their reasons, the current state of the work, what remains, and any exact text that must survive (names, commands, error messages). The user can add focus instructions to a manual compaction. Extensions can add to the instructions.
2. **Carry state mechanically.** Things that must not depend on a summary's accuracy are copied, not summarised:
   - the plan, from the `plan` tool, including edits the user made;
   - the list of files read and files changed in the session, with their current hashes;
   - the last user message, word for word;
   - references to artifacts mentioned in the last few rounds.
3. **Start the epoch.** Rebuild the system prompt and tool list (re-reading instruction files), then begin the history with one `summary` entry that holds the summary and the carried state.
4. **Recent turns.** By default nothing else is replayed. A setting (`compaction.keep_recent_tokens`, default 0) replays the most recent turns after the summary with their reasoning removed and their text, tool calls and results intact. Replaying them with reasoning intact is not possible on the providers that bind reasoning to history.

The originals stay in the log. Compaction changes what is sent, never what is stored. A compaction can be undone by rewinding to before it.

**What the user sees.** A notice with the token counts before and after, and the summary available to read. Automatic compaction can be turned off, in which case the loop stops and asks when the budget is reached.

### In the background

A compaction that stops the session to write a summary costs the user time. So the summary is prepared before it is needed.

1. When the estimate passes a lower threshold (default 70% of the limit), the loop starts summarising the epoch so far, in the background, on the `fast` model role. The session carries on.
2. When the estimate reaches the compaction trigger, at the next safe boundary, the loop starts the new epoch from that summary. The turns that happened after the summary was taken are replayed after it, with their reasoning removed and their text, tool calls and results intact.
3. If the trigger arrives before the summary is ready, the loop waits for it. This is the only case in which compaction stops the session.

Replaying the newer turns without their reasoning is what makes this valid on providers that bind reasoning to history. The cost is that the model loses its reasoning for those few turns, and one extra summary request is made. If the background summary turns out not to be needed (the session ends first), its tokens were spent for nothing; the lower threshold is set to keep that rare.

**Provider-side context management.** Some providers can compact or clear old tool results on their side without invalidating reasoning. The design leaves room for a protocol adapter to use those features in place of the steps above, per provider, as a later optimisation. The client-side method works on every provider and is the default.

## Secrets in sessions

- Session logs and artifacts can contain anything a tool printed, including secrets. They are stored with owner-only permissions and never leave the machine on their own.
- A redaction filter runs when entries and artifacts are written. It masks the harness's own credentials by value, and strings that match well-known key formats. It is best-effort and is described as such.
- `cortex sessions export` applies a stricter pass and shows what it masked. There is no built-in upload or sharing feature.

See [../security.md](../security.md).
