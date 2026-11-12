# 0021: Local statistics are append-only files with a small aggregate

- **Status:** Accepted. Flagged as a conflict between the vision and the speed budgets, with this as the recommended resolution.
- **Date:** 2026-11-12

## Context

The vision's adaptive features need numbers measured on the user's machine: success rates per model and edit format, command durations, cache hit rates, turn outcomes, and later the results of replays. The vision also requires that none of it leaves the machine.

Storing and querying statistics pulls against three budgets: start-up (nothing may be loaded that is not needed), binary size and build time (a database is a large C dependency), and idle cost (no background work).

## Options considered

| Option | Assessment |
|---|---|
| Bundled SQLite | Real queries and indexes. A C library compiled into every build on both OSes, and a database file to open, migrate and keep consistent. Its build and size cost is in [measurements/dependency-costs.md](../measurements/dependency-costs.md). |
| Append-only JSON lines, with a small aggregates file rebuilt from them | No dependency. Appends are cheap and crash-safe. Queries are whatever the aggregates hold; anything else means a scan. |
| In-memory only | No persistence, so nothing to adapt from tomorrow. |

## Decision

- Events are appended as JSON lines, one file per month, by the log-writer thread.
- A small aggregates file (counts and sums keyed by model, variant, tool and project) is rebuilt incrementally and is the only thing adaptive decisions read.
- Nothing is read at start-up. The aggregates are loaded on first use.
- Old months are folded into the aggregates and deleted after a retention period.
- `cortex stats` shows, resets and disables.

## Why

- The questions the first release asks are simple counts and rates, which aggregates answer in microseconds.
- The same choice was made for sessions ([0006](0006-session-storage.md)), for the same reasons, and one storage style is easier to maintain than two.
- It keeps the C toolchain cost for the one dependency that earns it (the script engine).

## What is given up

Ad-hoc queries over history. The run engine's evaluation reports will want richer slicing than fixed aggregates.

## Revisit when

The run engine's reports need queries that aggregates cannot serve. A database can then be added behind `cortex-stats` for that data alone, as an optional feature, without touching the hot path.
