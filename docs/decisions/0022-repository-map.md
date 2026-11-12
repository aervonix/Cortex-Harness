# 0022: The repository map uses approximate, pure-Rust outlines

- **Status:** Provisional. Flagged as a conflict between the vision and the speed budgets, with this as the recommended resolution.
- **Date:** 2026-11-12

## Context

The vision asks for lean context: a structural map of the repository instead of whole files. That needs an outline of each source file (its definitions and where they are) for many languages, kept up to date, without hurting start-up, memory, binary size or build time.

## Options considered

| Option | Accuracy | Cost | Assessment |
|---|---|---|---|
| Parser grammars (one per language) | High | Each grammar is a generated C parser: build time on both OSes and roughly a megabyte or more of binary each. Fifteen languages would about double the binary and add minutes to a clean build. | The accurate answer, at a price the budgets cannot absorb in the first release. |
| Language servers | Highest | A separate program per language to find, start and wait for. Dozens of integrations to maintain. | Too slow to start and too much surface for a map. |
| Line-oriented extractors written in Rust | Approximate | A few hundred lines per language, no dependencies, megabytes per second. | Good enough to navigate by. Misses unusual formatting. |
| File tree only | None | Free. | Tells the model where files are, not what is in them. |

## Decision

- Approximate per-language extractors in `cortex-index`, for about fifteen languages, with an indentation-based fallback.
- Outlines cached on disk per project, keyed by path, size and modification time.
- Indexing in the background after the first frame, under caps on files, bytes and time. Never on the start-up path. Not held in memory.
- The map goes in the first user message of a context epoch inside a token budget, or follows as a notice if it is not ready.
- An `outline` tool for looking closer.

## Why

- A map that is right about most definitions saves most of the tokens. The model verifies by reading the part it needs.
- It adds nothing to the binary beyond its own code and nothing to start-up.
- The extractor interface (text in, outline out) is the same one a parser-based extractor would implement, so accuracy can be bought later, per language, behind a cargo feature.

## What is given up

Precision. Outlines can miss or mislabel definitions in code with unusual layout, and nested structure is shallow.

## Revisit when

- Measurements from real sessions show the map's errors cost more round trips than it saves.
- The size budget has room: add parser-based extractors for the few languages that matter most to users, as an optional feature.
