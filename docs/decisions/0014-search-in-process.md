# 0014: Search and file listing are in-process

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

The `search` and `list` tools are among the most used. They must be fast on large trees, respect ignore files, and behave the same on Linux and Windows.

## Options considered

| Option | Assessment |
|---|---|
| Call an external search program | No build cost. The program must be installed and on the path, which is not a given on Windows. Output parsing, version differences and process start-up per call. Both reference harnesses do this; one installs the programs in CI. |
| Embed the ripgrep libraries (`ignore`, `grep-searcher`, `grep-regex`, `globset`) | The same engine as the best external tool, no external dependency, cancellable, results as data. Costs build time and size. |
| A simple walker and the `regex` crate | Smaller. Loses the ignore-file semantics, binary detection and the tuned searcher. |

## Measurement

The libraries add 2.1 MB to a stripped release binary and 8.3 s to a clean release build on 4 cores (2.9 s in a dev build), 27 crates ([measurements/dependency-costs.md](../measurements/dependency-costs.md)). Most of it is the regex engine, which other parts of the harness use too.

## Decision

Embed the libraries, in `cortex-tools` only. The repository index reuses the walker.

## Why

- "It works on a clean Windows machine" is worth 2 MB.
- No process per search, which matters for a tool the model calls dozens of times a session and that the loop starts early.
- Results arrive as structured data, so truncation, grouping and deterministic ordering are exact.

## Revisit when

The size budget is under pressure and measurements show a simpler searcher would be fast enough.
