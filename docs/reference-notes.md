# Reference notes: Pi and Opencode

What two existing harnesses do about the problems Cortex-Harness faces, what is worth matching in capability, and where Cortex-Harness differs and why.

## Ground rules for these notes

- The references are **Pi 1.5.2** and **Opencode 1.19.5**. Their source and docs were read to understand how they solve problems.
- Everything here is in our own words. There is no code from either project in this repo, and no copied prompt text, UI wording, key bindings, colour values or configuration formats.
- Citations give the project, version and a path inside that project's repository, or a public URL.
- Design work was done from these notes, not with the source open. The architecture pages are the design. This page records what was learned and what was decided because of it.
- No dependency of Cortex-Harness is a port or extract of either project's code. That rule also excludes the terminal UI library that Opencode is built on and Pi's own TUI package.

Both projects are moving quickly. These notes describe the two versions named above and nothing later.

## At a glance

| Topic | Pi 1.5.2 | Opencode 1.19.5 | Cortex-Harness |
|---|---|---|---|
| Language and runtime | TypeScript on Node | TypeScript on Bun, compiled to one executable | Rust, one native binary |
| Core and UI | One process. Four modes over one session class. | UI on the main thread, server in a worker thread of the same process, talking through an emulated HTTP channel. | One process. Typed operations and events in-process; the same types as JSON-RPC outside. |
| Session store | One JSONL file per session, with a tree inside | SQLite | JSONL log per session, with a tree inside, plus artifacts by reference |
| Permissions | None by design, apart from project trust | Rule-based allow, ask, deny. No sandbox. | Rules and approvals, plus an OS sandbox where the platform allows |
| Extensions | TypeScript loaded into the process | TypeScript loaded into the process | JavaScript in an embedded, sandboxed script runtime; MCP servers for anything that needs a process |
| MCP | Built in. Tools hidden from the model by default and reached through a scripting tool or a search tool. | Built in. Every tool declared to the model. | Built in, tools only. A fixed search-and-call pair of tools, shared with extension tools. |
| TUI | Own line-based renderer. Full screen by default since 1.0, inline still available. | Full screen, on an external native-core renderer. | Own line-based renderer. Inline by default, full screen as a fallback. |

## Architecture

**Pi.** A monorepo of libraries with a clear direction: a multi-provider client, an agent loop on top of it, a standalone TUI library, and an application package that is also the SDK. All four modes (interactive, print, JSON, RPC) drive one session object, so behaviour does not drift between them (Pi 1.5.2, `packages/coding-agent/docs/sdk.md`, `how-pi-works.md`). The cost is concentration: the session class is about 4,500 lines and the interactive mode about 7,300, with retries, compaction, overflow recovery, queues and extension hooks in one type (Pi 1.5.2, `packages/coding-agent/src/core/agent-session.ts`, `src/modes/interactive/interactive-mode.ts`). A second, experimental harness stack lives beside the main one.

**Opencode.** Every client is a client of a local server with a published API description and generated SDKs (Opencode 1.19.5, `packages/web/src/content/docs/server.mdx`, `sdk.mdx`). By default no network port is opened: the server runs in a worker thread and the TUI reaches it through a channel that imitates HTTP requests (Opencode 1.19.5, `packages/opencode/src/cli/cmd/tui.ts`). Version 1.19.5 is in the middle of a rewrite. Two generations of the core coexist with a bridge between them, which means two event systems, two message shapes and two compaction paths (Opencode 1.19.5, `CONTEXT.md`, `specs/v2/session.md`). The design document for the new core is the most useful single file in the tree.

**Worth matching.** One core that every front end drives through one contract. A library-and-application split.

**Where Cortex-Harness differs.**
- The contract is defined once as Rust types. The in-process carrier is a typed channel, not an emulation of a network protocol, so the UI pays no serialisation cost. See [architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md).
- The loop, the session store, the policy engine and the provider client are separate crates with narrow traits between them, so the central type cannot grow the way both references' did. See [architecture/01-workspace.md](architecture/01-workspace.md).
- One runtime, one generation. Opencode's dual core shows what a mid-life rewrite costs, which is the reason this design step exists.

## Agent loop

**Pi.** Tool calls are checked one at a time in order, then run in parallel, and their results are written in the assistant's original order. A per-path queue serialises edits to the same file (Pi 1.5.2, `packages/agent/src/agent-loop.ts`, `packages/coding-agent/src/core/tools/file-mutation-queue.ts`). Input typed during a run goes to one of two queues: delivered after the current tool round, or held until the agent would stop (Pi 1.5.2, `packages/coding-agent/docs/usage.md`). Provider failures arrive as messages with an error stop reason and partial content, never as thrown errors. Retries live outside the loop and decide retryability by pattern-matching error text against a long list (Pi 1.5.2, `packages/ai/src/utils/retry.ts`). Because retries and queued work can follow the run-end event, Pi added a second "settled" event that every integration must wait for instead. No cap on requests or cost per run was found.

**Opencode.** Each loop iteration reloads the whole history from the database. Tool concurrency is left to a third-party SDK. Retries use exponential backoff, honour server wait headers, and also classify partly by message text (Opencode 1.19.5, `packages/opencode/src/session/prompt.ts`, `processor.ts`, `retry.ts`). Three identical tool calls in a row raise a permission question. The design for the new core adds a durable inbox with two delivery modes, repair of unfinished tool calls at start-up, and a token budget check before each request (Opencode 1.19.5, `specs/v2/session.md`).

**Worth matching.** Parallel tools with ordered results. Two delivery modes for input during a run. Failures as data. Visible retry state. Repair of unfinished tool calls. A budget check before sending.

**Where Cortex-Harness differs.**
- Errors are classified from status codes, provider error codes and headers. Text matching is a last resort with a fixture. Both references show where the other approach leads.
- The loop is an explicit state machine, and a completed turn means nothing automatic remains, so there is one event to wait for.
- The scheduler is ours, with declared concurrency classes per tool.
- Limits on requests, cost and time exist from the start.
See [architecture/03-agent-loop.md](architecture/03-agent-loop.md).

## Providers

**Pi.** Separates wire-protocol implementations from providers (catalog, base URL, credentials, per-model compatibility flags), with about forty built-in providers (Pi 1.5.2, `packages/ai/README.md`, `packages/ai/src/api/`, `src/providers/`). Assistant messages record which provider and model produced them, so replay rules can be applied later: reasoning is replayed unchanged to the same model and dropped on a switch. The system prompt and tool list are stored as leading messages and later changed by patch messages, to keep provider caches valid. It warms caches with cheap refresh requests when the estimated saving justifies it. The model catalog merges several outside feeds with hand corrections and is refreshed at runtime from the project's own site (Pi 1.5.2, `packages/ai/catalog/README.md`). Credentials are stored in one owner-only JSON file, with a lock around token refresh (Pi 1.5.2, `packages/coding-agent/src/core/auth-storage.ts`). It supports subscription logins for several vendors.

**Opencode.** Builds on a third-party multi-provider SDK, with about 4,000 lines of per-model adjustment code keyed on model-name fragments and about twenty patched dependencies (Opencode 1.19.5, `packages/opencode/src/provider/transform.ts`, `provider.ts`, `patches/`). A newer in-house layer separates endpoints from protocols and places cache breakpoints automatically (Opencode 1.19.5, `packages/llm/README.md`). The catalog comes from models.dev with a snapshot built in and an hourly background refresh. Credentials are a plain JSON file; no OS keychain use was found.

**Worth matching.** The split between protocol and provider. Capability metadata that drives behaviour. Recording the producer of each message. Dropping, not translating, reasoning on a model switch. Usage split into fresh input, cache read, cache write, output and reasoning. A catalog with a built-in snapshot.

**Where Cortex-Harness differs.**
- Quirks are data on catalog and provider entries, never branches on model names.
- The raw provider block is stored beside the neutral view and replayed byte for byte, which also preserves block types the harness does not know yet.
- History is append-only within an epoch as a core rule, not as a per-provider workaround.
- The catalog is a file in this repo. Nothing is fetched at build time or at start-up, and there is no hosted service.
- API keys only. No subscription logins in the first release.
- Few built-in providers. Compatible servers are configuration.
See [architecture/04-providers.md](architecture/04-providers.md).

## Tools

**Pi.** Four tools on by default (read, a shell tool, edit, write) and three more that can be enabled. Output is limited by lines and bytes, whichever comes first, with the head kept for reads and the tail for commands, and the full output written to a temporary file (Pi 1.5.2, `packages/coding-agent/src/core/tools/`). The edit tool takes several replacements matched against the original file, each required to be unique, with a normalising fallback for whitespace and typographic characters. The shell tool has no default timeout. On Windows it requires a Unix-style shell and looks for one in several places; PowerShell is a separate optional tool (Pi 1.5.2, `packages/coding-agent/docs/windows.md`). Search and file-finding call external programs.

**Opencode.** A larger set, including a sub-agent tool, a task list, web fetch and a patch-format edit tool used for some model families (Opencode 1.19.5, `packages/opencode/src/tool/`). Output over the limit goes to a managed file kept for a week. The edit tool tries nine matching strategies in turn, with a guard against matches much larger than the request. After an edit it waits briefly for language-server diagnostics and appends them to the result, and runs a formatter (Opencode 1.19.5, `packages/opencode/src/lsp/`, `src/format/`). The shell tool parses commands with a grammar compiled to WebAssembly to build permission patterns, defaults to a two-minute timeout, and on Windows tries PowerShell first. No check that a file was read before it is overwritten was found in either project.

**Worth matching.** Bounded output with the full text saved. Edits matched against the original file. Per-path serialisation. Process-tree termination. Preserved line endings. A default command timeout.

**Where Cortex-Harness differs.**
- A staleness guard on write and edit: the file must have been read in the session and be unchanged since.
- Two matching tiers for edits, not nine, and the result says when the tolerant tier was used.
- Search and listing are in-process, so nothing external is needed, which matters most on Windows.
- PowerShell is the default Windows shell and the model is told which shell it has.
- Full outputs are session artifacts, so they survive with the session.
- Diagnostics after edits and formatters are left for later. They are valuable, and they bring dozens of language-specific integrations to maintain.
See [architecture/05-tools.md](architecture/05-tools.md).

## Permissions and sandboxing

**Pi.** Deliberately has no permission system and does not ask before tool calls. Its stated reasoning is that approval prompts are not a security boundary and that real safety comes from limiting what the process can reach, so it documents running inside containers or virtual machines (Pi 1.5.2, `packages/coding-agent/docs/security.md`, `containerization.md`). Its one built-in gate is project trust: before loading anything executable from a working folder, it asks, and remembers the answer per directory. Extensions can add their own gates through a hook on every tool call.

**Opencode.** A rule language of permission, pattern and action, where the last matching rule wins and defaults are permissive. Shell commands are split and each part is checked, with remembered approvals offered as command prefixes. "Always" approvals last for the running instance. There is a guard for paths outside the project and a mode that approves everything not denied (Opencode 1.19.5, `packages/opencode/src/permission/index.ts`, `packages/web/src/content/docs/permissions.mdx`). Its security document says the permission system is a usability aid and not isolation.

**Worth matching.** Pi's honesty about what prompts are worth, and its trust gate. Opencode's uniform rules, per-part checking of commands, and denial with a note back to the model.

**Where Cortex-Harness differs.**
- It agrees with both that prompts are not a boundary, and draws the other conclusion: ship the boundary. A native binary can apply an OS sandbox to its children without a container.
- The default follows the product vision: reads run, edits apply with a snapshot to undo them, and commands ask unless they are on a safe list that includes what the harness has learned about the project.
- Deny beats ask beats allow, whatever the order, so a broad allow cannot hide a deny.
- A project can tighten rules freely and can loosen them only with trust.
- Trust is pinned to a hash of what was trusted.
- Command analysis fails closed.
See [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md).

## Sessions and context

**Pi.** An append-only JSONL file per session. Entries carry an id and a parent id, so branches live in one file. Writes are one line at a time, and a failed write is rolled back in memory (Pi 1.5.2, `packages/coding-agent/docs/session-format.md`). Compaction keeps the originals, never cuts inside a tool exchange, summarises with a fixed outline, feeds the previous summary into the next, and carries lists of files read and changed (Pi 1.5.2, `packages/coding-agent/docs/compaction.md`). A separate entry type hides or replaces one earlier message in the model's view without rewriting the file. Instruction files are collected from the user's directory and from every ancestor directory down to the working directory.

**Opencode.** SQLite with message rows that hold JSON. Undo uses a hidden git repository outside the project that snapshots the working tree at step boundaries (Opencode 1.19.5, `packages/opencode/src/snapshot/index.ts`). Compaction summarises older turns and keeps a recent tail; a separate optional pass clears old tool outputs. Instruction files: the first project-level match found walking up wins, with nested files loaded when a file under them is read (Opencode 1.19.5, `packages/opencode/src/session/instruction.ts`). The new design introduces an "epoch" for the exact context sent, and emits changes as later messages to keep caches valid.

**Elsewhere.** Another Rust harness that also uses JSONL logs added an index and paging after session files reached hundreds of megabytes from raw tool output stored inline (source in the research notes).

**Worth matching.** Append-only JSONL with a tree. Compaction that keeps the originals and never splits a tool exchange. File lists carried mechanically. Nested instruction files loaded on demand. The epoch idea.

**Where Cortex-Harness differs.**
- Tool output is stored by reference from the start, so logs stay small.
- There is a rebuildable index for listing, and no database.
- Compaction starts a fresh epoch from a summary by default, because keeping recent turns verbatim with their reasoning is no longer valid on the providers that bind reasoning to history.
- No entry type edits earlier context in place. Old tool output is reclaimed only at an epoch boundary.
- Undo is per-file checkpoints of what the file tools changed. A whole-tree snapshot is heavier and can be added later.
See [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md).

## Configuration

**Pi.** A user scope and a project scope, with separate JSON files by concern and published schemas. Some keys are user-only for safety. The option surface is very large, with many terminal-specific switches, and list values have an add-and-remove syntax (Pi 1.5.2, `packages/coding-agent/docs/settings.md`).

**Opencode.** JSON with comments. Eight layers, from remote organisational defaults to managed system files, about sixty environment flags, and markdown files that define agents and commands (Opencode 1.19.5, `packages/web/src/content/docs/config.mdx`, `packages/opencode/src/config/config.ts`). Its public docs and its code disagree on precedence.

**Worth matching.** Layering with a managed top layer. User-only keys. Schema validation.

**Where Cortex-Harness differs.** TOML. Fewer layers. No list-editing syntax. Provenance for every value, with a command that shows it. All terminal overrides in one section.
See [architecture/08-configuration.md](architecture/08-configuration.md).

## Terminal UI

**Pi.** Its own TUI library renders components as lines, keeps the previous frame, rewrites from the first changed line inside the terminal's atomic-update mode, coalesces redraws to about one per 16 ms, and lets key presses bypass the throttle (Pi 1.5.2, `packages/tui/src/tui.ts`). Streaming markdown re-renders only the last block. It has two renderers, one writing into the terminal's scrollback and one owning the screen; the second became the default in 1.0.0 and both are maintained (Pi 1.5.2, `packages/coding-agent/CHANGELOG.md`). Its docs have more than a dozen terminal-specific sections.

**Opencode.** A full-screen application on an external renderer with a native core, with events batched to about one update per 16 ms before they reach the UI (Opencode 1.19.5, `packages/tui/src/context/sdk.tsx`, `src/app.tsx`). A lighter mode for one-shot runs commits finished content to scrollback. On Windows it adjusts console modes through native calls, and its docs recommend the Linux subsystem.

**Worth matching.** Atomic frame updates. Line-level diffing. Coalesced redraws with input given priority. Block-wise streaming markdown. A virtual terminal for tests. Detection with an override for every terminal capability.

**Where Cortex-Harness differs.** The measured comparison of a Rust TUI with a TypeScript one is in [decision 0001](decisions/0001-tui-language.md). The choice between inline and full screen is in [decision 0002](decisions/0002-tui-rendering.md). The look, the commands and the key bindings are our own, described in [architecture/09-tui.md](architecture/09-tui.md).

## Headless and RPC

**Pi.** Print mode writes the final text. JSON mode writes the same event stream as RPC. The two disagree on exit status: print mode exits non-zero on error, abort or a length stop, JSON mode does not (Pi 1.5.2, `packages/coding-agent/docs/json.md`, `src/modes/print-mode.ts`). RPC is newline-delimited JSON over standard I/O with commands answered by id and events streamed. A prompt's response means "accepted". No protocol version or capability handshake was found (Pi 1.5.2, `packages/coding-agent/docs/rpc.md`).

**Opencode.** A run command with a human format and a line-delimited JSON format. Exit status is 0 or 1 (Opencode 1.19.5, `packages/opencode/src/cli/cmd/run.ts`). The server speaks HTTP with a live event stream. Authentication on a network port is optional HTTP Basic, and without a password the server runs unauthenticated with a warning (Opencode 1.19.5, `packages/opencode/src/server/auth.ts`, `SECURITY.md`).

**Worth matching.** One event vocabulary for headless and RPC. Accepted versus completed. Standard output reserved for the protocol.

**Where Cortex-Harness differs.** One exit-code table for every format. A single-object JSON result as well as a stream. A version and capability handshake from the first release. Local sockets restricted to the user, and no unauthenticated network mode.
See [architecture/10-headless.md](architecture/10-headless.md) and [architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md).

## Extensions

**Pi.** TypeScript modules loaded into the process at runtime with no build step. They can register tools, commands, key bindings, renderers and providers, and subscribe to a long list of events, including ones that block or rewrite tool calls. There is no isolation, and the project's stated policy is not to preserve backward compatibility (Pi 1.5.2, `packages/coding-agent/docs/extensions.md`, `AGENTS.md`). Extension packages are installed from npm or git. Skills follow the open Agent Skills folder layout, with only a name and description placed in the prompt.

**Opencode.** Plugins are JavaScript or TypeScript loaded into the process, from local folders or from npm packages installed at start-up. Hooks cover events, chat parameters, permissions and tool calls. Custom tools are files in a folder; custom commands and agents are markdown files (Opencode 1.19.5, `packages/web/src/content/docs/plugins.mdx`, `packages/plugin/src/index.ts`). There is no isolation.

**Worth matching.** Tools, commands and hooks as the three kinds of contribution. Hooks that can block or change a tool call. Commands as markdown files. Skills with progressive disclosure. Project trust before anything from a repository runs.

**Where Cortex-Harness differs.** In both references an extension is code loaded into the agent with the user's full rights, which is the largest unaddressed risk in each. Extensions here run in an embedded script runtime that has no access of its own: every file, process and network action is a host function checked against a manifest the user approved and then against the permission rules. That holds on Windows as well as Linux, and it is what lets the harness write an extension for the user and load it on the spot. Hooks have deterministic ordering and deadlines. The event surface starts small. Tools that need a process are MCP servers. The measured comparison of mechanisms and languages is in [decision 0005](decisions/0005-extension-mechanism.md).

## MCP

**Pi.** Added MCP after deliberately not having it. Its client is its own. By default MCP tools are not declared to the model; they are reached from a sandboxed scripting tool, through a search tool that loads matches for the next request, or declared directly per server (Pi 1.5.2, `packages/coding-agent/docs/mcp.md`). The stated aim is a small prompt that caches well.

**Opencode.** Uses the official SDK, supports three transports and OAuth, and declares every server's tools beside the built-in ones, with per-server and per-agent switches as the only cost control (Opencode 1.19.5, `packages/opencode/src/mcp/index.ts`).

**Worth matching.** Pi's goal: MCP must not bloat or destabilise the prompt.

**Where Cortex-Harness differs.** A fixed pair of tools, search and call, with no changes to the declared tool list. Existing MCP configurations are read so people can switch without rewriting them. See [architecture/13-mcp.md](architecture/13-mcp.md).

## Testing and CI

**Pi.** A scripted fake provider that is part of its public client library, session-level tests driven by it with real providers forbidden, a virtual terminal for TUI tests, and a test runner script that clears the environment and redirects the home directory (Pi 1.5.2, `packages/coding-agent/test/suite/README.md`, `test.sh`). CI splits tests into three shards.

**Opencode.** A local fake provider server that streams scripted responses, recorded HTTP traffic replayed for provider tests, a headless renderer for TUI tests, and a check that generated clients are current. CI runs on Linux and Windows (Opencode 1.19.5, `packages/opencode/test/lib/llm-server.ts`, `packages/http-recorder/README.md`, `.github/workflows/test.yml`).

**Worth matching.** All of it: scripted provider, wire-level replay, virtual terminal, hermetic environment, generated-artifact checks, both operating systems.

**Where Cortex-Harness differs.** A time budget for the blocking checks from the first commit, and performance budgets checked automatically. See [testing-and-ci.md](testing-and-ci.md) and [performance-budgets.md](performance-budgets.md).

## Distribution and start-up

**Pi.** An npm package with a pre-bundled entry point, plus standalone executables built with a JavaScript runtime's compile mode. Its changelog records start-up work: bundling, lazy loading of the extension loader and of syntax grammars, and moving the catalog refresh out of start-up (Pi 1.5.2, `packages/coding-agent/CHANGELOG.md`). Requires Node 22.19 or later when installed from npm.

**Opencode.** One self-contained executable per platform built with Bun's compile mode, behind a small launcher script when installed from npm (Opencode 1.19.5, `packages/opencode/script/build.ts`).

Neither project publishes start-up or memory figures. Measured for this design on a laptop, with both tools installed from the npm registry into scratch space: Pi 1.5.2 answers `--version` in 175 ms and shows its first screen in about 280 ms, holding 124 MB; Opencode 1.19.5 takes 371 ms and about 540 ms, holding 614 MB, and its executable is 214 MB. Method and caveats are in [measurements/tui-language-spike.md](measurements/tui-language-spike.md).

**Where Cortex-Harness differs.** A native binary with no runtime to embed, and budgets for start-up, memory and size that are checked in CI.

## Lessons taken from both

1. **A minimal core does not stay minimal on its own.** Pi's does more with each release. Scope here is held by the crate boundaries and the build plan.
2. **Text-matching errors grows without end.** Classify structurally.
3. **A second core is the most expensive feature.** Opencode is paying for it now. Get the contracts right first.
4. **In-process extensions are a liability** for security and for API stability. Both projects warn their own users about it.
5. **Terminals are the long tail.** Both carry many terminal-specific fixes. Keep quirks in one place, detect before assuming, and give every detection an override.
6. **Windows is where shortcuts show.** Requiring a Unix shell, or recommending the Linux subsystem, are both ways of not supporting Windows. Native Windows is tested from the first commit here.
7. **Docs drift from code.** Opencode's public docs, in-repo docs and code disagree in places. Here, generated artefacts (schemas, configuration reference) are checked in CI, and the final review step checks docs against code.
