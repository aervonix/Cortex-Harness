# 0005: Extensions are JavaScript on an embedded QuickJS engine; MCP for process tools; Lua as the fallback

- **Status:** Provisional. The mechanism and the language are the owner's call, and the Windows build of the engine is not yet confirmed.
- **Date:** 2026-11-12

## Context

The extension system lets users add tools, slash commands and hooks. The [vision](../vision.md) gives it a harder job than "plugins":

- **Self-written.** The user asks for a new tool or command, the harness writes it, the user reviews it, and it loads at once.
- **Sandboxed**, including on Windows, where the first release has no OS sandbox.
- **Hot-loaded**, with no restart.
- **For the broad public**, so it cannot assume Python, Node or a compiler is installed.

The brief asked for a real comparison of at least WASM components, a subprocess speaking JSON over standard I/O, and embedded scripting, on sandboxing, start-up and memory cost, API stability, and how each behaves on Linux and Windows. A follow-up asked that, if scripting were chosen, the languages themselves be compared: an embedded JavaScript or TypeScript engine, Lua, Rhai, and WASM components as a target for self-written code, weighing familiarity to authors, how reliably models write each, binary size and cold start against the budgets, and sandbox strength where the OS cannot help.

Both reference harnesses load extensions (TypeScript modules in one, JavaScript or TypeScript plugins in the other) into their own process, with the user's full rights and no isolation ([reference-notes.md](../reference-notes.md)). That is natural for a program that is itself running in a JavaScript runtime, and it is where a Rust harness can do better.

## Part 1: the mechanism

The same tool was built behind each mechanism and measured. Method and full tables: [measurements/extension-mechanisms.md](../measurements/extension-mechanisms.md). Host costs: [measurements/dependency-costs.md](../measurements/dependency-costs.md). Desktop figures; build times are clean builds pinned to 4 cores.

| | WASM components | Subprocess speaking JSON | Embedded scripting |
|---|---|---|---|
| **Can the harness write one and load it at once, on any machine?** | No. It must be compiled first: a toolchain on the user's machine and about 13 s per build. | Only if the right runtime happens to be installed. | **Yes.** |
| **Sandbox with no help from the OS** | Strongest: nothing is reachable unless the host hands it over. | None: the process has the user's rights. Confinement depends on the OS sandbox, which Windows lacks in the first release. | Strong: a script reaches only the host functions it is given. |
| **Start-up** | 0.14 ms precompiled; 10 ms compiled at load | 0.34 ms for a native program; 10 to 16 ms for Bun, Python or Node | 0.04 to 0.12 ms |
| **Per call, 200 bytes** | 1.4 µs | 27 to 35 µs | 4 to 20 µs |
| **Per call, 64 KiB of text** | 0.06 ms | 0.10 to 0.31 ms | 0.8 to 5.3 ms |
| **Memory per extension** | 0.3 MB (up to 17 MB while compiling at load) | A process each: 2 MB native, 11 MB Python, 23 MB Bun, 47 MB Node | 0.2 to 1.4 MB |
| **Cost to the harness binary** | + 10.2 MB | none | 0.4 to 2.4 MB, by language |
| **Cost to a clean release build** | + 64.9 s | none | 1.5 to 11 s, by language |
| **API stability** | The runtime ships a new major version about monthly. The component model and its interface language are still moving, and guest toolchains differ per language. | We define and version the protocol. Stable, and a second protocol to maintain beside MCP. | We define the host functions. The languages themselves are stable. |
| **Linux and Windows** | Identical. | The fragile option on Windows: script shims, no shebang lines, newline translation, code pages, no signals. | Identical once the engine builds. |
| **Hot reload** | Recompile, then reload. | Restart the process. | Re-read the file. |
| **Libraries available to the author** | Whatever compiles to WASM. | Everything. | Only what the host exposes. |

**What decides it.** Not speed: every option adds microseconds to a tool call that sits inside a model round trip of seconds. The vision decides it. Only embedded scripting lets the harness write an extension and load it immediately on a machine with nothing else installed, inside a sandbox that works on Windows today.

Subprocesses keep one job, tools that need a real ecosystem, and that job already has a standard. The harness supports MCP anyway ([0010](0010-mcp-in-first-release.md)), so there is no second process protocol of our own.

## Part 2: the language

| | JavaScript on QuickJS | JavaScript on Boa (a pure-Rust engine) | Lua 5.4 | Rhai | Rust compiled to WASM |
|---|---|---|---|---|---|
| **Familiar to extension authors** | The most widely known scripting language; what most people who script their tools already write. | The same language. | A minority, from editors and games. | Almost unknown. | Few, and not casually. |
| **Models write it correctly first try** (19 tools, two models, 38 attempts) | 38 of 38 | 38 of 38 (same sources) | 38 of 38 | 34 of 38 | 38 of 38, given a compiler |
| **Start a runtime** | 0.07 ms | 0.15 ms | 0.04 ms | 0.12 ms | 0.14 ms precompiled |
| **Per call, 200 bytes** | 20 µs | 120 µs | 4 µs | 8 µs | 1.4 µs |
| **Per call, 64 KiB of text** | 5.3 ms | 155 ms | 0.8 ms | 1.9 ms | 0.06 ms |
| **Memory per runtime** | 0.9 MB | 4.2 MB | 0.2 MB | 1.4 MB | 0.3 MB |
| **Added to the harness binary** | + 1.1 MB | + 6.7 MB | + 0.4 MB | + 2.4 MB | + 10.2 MB |
| **Added to a clean release build** | + 8.6 s | + 55.2 s | + 1.5 s | + 11.0 s | + 64.9 s |
| **Added to a clean dev build** | + 3.3 s | + 10.5 s | + 0.6 s | + 5.5 s | + 19.6 s |
| **Crates added** | 3 | 70 | 9 | 15 | 57 |
| **Cold start of the harness** | No effect: the engine is created on first use. | The same. | The same. | The same. | The same. |
| **Sandbox with no help from the OS** | Strong. No ambient access. A memory limit and an interrupt hook stop runaway scripts. The engine is C, so a bug in the engine itself is the residual risk. | Strong, and the engine is memory-safe Rust. | Strong. The host chooses which libraries exist. A memory limit and an instruction hook. The engine is C: small, old and widely embedded. | Strong. Memory-safe Rust, with operation limits built in. | Strongest. |
| **Build needs** | A C compiler. The binding's support for the Windows toolchain must be confirmed. | Rust only. | A C compiler. Well-trodden on Windows. | Rust only. Pulls in one MPL-2.0 crate. | Rust only for the host. A toolchain on the user's machine to write one. |

"Added to the harness" is the difference between a binary with the core dependency set and the same binary plus the engine. Each engine built alone costs more than that, because the harness already has some of its dependencies; both sets of figures are in the measurement page.

TypeScript was considered as the authoring language. Running it needs a type-stripping step in front of the engine, which means a parser to carry. Extensions are plain JavaScript; a type declaration file for the host functions gives editors the same help.

### The first-try experiment

Models were asked to write 19 small tools in each language, once, with no way to run or test them, from a one-page contract per language. The files were then executed in the real engines against 80 fixed test cases. A tool counted only if all its cases passed.

| | Strong model | Small model |
|---|---|---|
| JavaScript (QuickJS and Boa) | 19 of 19 | 19 of 19 |
| Lua | 19 of 19 | 19 of 19 |
| Rust | 19 of 19 | 19 of 19 |
| Rhai | 19 of 19 | 15 of 19 |

All four Rhai failures were calls to standard-library functions that do not exist in Rhai.

**How it was run.** The writers were sixteen subagents of the design session, one per model, language and task set: the model the session ran on (Claude Fable 5.2) and the same family's small model (Claude Haiku 5.5). They ran on the owner's plan; no paid API was called.

**The caveat.** This approximates "a model writing an extension inside the harness". It is not that: the subagents had a general coding-assistant prompt, the models are two from one vendor, the tools are 10 to 60 lines, and each was tried once. It is enough to say that JavaScript, Lua and Rust are written reliably at this size and that Rhai trails with a weaker model. It is not enough to rank JavaScript against Lua. The full method, so it can be repeated with other models, is in [measurements/extension-mechanisms.md](../measurements/extension-mechanisms.md).

### Reading the comparison

- **Models do not separate JavaScript, Lua and Rust.** Rhai is behind, for lack of training material, and it would stay behind.
- **People do separate them.** For a product aimed at the broad public, JavaScript is the language authors already have. An extension the harness wrote is also one the user must be able to read before approving it.
- **Boa is too slow for the job today.** It is the only option that is both JavaScript and free of C. It is 6 times slower than QuickJS on a small call and about 30 times slower on real text work (155 ms against 5 ms; 296 ms against 12 ms on the laptop), and it costs six times the binary and build time.
- **WASM is the strongest sandbox and cannot do the job.** No toolchain on the user's machine means no self-written extension. It is also the largest dependency measured.
- **Lua is the cheapest and fastest by a clear margin**, and loses only on familiarity.

## Decision

1. **Extensions are JavaScript, run by an embedded QuickJS engine** in `cortex-ext`, behind the `scripts` cargo feature. One runtime per extension, created on first use, with a memory limit and a time limit. A script can reach nothing except host functions, and each host function is checked against the extension's manifest and then against the session's permissions.
2. **Tools that need an ecosystem are MCP servers.** No bespoke subprocess protocol.
3. **The simplest hooks are a command line** in configuration, with no script.
4. **Custom commands and skills are files** and involve no code.
5. **The fallback is Lua 5.4.** The extension build step starts with a one-hour check that the QuickJS binding builds and passes its tests on the Windows runner. If it does not, the step switches to Lua. Host functions are defined once in Rust and are the same for either language, so only the engine glue and the examples change.
6. **No WASM runtime** in the first release.

Specified in [architecture/12-extensions.md](../architecture/12-extensions.md).

## Why

- It is the only combination that does all four things the vision asks: self-written, sandboxed without the OS, hot-loaded, and usable by anyone.
- Its cost is measured and modest: 1.1 MB of binary and under 9 s of clean release build, nothing at start-up, about a megabyte per loaded extension.
- JavaScript is the language the audience knows, and models write it reliably.
- One process protocol (MCP) instead of two.

## What is given up

- **A C library inside the harness.** This is the one accepted exception to the rule against C dependencies ([architecture/01-workspace.md](../architecture/01-workspace.md)). A memory-safety bug in the engine could let a script escape. Against that: every script was shown to the user and approved, its manifest bounds what it can ask for, and a changed script is disabled until reviewed again.
- **No library ecosystem for scripts.** No package manager, no Node APIs. Anything that needs one is an MCP server.
- **Lua's speed and size.** QuickJS costs about three times the binary and five times the per-call time of Lua. Both are far inside the budgets.
- **Extensions are not portable** to or from other harnesses. That follows from the project's rules as well as from this choice.

## Conflict with the speed budgets

This is conflict C1 in the [build plan](../build-plan.md). The vision's extension requirements cannot be met without carrying an engine. The resolution recommended here is the engine with the best ratio of fit to cost, behind a feature flag so a build without it stays possible, and with its cost recorded in the size budget.

## Revisit when

- **The Windows build check fails:** switch to Lua, as above.
- **A pure-Rust JavaScript engine comes within a few times of QuickJS:** switch engines. Same language, same extensions, no C.
- **A public registry of third-party extensions is wanted:** code the user has not read deserves a stronger sandbox. Reconsider a WASM tier for that case only.
- **Evidence from real use** shows models writing extensions unreliably in JavaScript, or authors asking for Lua.
