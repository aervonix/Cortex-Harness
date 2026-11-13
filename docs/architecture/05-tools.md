# Built-in tools

Tools live in `cortex-tools`. This page fixes each tool's contract: its inputs, what it returns to the model, its limits, and how it behaves on Linux and Windows.

Several contracts here serve the [vision](../vision.md)'s efficiency goals directly: bounded and compressed output, re-reads that send only what changed, outlines instead of whole files, a plan the user can edit, and tools that are found on demand instead of filling the prompt.

## The tool trait

```rust
pub trait Tool: Send + Sync {
    /// Name, description, argument schema, default permission, concurrency class.
    fn spec(&self) -> &ToolSpec;

    /// Validate arguments and declare effects. No side effects.
    fn plan(&self, args: &Json, cx: &PlanCx) -> Result<Plan, ToolError>;

    /// Do the work. Streams progress through `out`. Must stop when `cx.cancel` fires.
    fn run<'a>(&'a self, plan: Plan, cx: &'a RunCx, out: &'a mut dyn ToolOutput)
        -> BoxFuture<'a, ToolResult>;
}

pub struct Plan {
    pub effects: Vec<Effect>,          // Read | ReadTree | Write | Exec | Network | Ext
    pub concurrency: Concurrency,      // ReadOnly | Keyed(paths) | Exclusive
    pub summary: String,               // one line for approval prompts and the UI
    // plus the validated arguments, private to the tool
}

/// The only way a tool is run: `run`, then the output policy.
pub async fn execute(tool: &dyn Tool, plan: Plan, cx: &RunCx, out: &mut dyn ToolOutput)
    -> ToolResult;
```

The two phases exist so the permission engine, the sandbox and the scheduler can act on declared effects before anything runs (see [03-agent-loop.md](03-agent-loop.md)). Built-in tools, extension tools and MCP tools all implement this trait, so one pipeline applies to all three. It was built in step 6, in `cortex-tools`.

**Planning.** `plan` checks the arguments against the tool's schema, resolves every path through the one resolver (below), and returns the plan or an error written for the model. It changes nothing. It may ask the file system where a path leads, which blocks for a few system calls, so the loop calls it through the blocking helper. `PlanCx` holds the path resolver and nothing else.

**Effects.** Each is one thing the call would do, and the permission engine decides each ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)).

| Effect | What it carries | Declared by |
|---|---|---|
| `Read` | A resolved path: one file, or the names in one directory | `read`, `list` of one level, `search` of one file |
| `ReadTree` | A resolved path: the tree under a directory | `search` of a directory, `list` with a glob or a depth |
| `Write` | A resolved path | `write`, `edit` |
| `Exec` | The command line and its working directory | `shell` |
| `Network` | A host, or any | `fetch`, MCP over HTTP |
| `Ext` | The full name of an extension or MCP tool | `tool_call`, beside whatever effects the tool itself declares |

- A path in an effect is the resolver's result: the real location, with every link followed, and whether it is inside the workspace. A rule is matched against that, never against what the model wrote.
- A plan with no effect but reads has no side effects, and is what the loop may start early ([15-efficiency.md](15-efficiency.md)). A call of an extension or MCP tool is never one.
- **A walk is checked twice.** `ReadTree` is decided once, before the walk. But a rule can protect one file inside an allowed tree, and the tree cannot be listed in advance. So a walk asks a gate (`ReadGate`, which the permission engine supplies) about each entry it meets, steps past what it may not read, and says in its result how many entries that was. Walks do not follow links, so they stay under the path they declared.

**Concurrency.** A tool's spec has a class, and each plan says how that call is scheduled, with the paths of a keyed call. `Plan::conflicts_with` is the rule the scheduler applies: an exclusive call conflicts with every call; two keyed calls conflict when they share a path; a keyed call conflicts with a read-only call that reads one of its paths or walks a tree that holds one, so that `edit a.rs` followed by `read a.rs` sees the edit; two read-only calls never conflict. Calls that conflict run in the model's order.

**Running.** `RunCx` is what one call runs with: its cancellation token, the resolver, the session's content store and table of seen files, the output limits, the walk gate, and what a session may or may not have (an outline source, the images the model accepts, the turn number). Everything in it is shared by handle, so a tool moves what it needs onto the blocking pool: file reads, walks, searches and diffs all run there, through one helper that takes the token and returns as soon as the token fires.

- A tool is run with `execute`, never by calling `run`: `execute` is where the output policy applies, so every result that reaches the loop is bounded.
- A cancelled call still ends with a result, whose outcome is `interrupted`. A failure is a result too. A plan handed to a tool that did not make it is an error result, not a panic.

## What every tool shares

**The result.** A tool returns content for the model, an optional structured payload for front ends (a diff, an exit code, match counts), and how the call ended: success, error, denied or interrupted. Anything but success is an error to the model. Failures are written so the model can correct itself: what was wrong, and what to try.

**Arguments are checked before a tool sees them,** against the tool's JSON Schema. The validator is our own and covers the part of JSON Schema that tool definitions use (types, required and unknown properties, enumerations, bounds, lengths, items, `anyOf`, `oneOf`, `allOf`, `not`, and references inside the schema). A mismatch names the argument and says what it should have been, and several are reported together. A keyword the validator does not know is ignored, not refused: the schemas of MCP and extension tools are written by others, and a call must not be refused for a keyword we have not heard of.

**One output policy.** Every result passes through the same truncation step before it enters the context.

| Limit | Default | Notes |
|---|---|---|
| Size of model-visible content | 32 KiB | Configurable, and never under 4 KiB. |
| Lines of model-visible content | 800 | Whichever limit is hit first applies. Never under 8. |
| Line length | 2,000 characters | Longer lines are cut with a marker that says how many characters are missing. Never under 256, and never so long that one line could take more than a quarter of the size limit. |

- **Compression comes first, for output that was produced.** Before the limits apply to the output of a command or of an extension or MCP tool, control sequences are removed, progress redraws collapse to their final state, and runs of identical lines collapse to one line and a count. Runs of lines that differ only in their numbers collapse to their first and last line and a count, but only in output that is over the limits, because that loses text. For commands the project profile knows as builds or tests, failure and error lines are kept ahead of routine ones. Details in [15-efficiency.md](15-efficiency.md).
- **Content that was asked for is never compressed.** A file read returns the lines exactly as they are in the file.
- Truncation always happens at line boundaries. A line is kept whole or left out, and where lines are left out a marker stands in their place with their number. The one cut inside a line is the line-length limit, which is announced on the line itself, and falls between characters, never inside one.
- What is kept depends on the tool: the head for file reads, the head and the tail (a quarter and three quarters) for command output and for whatever a tool returns unshaped, the first matches for search.
- When content is cut, the full output is stored as an artifact in the session directory, and the result ends with a notice that states what was cut and how to get the rest: the artifact's id, which `read` and `search` accept in their `artifact` argument. In the TUI the full output is one key press away.
- **An artifact is named by its id, never by a path.** Its file is in the session directory, outside the workspace: a path to it would be long, would show the user's home directory, and would look like any other outside path to the permission check. And a made-up path form could be the name of a real file, which is the ambiguity the path rules refuse. So `read` and `search` take either a `path` or an `artifact`, and never both.
- A tool that knows its own output shapes it itself and says what it left out in its own terms: `read` gives the next `offset`, `search` and `list` say how many more there are. These store no artifact, because the rest is one call away.
- Truncation happens once, when the result is created. A result within the limits passes unchanged, so applying the policy twice changes nothing, and a stored result is never shortened later (invariant 1).
- An image a tool returns is checked against what the model takes. One it does not take (no image support, another type, too large) is replaced by a line that says so.

**Paths.** Tools take paths relative to the workspace root, absolute paths, or paths that begin with `~` for the home directory. One resolver in `cortex-base` reads all of them, by the rules below. It was built in step 4.

- Both `/` and `\` are accepted as separators on Windows. Paths shown to the model use `/`: relative to the workspace when inside it, absolute otherwise.
- A path is resolved to a canonical absolute form before any check. Symbolic links, and on Windows junctions, are resolved, and the permission check applies to the target. The part of a path that exists is replaced by its real location, spelled on Windows as the file system spells it. The part that does not exist yet (a file about to be created) is kept as written.
- **Links are followed by the harness, one at a time, never by the OS on its behalf.** Each link's target is read and checked by these same rules before it is followed, so a link cannot lead anywhere the rules have not seen first. A link to a refused place is refused where the link is.
- A path written with a separator at its end (`out/`) names a directory. If it leads to an existing file it is refused, and if it does not exist yet the tool is told, so that it does not create a file there.
- `..` means what the OS makes it mean. On Linux it is the parent of the real directory, so `link/..` is the parent of the link's target. On Windows it is resolved as text before anything is opened.
- On Windows, comparison is case-insensitive and the on-disk spelling is kept. Short names (`PROGRA~1`) resolve to the long ones, so a rule on the long name cannot be dodged through the short one.
- Paths outside the workspace are allowed only by permission (see [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). Resolving one is not an error: the resolver says where it leads and that it is outside.

**A path that could mean two things is refused, not guessed at,** by the reasoning of [decision 0028](../decisions/0028-enforced-values-fail-closed.md): a permission rule is matched against a path, so a second spelling of a protected file is a way round the rule. Each refusal has its own error, written for the model.

| Refused | Where | Why |
|---|---|---|
| An empty path, or one with a NUL character | Everywhere | Neither names a file. |
| A path, or a real location, that is not valid Unicode | Everywhere | Rules match text, and a lossy conversion would make two different files look alike. The refusal is of that one path: a directory walk reports the entry and goes on. |
| A symbolic link whose target does not exist | Everywhere | Writing "through" it would create a file at a place the path does not show. |
| Device names: `NUL`, `CON`, `PRN`, `AUX`, `COM1` to `COM9`, `LPT1` to `LPT9` (and `0`, and the superscript digits), `CONIN$`, `CONOUT$`, with any extension | Windows | They are devices in every directory. |
| The device and extended-length forms, `\\.\…` and `\\?\…` | Windows | They bypass the rules that the rest of this list depends on. |
| A name that ends with a dot or a space | Windows | Windows drops them: `secret.env.` opens `secret.env`. |
| A colon in a name, and `< > " \| ? *` and control characters | Windows | A colon selects an alternate data stream: `secret.env::$DATA` is the content of `secret.env`. The rest are wildcards or not allowed in names. |
| `C:name` and `\name` | Windows | They are relative to a current directory or drive that nobody stated. |
| A network path, `\\server\share\…`, unless configuration allows that server and share | Windows | Reaching for a share sends the account's credentials to whoever answers. The refusal is made on the text, before the network is asked anything, and that holds for a link in the workspace that points at a share: its target is read, not followed. A drive letter the user has mapped to a share is refused too, on the resolved path; by then the share has been asked, and it is one the user connected themselves. |
| A directory that is a mounted volume with no drive letter | Windows | Its real location has no ordinary path to check. |

The Windows rows apply on Windows only. On Linux `notes:2026` and `name.` are ordinary file names.

**What resolution does not promise.**

- **That nothing changes afterwards.** Resolving a path and opening it are two steps, and the file system can change between them: a directory can be swapped for a link after the check. The harness opens the location it checked, never the text it was given, which closes the gap for everything that is not actively racing it. A process that is racing it is the sandbox's to confine ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)).
- **That one file has one path.** A file with a second hard link has two, and both are real. And Linux is taken to compare names exactly: on a mount that ignores case there (a FAT or NTFS volume, a directory with case folding on), two spellings of one file resolve to two paths. A rule that protects a file by name does not protect its other names.
- **That its case folding is exactly Windows's.** Windows compares names by a table of its own. The harness folds case by the same rule with the Rust standard library's tables, which differ for a few letters outside ASCII. Inside or outside the workspace does not depend on it: that is decided on the file system's own spelling, compared exactly. Rule matching does depend on it ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)), so there a deny may match a little too much and an allow must not.

- **A directory walk names every entry through the same resolver.** An entry whose name the rules refuse (one that is not valid Unicode, and on Windows a device name or a name that ends with a dot, which exist on disk when something made them through an extended-length path) is stepped past, with everything under it if it is a directory, and the result says how many entries were skipped and why the first was. The walk never stops for one name.
- **A walk does not follow links.** It reports a link and goes on. `search` counts the links it left, and `list` shows a link as what it leads to, found by the resolver's rules, without going in. A link named as the `path` of a call is followed by the resolver like any other.

**Text files.** Tools read and write UTF-8. A file that is not valid UTF-8 is refused by the editing tools with a message that says so. `read` shows such a file with the bytes that are not UTF-8 replaced, and a note that the editing tools will refuse it. Line endings and a byte-order mark are detected on read and preserved on write, and are not part of a line as `read` shows it.

**File snapshots.** When a tool reads or writes a file it records the file's size, modification time and content hash in the session's table of seen files. The hash is what decides whether a file has changed: it is `ContentHash` of `cortex-base` (SHA-256). The editing tools use the table for the staleness guard below, and `read` for changed-only re-reads. The table also remembers which lines of each file the model has been shown in the current context epoch; a new epoch forgets that and keeps the snapshots, because the guard is about the session.

**The content store.** Artifacts and file versions go through one trait, which the session implements. An artifact is written in pieces, so output still arriving never has to be held in memory. A file version is the content of a file as the model last saw it, kept under its hash, so that a later read can send a diff. Only files of up to 1 MiB are kept as versions.

**Checkpoints.** Before a tool changes or creates a file, the previous content (or the fact that the file did not exist) is saved in the session's checkpoint store. The checkpoints of one turn form that turn's snapshot. This is what lets edits apply without asking: every one of them can be undone (see [07-sessions-and-context.md](07-sessions-and-context.md)).

**Atomic writes.** New content goes to a temporary file in the same directory, is flushed, and is renamed over the target. On Windows the replace is retried briefly when another process holds the file open (six more attempts over half a second), and fails with a clear message if it stays locked. File permissions are preserved: the new file takes the mode of the one it replaces, and the temporary file is private until it does.

- A target that could not have been written in place is refused. A rename needs only the right to change the directory, so without this it would replace a file its owner had protected. Refused: a file marked read-only (on Linux one with no write permission at all, on Windows one with the read-only attribute), and on Linux also a file the user may not open for writing.
- Only a regular file is replaced. A directory, a device, a socket or a pipe under the target's name is refused, not swapped for a file.
- A symbolic link is written through: the file it points to is replaced, and the link stays.
- Not preserved: hard links (the file's other names keep the old content), the owner of a file that belonged to someone else, and on Windows the hidden and system attributes and any access rules set on the file itself, as opposed to inherited from its directory.

## `read`

Reads a text file, or part of it.

| Argument | Type | Meaning |
|---|---|---|
| `path` | string | The file. |
| `artifact` | string | In place of `path`: the id of a stored full output, from the notice of a cut result. |
| `offset` | integer, optional | First line to return, counted from 1. |
| `limit` | integer, optional | Number of lines. |
| `fresh` | boolean, optional | Return the content even if it was already read. |

- Exactly one of `path` and `artifact` is given.
- Returns a one-line header (path, the line range returned, the total line count) and then the lines, without line-number prefixes. Search results carry line numbers, and `offset` takes one, so the model can target a range without paying for a prefix on every line or copying prefixes into edits.
- Reads from disk as a stream, once: the same pass hashes the file, counts its lines and keeps the lines asked for. A large file never has to fit in memory, and neither does a very long line, of which only as much is kept as the line limit can show.
- When the range is cut by the output policy, the notice gives the next `offset`.
- **A large file is introduced by its outline.** When a file is over the output limit and no range was asked for, the result is the file's outline with line numbers ([15-efficiency.md](15-efficiency.md)) followed by the first lines, so the model can ask for the part it needs. The outline takes at most a quarter of the room. It comes from the repository index through a trait; until the index exists, such a file is returned as its first lines.
- **A re-read sends only what changed.** If the model has already been given these lines in the current context epoch: an unchanged file returns one line saying so, with the lines it has and when it read them; a changed file returns a diff against what the model last saw, when the diff is smaller than the content. `fresh: true` forces the content. The exact rules are in [15-efficiency.md](15-efficiency.md).
- A binary file is refused with its size and detected type: a file is binary when its first 8 KiB have a NUL byte or begin as a known binary format. Images in formats the model accepts (PNG, JPEG, GIF, WebP) are returned as image parts when the model takes images and the file is under the provider's size limit. The image is stored as an artifact and the part refers to it, so that the session log does not carry it. Resizing is out of scope for the first release.
- A missing file returns an error that lists close matches in the same directory: the same name in another case, a name a few letters away, or the same name with another extension. When a directory on the way is what is missing, the error says which.
- A directory, and anything that is not a regular file, is refused with what to use in its place.
- An artifact is read with the same ranges and limits. It is not a seen file: reading it twice sends it twice.
- Effects: one read of the resolved file. None for an artifact, which is the session's own record of what a tool already returned.
- Concurrency: read-only.

## `write`

Creates a file or replaces its whole content.

| Argument | Type | Meaning |
|---|---|---|
| `path` | string | The file. Parent directories are created. |
| `content` | string | The full new content. |

- **Staleness guard.** If the file exists, it must have been read in this session and must not have changed on disk since. Otherwise the call fails and tells the model to read the file first. This stops the model from overwriting work it has not seen. Neither reference harness has this guard on its edit path.
- When replacing a file that uses CRLF line endings, the new content is written with CRLF.
- Returns a short confirmation (lines written, lines added and removed). Front ends receive the diff as a structured payload.
- Concurrency: keyed by path.

## `edit`

Replaces exact pieces of text in a file.

| Argument | Type | Meaning |
|---|---|---|
| `path` | string | The file. |
| `edits` | array | One or more `{ old, new, count }`. `count` (optional, default 1) is how many times `old` must occur. |

- Every `old` is matched against the file as it was before the call, not after earlier edits in the same call. Edits must not overlap. Either all apply or none do.
- **Matching, in two tiers.**
  1. Exact match.
  2. If there is no exact match: a line-wise match that ignores trailing whitespace, line-ending differences and a uniform difference in leading indentation. It is accepted only if it finds exactly `count` places. The result then says that the tolerant match was used and shows the text actually replaced.
  There is no further fuzzy matching. No Unicode folding, no edit-distance search. A longer ladder of fallbacks makes more edits succeed and also makes a wrong edit more likely. This is revisited with eval data.
- **On failure** the result shows the closest region of the file and how it differs from `old`, so the model can fix the call in one step. Too many or too few occurrences are reported with their line numbers.
- **Staleness guard:** as for `write`.
- Line endings and a byte-order mark are preserved. A `new` text with LF endings is converted when the file uses CRLF.
- Returns a short confirmation with the changed line ranges. Front ends receive the unified diff as a structured payload. The model does not get the diff back, because it already knows what it asked for.
- Concurrency: keyed by path.

## `shell`

Runs a command in the platform shell and returns its output.

| Argument | Type | Meaning |
|---|---|---|
| `command` | string | The command line. |
| `cwd` | string, optional | Working directory. Defaults to the workspace root. |
| `timeout_ms` | integer, optional | Default 120,000. Capped by configuration (default cap 30 minutes). |

**Which shell.** The model is told, in the tool description, which shell and version it is writing for.

| OS | Default | Fallback | Configurable |
|---|---|---|---|
| Linux | `bash` with no profile or rc files | `sh` | `shell.program`, `shell.args` |
| Windows | PowerShell 7 (`pwsh`) with no profile | Windows PowerShell 5.1, then `cmd.exe` | The same. `cmd.exe` and Git Bash can be chosen. |

All three Windows shells are supported and tested: each has its own quoting, its own way to merge the output streams, and its own exit-status rules, and the tool handles each explicitly. Nothing requires a Unix-style shell on Windows.

**Environment.** Inherited from the harness, with a deny-list of variables that are removed (provider credentials by default; the list is configurable), plus settings that make tools non-interactive: no pager, no colour unless asked, a dumb terminal type. Standard input is closed. There is no pseudo-terminal: output comes through pipes, with stdout and stderr merged in arrival order.

**Process tree.**

| | Linux | Windows |
|---|---|---|
| Start | New session and process group. The child is told to die if the harness dies. | New process group inside a Job Object set to kill its members when closed. |
| Polite interrupt | SIGINT to the group | Ctrl-Break event to the group |
| Terminate after grace (default 2 s) | SIGTERM to the group | n/a |
| Hard kill after a second grace | SIGKILL to the group | Terminate the Job Object |

The whole tree is cleaned up on timeout, on cancellation, and when the harness exits for any reason, including a crash.

**Output.**

- Streamed to front ends as it arrives, coalesced to one update per frame.
- Held in a bounded buffer: the first 8 KiB and the last 24 KiB. Once output passes the buffer, all of it is also written to an artifact file.
- The result contains the kept head and tail with a marker between them, the exit code, the duration, and flags for timeout and interruption. An exit by signal is reported as such.
- A command that exits while background children keep the pipes open is detected: the tool stops waiting after a short grace and says what happened.

**Sandbox.** The command is started through the sandbox launcher with the session's policy (see [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). When the sandbox blocks something, the result says which rule blocked it, so the model does not retry blindly.

Concurrency: exclusive, unless the policy engine has classified the command as read-only.

After the first release, when the command is exactly one of the project's known checks and speculative verification has already run it against the current state of the files, that result is returned with a note saying so ([15-efficiency.md](15-efficiency.md)).

Long-lived background processes (development servers, watchers) are not in the first release. The design leaves room for a handle-based variant of this tool.

## `search`

Searches file contents with a regular expression.

| Argument | Type | Meaning |
|---|---|---|
| `pattern` | string | Regular expression (Rust regex syntax). |
| `path` | string, optional | File or directory to search. Defaults to the workspace root. |
| `artifact` | string, optional | In place of `path`: the id of a stored full output to search. |
| `glob` | string, optional | Only files matching this glob. |
| `ignore_case` | boolean, optional | |
| `context` | integer, optional | Lines of context around each match (default 0, maximum 5). |
| `mode` | `"lines"`, `"files"` or `"count"`, optional | Matching lines (default), names of files with matches, or matching lines per file. |
| `max_results` | integer, optional | Default 100, at most 2,000. Matches in `lines` mode, files in the other two. |

- Implemented in-process with the ripgrep libraries (`ignore`, `grep-searcher`, `grep-regex`, `grep-matcher`, `globset`). No external program is needed, which matters on Windows. See [decision 0014](../decisions/0014-search-in-process.md).
- A pattern is matched within one line, and `^` and `$` are the start and end of a line with either line ending. A pattern that is not a regular expression, or that could only match across lines, is refused when the call is planned, with the reason.
- Honours `.gitignore`, `.ignore`, `.git/info/exclude` and hidden-file rules, at every level, including ignore files above the directory searched. A `.gitignore` counts whether or not the tree is a git repository. The user's global git ignore file is not read: it is the machine's, not the project's. Skips binary files (a file with a NUL byte). A file named as `path` is searched even where a walk would leave it out.
- **A glob with no `/` matches a name at any depth** (`*.rs`). One with a `/` matches the path below the directory searched (`src/**/*.rs`). `*` stops at a separator and `**` does not. On Windows a glob may be written with either separator and ignores case.
- Output is grouped by file, with line numbers, in path order so results are deterministic whatever order the walk's threads find files in. A match is a matching line. A line of context is marked with `-` after its number where a match has `:`. When the cap is hit, the result says how many more matches exist: the search goes on counting past what it shows. The header gives the totals.
- **Memory does not grow with the number of matches.** Only as many matches as the result can show are kept, in path order, while the search runs; the rest are counted.
- Runs on the blocking pool with a parallel directory walk, and stops when cancelled: each thread checks before each file, and the call returns at once.
- A walk steps past what it may not use and says so (above, "Paths"): refused names, entries the permission rules protect, files that cannot be opened, and links.
- Effects: a walk of the tree for a directory, a read for a file, none for an artifact.
- Concurrency: read-only.

## `list`

Lists a directory, or finds files by glob.

| Argument | Type | Meaning |
|---|---|---|
| `path` | string, optional | Directory. Defaults to the workspace root. |
| `glob` | string, optional | For example `**/*.rs`. Without it, the directory's entries are listed. |
| `depth` | integer, optional | Maximum depth. Default 1 without a glob, unlimited with one. |
| `max_results` | integer, optional | Default 200, at most 5,000. |
| `include_ignored` | boolean, optional | Also list hidden files and files that ignore files leave out. |

- Returns relative paths, one per line, directories marked with a trailing `/`, sorted by path. A path is relative to the workspace, not to the directory listed, so that it can be passed to another tool as it is. A header line gives the number of entries, and when the cap or the output limit is reached, a note says how many more there are.
- Honours the same ignore rules as `search`, and reads a glob the same way. `include_ignored` includes ignored and hidden files.
- A glob matches directories as well as files.
- One tool covers both listing and finding, so the model has one less choice to make.
- A link is listed as what it leads to (a directory gets its `/`), and is not gone into.
- Effects: a read of the directory for one level, and a walk of its tree for more.
- Concurrency: read-only.

## `outline`

Returns the structure of a file or directory without its content.

| Argument | Type | Meaning |
|---|---|---|
| `path` | string | A file or a directory. |
| `depth` | integer, optional | For a directory: how deep to list. |

- For a file: its definitions with line numbers. For a directory: the outlines of its files, up to the output limit.
- Backed by the cached index in [15-efficiency.md](15-efficiency.md). Outlines are approximate, and the result says so for languages without an extractor.
- Concurrency: read-only.

## `plan`

Keeps the plan for the current work: a task list that the user can see and change while the agent works.

| Argument | Type | Meaning |
|---|---|---|
| `items` | array | The full list: `{ id, text, status }`, where `status` is `pending`, `active`, `done` or `skipped`. Each call replaces the list. |

- Stored in the session. Shown in the TUI's plan panel. Carried across compaction mechanically, not through the summary.
- **The user can edit it.** Reordering, skipping, rewording, adding or removing an item in the panel (or through the RPC protocol) updates the stored plan at once. The change reaches the model as an appended notice at the next safe boundary, saying exactly what changed. The tool's next result always returns the current plan, so the model never works from a stale copy.
- No side effects outside the session. Concurrency: read-only.

## `remember`

Proposes a fact or convention for the project's memory.

| Argument | Type | Meaning |
|---|---|---|
| `kind` | `"fact"` or `"convention"` | |
| `text` | string | One or two sentences. |
| `evidence` | string | Where this was learned: a path and line, a command, or the user's words. |
| `check` | object, optional | A read-only test that shows the entry still holds: `exists`, `contains` or `command` ([14-adaptation.md](14-adaptation.md)). It is stored and shown with the proposal. Checks are run from the checked-memory step onward. |

- The entry is stored as proposed, with its source, and becomes active only when the user accepts it ([14-adaptation.md](14-adaptation.md)).
- Concurrency: read-only.

## `tool_search` and `tool_call`

Tools that are not built in (MCP tools and extension tools) are not declared to the model one by one. They are found and called through this fixed pair, so the declared tool list never changes during a context epoch, however many tools exist and whenever they appear.

| Tool | Arguments | Returns |
|---|---|---|
| `tool_search` | `query` (words, or a `source/` prefix), `limit` | Matching tools: full name, description and argument schema. |
| `tool_call` | `tool` (full name), `arguments` (object) | The tool's result. |

- **The registry.** MCP servers and extensions register their tools with the dynamic-tool registry, each under the name of its source. A tool's full name is `source/tool`. A source is replaced whole, so a server whose tool list changes is never seen half-updated.
- **`tool_search` is a ranked match done in the harness,** with no model call. A tool matches when every word of the query is in its name, its source's name or its description, and a word in the name counts for more. `limit` is 8 unless given, at most 25. A search that finds nothing lists the sources.
- Descriptions and schemas in a search result are third-party text. The result says so, and a description is cut at 2 KiB.
- Arguments are validated against the tool's schema before the call. A mismatch is an error result that includes the schema, and the tool is not run.
- The call is planned, checked by the permission engine and bounded by the output policy exactly like a built-in tool. The plan of a `tool_call` carries the tool's own effects and its own scheduling, and besides them an `ext` effect that names the tool, which is what a rule on `source/tool` matches. `tool_call` asks by default; `tool_search` touches nothing.
- A tool that is not there is an error with the closest names.
- A new tool, including one the user just asked the harness to write, is usable immediately, with no change to the request prefix. The definitions of the pair are fixed text: nothing in them depends on what is registered.
- A source can be set to expose chosen tools directly instead. See [13-mcp.md](13-mcp.md) and [12-extensions.md](12-extensions.md).

## In the first usable release, and after it

| Tool | First usable release | Notes |
|---|---|---|
| `read`, `write`, `edit`, `shell`, `search`, `list` | yes | The required set. |
| `outline` | yes | Lean context: structure before content. |
| `plan` | yes | The live, editable plan. |
| `remember` | yes | Project memory. |
| `tool_search`, `tool_call` | yes | MCP and extension tools. |
| `propose_extension` | yes | The agent writes an extension for the user to review ([12-extensions.md](12-extensions.md)). It only proposes; nothing loads without approval. |
| A second edit format (`patch`) | after | Needed before edit formats can be chosen per model. See [14-adaptation.md](14-adaptation.md). |
| `fetch` (retrieve a URL as text) | after | Needs an HTML-to-text step and the injection and SSRF defences in [../security.md](../security.md). |
| `task` (sub-agents) | after | Needs budgets and a session-tree UI. |
| Background processes for `shell` | after | |
| Web search | after | Through provider-side tools where the provider has them. |
| Image resizing for `read` | after | |

## Windows differences in one place

| Topic | Behaviour |
|---|---|
| Path separators | `/` and `\` accepted. `/` shown. |
| Case | Comparisons are case-insensitive. The on-disk case is kept. |
| Line endings | Detected and preserved per file. New files use LF unless configuration says otherwise. |
| Shell | PowerShell by default, with `cmd.exe` supported. The model is told which. |
| Process tree | Job Objects. |
| Locked files | Replace is retried briefly, then fails with the reason. |
| Long paths | Handled internally with extended-length paths. Not shown to the model. |
| Reserved names and device paths | Rejected. |
| Trailing dots and spaces, alternate data streams | Rejected. |
| Drive-relative and root-relative paths (`C:name`, `\name`) | Rejected. |
| Network paths | Rejected unless the server and share are allowed in configuration. |
| Junctions and directory links | Resolved like symbolic links. The check applies to the target. |
| Short (8.3) names | Resolved to the long name. |
| Names on disk that the rules refuse (made through an extended-length path) | A walk steps past them and says so in its result. |
| Globs | Either separator accepted. Matching ignores case. |
| Script shims (`.cmd`, `.bat`) | Never executed directly with untrusted arguments. See [12-extensions.md](12-extensions.md). |

Each row has tests that run on the Windows CI job (see [../testing-and-ci.md](../testing-and-ci.md)).
