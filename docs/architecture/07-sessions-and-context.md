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
    index.lock                       keeps two writers of the index apart
    <session id>/
        log.jsonl                    the session: append-only, the source of truth
        lock                         held by the one process that writes the log
        log.cut-<offset>             the bytes of a last line a crash cut off, set aside
        artifacts/<id>.<ext>         full tool outputs and other large content
        checkpoints/<hash>           file contents, filed under their hash
```

- The data directory follows the platform convention (`$XDG_DATA_HOME/cortex-harness` on Linux, `%LOCALAPPDATA%\cortex-harness` on Windows) and is configurable (`session.data_dir`). `cortex-session` takes every path as an input; `cortex-config` says where the data directory is.
- **The project key** is `AbsPath::project_key` in `cortex-base`: the first 32 lowercase hexadecimal digits (128 bits) of the `ContentHash` (SHA-256) of the comparison form (`AbsPath::key`) of the workspace's **resolved** root. Sessions, stored approvals, project trust, statistics and the project's knowledge are all filed under it, so it is one definition beside the path code, and two projects must never share one: 128 bits of a hash nobody can make collide put a collision between two real workspaces out of reach, and keep paths short on Windows. Two spellings of one Windows path have one key. It has no readable prefix, so a directory listing does not show the names of a user's projects. Decided in step 8.
- Session ids are time-ordered, so sorting by id sorts by creation time. An id is `s_` and 22 characters: ten for the creation time in milliseconds and twelve random ones, in a 32-character alphabet of digits and lowercase letters that sorts as the numbers do (`cortex-base`, `id`). Lowercase only, because the id is a directory name and Windows ignores case. Ids made by one process never go backwards, even within a millisecond or after the clock is set back.
- **Every file and directory is created owner-only**, through `cortex_base::private` ([decision 0029](../decisions/0029-owner-only-files-on-windows.md)): a new file with `private::create_new`, a rewritten one with `write_atomic` and `owner_only`, every time. A file is never created first and protected afterwards. On a file system that keeps no permissions (FAT, exFAT, some network shares) the session is still written, and opening it reports which of its files are not private.
- **Artifacts** are named by counted ids, `a_0001`, the form the model is told; a new artifact takes the next number after any in the directory. It is written under `<id>.part` and renamed when it is finished, so a half-written artifact never has an artifact's name.
- **The checkpoint store** is content-addressed: a file's content filed under its hash, written once, and checked against the hash when it is read. The checkpoints behind undo and the file versions behind changed-only re-reads ([05-tools.md](05-tools.md), "The content store") are the same thing, so they share it.

JSONL files were chosen over a database. The comparison and the measured cost of bundling SQLite are in [decision 0006](../decisions/0006-session-storage.md).

## The log

One JSON object per line. Every entry has the same envelope.

```json
{"v":1,"id":"e_0007","parent":"e_0006","ts":"2026-11-12T10:15:02Z","turn":"t_3","kind":"assistant", "...": "kind-specific fields"}
```

- `v` is the format version the line was written in: 1.
- `id` counts up within the session. `e_0000` is the first line, the `session` entry, which is the root of the tree; every other entry is `e_0001` onwards.
- `parent` makes the log a tree. Rewinding to an earlier point and continuing adds a sibling; nothing is removed.
- **Session records.** An entry with `parent: null` after the first line is outside the conversation tree: `base`, `lease`, `meta`, `usage`, `outcome`, `escalation`, `decision`, and `ext`. They belong to the session, not to a branch, so a rewind never hides the base state or a lease.
- **The active position is the last tree entry in the file**, and the active branch is the path from it to the root. A rewind, an undo and a switch of branch each append a `rewind` entry whose parent is the entry moved to, so the position survives a restart without anything being edited.
- `turn`, when present, is the turn the entry was made in (`t_N`). Undo finds a turn's start, its checkpoints and its outcomes by it.

| Kind | Content | Sent to the model |
|---|---|---|
| `session` | First line: `session` (the id), `workspace` (the root, with `/`), `harness` (the version that created it), `forked_from` (`{session, entry}`) if forked. The creation time is its `ts`. | no |
| `base` | The workspace's starting state (below). Taken in the background after the session starts. It is what makes a session replayable ([16-run-engine.md](16-run-engine.md)). A record. | no |
| `epoch` | Start of a context epoch: `provider`, `protocol`, `model`, `effort`, the `system` prompt text, the `tools` definitions, and a `hash` of both. | yes, as the request prefix |
| `user` | `item`: the protocol's user message item (text, attachments by artifact reference, delivery mode). | yes |
| `assistant` | `item`: the protocol's assistant message item (neutral parts, the provider's raw blocks, stop reason, usage, cost), and `request_id`, the provider's id for the request. | yes |
| `tool_result` | `item`: the protocol's tool result item (call id, model-visible content, artifact references, outcome, structured payload, timing). | yes |
| `notice` | `item`: a message appended by the harness: changed instructions, a steering note, a repetition warning. | yes |
| `summary` | `item`: a compaction summary, and `carried`, the carried state. Starts an epoch's history. | yes |
| `attempt` | A failed or abandoned request attempt: the `item` it would have been, the `attempt` number, the `error`, and the partial `parts`. | no |
| `usage` | Usage outside the main loop: `purpose` (compaction, titles) and `usage`. A record. | no |
| `checkpoint` | Which files a tool round changed: `files`, each with `path` (relative to the workspace, or absolute outside it), `before` and `after` (`{hash, bytes}`, or `null` for a file that did not exist; `"kept": false` when the content was too large to keep). The checkpoints of a turn are that turn's snapshot. | no |
| `outcome` | How a turn ended (`ended`, `verification`, `stop_reason`, `error`, `usage`, `duration_ms`, from the turn's `turn.completed`), and in a later record for the same turn, `reverted`: `undone` or `rewound`. A record. | no |
| `meta` | `title`, `labels`, `note`. A later record replaces what it sets. A record. | no |
| `ext` | State saved by an extension: `ext` (its id) and `data`. A record. | no |
| `escalation` | A question waiting for the user, and later its answer. Used by the Coordinator ([17-coordinator.md](17-coordinator.md)), with that page's fields; its `kind` is stored as `category`, because `kind` is the envelope's. Reserved in the first release, and its shape may change before the first use. A record. | no |
| `decision` | Something a Manager session decided without asking, with its reason and the lease clause that allowed it. Reserved like `escalation`. A record. | no |
| `lease` | A lease granted, extended or ended: `event` (`granted`, `extended`, `ended`) and `lease`, its exact terms ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). The standing lease is recorded when a session starts. Task and team leases come after the first release. A record. | no |
| `rewind` | The active position moved to this entry's parent: `reason` (`rewind`, `undo`, `switch`) and `from`, the position before. | no |

**A lease is read strictly.** Every other entry is tolerant. A `lease` record is read through the protocol's strict decoder ([decision 0028](../decisions/0028-enforced-values-fail-closed.md)): exactly `event` and `lease`, an event this build knows, and a lease it understands in full. Anything else refuses the record whole, with the code `lease_not_understood`. If any lease record of a session is refused, the session still opens, and asking for the lease in force returns that refusal, never a lease: the leases are then unknown, and a lease is never read in part.

**Writing.**

- An entry is one write of one complete line, at the end of the file. The file is opened for writing, not in append mode: on Windows a file opened for appending cannot be cut back, and cutting back is how a failed write is undone. One process writes a session at a time: it holds the session's `lock` file (not a lock on the log, which on Windows would stop other processes reading it). A second writer is refused; it can open the session read-only.
- The file is created when the first user message is recorded, so an empty session leaves nothing behind. Until then the first entries (the header, a lease, an epoch) are held, and the log is created with all of them in one atomic, owner-only write, so it never exists without its first line.
- Durability is a setting, `session.durability`: flush to disk after every item (`item`, the default), at the end of every turn (`turn`), or never (`never`, left to the operating system). The default costs one flush per item and loses at most the item in progress on a crash. Only the user may set it: a project that could choose `never` could cost the user their sessions.
- **If a write fails, the entry is not added to the in-memory session either, and the file is cut back to where it was** (and the cut flushed), so a line that reached the disk does not come back after a crash. Memory never holds entries that are not on disk. If cutting back fails too, the session refuses every later write until it is opened again, which repairs the tail. With `turn`, a flush that fails at the end of a turn is reported and removes nothing: the lines were written, and whether they reach the disk is the operating system's.
- **Ids.** While a session is open, its owner (the agent loop) makes its turn and item ids, starting after the highest in the log (`Log::last_ids`), because it hands out a turn id the moment a prompt is queued and must not wait for the session behind a flush. The store makes item ids only when it repairs: on resume, before the loop starts, and after a rewind or a switch, which report the results they added.

**Reading.**

- The log is parsed as a stream, and what is kept of each entry is its envelope, the id of the item it holds, and where its line is. The rest of a line is checked and skipped, and read again from the file when it is asked for. A long session therefore costs little memory, and its active branch is read in one pass.
- Unknown kinds and unknown fields are skipped, and kept as they are: a fork copies them. A kind this build does not know stays in the tree, so its children keep their place.
- **A final line that is cut off by a crash is ignored and reported.** A final line with no newline is kept if it is a whole entry: a cut can never turn part of an entry into a different whole one. A damaged line elsewhere is skipped and reported, and an entry whose parent is not an earlier entry starts a branch that stops there, and is reported.
- **Opening a session to read it never changes the file**, whatever it finds: another process may be in the middle of writing a line, which looks exactly like a line a crash cut off. Only a resume, which holds the lock, repairs.
- **Repair on resume.** The bytes of a cut last line are set aside in `log.cut-<offset>`, a private file, and the log is cut back to its last whole line; a missing final newline is added. If the active branch ends with tool calls that have no results (a crash during a tool round), an "interrupted" result is appended for each before anything is sent. Tools are never re-run automatically. The same repair follows a rewind or a switch to a response that asked for tools.
- The format has a version number in every line. A session whose first line has a newer version than this build opens read-only: it can be read and listed, not written or forked. A format change ships with a migration that reads the old version.

**Large content is stored by reference.** A tool result entry holds only what the model saw. The full output is an artifact. Session logs therefore stay small, and opening a long session stays fast. Other harnesses report session files of hundreds of megabytes where raw tool output is stored inline (see [../reference-notes.md](../reference-notes.md)).

**The index** is a cache: one record per session with id, workspace, title, first prompt, entry and turn counts, the main model, the parent, the log's size, and the created and last-updated times. It is rewritten atomically and owner-only, under `index.lock`, at the end of a turn and when a session is closed, without a flush, because it can be rebuilt. Listing reads it and makes one directory listing of the project: a session the index does not have is read from its log and added, one whose directory is gone is dropped, and an index that is missing, unreadable or of another version is rebuilt from the logs. `--resume` takes a unique prefix of an id.

## The base state

What a session records about the workspace it started in, so that it can be replayed ([16-run-engine.md](16-run-engine.md)). It is taken on a thread of its own after the session starts, never delaying the first prompt, and appended as a `base` record when it is ready.

- **In a git repository:** the commit (none in a repository with no commits), the branch (none when the head is detached), the workspace's place in the repository, the uncommitted changes to tracked files as a binary patch stored as an artifact (in a repository with no commits, the staged changes and then the unstaged ones), and the untracked files that are not ignored, stored in the checkpoint store: up to 1 MiB a file, 1,000 files and 50 MiB in all, and a patch of up to 50 MiB. Anything over a limit is listed as omitted, with the reason.
- **Outside git:** a listing of the files with their sizes and hashes, up to 20,000 files and 2 seconds, hashing files up to 64 MiB. Their content is not kept, so a replay of such a session needs the files to still match their hashes. Links are not followed.
- **Project trust is an input.** A repository's own configuration can make git run programs: a clean filter runs on `git diff` and `git status`, and an untrusted repository can carry that configuration. For a project that is not trusted only the commit and the branch are read (`git rev-parse` and `git symbolic-ref`, which run nothing of the repository's), and the record is marked incomplete. A capture after the project is trusted appends a second, complete record, which the session's base state then is.
- **Likely secrets are never kept**, trusted or not, within the caps or not: environment files (`.env*`), keys and certificates (`*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_*` but not `id_*.pub`) and `.netrc`. Tracked ones are left out of the patch by name, and every one is listed as omitted with the reason `secret`. Step 12 replaces this list with the read rules.
- **git is run hardened**, and later steps that run git use the same flags (`cortex_session::base::GIT_HARDENING` and `DIFF_FLAGS`): `--no-optional-locks` (git writes nothing it does not have to), `-c core.fsmonitor=false` (no file-system monitor, which a repository's configuration could name as any program), `-c core.quotepath=off`, `-c diff.noprefix=false` and `-c diff.mnemonicPrefix=false`; for a diff also `--no-ext-diff --no-textconv --no-color --no-renames --binary --full-index` with the `a/` and `b/` prefixes, so the patch applies whatever the user's configuration says; and `GIT_TERMINAL_PROMPT=0`, with standard input closed.

## Resume, branches and forks

| Action | What happens |
|---|---|
| Continue (`cortex --continue`) | Opens the most recent session for this workspace. |
| Resume (`cortex --resume <id>`, or the picker) | Opens a chosen session. A unique id prefix is enough. |
| Rewind (TUI) | Moves the active position to an earlier entry, with a `rewind` entry. The next prompt starts a sibling branch from there. Optionally restores files from checkpoints to that point. Each undone turn gets an `outcome` record saying it was rewound. |
| Switch branch (TUI) | Makes another branch active, with a `rewind` entry whose parent is its last entry. Files are not touched. |
| Fork (`cortex --fork <id>`, or a command) | Creates a new session whose log begins with a copy of the active branch (or of the branch up to a chosen entry), line for line, and the session's `base` records, and records where it came from. Leases, outcomes and metadata are not copied: a fork must not inherit a task lease, and records its own standing lease. The artifacts and the checkpoint store are shared by hard links where the file system allows, and copied otherwise. The original is untouched. |

A resumed session starts a new context epoch if the provider's cache has certainly expired or the harness version changed the prompt or tools. Otherwise it continues the old epoch and may get a cache hit.

## Snapshots and undo

Edits apply without asking ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). That default is only acceptable because every turn can be undone.

- **Per-turn snapshot.** Before any tool writes a file, its previous content is saved in `checkpoints/` under its hash (or the fact that it did not exist), and the round's `checkpoint` entry lists what changed, with what each file was before and what the round left. Together, a turn's checkpoints are its snapshot. Taking one costs a file copy per changed file and nothing when no file changes.
- **A file over 16 MiB is not copied.** Its hash and size are recorded, and it is marked as not kept. The files the editing tools change are source files, far smaller; a file over it is generated output, a database or a media file, where a copy per turn costs time and disk for little; and a restore holds the content in memory, which this keeps inside the memory budgets. A rewind lists such files before it starts, as files it cannot put back.
- **Undo** restores the files changed by the last turn and rewinds the session to before it. **Rewind** to any entry does the same back to that point. Both are available from the TUI and the protocol. A rewind is planned first: for each file, what it will be put back to, and whether that is safe.
- A restore checks each file against the state the harness left it in, and does not overwrite something the user has edited since unless the user says so. The check is made again when the rewind is carried out.
- **Changes made by shell commands** are not captured by file checkpoints. The harness detects them: it compares the workspace's changed-file list before and after each command (through git where there is a repository, otherwise by modification times under the workspace, within a time cap), records what it saw, and saves the previous content of any tracked file the command changed when that content is still recoverable from git. An undo that crosses such a command says exactly which changes it can and cannot restore. The same size limit applies.
- **Whole-workspace snapshots** (a copy-on-write or shadow-repository snapshot per turn) would make every change undoable at a cost in time and disk on large trees. They are not in the first release. The owner accepted this resolution on 2026-11-12 (conflict C5 in [../build-plan.md](../build-plan.md)).

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
- A redaction filter runs when entries and artifacts are written. It masks the harness's own credentials by value, and strings that match well-known key formats. It is best-effort and is described as such. `cortex-session` applies it, through its `Redactor` hook, to every string of an entry and to text artifacts line by line as they are written; the filter itself is built in step 13. It does not see the provider's own blocks (`raw`), because changing a byte of one makes the provider refuse it on replay, nor the file contents kept for undo, which must be put back exactly, nor artifacts that are not text. Whether a raw block that holds a secret is dropped or kept is open for step 13 ([../security.md](../security.md), section 4).
- `cortex sessions export` applies a stricter pass and shows what it masked. There is no built-in upload or sharing feature.

See [../security.md](../security.md).
