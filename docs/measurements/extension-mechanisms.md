# Extension mechanisms and languages

The measurements behind [decision 0005](../decisions/0005-extension-mechanism.md): how extensions run, and which language they are written in. Taken on 2026-11-12.

Two things were measured:

1. **The mechanisms.** The same small tool behind each way of running extension code, timed and weighed.
2. **The languages.** Whether a model writes a correct extension on the first try in each candidate language.

The spike code is throwaway and is not in this repo.

## Part 1: mechanisms

### What was built

One tool, `summarise`: it takes JSON with a `text` field and returns JSON with the word count, the line count and the longest word. It was written once for each mechanism, each time in the way a careful extension author would write it, and called from one Rust benchmark program.

| Mechanism | How the tool runs |
|---|---|
| Native function (the floor) | A Rust function in the benchmark, with the same JSON in and out. |
| Subprocess, native | A small Rust program. The host starts it once and exchanges one JSON object per line over its standard input and output. |
| Subprocess, Bun / Node / Python | The same protocol, with the tool as a script for each runtime (Bun 1.4.4, Node 26.13, Python 3.12). |
| WASM component, compiled at load | A Rust guest built for `wasm32-wasip2` (127 KB), compiled by the embedded runtime (wasmtime 50, Cranelift) when loaded. |
| WASM component, precompiled | The same guest compiled ahead of time to a 318 KB file and loaded from it. |
| WASM component, new instance per call | Precompiled, with a fresh instance for every call, which is the strongest isolation between calls. |
| Embedded QuickJS | The tool as a JavaScript function in a QuickJS runtime (the rquickjs 0.14 binding). |
| Embedded Boa | The same JavaScript source in Boa 0.22, a JavaScript engine written in Rust. |
| Embedded Lua | The tool in Lua 5.4 (the mlua 0.12 binding, vendored Lua). |
| Embedded Rhai | The tool in Rhai 1.27, a scripting language written in Rust. |

### What was measured

| Metric | How |
|---|---|
| **Init** | Time to make the mechanism ready to take a call: start the process and get its ready message, or create the runtime and load the code. Median of 7 cold inits. |
| **First call** | The first call after init, which pays for any lazy set-up. |
| **Small call** | A call with about 200 bytes of text. 2,000 calls; median and 99th percentile. This is the fixed overhead of crossing into the extension. |
| **64 KiB call** | A call with 64 KiB of text, 200 calls, median. The tool splits it into words, so this measures the language doing real work on a large tool output, plus moving the data across. |
| **Host memory** | Growth of the host's resident memory from creating the runtime and loading the code. |
| **Child memory** | Resident memory of the extension's own process, for the subprocess rows. |

Three benchmark runs on the desktop; the figures are the median run. Boa was added afterwards in a second set of three desktop runs, in which every other row repeated to within a few percent. One run on the laptop.

### Results: desktop

| Mechanism | Init | First call | Small call, median / 99th | 64 KiB call, median | Host memory | Child memory |
|---|---|---|---|---|---|---|
| Native function (floor) | 0 | 0.001 ms | 0.5 / 0.7 µs | 0.04 ms | 0.1 MB | |
| Subprocess, native | 0.34 ms | 0.036 ms | 26.6 / 46.9 µs | 0.10 ms | 0 | 2.0 MB |
| Subprocess, Bun | 10.2 ms | 0.120 ms | 29.5 / 86.5 µs | 0.26 ms | 0 | 22.9 MB |
| Subprocess, Node | 15.5 ms | 0.224 ms | 29.6 / 125 µs | 0.26 ms | 0 | 47.4 MB |
| Subprocess, Python | 10.3 ms | 0.083 ms | 35.1 / 75.5 µs | 0.31 ms | 0 | 10.8 MB |
| WASM, compiled at load | 10.0 ms | 0.022 ms | 1.4 / 1.6 µs | 0.06 ms | 7.5 to 16.7 MB | |
| WASM, precompiled | 0.14 ms | 0.012 ms | 1.4 / 1.6 µs | 0.06 ms | 0.3 MB | |
| WASM, new instance per call | 0.10 ms | 0.036 ms | 28.2 / 67.8 µs | 0.12 ms | 0.3 MB | |
| Embedded QuickJS | 0.07 ms | 0.025 ms | 20.4 / 46.2 µs | 5.3 ms | 0.9 MB | |
| Embedded Boa | 0.15 ms | 0.142 ms | 120 / 737 µs | 155 ms | 4.2 MB | |
| Embedded Lua 5.4 | 0.04 ms | 0.005 ms | 4.1 / 8.7 µs | 0.79 ms | 0.2 MB | |
| Embedded Rhai | 0.12 ms | 0.013 ms | 8.0 / 15.5 µs | 1.9 ms | 1.4 MB | |

### Results: laptop

| Mechanism | Init | Small call, median / 99th | 64 KiB call, median | Host memory | Child memory |
|---|---|---|---|---|---|
| Native function (floor) | 0 | 1.3 / 1.5 µs | 0.08 ms | 0.1 MB | |
| Subprocess, native | 0.50 ms | 8.7 / 11.7 µs | 0.12 ms | 0 | 2.3 MB |
| Subprocess, Bun | 15.5 ms | 13.4 / 24.9 µs | 0.45 ms | 0 | 23.2 MB |
| Subprocess, Node | 23.0 ms | 15.2 / 38.2 µs | 0.49 ms | 0 | 47.6 MB |
| Subprocess, Python | 13.5 ms | 16.2 / 19.8 µs | 0.48 ms | 0 | 11.6 MB |
| WASM, compiled at load | 31.2 ms | 3.4 / 3.6 µs | 0.13 ms | 4.2 MB | |
| WASM, precompiled | 0.27 ms | 3.2 / 3.4 µs | 0.12 ms | 0.4 MB | |
| WASM, new instance per call | 0.20 ms | 37.2 / 55.1 µs | 0.21 ms | 0.3 MB | |
| Embedded QuickJS | 0.15 ms | 47.4 / 50.7 µs | 12.2 ms | 1.1 MB | |
| Embedded Boa | 0.32 ms | 263 / 1,388 µs | 296 ms | 4.2 MB | |
| Embedded Lua 5.4 | 0.06 ms | 6.4 / 14.4 µs | 1.1 ms | 0.3 MB | |
| Embedded Rhai | 0.22 ms | 16.1 / 25.6 µs | 4.0 ms | 1.4 MB | |

Subprocess round trips are faster on the laptop (native Linux) than on the desktop (WSL 2), where pipe wake-ups cost more. Everything computed in-process is faster on the desktop.

### What each mechanism costs the host

From [dependency-costs.md](dependency-costs.md): clean builds pinned to 4 cores, and the size added to a stripped release binary. "In the harness" is the cost of adding the engine to a binary that already has the core dependency set, which is the honest number, because some of an engine's dependencies are already there.

| Engine | Alone: release build | Alone: size added | Crates | In the harness: release build added | In the harness: size added |
|---|---|---|---|---|---|
| QuickJS (rquickjs) | 22.2 s | 1.2 MB | 11 | + 8.6 s | + 1.1 MB |
| Lua 5.4 (mlua, vendored) | 8.3 s | 0.4 MB | 23 | + 1.5 s | + 0.4 MB |
| Luau (mlua) | 24.6 s | 1.7 MB | 22 | not measured | not measured |
| Rhai | 24.2 s | 2.3 MB | 27 | + 11.0 s | + 2.4 MB |
| Boa | 74.1 s | 7.0 MB | 133 | + 55.2 s | + 6.7 MB |
| wasmtime, trimmed (runtime, Cranelift, component model) | 86.1 s | 10.5 MB | 98 | + 64.9 s | + 10.2 MB |
| wasmtime, default features | 102.3 s | 14.4 MB | 147 | not measured | not measured |
| Subprocess | 0 | 0 | 0 | 0 | 0 |

Building the WASM guest itself took about 13 s on the laptop, and needs a Rust toolchain with the WASM target on the machine that builds it.

### Reading the mechanism numbers

1. **Per-call overhead decides nothing.** The slowest mechanism adds tens of microseconds to a tool call that lives inside a model round trip of seconds.
2. **Init matters only for script runtimes started as processes** (10 to 23 ms each, 11 to 47 MB each). Every in-process option is ready in well under a millisecond.
3. **Real work in the extension is where the languages differ.** On 64 KiB of text: WASM 0.06 ms, Lua 0.8 ms, Rhai 1.9 ms, QuickJS 5.3 ms, Boa 155 ms. For the tools extensions are expected to be (glue around host functions, shaping some text) all but Boa are comfortably fast.
4. **WASM is the fastest and the most expensive to carry**: the largest dependency measured, by a wide margin.

## Part 2: can a model write it?

The vision asks for extensions the harness writes itself. So the language has to be one that models write correctly without a compile-and-fix loop.

### Method

- **Tasks.** 19 tools specified in prose: 10 small ones (word statistics, bumping a semantic version, parsing test-runner output, scanning for to-do comments, reading a path out of JSON, making a slug, removing duplicate lines, listing changed files through a host function that runs `git`, summarising a CSV column, parsing a duration) and 9 harder ones (sorting version strings, wrapping text, summarising a unified diff, glob matching, summarising a log, counting top words, parsing an INI file, evaluating reverse Polish notation, computing a relative path).
- **Tests.** 80 fixed cases in total (38 for the small tools, 42 for the harder ones), written before any code was generated. A tool passes only if every one of its cases passes.
- **Languages.** JavaScript, Lua, Rhai, and Rust (as the source language for a WASM component). Each had a one-page contract: the entry point, how arguments arrive, the two host functions available (`read_file` and `exec`), and what is not available (for JavaScript: no Node APIs, no modules, no console).
- **Writers.** Two models: the strong model this design session ran on (Claude Fable 5.2), and the small fast model of the same family (the agent tool's `haiku` option, which was Claude Haiku 5.5 at the time). Each wrote every tool in every language.
- **One shot.** Each file was written once. The writer could not run anything, see a test, or revise.
- **Execution.** The files were then loaded into the real engines (QuickJS, Boa, Lua 5.4, Rhai) and run against the cases. The Rust files were compiled into a native test runner, and checked to compile for the WASM target.
- **Count.** 19 tools × 2 models = 38 attempts per language.

### Results

| Language | Small tools, strong model | Small tools, small model | Harder tools, strong model | Harder tools, small model | Total |
|---|---|---|---|---|---|
| JavaScript, run on QuickJS | 10 / 10 | 10 / 10 | 9 / 9 | 9 / 9 | **38 / 38** |
| The same JavaScript, run on Boa | 10 / 10 | 10 / 10 | 9 / 9 | 9 / 9 | **38 / 38** |
| Lua 5.4 | 10 / 10 | 10 / 10 | 9 / 9 | 9 / 9 | **38 / 38** |
| Rust (for WASM) | 10 / 10 | 10 / 10 | 9 / 9 | 9 / 9 | **38 / 38** |
| Rhai | 10 / 10 | 7 / 10 | 9 / 9 | 8 / 9 | **34 / 38** |

All four Rhai failures were the small model calling standard-library functions that Rhai does not have. They are the kind of error a model makes in a language it has seen little of.

### How it was run, and what that limits

- **The writers were subagents of the design session**, sixteen in all: one per model, language and task set (2 × 4 × 2). They ran on the owner's plan; no paid API was called. The strong writer is the same model that wrote these docs.
- **This approximates "a model writing an extension", it does not reproduce it.** The subagents had a general coding-assistant system prompt, not the harness's own, and received the task list as a document. In the harness, the model would have the extension contract in its tool description and the user's request in conversation.
- **Two models from one family.** Other vendors' models may rank the languages differently, most likely in Rhai and least likely in JavaScript.
- **Small tools.** 19 tools of 10 to 60 lines. Larger extensions were not tested.
- **One sample each.** No repeats, so 38 of 38 means "no failures seen", not "never fails".
- **The Rust result assumes a compiler,** and a compile step would catch many errors that a script would hit at run time. That favours Rust in a setting where a compiler exists and rules it out where one does not.

The result is strong enough for the question asked: JavaScript, Lua and Rust are all written reliably at this size, and Rhai is measurably behind with a weaker model.

To repeat it: give a model the task list and one contract, ask for one file per tool written once with no execution, and run the files against the fixed cases in the engine.

## What was not measured

- **Windows.** None of the engines was built or run on Windows here. The Rust-only ones (Boa, Rhai, wasmtime) are not expected to differ. The C-based ones (QuickJS, Lua) need a C compiler; whether the QuickJS binding builds cleanly with the Windows toolchain is the first check in the extension build step and is on the [Windows checklist](windows-terminal-checklist.md).
- **Hostile scripts.** Sandbox strength is argued from each engine's design in the decision record, not tested here. The extension step has tests that try to break out.
- **Many extensions at once.** Memory was measured for one runtime. The per-runtime figures are small enough that tens of extensions fit within the budget.
