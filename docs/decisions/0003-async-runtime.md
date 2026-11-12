# 0003: Tokio, current-thread, on a dedicated core thread

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

The core's work is I/O-bound: one or a few HTTP streams, a handful of child processes, timers, channels. It must never stall a model stream, and it must start fast and stay small.

## Options considered

| Option | Assessment |
|---|---|
| Tokio, multi-thread scheduler | The default most projects use. Work stealing and locks for a workload that has almost no CPU work to share. |
| Tokio, current-thread scheduler on one dedicated thread, with a bounded blocking pool | No locks around session state, deterministic task order, fewer threads. One blocking call stalls everything, so blocking work must be kept off it. |
| Another runtime (smol, async-std) | Smaller. The HTTP stack and the MCP SDK are built on Tokio, so a second runtime would be in the process anyway. |
| No async: threads and blocking I/O | Simplest to reason about. Cancellation of streams and child processes, timeouts and several concurrent sessions become hand-built. |

## Measurements

Desktop, release build, a binary with the planned dependency set ([measurements/dependency-costs.md](../measurements/dependency-costs.md)).

| | Current-thread | Multi-thread (2 workers) |
|---|---|---|
| Runtime creation | 23 to 35 µs | 89 to 105 µs |
| Threads after start | 1 | 3 |
| Resident memory with the runtime and an HTTP client alive | 6.4 MB | 6.4 MB |
| Tokio's own cost: clean release build | 6.8 s | 7.3 s |
| Tokio's own cost: binary size | +187 KiB | +267 KiB |

The two flavours cost about the same. The choice is about behaviour, not footprint.

## Decision

Tokio, current-thread, on a dedicated core thread. Blocking and CPU-bound work goes to a bounded blocking pool through one helper. Tasks keep `Send` bounds, so switching flavour later is a one-line change.

## Why

- All session state lives on one thread, so it needs no locks and has no data races to test for.
- Task order is deterministic, which makes loop tests with a fake clock exact.
- The UI is on another thread in any case, so the core thread has nothing to compete with.
- The risk, a blocking call on the core thread, is controlled by a lint that bans blocking standard-library calls in async crates and by a debug-build stall detector.

## Revisit when

Profiling shows the core thread saturated, most likely with many concurrent sessions under the run engine or the RPC server.
