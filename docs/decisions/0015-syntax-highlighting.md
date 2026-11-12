# 0015: Syntax highlighting with syntect, loaded lazily, behind a feature

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

Code blocks and diffs read better highlighted. Highlighting is the most expensive optional part of the UI: grammars are large, and loading them takes time. It must never delay start-up or a frame.

## Options considered

| Option | For | Against |
|---|---|---|
| **No highlighting** | Free. | A visibly poorer UI for a tool whose output is mostly code. |
| **syntect** with its pure-Rust regex engine and a trimmed, precompiled grammar set | Broad language coverage from one dependency. Pure Rust. Line-oriented, which suits streaming. | Size, and a load cost for the grammar set. Regex grammars are approximate and can be slow on pathological lines. |
| **Parser-based highlighting** (a grammar per language) | Accurate. Could share parsers with the repository outline. | Each language is a C grammar: build time and roughly a megabyte each. Rejected for the outline for the same reason ([0022](0022-repository-map.md)). |
| **A small hand-written highlighter** for strings, comments, numbers and keywords | Tiny, fast, no dependency. Good enough for diffs. | A table per language to maintain, and worse results on anything unusual. |

## Measurement

Desktop pinned to 4 cores, syntect with default features off, the pure-Rust regex engine and the default packed grammar set ([measurements/dependency-costs.md](../measurements/dependency-costs.md)):

| | Dev build | Release build | Size added | Crates |
|---|---|---|---|---|
| syntect | 5.2 s | 11.4 s | 2.3 MB | 45 |

**Load and first use** (three runs). Loading the packed default grammar and theme sets takes 0.6 to 0.7 ms, because the sets are unpacked lazily. The cost moves to first use: highlighting 200 short Rust lines from cold took 27 ms in total, which includes compiling the grammar's patterns the first time they are needed. The two were not separated. 27 ms is more than one frame, which is why highlighting never runs on the UI thread.

## Decision

- syntect, default features off, pure-Rust regex engine, behind the `highlight` feature of `cortex-tui` (on by default).
- A trimmed grammar and theme set packed at build time by `xtask`, covering the languages the outline extractors cover plus common configuration and markup formats.
- Loaded on first use, on the blocking pool, never on the UI thread. Until it is ready, code is shown plain. Highlighting applies to lines committed after that; lines already in the scrollback are not rewritten.
- A per-line time limit. A line that exceeds it is shown plain.
- Colours come from the theme's small set of syntax roles, not from an imported editor theme, so highlighting follows the terminal's light or dark background and the 16-colour fallback.

## Why

- It is the only option that gives broad coverage without C code.
- Lazy loading and the feature flag keep its cost off start-up and make it removable where size matters more.
- A hand-written highlighter was close, and remains the fallback if the size budget tightens.

## Revisit when

- The binary size budget is missed: drop to the hand-written highlighter or trim the grammar set further.
- The repository outline moves to real parsers, at which point sharing them becomes attractive.
