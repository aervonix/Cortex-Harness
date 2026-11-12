# 0001: The TUI is written in Rust, inside the main binary

- **Status:** Provisional. The owner approves this choice. The Windows Terminal checklist has not been run yet.
- **Date:** 2026-11-12

## Context

The core is Rust. The interactive TUI could be Rust inside the same binary, or TypeScript as a separate front end that talks to the core over the RPC protocol. The brief was to settle this with measurements.

## What was measured

Two comparable throwaway TUIs drove the same stub core, which streamed fake model output and tool results at 100, 400 and 2,000 tokens per second. Rust used ratatui (the strongest Rust TUI crate) and, as a third data point, a small line-based renderer of our own. TypeScript used Ink 8 (the strongest maintained TypeScript terminal UI library) on both Node and Bun. A harness ran each under a pseudo-terminal, recorded the output with timestamps, typed keys during the stream, and replayed the recording through a terminal-state parser.

The method, every table, and what was not measured are in [measurements/tui-language-spike.md](../measurements/tui-language-spike.md).

## The numbers

Desktop, medians. "Light" is a one-line live region; "heavy" is a 30-line live region re-rendered every frame. The laptop run gave the same ordering and ratios.

| Measure | Rust (ratatui, inline) | Rust (own line renderer) | TypeScript (Ink on Node) | TypeScript (Ink on Bun) |
|---|---|---|---|---|
| Cold start to first frame | 1.0 ms | 0.8 ms | 98 ms | 51 ms |
| Idle memory | 2.5 MB | 2.0 MB | 84.5 MB | 50.6 MB |
| Streaming memory, peak (2,000 tok/s) | 5.3 MB | 4.9 MB | 132 MB | 127 MB |
| Memory after a 60-second fast session | 4.2 MB | 3.1 MB | 180 MB | 146 MB |
| Frame time, light, 2,000 tok/s (median / 99th) | 0.10 / 0.20 ms | 0.02 / 0.04 ms | 0.68 / 1.39 ms | 0.64 / 1.43 ms |
| Frame time, heavy, 2,000 tok/s (median / 99th) | 0.21 / 0.28 ms | 0.03 / 0.04 ms | 2.09 / 3.17 ms | 2.48 / 4.64 ms |
| Dropped frames at 60 per second | 0% | 0% | 0 to 0.3% | 0 to 0.3% |
| Token arriving to token on screen (median / 99th) | 8.1 / 16.7 ms | 8.1 / 16.6 ms | 8.5 / 16.5 ms | 8.7 / 17.1 ms |
| Input latency while streaming, light (median / 99th) | 0.15 / 0.27 ms | 0.09 / 0.18 ms | 0.91 / 1.58 ms | 0.96 / 1.65 ms |
| Input latency while streaming, heavy (median / 99th) | 0.25 / 0.43 ms | 0.11 / 0.20 ms | 2.27 / 3.96 ms | 2.81 / 5.12 ms |
| CPU while streaming at 400 tok/s | 1.7% of a core | 1.4% | 8.1% | 10.5% |
| CPU while streaming at 2,000 tok/s, heavy | 3.7% | 2.7% | 29.9% | 40.1% |
| Visible flicker, terminal with synchronized updates | none | none | none | none |
| Visible flicker, terminal without them: frames that could show a blank flash (of about 680) | 0 light, 5 heavy | 0 | 285 light, 493 heavy | 268 light, 510 heavy |
| Size of what ships | 0.8 MB of UI in the one binary | the same | 0.4 MB bundle + a 150 MB runtime + the core binary | a 110 MB executable + the core binary |
| Behaviour on a Linux terminal | Correct under the harness's terminal model | Correct | Correct | Correct |
| Behaviour in Windows Terminal | Not measured. See "The gap". | | | |

Two findings worth stating plainly:

- **TypeScript is smooth enough.** Built with care, the Ink prototype holds 60 frames a second at every rate tested. Frame time and dropped frames do not decide this.
- **Rust is one to two orders of magnitude leaner** on start-up (50 to 100 times), memory (20 to 40 times, and flat where TypeScript grows with the session), CPU (5 to 10 times) and shipped size (over 100 times), and 5 to 20 times quicker to echo a key.

## What else was weighed

| Consideration | Rust in the binary | TypeScript over RPC |
|---|---|---|
| **Speed of UI iteration** | Slower. An edit means a rebuild and relink of seconds (the measured floor for relinking the full dependency set is 0.4 s), and there is no live reload of code. Themes and key bindings do reload live. Snapshot tests give fast feedback on layout. | Faster. Component reload in well under a second and a large body of React practice. This is TypeScript's real advantage. |
| **CI time and toolchains** | One toolchain. | A second toolchain on both OSes: runtime install, package install, type-check, lint and tests. Not measured; an estimate of one to one and a half minutes more per job, against a 5-minute limit that is already tight on Windows. A second lockfile and a second dependency tree to audit (about 45 installed packages for the prototype alone). |
| **Distribution and installation** | One file of about 12 to 16 MB. | Two artefacts to ship and keep in step: the core binary and either a 110 MB compiled front end or a package that needs the user to have Node. A protocol version mismatch between them becomes a support case. |
| **Extensions** | No effect. Extensions run in the core's script runtime ([0005](0005-extension-mechanism.md)) whatever the UI is written in. | No effect. |
| **A future GUI, web UI or editor client** | These will be TypeScript and will use the RPC protocol. The protocol and its generated TypeScript declarations exist regardless ([0008](0008-rpc-protocol.md)), and a small TypeScript client is part of the RPC step's tests. | The TUI would exercise the protocol every day, which is a real benefit. The in-process UI uses the same operations and events, and a conformance test compares the two carriers, which gets most of that benefit. |
| **The vision** | "Near-instant start-up, small memory use" is a headline. | A 50 to 100 ms start and 50 to 180 MB would be the product's floor. |

## Decision

Rust, in the main binary, in the `cortex-term` and `cortex-tui` crates.

The RPC protocol is nonetheless complete enough for a TypeScript client to be built later with no change to the core: it is the same contract the built-in TUI uses.

## Why

- The measured differences are in exactly the dimensions the project puts first, and they are large.
- The second toolchain's cost lands on the tightest budget in the project, the Windows CI job.
- One binary is the simplest thing to install, update and support, for an audience that is the broad public.
- TypeScript's advantage, iteration speed, is real and is the price. It is partly offset by pure view functions with snapshot tests, and by live reload of themes and key bindings.

## The gap

**Windows Terminal was not measured.** The measurement harness runs on Linux; the desktop used for measurements runs Linux under WSL 2. The Rust side compiles for Windows, and the harness itself can run there under ConPTY, but how a real Windows Terminal window behaves (flicker, resize, keys) needs eyes. The checklist is in [measurements/windows-terminal-checklist.md](../measurements/windows-terminal-checklist.md), for the owner to run on the desktop. Its results belong in this record.

Nothing known about Windows favours TypeScript here: both prototypes send the same kinds of terminal sequences, and Node's start-up is slower on Windows than on Linux.

## Revisit when

- The Windows checklist shows the Rust TUI misbehaving in a way a TypeScript one would not.
- UI work becomes the bottleneck of the project and iteration speed matters more than footprint.
- A TypeScript terminal UI library with a native core that the project's rules allow reaches Rust-like start-up and memory.
