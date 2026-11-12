# 0019: Logging through the tracing crate with our own small subscriber

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

The harness needs structured logs for diagnosis and span timings for the performance budgets (prompt to request sent, to first byte, to first frame). Logging must cost nothing noticeable at start-up or per event, must never write to standard output, and must never record a secret.

## Options considered

| Option | For | Against |
|---|---|---|
| **The `log` crate** with a simple writer | Smallest. | No spans, so timings and request correlation are built by hand. |
| **`tracing` with `tracing-subscriber`** | Spans and structured fields. The standard choice. | The stock subscriber brings a formatting stack, a regex-based filter and more crates than we use. |
| **`tracing` with a subscriber of our own** | Spans and fields, the ecosystem's instrumentation in dependencies, and only the code we need: a level and prefix filter, a JSON-lines writer, span timing. | A few hundred lines to write and test. |
| **Our own macros** | No dependency at all. | Dependencies that emit through `tracing` or `log` would be invisible. |

## Measurement

Desktop pinned to 4 cores ([measurements/dependency-costs.md](../measurements/dependency-costs.md)):

| | Dev build | Release build | Size added | Crates |
|---|---|---|---|---|
| `tracing`, `tracing-subscriber` (formatter and registry only, no regex filter) and `tracing-appender` | 2.7 s | 5.0 s | 202 KB | 27 |

`tracing` alone is a small fraction of that. Most of the 27 crates come from the subscriber and the appender.

## Decision

- `tracing` as the facade in every crate.
- A small subscriber of our own in `cortex-base`: a level filter with target-prefix overrides, JSON lines to a rotated file through the log-writer thread, a human-readable stderr sink for headless and RPC modes, and span enter and exit times kept for the performance probes.
- `trace` level compiled out of release builds.
- Messages from dependencies that use the `log` crate are bridged at `warn` and above.

Specified in [architecture/02-runtime.md](../architecture/02-runtime.md).

## Why

- Span timings are needed for the budgets, and spans are what `tracing` is for.
- Our own subscriber avoids most of the dependency cost and lets the redaction rules live in one place we control.

## Revisit when

The subscriber grows features the stock one already has. Then take the stock subscriber with a trimmed feature set and re-measure.
