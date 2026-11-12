# The extension system

Extensions let users add their own tools, slash commands and hooks without changing the harness. The [vision](../vision.md) gives the extension system a concrete job on top of that:

- **Self-written extensions.** The user asks for a new tool or command; the harness writes it, the user reviews it, and it loads at once.
- **Sandboxed**, for an audience that includes Windows users, where no OS sandbox exists in the first release.
- **Hot-reloaded**, like configuration, themes and key bindings.
- **For everyone.** It cannot assume that Python, Node or a compiler is installed.

This page compares the ways to build it, recommends one, and specifies it. The choice is recorded in [decision 0005](../decisions/0005-extension-mechanism.md). The owner accepted it on 2026-11-12.

## What an extension can add

| Contribution | What it is | Example |
|---|---|---|
| **Custom command** | A prompt template in a markdown file. No code. | `/review` expands to the team's review checklist. |
| **Skill** | A folder of instructions the model reads on demand. No code of ours. | "How we write database migrations." |
| **Hook** | Code that runs when something happens in the agent's lifecycle. It can observe, add context, or decide. | Refuse edits to generated files. Run the formatter after every edit. |
| **Tool** | A function the model can call. | Query the team's issue tracker. |
| **Command with code** | A slash command handled by code. | `/deploy-preview` runs a script and reports back. |

## The options

### Three mechanisms

The same tool (JSON in, JSON out) was built behind each mechanism and measured, together with the cost each adds to the host. Method and full tables: [../measurements/extension-mechanisms.md](../measurements/extension-mechanisms.md) and [../measurements/dependency-costs.md](../measurements/dependency-costs.md). Desktop figures.

| | WASM components | Subprocess speaking JSON | Embedded scripting |
|---|---|---|---|
| **What it is** | Extensions compiled to WebAssembly and run in-process by an embedded runtime. | Extensions are ordinary programs. The harness starts them and exchanges JSON over standard I/O. | An interpreter inside the harness runs extension scripts. |
| **Can the harness write one and load it at once, on any machine?** | No. The source must be compiled to WASM first, which needs a toolchain on the user's machine and about 13 s per build. | Only if the right runtime (Python, Node) happens to be installed. | **Yes.** The interpreter is in the binary. |
| **Sandbox when the OS cannot help** | Strongest. No access to anything the host does not hand over. | None. The process has the user's rights. | Strong. A script has only the host functions it is given. |
| **Start-up** | 0.14 ms precompiled, 10 ms when compiled at load | 0.34 ms native, 10 to 16 ms for a script runtime | 0.04 to 0.12 ms |
| **Per call (200 bytes)** | 1.4 µs | 27 to 35 µs | 4 to 20 µs |
| **Per call (64 KiB)** | 0.06 ms | 0.10 to 0.31 ms | 0.8 to 5.3 ms |
| **Memory** | 0.3 MB per extension; 17 MB transiently when compiling at load | A process each: 2 MB native, 11 MB Python, 23 MB Bun, 47 MB Node | 0.2 to 1.4 MB per extension |
| **Cost to the host** | The largest dependency measured: 10.2 MB of binary and 65 s of clean release build on 4 cores when added to the harness (10.5 MB and 86 s built alone), and a new major version about monthly. | Nothing beyond what the harness already has. | Small to moderate, depending on the language. See the next table. |
| **Linux and Windows** | Identical. | Script shims, no shebang lines, newline translation, code pages, signals. | Identical once built. |
| **Access to libraries** | Whatever compiles to WASM. | Everything. | Only what the host exposes. |

**What decides it.**

- Per-call speed decides nothing. Every option adds microseconds to a tool call that sits inside a model round trip of seconds.
- The vision's requirements decide it. Only embedded scripting lets the harness write an extension and load it immediately on a machine with nothing else installed, with a sandbox that works on Windows today.
- Subprocesses keep one job: tools that need a real ecosystem (an official client library, a native tool). That job already has a standard, MCP, which the harness supports anyway ([13-mcp.md](13-mcp.md)).

### Which language

With embedded scripting chosen, the candidates are the languages themselves, plus WASM as the target for code the harness writes.

| | JavaScript on QuickJS | JavaScript on a pure-Rust engine (Boa) | Lua 5.4 | Rhai | Rust compiled to WASM |
|---|---|---|---|---|---|
| **Familiar to extension authors** | Very widely known. The first language of most people who script their tools. | The same language. | Known from editors and games; a minority. | Almost unknown. | Known to few; hard to write casually. |
| **Models write it correctly on the first try** (19 tools, a strong and a small model, 38 attempts) | 38 of 38 | 38 of 38 (the same sources) | 38 of 38 | 34 of 38. All four failures were the small model calling functions that do not exist. | 38 of 38 compile and pass, given a compiler. |
| **Start a runtime** | 0.07 ms | 0.15 ms | 0.04 ms | 0.12 ms | 0.14 ms precompiled |
| **Per call, 200 bytes** | 20 µs | 120 µs | 4 µs | 8 µs | 1.4 µs |
| **Per call, 64 KiB of text processing** | 5.3 ms | 155 ms | 0.8 ms | 1.9 ms | 0.06 ms |
| **Memory per runtime** | 0.9 MB | 4.2 MB | 0.2 MB | 1.4 MB | 0.3 MB |
| **Added to the harness binary** | 1.1 MB | 6.7 MB | 0.4 MB | 2.4 MB | 10.2 MB |
| **Added to a clean release build of the harness (4 cores)** | 8.6 s | 55 s | 1.5 s | 11 s | 65 s |
| **Sandbox with no help from the OS** | Strong: no ambient access; memory limit and interrupt hook. The engine is C, so its own bugs are a risk. | Strong, and the engine is memory-safe Rust. | Strong: libraries are chosen by the host; memory limit and instruction hook. The engine is C, small and old. | Strong, memory-safe Rust, with built-in operation limits. | Strongest. |
| **Build needs** | A C compiler. Its Windows (MSVC) support in the Rust binding must be confirmed. | Rust only. | A C compiler. Well-trodden on Windows. | Rust only. Brings one MPL-licensed crate. | Rust only for the host. A full toolchain on the user's machine to write one. |

How the first-try experiment was run, and its limits, are in the measurement page: the code was written by subagents of the session that produced these docs, told to write each file once without running anything, and then executed in the real engines against fixed tests.

**Reading the table.**

- **Models do not separate JavaScript, Lua and Rust** at this size of tool. Rhai trails with a weaker model, because little Rhai exists to learn from.
- **People do separate them.** The audience is the broad public, and JavaScript is the language most of them already write.
- **The pure-Rust JavaScript engine is too slow for the job.** A tool that tidies a 64 KiB command output takes 155 ms in it on a fast desktop and 296 ms on a laptop, about 30 times longer than in QuickJS, and it costs six times the binary.
- **WASM is the strongest sandbox and cannot do the job**: the harness cannot write and load one on a machine without a toolchain.

## Decision

1. **Extensions are JavaScript, run by an embedded QuickJS engine.** One runtime per extension, with memory and time limits, and no access to anything except host functions that are checked one by one.
2. **Tools that need an ecosystem are MCP servers.** There is no second, bespoke process protocol.
3. **The simplest hooks can be a command line** in configuration, with no script.
4. **Fallback: Lua.** If the QuickJS binding does not build cleanly on Windows in the extension step's first hour, the step switches to Lua 5.4, which is smaller, faster and proven on Windows. The host functions are defined once in Rust and are the same for either language, so the switch is contained.

**What is given up.** The script engine is a C library inside the harness. A memory-safety bug in it could, in principle, let a script escape. Against that: scripts are code the user approved, each has a manifest that bounds what it may ask for, and the engine is the one C dependency accepted. The alternative that removes this risk, a pure-Rust JavaScript engine, measured too slow today.

**When to revisit.**
- A pure-Rust JavaScript engine reaches within a few times of QuickJS: switch engines, same language, same extensions.
- A public registry of third-party extensions is wanted: reconsider a WASM tier for code the user has not read.
- Models' reliability in JavaScript and Lua diverges on larger tools.

## The tiers

Each is simpler than the next. Most needs are met by the first three.

### Tier 1: custom commands (files, no code)

A markdown file is a slash command.

```markdown
---
description: Review the staged changes
arguments: [focus]
---
Review the staged changes in this repository. Focus on {{focus | "correctness"}}.
Report problems as a list, most serious first.
```

- **Where.** `<workspace>/.cortex/commands/*.md` and `<config dir>/commands/*.md`. The file name is the command name. Project commands override user commands of the same name.
- **Front matter.** A description for the menu, named arguments with optional defaults, and optionally a settings profile for the turn.
- **Body.** Sent as the user's prompt, with arguments substituted. `{{args}}` is everything typed after the command.
- **No execution.** A command file cannot run anything or include files by itself. It is text for the model, so it needs no trust.

### Tier 2: skills (folders, read on demand)

A skill is a folder with a `SKILL.md` that starts with a name and a description. This is the open Agent Skills layout, read as it is so that people's existing skills work.

- **Where.** `.cortex/skills/`, `.agents/skills/` and `.claude/skills/`, in the project and in the user's home.
- **In the prompt:** each skill's name, description and path, and nothing else. The model reads the file when a task calls for it.
- **Scripts inside a skill** are just files. If the model runs one, it runs through the shell tool under the normal permissions.
- Skills need no trust, for the same reason as instruction files.

### Tier 3: hooks

A hook runs when an event matches. It is either a command line or a function in an extension script.

```toml
[hooks.format-after-edit]
event   = "tool.after"
tools   = ["edit", "write"]
command = ["cargo", "fmt", "--", "{path}"]
observe = true
```

**Events in the first release.**

| Event | When | A hook may |
|---|---|---|
| `session.start` | A session opens. | Add context. |
| `prompt.submit` | The user sends a prompt, before it reaches the model. | Add context. Refuse the prompt with a message. |
| `tool.before` | A tool call has been planned and passed policy, before approval and execution. | Allow, deny, ask the user, or change the arguments. |
| `tool.after` | A tool call finished. | Add a note to the result. |
| `turn.end` | A turn completed. | Observe. Add a notice for the next turn. |
| `compaction.before` | Before a summary is requested. | Add instructions for the summary. |
| `session.end` | The session closes. | Observe. |

The set is small on purpose. Every event is a compatibility promise, and both reference harnesses show how large an event list grows when adding is free.

**Contract, the same for both forms.**

- **Input.** One JSON object: the event name, session and turn ids, the workspace path, and the event's own fields (for tool events: the tool name, the arguments, the plan summary and, afterwards, the result). A command receives it on standard input. A script function receives it as its argument.
- **Output.** Nothing, or one object.

| Field | Meaning |
|---|---|
| `decision` | For gating events: `allow`, `deny` or `ask`. Absent means no opinion. |
| `reason` | Shown to the user, and to the model when the decision is `deny`. |
| `arguments` | For `tool.before`: replacement arguments. They are validated, re-planned and re-checked by policy, and the change is shown to the user. |
| `context` | Text appended to the conversation as a notice, labelled as coming from the hook. Capped at 8 KiB. |

- **Failure and timeout.** A gating hook that fails or times out counts as `deny`, with the failure shown. A hook marked `observe = true` cannot decide anything, and its failure is logged and ignored. Default timeouts: 5 s for gating events, 60 s for observers.
- **Several hooks on one event** run concurrently. Decisions combine as `deny` over `ask` over `allow`. Argument changes apply only if exactly one hook proposes one. The outcome does not depend on order.
- **`{path}`-style placeholders** in a `command` are filled as separate arguments. They are never interpolated into a shell string. Hook commands run in the sandbox ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)).

### Tier 4: script extensions

An extension is a folder with a manifest and JavaScript.

```
<config dir>/extensions/tracker/
    extension.toml
    main.js
```

**The manifest** says what the extension is and what it may ask the host to do.

```toml
name = "tracker"
version = "0.1.0"
api = 1
description = "Issue tracker tools"
main = "main.js"

[capabilities]
exec  = ["gh"]                  # programs it may ask the host to run
read  = ["**"]                  # workspace paths it may read
write = []                      # workspace paths it may write
net   = []                      # hosts it may contact through the host's fetch
```

**The script** registers what it contributes.

```js
cortex.tool({
  name: "find_issue",
  description: "Search issues by text",
  input: { type: "object", properties: { query: { type: "string" } },
           required: ["query"], additionalProperties: false },
  readOnly: true,
  async run(args) {
    const r = await cortex.exec("gh", ["issue", "list", "--search", args.query, "--json", "number,title"]);
    if (r.code !== 0) throw new Error(r.stderr);
    return { issues: JSON.parse(r.stdout) };
  },
});

cortex.on("tool.before", { tools: ["edit", "write"] }, (event) => {
  if (event.arguments.path.startsWith("generated/")) {
    return { decision: "deny", reason: "generated files are rebuilt from schema/" };
  }
});
```

**Host functions.** This is the whole surface a script has. There is no file, process, network, timer or module API other than these.

| Function | Does | Checked against |
|---|---|---|
| `cortex.tool(def)`, `cortex.command(def)`, `cortex.on(event, filter, fn)` | Register contributions. | The manifest's name and API version. |
| `cortex.exec(program, args, options)` | Asks the host to run a program. Returns exit code and bounded output. | `capabilities.exec`, then the permission engine as an `exec` effect, then the sandbox. |
| `cortex.fs.read`, `cortex.fs.write`, `cortex.fs.list`, `cortex.fs.search` | Workspace file access through the same code as the built-in tools. | `capabilities.read` and `write`, then the permission engine. |
| `cortex.http.fetch(url, options)` | An HTTP request made by the host. | `capabilities.net`, then the permission engine as a `network` effect. |
| `cortex.ask(question, choices)`, `cortex.notify(text)`, `cortex.progress(text)` | Talk to the user through the front end. | Rate limits. |
| `cortex.state.get`, `cortex.state.set` | A small key-value store per extension, kept in the session and the state directory. | A size cap. |
| `cortex.log(...)` | Writes to the log. | |

There is no function that returns credentials, reads the session, changes configuration or grants permission.

**The runtime.**

| Property | Design |
|---|---|
| Isolation | One runtime per extension. Extensions cannot see each other. |
| Limits | Memory per runtime (default 32 MB, hard cap 64 MB). Time per call (2 minutes for tools, the hook timeouts for hooks), enforced by the engine's interrupt hook, so a tight loop is stopped. |
| Language | JavaScript modules. Imports resolve only inside the extension's own folder. Types for the host functions ship as a declaration file for editors. |
| Threading | All runtimes live on one script-host thread, off the core thread. Host functions are asynchronous: a script awaits them, and the core does the work. |
| Start-up cost | None. What an extension registers is cached with its hash when it is approved. At start-up the harness reads the cache and knows the tools, commands and hooks without starting the engine. The engine starts the first time something of the extension's is used. |
| Failure | An exception is an error result for that call. A runtime that hits a limit is torn down and recreated on next use. The session is never affected. |

**Tools from extensions** are reached through `tool_search` and `tool_call` ([05-tools.md](05-tools.md)), so adding one never changes the declared tool list. An extension can ask for a tool to be declared directly from the next context epoch (`expose = "direct"` in the manifest), and the harness may promote frequently used ones the same way.

## Self-written extensions

"Write me a tool that…" is an ordinary request to the agent, or `/extend <request>`.

1. **The agent writes a proposal** with the `propose_extension` tool: a name, a manifest, and the files. It can include test cases (arguments and expected results).
2. **The harness checks it** before the user sees it:
   - the manifest is valid, and its capabilities are flagged if broad (any program, any host, write access to everything);
   - every file parses;
   - the script is loaded in a throwaway runtime with host functions stubbed, to list what it registers;
   - test cases run in the same throwaway runtime. Host calls made during tests are recorded and shown, not performed.
3. **The user reviews** in a dialog: what it adds, what it may ask the host to do in plain words, the test results, and the full source in the viewer. Choices: approve for me, approve for this project, edit, or reject with a note that goes back to the agent.
4. **On approval** the files are written, the hash is pinned, and the extension is live: its tools through `tool_search`, its commands in the menu, its hooks from the next event. Nothing is restarted.
5. **Later changes** by the agent are new proposals, shown as a diff. Changes the user makes by hand are picked up by hot-reload and ask for the same review.

The agent writes extensions with no more power than the user grants at review: the manifest shown in the dialog is exactly what the runtime enforces.

## Hot-reload

Covered with configuration in [08-configuration.md](08-configuration.md). For extensions: a changed file is noticed at the next check, the extension's hash is compared with the approved one, and either the runtime is recreated (hash unchanged or change approved) or the extension is disabled pending review.

## Safety

| Measure | Detail |
|---|---|
| No ambient access | A script can do nothing except call host functions. This holds on every OS. |
| Declared capabilities | The manifest bounds what the extension may ask for. The runtime checks every host call against it. |
| Policy still applies | After the manifest check, each host call goes through the permission engine like a built-in tool's effects. An extension's tool asks by default. |
| The sandbox still applies | Programs run on an extension's behalf go through the sandbox launcher. |
| Review before load | Nothing the agent or a repository supplies runs before the user has seen it. |
| Trust pinned by hash | Any change disables the extension until approved again. |
| Limits | Memory, time and output size, per call and per runtime. |
| Bounded, labelled output | Tool results pass the standard output policy and are labelled as coming from the extension. |

The threats are in [../security.md](../security.md), section 3.

## Windows

- Extension scripts behave identically: the engine is in the binary and the host functions are the same code.
- `cortex.exec` names a program and its arguments as a list. Nothing passes through a shell. Script shims (`.cmd`, `.bat`) are resolved by the host and run with arguments it escapes itself; arguments that cannot be escaped safely are refused.
- Hook commands follow the same rules.

## What is in the first usable release

| | First usable release | Later |
|---|---|---|
| Custom commands | yes | |
| Skills | yes, read from the three folder conventions | |
| Hooks | yes: the seven events, as commands or script functions | More events as needs are shown. |
| Script extensions | yes: tools, commands, hooks, the host functions above | UI contributions (status items, panels). |
| Self-written extensions | yes: proposal, checks, review, immediate load | Sharing and installing extensions from a git URL at a pinned commit. |
| Hot-reload | yes | |
| Management | `cortex ext list`, `inspect`, `disable`, `remove`, `add <path>` | |
| Examples | Three small extensions under `examples/`, used as tests | |

## Relation to MCP

| If you want to… | Use |
|---|---|
| Add a prompt you type often | A custom command. |
| Teach the agent a procedure | A skill. |
| Gate or observe what the agent does | A hook. |
| Add a tool that glues together things already on the machine | A script extension. The agent can write it for you. |
| Add a tool that needs a client library, a database driver or a long-running process | An MCP server ([13-mcp.md](13-mcp.md)). |

Script extensions and MCP servers share the dynamic-tool registry, the trust store, the permission check and the output policy.
