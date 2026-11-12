# 0012: Build profiles, and unwinding on panic

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

The build profiles set three things the project budgets for: how long builds take (locally and in CI), how large the shipped binary is, and how the program behaves when a bug causes a panic. They are cheap to choose now and tedious to change once CI caches and baselines depend on them.

## Measurements

A program with the core dependency set, clean release builds pinned to 4 cores on the desktop, binary stripped. Method: [measurements/dependency-costs.md](../measurements/dependency-costs.md).

| LTO | Codegen units | Opt level | Panic | Build | Binary |
|---|---|---|---|---|---|
| off | 16 | 3 | abort | 31.6 s | 7.77 MB |
| thin | 16 | 3 | abort | 34.8 s | 8.15 MB |
| thin | 16 | 3 | unwind | 37.2 s | 8.39 MB |
| thin | 1 | 3 | abort | 36.6 to 38.5 s | 7.16 MB |
| thin | 1 | 3 | unwind | 38.4 s | 7.42 MB |
| fat | 1 | 3 | abort | 51.9 to 54.1 s | 6.66 MB |
| fat | 1 | 3 | unwind | 54.6 s | 6.83 MB |
| fat | 1 | s | abort | 38.7 s | 5.25 MB |
| fat | 1 | z | abort | 34.2 s | 5.06 MB |

What the table shows:

- **Unwinding costs 0.17 to 0.27 MB (2.5 to 3.7%) of binary** and no measurable build time.
- **Fat LTO with one codegen unit** gives the smallest binary at full optimisation, for about 15 s more build than thin.
- **Optimising for size** (`s` or `z`) saves a further 1.4 to 1.6 MB. Its effect on speed was not measured.
- **Thin LTO with 16 codegen units** is the largest of the optimised builds and only 3 s slower to build than no LTO.

Run-time speed was not measured across profiles. The harness is mostly waiting on the network and on child processes; its own hot paths are rendering, search, diffing and JSON. Of these, rendering and JSON were measured in the TUI spike, which was built like `dist` below (fat LTO, one codegen unit) but with `panic = "abort"`.

## Options for the panic strategy

| Option | For | Against |
|---|---|---|
| `panic = "abort"` | Smaller binary. No unwinding tables. Simple: a bug ends the process. | One bug in one tool, one extension call or one stream decoder ends the user's session, and with it any unsaved state and running child processes' supervision. Tests need unwinding anyway, so the tested and shipped configurations would differ. |
| `panic = "unwind"`, caught at task boundaries | A panic inside a tool task, an extension call or a provider stream task becomes an error result for that item, and the session goes on. The shipped build is the tested build. | The size cost above. Code must not assume state is consistent after a caught panic, which limits where catching is allowed. |

A panic hook that restores the terminal and writes a crash report works under either strategy. It is not what decides this.

## Decision

| Profile | Use | Settings |
|---|---|---|
| `dev` | Local work | `debug = "line-tables-only"`; dependencies at `opt-level = 1` with no debug info |
| `test` | CI tests | As `dev`, with debug info off in CI through the environment |
| `release` | Local optimised builds and performance runs | `opt-level = 3`, thin LTO, 16 codegen units, no debug info |
| `dist` | Published binaries | Inherits `release`; fat LTO, 1 codegen unit, stripped |

- **`panic = "unwind"` in every profile.**
- Panics are caught only at three boundaries, each of which owns no shared mutable state: a tool execution task, an extension call, and a provider stream task. The result is an `Internal` error for that item. Anywhere else, a panic ends the process through the hook, with the terminal restored and a crash report written.
- A caught panic is logged with its backtrace and counted. It is a bug to be fixed, not a handled condition.
- `opt-level = 3` for `dist`. Size optimisation is held in reserve.

Specified in [architecture/01-workspace.md](../architecture/01-workspace.md) and [architecture/02-runtime.md](../architecture/02-runtime.md).

## Why

- A coding session can hold an hour of context. Losing it to a bug in one tool call is a worse outcome than a few hundred kilobytes.
- Extensions and MCP feed the harness data it did not write. Containing a panic in the code that parses them is cheap insurance.
- Shipping the configuration that is tested removes a class of "works in tests" surprises.
- Two optimised profiles keep performance runs quick to build while published binaries get the smallest size at full speed.

## What is given up

- 0.17 to 0.27 MB (2.5 to 3.7%) of binary size.
- `dist` builds are slow, about 50 s for the dependencies alone on 4 fast cores. They run only on tags and in the nightly tier, never in pull-request checks.

## Revisit when

- **The binary size budget is missed.** The reserve, in order: `opt-level = "s"` for `dist` after measuring the speed cost with `xtask perf`; then `panic = "abort"` with tool and extension calls isolated some other way.
- **Caught panics hide real corruption** in practice: narrow the boundaries further.
