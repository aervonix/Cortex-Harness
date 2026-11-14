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

    /// What a planned call would change in a file, for an approval: work that reads
    /// the file and returns the change as a bounded diff. `None` by default.
    fn preview(&self, plan: &Plan, cx: &PlanCx) -> Option<Preview> { None }
}

pub struct Plan {
    pub effects: Vec<Effect>,          // Read | ReadTree | Write | Exec | Network | Ext
    pub concurrency: Concurrency,      // ReadOnly | Keyed(paths) | Exclusive
    pub summary: String,               // one line for approval prompts and the UI
    pub changes_session: bool,         // the call changes the plan or the proposals: see below
    // plus the validated arguments, private to the tool
}

/// The only way a tool is run: `run`, then the output policy.
pub async fn execute(tool: &dyn Tool, plan: Plan, cx: &RunCx, out: &mut dyn ToolOutput)
    -> ToolResult;

/// The output policy for a result that no tool run produced: a plan error, a denial.
pub async fn bound(result: ToolResult, tool: &str, cx: &RunCx) -> ToolResult;
```

The two phases exist so the permission engine, the sandbox and the scheduler can act on declared effects before anything runs (see [03-agent-loop.md](03-agent-loop.md)). Built-in tools, extension tools and MCP tools all implement this trait, so one pipeline applies to all three. It was built in step 6, in `cortex-tools`.

**The preview** (added after the review of step 13a). `write` and `edit` answer `preview` with the change they would make, worked out as `run` would make it (the line endings a write keeps, the tolerant tier of an edit's matching) and changing nothing, so that an approval for one shows the change itself: `approval/request`'s `diff` detail ([11-rpc-protocol.md](11-rpc-protocol.md)). The loop runs the preview on the blocking pool just before it asks, and only for a call it asks about. The diff has the limit of a result's payload, 256 KiB: a larger one is left out with a sentence that says so and the file's path, and so is a change that cannot be worked out before the call runs (an edit whose text is not there), with the reason the call will give. A file the read gate keeps from the agent (one that looks like a secret, one a rule asks about or denies) is not shown either: a question goes to every front end of the session and is not filtered, so its diff is left out, with a sentence saying why. The preview does not ask the table of seen files: the staleness guard is the call's, when it runs.

**Planning.** `plan` checks the arguments against the tool's schema, resolves every path through the one resolver (below), and returns the plan or an error written for the model. It changes nothing. It may ask the file system where a path leads, which blocks for a few system calls, so the loop calls it through the blocking helper. `PlanCx` holds the path resolver and nothing else.

**Effects.** Each is one thing the call would do, and the permission engine decides each ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)).

| Effect | What it carries | Declared by |
|---|---|---|
| `Read` | A resolved path: one file, or the names in one directory | `read`, `list` of one level, `search` of one file |
| `ReadTree` | A resolved path: the tree under a directory | `search` of a directory, `list` with a glob or a depth |
| `Write` | A resolved path | `write`, `edit` |
| `Exec` | The command line and its working directory | `shell`, beside a `Write` of the workspace root |
| `Network` | A host, or any | `fetch`, MCP over HTTP |
| `Ext` | The full name of an extension or MCP tool | `tool_call`, beside whatever effects the tool itself declares |

- A path in an effect is the resolver's result: the real location, with every link followed, and whether it is inside the workspace. A rule is matched against that, never against what the model wrote.
- A plan with no effect but reads has no side effects, and is what the loop may start early ([15-efficiency.md](15-efficiency.md)). A call of an extension or MCP tool is never one.
- **A call that changes the session says so on its plan** (`Plan::changes_session`, added in step 10 as an agreed addition to the contract). `plan` and `remember` touch nothing outside the session, so they have no effect to declare and are scheduled as read-only. Without the marker the loop would start them while the model is still writing, and a response that was then thrown away would leave a replaced plan or a proposed memory behind. The loop reads the marker beside `has_side_effects`: such a call never starts before its response has ended. It is not an effect: there is nothing for the permission engine to decide. The plan of a `tool_call` carries the marker of the tool it wraps.
- **A walk is checked twice.** `ReadTree` is decided once, before the walk. But a rule can protect one file inside an allowed tree, and the tree cannot be listed in advance. So a walk asks a gate (`ReadGate`, which the permission engine supplies) about each entry it meets, steps past what it may not read, and says in its result how many entries that was. Walks do not follow links, so they stay under the path they declared. The engine's gate (`Engine::may_read`, step 12) lets an entry through unless a rule denies or asks about it, or it looks like a secret and no rule allows it: an entry that would have to be asked about is stepped past, because a walk asks nobody. It does not ask about the scope again, which the tree's own decision settled.

**Concurrency.** A tool's spec has a class, and each plan says how that call is scheduled, with the paths of a keyed call. `Plan::conflicts_with` is the rule the scheduler applies:

- an exclusive call conflicts with every call;
- two keyed calls conflict when they share a path, or when a path of one is inside a path of the other;
- a keyed call conflicts with a read-only call that reads one of its paths, a directory that holds one, or something inside one, so that `edit a.rs` followed by `read a.rs` sees the edit, and `write src/new.rs` followed by `list src` sees the file;
- two calls that both change the session conflict, so that of two `plan` calls in one response the second is the one that stands. Beside any other call they run as read-only calls do;
- a read-only call with an `exec` effect conflicts with every keyed call. That is a shell command the permission engine has marked read-only: its plan cannot say which files it reads, so it is taken to read all of them, and `edit a.rs` followed by `cat a.rs` sees the edit (step 12);
- two read-only calls never conflict otherwise.

Calls that conflict run in the model's order.

**Running.** `RunCx` is what one call runs with: its cancellation token, the resolver, the session's content store and table of seen files, the output limits, the walk gate, and what a session may or may not have (an outline source, the images the model accepts, the turn number, the round number).

- **The round number** (`RunCx::round`, added in step 10) is the number of the tool round a call belongs to: the calls of one response of the model. Two calls with the same number were written in one response, so neither was written with the other's result in view. A call with a higher number was written after the results of every earlier round had been sent to the model: the loop moves the number on only when a round's results have been recorded ([03-agent-loop.md](03-agent-loop.md), "Tool rounds"), and a request that is sent again keeps its number. 0 means the caller did not say, and a tool must then assume nothing. `plan` is the tool that needs it ("`plan`", below). Everything in it is shared by handle, so a tool moves what it needs onto the blocking pool: file reads, walks, searches and diffs all run there, through one helper that takes the token and returns as soon as the token fires.

- A tool is run with `execute`, never by calling `run`: `execute` is where the output policy applies, so every result that reaches the loop is bounded.
- A result that no run produced passes the same policy through `bound`: the error of a call that could not be planned, or a denial. Such an error can be long, because one for arguments that do not fit quotes a third-party schema.
- A cancelled call still ends with a result, whose outcome is `interrupted`. A failure is a result too. A plan handed to a tool that did not make it is an error result, not a panic.

## What every tool shares

**The result.** A tool returns content for the model, an optional structured payload for front ends (a diff, an exit code, match counts), and how the call ended: success, error, denied or interrupted. Anything but success is an error to the model. Failures are written so the model can correct itself: what was wrong, and what to try.

**Arguments are checked before a tool sees them,** against the tool's JSON Schema. The validator is our own and covers the part of JSON Schema that tool definitions use (types, required and unknown properties, enumerations, bounds, lengths, items, `anyOf`, `oneOf`, `allOf`, `not`, and references inside the schema). A mismatch names the argument and says what it should have been, and several are reported together. A keyword the validator does not know is ignored, not refused: the schemas of MCP and extension tools are written by others, and a call must not be refused for a keyword we have not heard of. **The whole schema is enforced here even where less of it is sent:** a provider's strict mode is sent each tool reduced to what it takes, the bounds and lengths said in words instead (step 13b, [04-providers.md](04-providers.md), "Tools"), and the validator still checks every bound and length the tool declares.

**One output policy.** Every result passes through the same truncation step before it enters the context.

| Limit | Default | Notes |
|---|---|---|
| Size of model-visible content | 32 KiB | Configurable, and never under 4 KiB. |
| Lines of model-visible content | 800 | Whichever limit is hit first applies. Never under 8. |
| Line length | 2,000 characters | Longer lines are cut with a marker that says how many characters are missing. Never under 256, and never so long that one line could take more than a quarter of the size limit. |

- **Compression comes first, for output that was produced.** The output of a command or of an extension or MCP tool is compressed whether or not it is over the limits: control sequences are removed, progress redraws collapse to their final state, and runs of identical lines collapse to one line and a count. Runs of lines that differ only in their numbers collapse to their first and last line and a count, but only in output that is over the limits, because that loses text. For commands the project profile knows as builds or tests, failure and error lines are kept ahead of routine ones. Details in [15-efficiency.md](15-efficiency.md).
- **Content that was asked for is never compressed.** A file read returns the lines exactly as they are in the file. What one of the harness's own tools returns is left as it is when it fits; only a result over the limits is treated as produced output.
- **A result holds text and images.** Any other kind of part is left out with a line that says so, and a provider's raw block on a text part is dropped. Neither is counted by the limits, so neither may ride along.
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
- **What is done about the gap, since step 10.** The last name of a path is never followed and never waited on when it is opened (`cortex_base::open_regular`): the open asks for the thing itself (`O_NOFOLLOW` and `O_NONBLOCK` on Linux; the reparse point, not its target, on Windows), and the handle is then asked what it is. So a file that was swapped for a link, or for a FIFO whose open would wait for ever, after it was looked at is refused, and what is read is the thing that was checked. `read`, the editing tools and the session's checkpoint store all open this way. The editing tools also resolve their path a second time when the call runs, which may be long after it was checked (an approval lies between), and stop if it leads anywhere else; and they write with a replace that follows no link. A directory earlier in the path that is swapped at exactly the right moment is still followed: that remains the sandbox's.
- **Placeholders on Windows.** A file in a folder synced with a cloud service, or projected by a virtual file system, is a reparse point that is not a link. Such a file is opened a second time in the ordinary way, so that the filter that owns it supplies its content, and between the two opens it could be swapped for a link. This is done only for a short list of reparse tags that mark such files; a link-like tag (a symbolic link, a junction, a WSL link, an app execution alias, the NFS and DFS tags) and any tag this build does not know are refused.
- **That one file has one path.** A file with a second hard link has two, and both are real. And Linux is taken to compare names exactly: on a mount that ignores case there (a FAT or NTFS volume, a directory with case folding on), two spellings of one file resolve to two paths. A rule that protects a file by name does not protect its other names.
- **That its case folding is exactly Windows's.** Windows compares names by a table of its own. The harness folds case by the same rule with the Rust standard library's tables, which differ for a few letters outside ASCII. Inside or outside the workspace does not depend on it: that is decided on the file system's own spelling, compared exactly. Rule matching does depend on it ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)), so there a deny may match a little too much and an allow must not.

- **A directory walk names every entry through the same resolver.** An entry whose name the rules refuse (one that is not valid Unicode, and on Windows a device name or a name that ends with a dot, which exist on disk when something made them through an extended-length path) is stepped past, with everything under it if it is a directory, and the result says how many entries were skipped and gives the reason for one of them (the one that sorts first, so that the result does not depend on the order the walk's threads ran in). The walk never stops for one name.
- **A walk does not follow links.** It reports a link and goes on. `search` counts the links it left, and `list` shows a link as what it leads to, found by the resolver's rules, without going in. A link named as the `path` of a call is followed by the resolver like any other.

**Text files.** Tools read and write UTF-8. A file that is not valid UTF-8 is refused by the editing tools with a message that says so. `read` shows such a file with the bytes that are not UTF-8 replaced, and a note that the editing tools will refuse it. Line endings and a byte-order mark are detected on read and preserved on write, and are not part of a line as `read` shows it.

As built in step 10, the rule the editing tools keep is that **what they do not replace does not change by a byte**:

- **Neither tool converts a file.** They work on the file's own bytes, never on text that `read` has shown, so a file that is not UTF-8 cannot be written back with its bad bytes replaced: both `write` and `edit` refuse it, and the message names the line of the first bad byte and says to use a shell command. A binary file is refused the same way.
- **A byte-order mark stays** where there was one, and is not added where there was none. One at the start of a `write`'s content is not written twice.
- **A line break in new text is written as the file writes its line breaks.** In a file with one kind of ending, that kind. In a file with both: `edit` gives each line break of its new text the ending of the first line it replaces, and `write` lets every line that the old and the new content have in common keep the ending it had. A line that has no ending to go by (the last line of a file that does not end with a line break) gets the ending most of the file's lines have, and with as many of one kind as of the other, the first line's.
- **A new file is written exactly as the model gave it,** on both OSes. Over a file with no line break at all, `write` does the same: there is no ending to keep.
- **A carriage return that is not part of a line break is content,** in a file and in new text: it is matched, kept and written like any other character. Only a carriage return directly before a line feed is a line ending.
- **`write` finds the lines it keeps by comparing the old content with the new.** Where the lines between the first change and the last are more than can be compared in full (4,000 lines, old and new together), they are paired in one pass instead, each new line with the next equal old line within reach. A kept line can then, rarely, be taken for a new one and get the file's usual ending. That matters only in a file with both kinds of ending.

**File snapshots.** When a tool reads or writes a file it records the file's size, modification time and content hash in the session's table of seen files. The hash is what decides whether a file has changed: it is `ContentHash` of `cortex-base` (SHA-256). The editing tools use the table for the staleness guard below, and `read` for changed-only re-reads. The table also remembers which lines of each file the model has been shown in the current context epoch; a new epoch forgets that and keeps the snapshots, because the guard is about the session.

**The content store.** Artifacts and file versions go through one trait, which the session implements: `cortex-session`'s `DiskStore`, which the core adapts to it (`begin_artifact`, `open_artifact`, `put_version` as `put_blob`, `get_version` as `get_blob`). File versions and checkpoints share its content-addressed store ([07-sessions-and-context.md](07-sessions-and-context.md), "On-disk layout"). An artifact is written in pieces, so output still arriving never has to be held in memory. A file version is the content of a file as the model last saw it, kept under its hash, so that a later read can send a diff. Only files of up to 1 MiB are kept as versions.

**Checkpoints.** Before a tool changes or creates a file, the previous content (or the fact that the file did not exist) is saved in the session's checkpoint store. The checkpoints of one turn form that turn's snapshot. This is what lets edits apply without asking: every one of them can be undone (see [07-sessions-and-context.md](07-sessions-and-context.md)).

- **The seam.** The store is `cortex-session`'s, which `cortex-tools` cannot see. The editing tools reach it through a trait of their own crate, `Checkpoints`: `before(file, content)` with the bytes the tool read (or nothing, for a file that does not exist), and `after(file, snapshot)` with what the tool left. A write that fails is reported too, as having left the file as it was: the record of a round is never completed by a later look at the file, which could find a change someone else made in the meantime and take it for the round's. The core fills it with the checkpoint of the tool round that is running. `WriteTool` and `EditTool` are made with it (`Editing`), so a session's tools record in that session.
- **The tool hands over the bytes it read,** not a path to read again. The file is read once, what is kept for undo is exactly what the tool replaced, and there is no moment between two reads in which the file could have become something else.
- **If the earlier content cannot be kept, the file is not changed.** A change that could not be undone is not made.
- **The editing tools do not change a file over 16 MiB,** which is the most a checkpoint keeps: the change could not be undone, and a change that cannot be undone must not apply without asking. The refusal says why and names another way (a shell command). The limit also bounds what one call reads into memory and hashes: at the limit an edit takes about 50 ms, and a typical one well under a millisecond before the flush to disk ([../performance-budgets.md](../performance-budgets.md)). Asking the user, through the permission engine, in place of refusing is left for a later step ([../build-plan.md](../build-plan.md), step 10).

**Atomic writes.** New content goes to a temporary file in the same directory, is flushed, and is renamed over the target. On Windows the replace is retried briefly when another process holds the file open (six more attempts over half a second), and fails with a clear message if it stays locked. File permissions are preserved: the new file takes the mode of the one it replaces, and the temporary file is private until it does.

- **A write can ask for an owner-only file,** and then the file it leaves can be opened by the current user alone, on both OSes, whether it existed before or not: mode `0600` on Linux, and on Windows an access list with one entry and inheritance from the folder off ([decision 0029](../decisions/0029-owner-only-files-on-windows.md)). The temporary file is created that way, before any content is in it. The option only ever tightens: a file that was readable by others is private after the write. Session, credential and state files are written this way, every time, because on Windows a write without the option gives the file its folder's rules again.

- A target that could not have been written in place is refused. A rename needs only the right to change the directory, so without this it would replace a file its owner had protected. Refused: a file marked read-only (on Linux one with no write permission at all, on Windows one with the read-only attribute), and on Linux also a file the user may not open for writing.
- Only a regular file is replaced. A directory, a device, a socket or a pipe under the target's name is refused, not swapped for a file.
- A symbolic link is written through: the file it points to is replaced, and the link stays.
- **Unless the write is told that its target must not be a link** (`follow_links` off, added in step 10). Then a link under the target's name is refused and nothing on the way to the replace follows one. The editing tools write this way: the path resolver has already followed every link, so the location they write to is a file's own, and a link that appears there afterwards must not carry the write somewhere else. A rename replaces a name and never follows it, so a link swapped in at the last moment is itself replaced.
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

As built in step 10:

- **What counts as read.** A file is "what the session last saw" when its content hashes to what `read` recorded, or to what `write` or `edit` themselves last left. So a second change to a file the tools just wrote needs no read in between, and any change made by someone else since does.
- **Content that is already there writes nothing,** and the result says so.
- **A path that names a directory** (it ends with a separator, or a directory is there) is refused.
- **Parent directories are made for the write and not before it,** after the checkpoint has taken its record. If the write then fails, the directories this call made are removed again, so a call that says nothing was changed has left nothing behind. One that someone else has put something into in the meantime is left. After a write that succeeded they stay, and an undo that removes the file leaves them, empty.
- **The order of a change, for both tools,** and what each step protects (`crates/cortex-tools/src/change.rs`): the path is resolved again and must lead where it led when the call was checked; the file is opened without following a link or waiting, and read whole, up to the limit; it must be UTF-8 text; the staleness guard; the new content is worked out, and a call that fails here has touched nothing; the file is looked at once more, and must still be the one that was opened; the checkpoint is given the bytes that were read; the write is atomic and follows no link; the table of seen files and the checkpoint are told what was left.
- **A change is never abandoned half-way.** The work runs to its end whatever happens to the call's token, which is asked while `edit` looks for its text and once more just before the write, and never after. A call cancelled before that changes nothing and says so. One cancelled after it has made its change, and its result says that. So a cancelled call is over within the time of one write, and on Windows within the half second a replace waits for a file in use.
- The payload: `kind` (`create`, `write` or `unchanged`), the path, the lines and bytes written, the lines added and removed, and the diff. A diff over 256 KiB, or of versions too long to compare, is left out, and the payload says so: the payload is stored in the session log with the result.
- Effects: one `write` of the resolved file. A path through a link names the file it leads to, which is what a rule is matched against and what is checkpointed.

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
- **Both tiers match against the text as `read` shows it** (the owner, 2026-11-13, in step 10). `read` shows a line without its line ending, so an `old` the model copies from a CRLF file has line feeds. Matched against the file's bytes, every such edit of more than one line would fall to the tolerant tier and be reported, with a "text replaced" that looks identical to `old`. So the file's text is matched with every CRLF read as one line break: an `old` copied from `read` is an exact match, and is not reported. The bytes replaced are still the file's own, and the new text takes the file's line ending ("Text files", above).
- **The tolerant tier, exactly.** `old` is matched line by line against whole lines of the file. Not compared: spaces, tabs and carriage returns at the end of a line, and a uniform difference in indentation, which means that every line of the file has the same extra spaces or tabs in front of what `old` has, or every line of `old` has. Tabs against spaces is not a uniform difference. A blank line matches a blank line only, and an `old` of blank lines alone matches nothing. The new text is given the same change of indentation, so that it fits where it is put; if one of its lines has less indentation than is to be taken off, the edit is refused. Whole lines are replaced by whole lines, each ending as the lines it replaces did.
- **Counting.** `old` must occur exactly `count` times, and every occurrence is replaced. Occurrences that share text (`aa` in `aaa`) are refused, as are two edits that would replace the same text: neither has one meaning. The tolerant tier is tried only when the exact tier finds nothing at all, never to make up a number.
- **An edit that changes nothing** (`old` and `new` the same) is refused when the call is planned. Edits that would leave the file exactly as it is write nothing, and the result says so.
- **Bounds.** A call holds at most 100 edits, and a `count` is at most 10,000. A file of more than 2,000,000 lines is refused like one over the size limit: the size alone does not bound the work, because a file of 16 MiB can be sixteen million empty lines. The search can be cancelled while it runs. The closest text is worked out for the first five edits of a call that fail.
- **On failure** the result shows the closest region of the file and how it differs from `old`, so the model can fix the call in one step. Too many or too few occurrences are reported with their line numbers.
  - Every edit of the call that failed is reported, not only the first.
  - The closest region is the run of lines that shares the most text with `old` as whole lines, whatever their indentation, or failing that the run around the line most like `old`'s longest line. It is shown as a diff from `old` to the file. Where the two differ only in spaces against tabs, which a diff does not show, a line says what each is indented with. When nothing in the file is like `old`, the result says so.
- **Staleness guard:** as for `write`.
- Line endings and a byte-order mark are preserved. A `new` text with LF endings is converted when the file uses CRLF.
- Returns a short confirmation with the changed line ranges. Front ends receive the unified diff as a structured payload. The model does not get the diff back, because it already knows what it asked for.
- Concurrency: keyed by path.
- The line ranges are those of the file as it now is. The payload: `kind` (`edit` or `unchanged`), the path, the ranges, the number of replacements, whether the tolerant tier was used, the lines added and removed, and the diff, under the same size limit as `write`'s.
- A file that does not exist is an error that says to use `write`. The order of a change, cancellation, the size limit and the effects are as for `write`.

## `shell`

Runs a command in the platform shell and returns its output. Built in step 11: the tool and the runner in `cortex-tools`, and the operating-system calls for process trees in `cortex-sandbox` ([decision 0030](../decisions/0030-process-tree-control-in-cortex-sandbox.md)).

| Argument | Type | Meaning |
|---|---|---|
| `command` | string | The command line. |
| `cwd` | string, optional | Working directory. Defaults to the workspace root. It goes through the path resolver like every other path, and must be a directory that exists. |
| `timeout_ms` | integer, optional | Default 120,000. At most the configured maximum (default 30 minutes), which the argument's schema states, so a larger value is refused when the call is planned. |
| `sandbox` | boolean, optional | Since step 26. Left out, or `true`: the command runs in the sandbox where one enforces. `false`: it runs outside it, with the user's full rights, after the user approves that one call ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md), "Leaving the sandbox"). |

**Which shell.** The model is told, in the tool description, which shell and version it is writing for.

| OS | Default | Fallback | Configurable |
|---|---|---|---|
| Linux | `bash` with no profile or rc files | `sh` | `shell.program`, `shell.args` |
| Windows | PowerShell 7 (`pwsh`) with no profile | Windows PowerShell 5.1, then `cmd.exe` | The same. `cmd.exe` and Git Bash can be chosen. |

- A configured program is the one used, and nothing else: one that is not found is an error, not a reason to use another. It is a name on `PATH` or an absolute path. A relative path (`bin/bash`, `.\pwsh.exe`) is refused, with the error saying so: it would be found from wherever the harness was started.
- **Only the absolute directories of `PATH` are looked in** (since the review of step 13a). A relative entry is common (`./node_modules/.bin`) and names a directory of wherever the harness was started, which is usually the workspace: a file there named `bash` would otherwise become the shell, run every command, and be started for its version as soon as a session opened, before anything was asked. `%ComSpec%` and `%SystemRoot%` are believed only when they are absolute. The search is the harness's own, on Windows too: it tries a name with `.exe` and as given, never the other extensions of `PATHEXT` (a `pwsh.bat` is not PowerShell), and never the current directory, which the system's own search for a program looks in first. A configured name with a drive and no root (`C:bash.exe`) is a relative path, and refused as one. What is found is an absolute path as found; a link is not resolved there, because a program that is several programs (`busybox`) knows which to be from the name it is started by. An absolute entry of `PATH` is the user's own, even one inside the workspace (an activated virtual environment), and is looked in. **git is found the same way:** the harness starts `git` itself, for a session's base state, a file's earlier content and what a command changed, and since the review of the fixes to step 13a it starts it by the path found in an absolute directory of `PATH`, never by its bare name, which the system would look for through a relative entry too, from the workspace. One search does both, `cortex_base::program`.
- How a shell is spoken to follows its program's name: `bash`, `pwsh`, `powershell` and `cmd` each by their own rules, and any other name as a POSIX shell that reads `-c <command>`.
- Finding the shell looks at `PATH` and starts nothing. Asking it for its version starts it once: about a millisecond for bash, and longer for PowerShell 7, so a harness does that off its start-up path. Windows PowerShell is 5.1, and is not asked. **As built (step 13b):** the core keeps the answer in the state directory (`shells.json`, owner-only), under the shell's resolved path (every link followed, in the system's own form; since the review of step 13a, when the key had been the path as found), its size and its modification time, so a replaced or updated shell is asked again. A program the system cannot resolve, a Windows app execution alias, is kept under the absolute path it was found at. On a miss the session's description names the shell without its version, and the shell is asked on a thread of its own for the runs after; `Core::shutdown` waits for that thread, so a short first run still keeps the answer (the first run on a machine can end up to the ask's own time limit later, after its result is written). Only the description reads the version: nothing that decides what a command may do depends on it. Measured on a hosted Windows runner, asking PowerShell 7 was about 180 ms of a headless run's 212 ms to its first request.

All three Windows shells are supported and tested: each has its own quoting, its own way to merge the output streams, and its own exit-status rules, and the tool handles each explicitly. Nothing requires a Unix-style shell on Windows.

| Shell | Started as | Around the command | Exit status |
|---|---|---|---|
| `bash` | `bash --noprofile --norc -c <command>` | nothing | The shell's own. |
| A POSIX shell | `sh -c <command>` | nothing | The shell's own. |
| PowerShell 7, Windows PowerShell | `-NoLogo -NoProfile -NonInteractive -OutputFormat Text -Command <script>` | A first line that sets the output to UTF-8 and turns progress bars off, and after an empty line a last line that sets the exit code. | PowerShell exits with 0 or 1 whatever a program it ran exited with. The last line makes it what a POSIX shell's would be: 0 when the last statement succeeded, else the exit code of the last program run, else 1. |
| `cmd.exe` | `cmd /d /e:on /v:off /s /c "<command>"` | The two quotes, which `/s` takes off again. | The shell's own: the last command's. |

**The command reaches the shell as it was written.** A permission rule is matched against the command the model wrote, and a person approves that text, so the text the shell reads must be that text.

- On Linux a process is given its arguments one by one, and the command is one of them.
- `pwsh` and `powershell` read their command line by the C runtime's rules. The script is one argument, quoted by the standard library by those same rules, so it arrives whole whatever quotes and backslashes it holds.
- `cmd.exe` reads its command line by rules of its own. The command is put on the command line as it is, between two quotes. Nothing in it is escaped, because `cmd.exe` would read an escape as part of the command.
- **A command that cannot arrive as written is refused when the call is planned,** before anyone is asked to approve it: one with a NUL character; one with a line break for `cmd.exe`, which reads one line and would drop the rest; and one too long for the system to pass (about 120,000 bytes on Linux, 32,000 characters on Windows, 8,000 for `cmd.exe`).
- **Quoting one argument** (`Shell::quote`, for the harness's own use and for later steps): single quotes for the POSIX shells and for PowerShell, where the four typographic quote characters PowerShell also reads as a single quote are doubled like the plain one; double quotes for `cmd.exe`, which refuses a text with a `"`, a `%` or a line break, because `cmd.exe` has no way to carry those that every program reads back.

- **A command cannot make the harness's last line part of itself.** A back-tick at the end of a PowerShell command joins the next line to it, and the next line is the empty one. Anything else that runs on (an open quote, an open bracket, a `|` at the end) is a script that does not parse, and PowerShell then runs none of it.

Three things on Windows are left as they are, and stated:

- **A PowerShell command that begins with a `using` statement fails,** because PowerShell wants those before everything else and the harness's first line comes first. Such a command belongs in a script file.
- **Windows PowerShell 5.1 drops a double quote and an empty text on their way to a program,** whatever the quoting. That is PowerShell's own doing, between its parser and the program, and PowerShell 7 does not do it. Texts with either reach PowerShell's own commands whole.

- **PowerShell's first line changes the console's output code page to UTF-8,** for the console the harness shares with the command. Without it PowerShell writes to a pipe in the console's code page and turns every character outside it into `?`. The harness itself does not depend on the code page.
- **`cmd.exe` is not told to write UTF-8.** Its built-in commands write in the console's code page, and text outside ASCII from them is shown with replacement characters. Programs that write UTF-8 are not affected.

**Environment.** Inherited from the harness, with a deny-list of variables that are removed (the key variables of the providers always; configuration adds to the list, `shell.env_remove`, and a name may end with `*`), plus settings that make tools non-interactive: no pager (`PAGER` and `GIT_PAGER` are `cat`), no colour unless asked (`NO_COLOR`), a dumb terminal type (`TERM`), and no prompt from git (`GIT_TERMINAL_PROMPT`). On Windows a variable's name is matched in any case. **Since step 13a four more things are settled there, whatever the user's environment holds.** `PWD` is the directory the command starts in, as it was resolved: a POSIX shell keeps an inherited `PWD` that names the same directory by another path, and for a workspace reached through a link `cd ..` would then go to the parent of the link, which is not where the command analysis followed it. `CDPATH`, `OLDPWD` and `RIPGREP_CONFIG_PATH` are removed: the first makes `cd dir` go somewhere other than `dir`, the second is where a command's first `cd -` goes, and the third lets a configuration file add options to an `rg` the model wrote. The command analysis depends on none of the three (it follows a `cd` only where the argument says where it starts, and `rg` is not on its table of commands that only read: [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md), "Command analysis as built"), so each removal is defence in depth: a command does what its text says, whatever the user's shell was set up to do. Git's file-system monitor is turned off for every git a command starts (`GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>=core.fsmonitor`, `GIT_CONFIG_VALUE_<n>=false`, after whatever the user's environment already counts, so theirs stay and ours wins; and at the end of `GIT_CONFIG_PARAMETERS` when the environment has one, because git reads that after the counted settings): a repository's configuration can name any program as the monitor, and `git status` would start it. And `PATH` keeps only its absolute directories: an empty or relative entry means "the directory the command runs in", which is the workspace, where a file the model wrote and named `ls` would then run as `ls`. Windows allows an entry in quotes, and it is judged by what is inside them; on Linux a quote is a character of a name, so `"/usr/bin"` is relative and goes (since the review of step 13a). (`cmd.exe` looks in the directory a command runs in before it looks at `PATH`, whatever `PATH` holds. Its lines are not analysed and so are always asked about; turning its search off, with `NoDefaultCurrentDirectoryInExePath`, would also stop a project's own batch files from finding each other, and is left open in [../outstanding/cmd-searches-the-current-directory.md](../outstanding/cmd-searches-the-current-directory.md).) None of the four is a setting. The removal list is also given the key variable of every configured provider, and any variable a provider's header reads. Standard input is closed: a command that reads it sees its end at once. There is no pseudo-terminal: output comes through one pipe, which is both the standard output and the standard error, so the two arrive in the order they were written.

**Process tree.**

| | Linux | Windows |
|---|---|---|
| Start | New session and process group, so no controlling terminal. The child is told to die if the harness dies, and the guard process is told the group. | New process group inside a Job Object set to kill its members when closed. The process is started suspended and let run once it is in the job, so that nothing it starts can be outside it. |
| Polite interrupt | SIGINT to the group | Ctrl-Break event to the group, while the command itself is alive |
| Terminate after grace (default 2 s) | SIGTERM to the group | n/a |
| Hard kill after a second grace | SIGKILL to the group | Terminate the Job Object |

- The whole tree is cleaned up on timeout, on cancellation, when the call is given up, and when the harness exits for any reason, including a crash.
- **A cancelled call ends within bounds.** The loop waits for the result of every call it started, so a tool that is slow to see its cancellation holds up an interrupt. A call cancelled before its turn starts nothing. One cancelled while its command runs sends the interrupt at once and ends when the tree is gone: at once for a command that listens, and after two graces at most for one that ignores everything short of a kill. The look for changed files that follows has its own cap.
- **When the harness dies:** on Windows the Job Object's handle closes with the process. On Linux the kernel signals the command itself, and a guard process, which the harness keeps for this, kills the rest of the group ([decision 0030](../decisions/0030-process-tree-control-in-cortex-sandbox.md)).
- A stage that cannot be sent is passed over without its grace: there is no console to send a Ctrl-Break through when the harness has none.
- **A signal never reaches a stranger.** On Linux a group's id is the command's process id, which the system may give away once the command has been reaped. So the command is waited for without being reaped, and reaped only after the last signal.
- **The whole tree is killed when the call ends,** whatever way it ends. Long-lived background processes are not in the first release, and a process left running would be one that nothing could manage.
- **What a command left running is found by asking, not by counting.** When the command has exited, the tree is asked what else of it is running: on Linux the members of its process group, on Windows the members of its Job Object, by process id, leaving out the command itself and any member that has exited. A Job Object goes on listing a process for a moment after its exit, and its own count of active processes includes it: read in that moment, the count said that a command which left nothing had left one process, itself, and the tool waited half a second for it (found on 2026-11-13, [steps/11/answered.md](../steps/11/answered.md)).

**Output.**

- Streamed to front ends as it arrives, coalesced to one update per frame.
- Held in a bounded buffer: the first 8 KiB and the last 24 KiB. Once output passes the buffer, all of it is also written to an artifact, from its first byte, piece by piece as it arrives.
- The result contains the kept head and tail with a marker between them, the exit code, the duration, and flags for timeout and interruption. An exit by signal is reported as such. What is kept is whole lines, and the marker says how many are missing.
- The tool shapes its own result with the output policy's shaping: compressed, then cut to the limits with the beginning and the end kept, or the lines that report a problem first for a command the project profile knows as a build or a test run. The notice counts what the buffer dropped as well as what the limits cut.
- **A command that exits and leaves processes behind is detected,** whether they hold the pipe or have an output of their own. The tool waits a short grace (half a second) for them to finish, then stops them, and the result says that they were stopped and that background processes are not supported yet, so that the model does not go on trying.
- **A process that left the tree cannot be stopped** (Linux: one that started a session of its own). When one still holds the output, the tool stops listening after a second more, and the result says that a process is still running and could not be stopped.
- **Output that could not be collected is said to be lost,** not shown as a command that printed nothing: when the content store fails under the reader, the reader goes on reading, so that the command is not left writing into a pipe nobody reads, and the result says that what was printed could not be collected.

**Changed files.** The harness looks for what a command changed, once before it and once after it, and puts what it found in the result's payload, where the session takes it for undo ([07-sessions-and-context.md](07-sessions-and-context.md)). The model is not told: it knows what it ran.

- In a git repository of a project the user trusts: two answers of `git status`, compared, with the size and modification time of every file git named. When the command moved `HEAD`, git is also asked which files differ between the two commits.
- Anywhere else, and in a project that is not trusted: one walk of the workspace afterwards, for files modified since the command started. That cannot tell a new file from a changed one and cannot see a deleted one.
- The payload says which files' earlier content can still be read from git, and from which commit: those the command found as committed. A file that had already been changed before the command is reported as changed and not restorable.
- Each look has a time cap (half a second) and a cap on the number of files, and says when it did not finish. Files that ignore rules leave out are left out.
- **Asking git runs nothing of the repository's that the user has not accepted.** A repository's own configuration can name programs for git to run when it is asked what has changed. One is a file-system monitor (`core.fsmonitor`), which the harness turns off, with no hooks directory and no optional locks, so that asking writes nothing either: the same hardening the session store's own git commands have. The other is a filter for a kind of file, which git runs to see whether such a file changed, and which no flag turns off. So git is asked only about a project the user trusts ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md), "Project trust"), and any other is looked at by modification times, which starts nothing. Even in a trusted project a confined command may rewrite the configuration mid-session, so while commands are confined the harness runs its own git inside the sandbox (read-only, no network): a filter git then runs runs confined too, and a filter that must write to answer fails, leaving the change list incomplete with its reason ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md), "As built in step 26").
- A call the permission engine has marked read-only is not looked at.

**The result's payload,** for front ends: the shell, the exit code or the signal, the duration, whether the command timed out or was interrupted and the last stage that stopping needed, whether background processes were stopped, the size of the output, and the changed files.

**Sandbox.** The command is started through the sandbox launcher with the session's policy (see [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). When the sandbox blocks something, the result says which rule blocked it, so the model does not retry blindly. Step 11 built the hook, `Launcher`: it is given the shell's command line and returns the one to start, with bytes for its standard input (the policy), and may add a line to the result. **Since step 26** the core gives every shell tool its session's launcher, which starts `cortex __sandbox -- <shell> <arguments>` with the policy on its standard input below `unrestricted` where the sandbox enforces, and starts the command as it is otherwise. Its line names the path of a write the sandbox refused, or says the network is off, and tells the model of `sandbox: false`. A call with `sandbox: false` is started as it is, after the user's approval, and its plan declares the effect `unconfined`. The core also looks at the protected paths around each confined command ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md), "As built in step 26").

**Effects.** An `exec` effect with the command as the model wrote it and its working directory, and a `write` effect on the workspace root, because a command may change anything there. A working directory outside the workspace is a second `write` effect. A call with `sandbox: false` also declares `unconfined`.

Concurrency: exclusive, unless the policy engine has classified the command as read-only. It does that when every part of the command is on its table of commands that only read and nothing the command names needs an answer ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md), "Command analysis as built"); the loop then sets the plan's concurrency to read-only.

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
- **Memory does not grow with the number of matches.** Only as many matches as the result can show are kept, in path order, while the search runs, and of one file no more than the output limit; the rest are counted. Of a very long line only the beginning is kept, with the number of characters that are missing.
- In `files` mode a file is done at its first match. A file whose first NUL byte comes after that is therefore named there, and left out as binary by the other two modes, which read on.
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
- Only the entries a result can show are kept while the walk runs; the rest are counted.
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

As built in step 10 (`PlanBoard` and `PlanTool` in `cortex-tools`):

- **The plan is a `PlanBoard`,** one for a session, shared by the tool and the core. The model writes it through the tool. The user's edits are applied with `PlanBoard::apply`, all of a batch or none, and an edit that does not fit (an item that is not there, an order that does not name every item once) is an error that changes nothing. Every change makes a new revision. A resumed session starts from the plan it had (`PlanBoard::with_plan`).
- **The notice** is the text `apply` returns: one line for each thing the user changed (the item, by its id and its text; a new item's id; a new order), and then the plan as it now is. The core hands it to the loop (`SessionHandle::notice` with `NoticeSource::Plan`). An edit that changes nothing, such as skipping an item that is skipped, has no notice.
- **The user's edit wins over a call that crosses it** (the owner, 2026-11-13). Each call replaces the whole list, so a call the model wrote before it saw the user's edit would silently undo it. While the user has changed something the model has not been shown, a call replaces nothing: its result is an error that says what the user changed and gives the current plan, and the model's next call goes through. It is the staleness guard of the file tools, for the plan. A call that sends exactly the current plan changes nothing and is not refused.
- **When the model has been shown an edit.** In one of two ways. By a result of the tool: but a result reaches the model only with the next request, so an edit that a call was told about counts as shown when a call of a **later round** arrives (`RunCx::round`). A second `plan` call in the same response was written without the first one's result, and is refused like the first. If a round's results could not be recorded, the round keeps its number, and what its calls were told does not count as shown. With no round number (a caller that does not say), nothing counts as shown by a result. Or by the notice: the core says when the loop has recorded it in the conversation (`PlanBoard::delivered`, with the revision the notice was for), and edits made after that revision stay unseen. With the loop's round numbers, a core that never calls `delivered` loses nothing but a call: the model's first `plan` call after an edit is answered with the current plan, even when the model had read the notice.
- **What a call returns.** When the list was taken as sent, one line: how many items, how many in each status, and the revision. The plan is then exactly what the model sent, so it is not sent back. When the call crossed an edit, the plan in full. Front ends get the plan in the payload either way.
- **The marker.** The call changes the session and says so on its plan ("The tool trait", above): it never runs for a response that is thrown away, and two calls in one response apply in order.
- Limits: at most 100 items; an id of up to 40 characters with no spaces, unique in the list; a text of one line and up to 400 characters. An item the user adds gets an id of the form `u1`.

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

As built in step 10 (`RememberTool` in `cortex-tools`):

- **Proposals go through a trait,** `Proposals`, because the project's memory (`cortex-knowledge`, step 17) is not built yet and is a crate the tools cannot see. A proposal carries the kind, the text, the evidence, the check and the turn it was made in. What takes it stores it as proposed: nothing in the tool can make an entry active. `MemoryProposals` keeps them in memory for a session with nowhere else to put them.
- **The result says that the entry is not in use,** so that the model does not rely on it in the session that proposed it. An entry that is already known, or already waiting for review, adds nothing, and the result says that too.
- **A check is validated when the call is planned:** `exists` takes `path`, `contains` takes `path` and `pattern`, `command` takes `command`, and nothing else. A check's path goes through the path resolver like every other path, must be inside the workspace, and is stored relative to its root. Nothing runs a check yet.
- Limits: a text of up to 600 characters, and evidence, a pattern or a command of up to 400.
- **A check's command is the model's text,** stored as it was written. "A command that only reads" is what the model is asked for, and nothing has verified it. Whatever runs checks later treats the command as untrusted, and puts it to the permission engine like any other command.
- The call changes the session and says so on its plan, like `plan`. It touches nothing outside the session and the project's store of proposals.

## `tool_search` and `tool_call`

Tools that are not built in (MCP tools and extension tools) are not declared to the model one by one. They are found and called through this fixed pair, so the declared tool list never changes during a context epoch, however many tools exist and whenever they appear.

| Tool | Arguments | Returns |
|---|---|---|
| `tool_search` | `query` (words, or a `source/` prefix), `limit` | Matching tools: full name, description and argument schema. |
| `tool_call` | `tool` (full name), `arguments` (object) | The tool's result. |

- **The registry.** MCP servers and extensions register their tools with the dynamic-tool registry, each under the name of its source. A tool's full name is `source/tool`. A source is replaced whole, so a server whose tool list changes is never seen half-updated. The names of sources and tools are ASCII letters, digits, `_`, `-` and `.`: a permission rule is written against a name, so a name must not have a look-alike.
- **`tool_search` is a ranked match done in the harness,** with no model call. A tool matches when every word of the query is in its name, its source's name or its description, and a word in the name counts for more. `limit` is 8 unless given, at most 25. A search that finds nothing lists the sources.
- Descriptions and schemas in a search result are third-party text. The result says so, and a description is cut at 2 KiB.
- Arguments are validated against the tool's schema before the call. A mismatch is an error result that includes the schema, and the tool is not run.
- The call is planned, checked by the permission engine and bounded by the output policy exactly like a built-in tool. The plan of a `tool_call` carries the tool's own effects, and besides them an `ext` effect that names the tool, which is what a rule on `source/tool` matches. `tool_call` asks by default; `tool_search` touches nothing.
- **A tool from outside runs alone unless its spec declares otherwise.** A tool whose spec says exclusive is scheduled as exclusive whatever its plan says, so an author who never thought about scheduling gets the safe answer. One whose spec declares another class is scheduled as each of its plans says.
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
| Line endings | Detected and preserved per file, and per line in a file with both kinds. A new file is written with the line breaks the model gave it, which are line feeds: nothing converts them, and there is no setting for it yet. |
| Shell | PowerShell by default, with `cmd.exe` supported. The model is told which. A command with a line break is refused for `cmd.exe`. |
| Process tree | Job Objects, with a suspended start. Ctrl-Break as the polite interrupt. |
| Command output | PowerShell is told to write UTF-8. `cmd.exe` writes in the console's code page. |
| Locked files | Replace is retried briefly, then fails with the reason. A file another program holds so that it cannot even be read fails at once. Either way the file is left byte for byte as it was, with no temporary file beside it. |
| Long paths | Handled internally with extended-length paths. Not shown to the model. |
| Reserved names and device paths | Rejected. |
| Trailing dots and spaces, alternate data streams | Rejected. |
| Drive-relative and root-relative paths (`C:name`, `\name`) | Rejected. |
| Network paths | Rejected unless the server and share are allowed in configuration. |
| Junctions and directory links | Resolved like symbolic links. The check applies to the target. One that is put where a checked file was is refused when the file is opened, and never written through. |
| Short (8.3) names | Resolved to the long name. |
| Names on disk that the rules refuse (made through an extended-length path) | A walk steps past them and says so in its result. |
| Globs | Either separator accepted. Matching ignores case. |
| Script shims (`.cmd`, `.bat`) | Never executed directly with untrusted arguments. See [12-extensions.md](12-extensions.md). |

Each row has tests that run on the Windows CI job (see [../testing-and-ci.md](../testing-and-ci.md)).
