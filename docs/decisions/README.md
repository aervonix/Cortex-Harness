# Decision records

One record per significant choice. Each says what the options were, what was chosen, why, and what would reopen it. Where a measurement settled the question, the numbers are in the record and the method is in [../measurements/](../measurements/README.md).

**Status.** *Accepted* means the choice follows from the brief and the measurements, and work proceeds on it. *Provisional* means it is the owner's call and has not been approved yet; work proceeds on the recommendation, and the matching entry in the open questions of the [build plan](../build-plan.md) says what changes if the answer differs.

| # | Decision | Status |
|---|---|---|
| [0001](0001-tui-language.md) | The TUI is written in Rust, inside the main binary | **Provisional** (Windows Terminal not yet checked) |
| [0002](0002-tui-rendering.md) | Inline rendering with our own line-based renderer; full screen as the alternative | **Provisional** |
| [0003](0003-async-runtime.md) | Tokio, current-thread, on a dedicated core thread | Accepted |
| [0004](0004-http-and-tls.md) | reqwest with rustls and the ring provider | Accepted |
| [0005](0005-extension-mechanism.md) | Extensions are JavaScript on an embedded QuickJS engine; MCP for process tools; Lua as the fallback | **Provisional** |
| [0006](0006-session-storage.md) | Sessions are append-only JSONL files, not a database | Accepted |
| [0007](0007-configuration-format.md) | Configuration in TOML, layered, with provenance | Accepted |
| [0008](0008-rpc-protocol.md) | One typed protocol, carried as JSON-RPC over newline-delimited JSON | Accepted |
| [0009](0009-sandboxing.md) | Autonomy default, permission rules and how far to sandbox | **Provisional** |
| [0010](0010-mcp-in-first-release.md) | MCP client scope in the first release | Accepted in principle; scope **provisional** |
| [0011](0011-cli-parsing.md) | Command-line parsing with lexopt and hand-written help | Accepted |
| [0012](0012-release-profile.md) | Build profiles, and unwinding on panic | Accepted |
| [0013](0013-naming.md) | The command, directory and crate names | **Provisional** |
| [0014](0014-search-in-process.md) | Search and file listing are in-process | Accepted |
| [0015](0015-syntax-highlighting.md) | Syntax highlighting with syntect, loaded lazily, behind a feature | Accepted |
| [0016](0016-context-management.md) | Append-only history, context epochs, summary-first compaction | Accepted |
| [0017](0017-model-catalog.md) | The model catalog is a file in the repo | Accepted |
| [0018](0018-credential-storage.md) | Credentials in an owner-only file on Linux, Credential Manager on Windows | **Provisional** |
| [0019](0019-logging.md) | Logging through the tracing crate with our own small subscriber | Accepted |
| [0020](0020-edit-matching.md) | The edit tool matches exactly, with one tolerant tier | **Provisional** |
| [0021](0021-local-statistics.md) | Local statistics are append-only files with a small aggregate | Accepted (flagged as a vision and budget conflict) |
| [0022](0022-repository-map.md) | The repository map uses approximate, pure-Rust outlines | **Provisional** (flagged as a vision and budget conflict) |
| [0023](0023-run-engine-after-first-release.md) | One orchestration layer for parallel attempts and replay, after the first release | **Provisional** |
| [0024](0024-coordinator.md) | The Coordinator is built on the orchestration layer, with limits enforced by the harness | **Provisional** |

## Format of a record

```
# NNNN: The decision in one line

- Status: Accepted | Provisional (and whose call it is) | Superseded by NNNN
- Date

## Context          What forced a choice.
## Options          What was considered, with measurements where they exist.
## Decision         What was chosen.
## Why              The reasons, in order of weight.
## What is given up (where it matters)
## Revisit when     The evidence that would reopen it.
```

A record is not edited to change its decision. A new record supersedes it and the old one is marked.
