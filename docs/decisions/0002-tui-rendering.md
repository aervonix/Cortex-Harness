# 0002: Inline rendering with our own line-based renderer; full screen as the alternative

- **Status:** Accepted by the owner on 2026-11-12: inline by default, with stacked panels. Still provisional until the owner runs the Windows Terminal checklist, before step 20.
- **Date:** 2026-11-12

## Context

A terminal UI either writes into the terminal's normal screen and scrollback ("inline") or takes over the whole screen ("full screen"). The choice shapes flicker, memory, what the terminal's own features can do, and how much the UI must reimplement. The vision asks for a clean chat-style flow, with panels that open only when there is something to show, and for smooth rendering even with huge outputs.

Of the reference harnesses, one renders full screen. The other began inline, added a full-screen renderer, made it the default recently, and maintains both. Other Rust harnesses that started on a cell-grid framework's inline mode replaced that part with their own code, and report trouble with multiplexers that mishandle scroll regions (research notes).

## Options considered

| Option | For | Against |
|---|---|---|
| **Full screen, cell-grid framework** | Full control of every cell. Re-wraps on resize. Side-by-side panes are natural. | The terminal's scrollback, search, selection and copy no longer apply, so the UI reimplements them. The whole transcript is kept in memory. When text scrolls, most cells change. |
| **Inline, the framework's inline viewport** | Scrollback stays the terminal's. | A fixed-height viewport. It asks the terminal for the cursor position, which races with input reading. History is inserted with scroll regions, which some multiplexers mishandle. |
| **Inline, our own line-based renderer, with a full-screen presenter over the same lines** | Scrollback stays the terminal's. Frames are small and written in one piece. No cursor queries, no scroll regions. One set of components serves both presenters. | We write and own the renderer and the second presenter. Committed text is not re-wrapped on resize. |

## Measurements

From [measurements/tui-language-spike.md](../measurements/tui-language-spike.md), desktop, at 2,000 tokens per second.

| | Own line renderer, inline | Framework, inline | Framework, full screen |
|---|---|---|---|
| Frame cost, light (median / 99th) | 0.02 / 0.04 ms | 0.10 / 0.20 ms | 0.18 / 0.28 ms |
| Frame cost, 30-line live region | 0.03 / 0.04 ms | 0.21 / 0.28 ms | 0.23 / 0.32 ms |
| Bytes per frame, light (median) | 820 | 617 | 5,844 |
| Bytes per frame, 30-line live region | 3,293 | 6,805 | 7,701 |
| Frames a terminal without synchronized updates could tear (of about 680), light / heavy | 0 / 1 | 0 / 534 | 542 / 533 |
| Memory after a 60-second fast session | 3.1 MB | 4.2 MB | 4.9 MB, and growing with the transcript |
| Code | about 60 lines for the presenter in the prototype | a dependency | a dependency |

The line renderer in the prototype is deliberately simple: it rewrites the live region whenever lines are committed above it. With the design's small live region, that is under a kilobyte a frame.

The framework's build and size cost is in [measurements/dependency-costs.md](../measurements/dependency-costs.md): with crossterm, 4.3 s of clean dev build, 6.7 s of release build, 240 KB and 64 crates. Its cost is not what decided this.

## Decision

- **Inline by default.** The transcript goes into the terminal's own scrollback. The harness owns a small live region at the bottom.
- **Commit as soon as stable.** Content leaves the live region the moment it can no longer change, so the live region stays a few lines tall however long the answer is.
- **Panels live in the live region**, bounded, opening and closing as needed.
- **Our own line-based renderer** in `cortex-term`, on crossterm for input and terminal modes. Components produce styled lines. A frame is one write inside a synchronized update: committed lines, then the changed lines of the live region. No clear-and-redraw, no scroll regions, no cursor queries.
- **A full-screen presenter** over the same lines, selectable by flag or setting, for terminals where inline misbehaves and for people who prefer it. It is where side-by-side panes can exist.
- **A viewer** (a temporary full-screen view) for long output, diffs and summaries, which is how inline mode offers "expand".
- No cell-grid framework dependency.

Specified in [architecture/09-tui.md](../architecture/09-tui.md).

## Why

- It is the cheapest per frame and the least likely to flicker, including on terminals without synchronized updates, because a frame fits in one write.
- Memory stays flat over a long session, because nothing committed is kept.
- The terminal's own scrollback, search and copy keep working, over SSH and inside multiplexers, with nothing reimplemented.
- It matches the vision: a chat-style flow, with panels that come and go.
- One renderer with two presenters costs less to maintain than the two renderers one reference harness now carries.

## What is given up

- Earlier output is not re-wrapped when the window is resized.
- Output already in the scrollback cannot be collapsed or expanded in place. The viewer covers that need.
- Side-by-side panes exist only in the full-screen presenter.

## Revisit when

- The Windows checklist or real use shows inline drawing unreliable in a terminal that matters: make full screen the default there.
- The owner prefers side-by-side panes by default: that means full screen by default.
