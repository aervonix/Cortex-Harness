# 0006: Sessions are stored as append-only JSONL files, not in a database

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

A session is a tree of entries (messages, tool calls and results, epochs, snapshots, outcomes) that must survive a crash at any byte, resume quickly, branch, and be readable by other tools. The store is on the start-up path of `--resume` and on the write path of every turn.

Of the reference harnesses, one keeps a JSONL file per session and the other keeps a relational database. Both work. The database gives listing and search across sessions; the files give simplicity and inspectability.

## Options considered

| Option | For | Against |
|---|---|---|
| **One JSONL file per session**, append-only, with a small index cache for listings | A crash can only damage the last line, which is detected and dropped. Appends are one write. Any tool can read it. No schema migrations: unknown kinds and fields are skipped. No dependency. | Listing and searching sessions need an index we maintain. No transactions across files. A very long session is a large file. |
| **Bundled SQLite** | Transactions. Queries across sessions. One file. | A C library compiled into every build. Schema migrations for ever. A binary format users cannot read or repair by hand. Locking behaviour differs on network file systems. |
| **A pure-Rust embedded database** | No C. | Younger, with formats and durability we would be trusting on less evidence, and the same migration burden. |

## Measurement

Bundled SQLite through rusqlite, on the desktop pinned to 4 cores ([measurements/dependency-costs.md](../measurements/dependency-costs.md)):

| | Dev build | Release build | Size added | Crates |
|---|---|---|---|---|
| rusqlite, bundled | 5.0 s | 28.6 s | 2.0 MB | 15 |

28.6 s is close to the release build time of the whole core dependency set (37 s), and it is C code that must also build on the Windows runner inside the 5-minute limit.

## Decision

- One JSONL file per session under the data directory, grouped by project, plus an artifacts directory and a snapshot store beside it.
- Every entry has an id, a parent id and a kind. The active branch is a pointer to a leaf.
- A per-project index file caches what listings need (title, times, counts). It is rebuilt from the session files if missing or stale, and is never the source of truth.
- Reads stream line by line. A session is never parsed whole into one value.
- Durability is a setting: flush at the end of each turn by default, per entry on request.

Specified in [architecture/07-sessions-and-context.md](../architecture/07-sessions-and-context.md).

## Why

- The access pattern is append and replay, which a log fits exactly.
- Crash safety is simple to reason about and to test: cut the file at every byte of the last entry and it must open.
- It adds nothing to build time, binary size or the Windows build.
- Sessions are the user's data. A text format they can read, search with ordinary tools and repair is worth more to this audience than query power.

## What is given up

- Full-text search across all sessions is a scan, or an index built later.
- Cross-session queries for evaluation reports are done by reading files. If that becomes slow, a database can be added for evaluation data alone, behind a feature, without touching the session format ([0021](0021-local-statistics.md)).

## Revisit when

- Session listing or search misses its budget with the index cache.
- The evaluation engine needs queries that are impractical over files.
