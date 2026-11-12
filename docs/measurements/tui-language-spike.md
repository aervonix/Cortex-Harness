# TUI language spike: Rust against TypeScript

The measurements behind [decision 0001](../decisions/0001-tui-language.md) (which language the TUI is written in) and [decision 0002](../decisions/0002-tui-rendering.md) (how it renders). Taken on 2026-11-12.

The spike code is throwaway and is not in this repo. This page records what was built and how it was measured, in enough detail to build it again.

## What was built

**One stub core, shared by every prototype.** A small Rust program that plays a deterministic script of agent events on a fixed schedule: reasoning, text (with a fenced code block), a shell tool whose output streams at 4,000 lines per second, more text, an edit tool with a 30-line diff, more text, usage. One event per token. It runs either linked into a prototype or as a child process that writes one JSON object per line. Every tenth text token is a unique marker word, and the stub logs when it emitted each one.

**Prototype TUIs, each showing the same thing:** committed transcript, a live region (streaming tail, a running tool with the last 8 lines of its output), a rule, a one-line editor, and a status line with a live token count.

| Name | Language and library | Rendering | Core |
|---|---|---|---|
| `rust-lines` | Rust, our own line-based presenter (about 60 lines) on crossterm | Inline: committed lines written once, live region rewritten in place | In-process |
| `rust-inline` | Rust, ratatui 0.30.2 on crossterm 0.29 | Inline viewport, history inserted with scroll regions | In-process |
| `rust-full` | The same | Alternate screen, cell diff | In-process |
| `rust-inline-rpc` | The same as `rust-inline` | Inline | Child process, JSON lines |
| `ink-node` | TypeScript, Ink 8.0.0 on React 19.3, bundled, on Node 26.13 | Inline, with Ink's `Static` for committed lines and incremental rendering on | Child process, JSON lines |
| `ink-bun` | The same bundle compiled to one executable with Bun 1.4.4 | The same | Child process, JSON lines |

Every prototype was given the same good practice: events are merged and drawn at most 60 times a second, key presses are drawn immediately, frames are wrapped in synchronized-update brackets. The Rust prototypes write each frame with one system call. The TypeScript prototype uses React's production build, Ink's `maxFps` raised so its own throttle does not delay key echo (lower values were tried and measured no better), and state kept outside React with one re-render per frame.

**Why Ink.** It is the most widely used TypeScript terminal UI library and the only maintained one with a streaming-friendly model. The older widget libraries (the blessed family, terminal-kit) are full-screen cell renderers with little maintenance. One newer library with a native core, made by the Opencode team for that product, was not used: the project's rules exclude dependencies that are extracts of that project. Its speed comes from native code, not from TypeScript.

## How it was measured

A benchmark harness (Rust) runs a prototype under a pseudo-terminal of 120 by 40, records every chunk of output with a monotonic timestamp, types keys on a schedule, and analyses the recording by replaying it through a terminal-state parser. It answers the queries a modern terminal answers (cursor position, device attributes, synchronized-update support, colours).

| Metric | How |
|---|---|
| **Start to first frame** | Time from spawning the process to the chunk after which the screen shows both the editor prompt and the status line. 30 runs, median. Warm file cache. |
| **Memory** | Resident set size of the whole process tree (the prototype and, where there is one, the stub core child), sampled at idle and every 200 ms while streaming. |
| **Stream to screen** | For each marker: time from the stub emitting it to the chunk after which it is visible on the parsed screen. This is the delay the UI adds between a token arriving and the user seeing it. |
| **Key echo** | A key is typed every 137 ms while the stream runs (and at idle before it). Time from writing the key to the chunk after which the editor row shows it. |
| **Frame cost** | Measured inside each prototype: the time to build a frame and hand it to the terminal. |
| **Frames and drops** | Frames are delimited by the synchronized-update brackets in the output. A drop is a gap between frames longer than one and a half frame intervals during streaming. |
| **Flicker** | See below. |
| **CPU** | Process-tree CPU time over the streaming window, as a share of one core. |
| **Size** | Bytes of what would ship. |

**Scenarios.** `light`: the live region holds one streaming line, as in the proposed design. `heavy`: the live region holds up to 30 lines of the block being streamed, re-rendered every frame, which is how a UI that does not commit lines early behaves. `long`: 20 turns back to back, 60 seconds at 2,000 tokens per second, to see whether cost grows with the transcript. Rates: 100 tokens per second (a typical large model), 400 (a fast one), 2,000 (faster than any hosted model today). Each run streams for 12 seconds. Three repetitions on the desktop, two on the laptop; medians.

**Flicker, measured.** Flicker is what a user sees when the terminal paints a frame that is only partly written. Two things are measured from the recording:

1. *Is each frame protected?* A frame inside synchronized-update brackets is painted atomically by terminals that support the mode.
2. *What would a terminal without that mode show?* Output arrives at the terminal in chunks. After every chunk inside a frame, the parsed screen is compared with the screen before the frame and after it. A row that is non-blank before and after but blank in between is a visible blank flash. A row that matches neither is torn.

**Machines.**

| | Desktop | Laptop |
|---|---|---|
| CPU | Intel Core i7-14700K, 28 threads | Intel Core i5-1135G7, 8 threads |
| System | Linux 6.18 under WSL 2 | Linux 7.0, native |
| Rust | 1.99.1 | 1.99.1 |

**Builds.** The Rust programs were built with `opt-level = 3`, fat LTO, one codegen unit, `panic = "abort"`, and stripped. The TypeScript prototype was bundled and minified for production.

**Two corrections made during the work.** The first desktop run used wall-clock timestamps. WSL 2 steps the wall clock, which corrupted a few frame-interval figures and broke key matching in three runs. The harness was changed to the monotonic clock and to match each key independently, and everything was run again. All figures below are from the corrected runs.

## Results: desktop

### Start-up, memory, size

| | `rust-lines` | `rust-inline` | `rust-full` | `rust-inline-rpc` | `ink-node` | `ink-bun` |
|---|---|---|---|---|---|---|
| Start to first frame (median) | 0.84 ms | 1.04 ms | 1.02 ms | 1.25 ms | 98.0 ms (91.4 with Node's compile cache) | 50.9 ms |
| Idle memory | 2.0 MB | 2.5 MB | 2.7 MB | 4.0 MB | 84.5 MB | 50.6 MB |
| Peak memory, streaming at 2,000 tokens per second | 4.9 MB | 5.3 MB | 5.3 MB | 7.3 MB | 132 MB | 127 MB |
| Memory after the 60-second session | 3.1 MB | 4.2 MB | 4.9 MB | 6.0 MB | 180 MB | 146 MB |
| CPU when idle | 0.0% | 0.0% | 0.0% | 0.0% | 0.0 to 0.5% | 0.0 to 0.5% |
| What ships | 0.8 MB (the UI and stub in one binary) | the same | the same | 0.8 MB + 0.3 MB core | 0.4 MB bundle + a 150 MB Node runtime + the core | a 110 MB executable + the core |

For scale, an empty program: Rust 0.36 ms and 2.9 MB; Bun 1.9 ms and 11.7 MB; Node 13.2 ms and 41.8 MB.

### Streaming

Medians of three runs. Times in milliseconds.

| Scenario | Prototype | Stream to screen, median / 99th | Key echo, median / 99th | Frame cost, median / 99th | Frames dropped | CPU, % of a core |
|---|---|---|---|---|---|---|
| light, 100 tok/s | `rust-lines` | 8.6 / 16.8 | 0.12 / 0.24 | 0.02 / 0.07 | 0% | 0.6 |
| | `rust-inline` | 8.4 / 16.6 | 0.15 / 0.31 | 0.07 / 0.18 | 0% | 1.0 |
| | `ink-node` | 8.1 / 16.9 | 0.81 / 1.85 | 0.61 / 1.76 | 0% | 7.9 |
| | `ink-bun` | 8.8 / 16.2 | 0.91 / 2.01 | 0.66 / 1.53 * | 0% | 11.0 |
| light, 400 tok/s | `rust-lines` | 8.2 / 16.7 | 0.08 / 0.16 | 0.01 / 0.08 | 0% | 1.4 |
| | `rust-inline` | 8.3 / 16.7 | 0.11 / 0.24 | 0.06 / 0.14 | 0% | 1.7 |
| | `ink-node` | 8.2 / 17.0 | 0.74 / 3.69 | 0.47 / 1.39 | 0.3% | 8.1 |
| | `ink-bun` | 8.2 / 17.2 | 0.82 / 3.39 | 0.61 / 1.48 * | 0.3% | 10.5 |
| light, 2,000 tok/s | `rust-lines` | 8.1 / 16.6 | 0.09 / 0.18 | 0.02 / 0.04 | 0% | 2.4 |
| | `rust-inline` | 8.1 / 16.7 | 0.15 / 0.27 | 0.10 / 0.20 | 0% | 3.3 |
| | `rust-full` | 8.3 / 16.8 | 0.26 / 0.45 | 0.18 / 0.28 | 0% | 3.5 |
| | `rust-inline-rpc` | 8.2 / 16.7 | 0.16 / 0.28 | 0.10 / 0.17 | 0% | 5.2 |
| | `ink-node` | 8.5 / 16.5 | 0.91 / 1.58 | 0.68 / 1.39 | 0% | 15.2 |
| | `ink-bun` | 8.7 / 17.1 | 0.96 / 1.65 | 0.64 / 1.43 * | 0% | 18.0 |
| heavy, 400 tok/s | `rust-lines` | 8.2 / 16.7 | 0.11 / 0.21 | 0.03 / 0.06 | 0% | 1.0 |
| | `rust-inline` | 8.4 / 16.8 | 0.21 / 0.44 | 0.15 / 0.27 | 0% | 1.8 |
| | `ink-node` | 9.3 / 18.1 | 2.04 / 2.99 | 2.04 / 3.27 | 0% | 19.0 |
| | `ink-bun` | 10.5 / 19.5 | 2.40 / 4.55 | 2.26 / 4.82 * | 0% | 26.6 |
| heavy, 2,000 tok/s | `rust-lines` | 8.0 / 16.7 | 0.11 / 0.20 | 0.03 / 0.04 | 0% | 2.7 |
| | `rust-inline` | 8.3 / 16.9 | 0.25 / 0.43 | 0.21 / 0.28 | 0% | 3.7 |
| | `rust-full` | 8.4 / 16.8 | 0.25 / 0.40 | 0.23 / 0.32 | 0% | 3.8 |
| | `ink-node` | 9.7 / 18.0 | 2.27 / 3.96 | 2.09 / 3.17 | 0% | 29.9 |
| | `ink-bun` | 10.9 / 19.9 | 2.81 / 5.12 | 2.48 / 4.64 * | 0% | 40.1 |
| long, 2,000 tok/s | `rust-lines` | 8.4 / 16.6 | 0.10 / 0.15 | 0.02 / 0.05 | 0% | 3.0 |
| | `rust-inline` | 8.4 / 16.7 | 0.16 / 0.27 | 0.10 / 0.19 | 0% | 3.5 |
| | `ink-node` | 8.4 / 16.3 | 0.83 / 1.61 | 0.67 / 1.34 | 0.3% | 14.2 |
| | `ink-bun` | 9.1 / 17.2 | 0.95 / 2.13 | 0.69 / 1.62 * | 0% | 17.1 |

\* The Bun frame-cost figures are from the first desktop run. In the corrected run the Bun prototype's internal timestamps were on a different clock from the harness and could not be matched. The other columns are from the corrected run.

**At idle** (8 keys typed before the stream starts in each run), key echo was 0.10 to 0.26 ms median for the Rust prototypes and 1.6 to 2.4 ms for the TypeScript one, with single keys as slow as 13 ms. The 99th percentile of the interval between frames while streaming was 16.8 to 17.1 ms for the Rust prototypes and 17.2 to 22.9 ms for the TypeScript one, against a 16.7 ms target.

Every prototype keeps the stream-to-screen delay at one frame interval: a median of half a frame and a worst case of one frame, which is the 60-per-second cap itself. No prototype drops frames in any meaningful number.

### Output and flicker

At 2,000 tokens per second. "Frames with a blank flash" and "torn frames" are out of about 680 frames in 12 seconds, as a terminal without synchronized updates would see them.

| Scenario | Prototype | Frames inside synchronized updates | Bytes per frame, median | Chunks per frame, worst | Frames with a blank flash | Torn frames | Clear-screen operations |
|---|---|---|---|---|---|---|---|
| light | `rust-lines` | 100% | 820 | 1 | 0 | 0 | 0 |
| | `rust-inline` | 100% | 617 | 1 | 0 | 0 | 0 |
| | `rust-full` | 100% | 5,844 | 3 | 0 | 542 | 0 |
| | `ink-node` | 100% | 833 | 5 | 285 | 288 | 0 |
| | `ink-bun` | 100% | 832 | 4 | 268 | 270 | 0 |
| heavy | `rust-lines` | 100% | 3,293 | 2 | 0 | 1 | 0 |
| | `rust-inline` | 100% | 6,805 | 4 | 5 | 534 | 0 |
| | `rust-full` | 100% | 7,701 | 3 | 0 | 533 | 0 |
| | `ink-node` | 100% | 3,436 | 8 | 493 | 500 | 0 |
| | `ink-bun` | 100% | 3,440 | 7 | 510 | 518 | 0 |

With synchronized updates honoured, every prototype shows zero blank or torn frames in every scenario.

Reading it:

- **All of them wrap frames in synchronized updates**, so on a terminal that supports the mode none of them flickers.
- **On a terminal that does not**, the difference is whether a frame reaches the terminal in one piece. A pseudo-terminal hands over output in chunks of about 4 KiB. `rust-lines` and `rust-inline` in the light scenario write each frame with one system call and stay under one chunk, so there is nothing partial to paint. Ink writes a frame in several pieces and erases lines before rewriting them, so a blank flash is possible in a third or more of frames.
- **A cell-grid renderer sends more than it needs to when text scrolls.** `rust-full` and `rust-inline` in the heavy scenario rewrite most cells of the region whenever a line scrolls: 6 to 8 KiB a frame, against 3 KiB for a line-based renderer and under 1 KiB when the live region is kept small.

## Results: laptop

Native Linux, a slower CPU, two repetitions. The ordering and the ratios are the same.

| | `rust-lines` | `rust-inline` | `rust-full` | `rust-inline-rpc` | `ink-node` | `ink-bun` |
|---|---|---|---|---|---|---|
| Start to first frame | 1.3 ms | 1.6 ms | 1.6 ms | 1.7 ms | 131 ms (123 with compile cache) | 63.9 ms |
| Idle memory | 2.6 MB | 2.9 MB | 3.1 MB | 4.9 MB | 84.8 MB | 51.1 MB |
| Peak memory, heavy at 2,000 tok/s | 5.1 MB | 5.8 MB | 5.9 MB | 8.0 MB | 171 MB | 167 MB |
| Key echo, light at 400 tok/s, median / 99th | 0.30 / 0.45 ms | 0.40 / 0.61 | 0.55 / 0.83 | 0.43 / 0.65 | 1.25 / 2.52 | 1.37 / 2.73 |
| Key echo, heavy at 2,000 tok/s, median / 99th | 0.27 / 0.43 ms | 0.48 / 0.76 | 0.49 / 0.78 | 0.39 / 0.71 | 3.28 / 4.56 | 5.67 / 13.8 |
| Frame cost, heavy at 2,000 tok/s, median / 99th | 0.01 / 0.05 ms | 0.25 / 0.33 | 0.27 / 0.35 | 0.25 / 0.34 | 2.94 / 4.82 | not matched |
| CPU, light at 400 tok/s | 2.2% | 2.9% | 3.9% | 4.1% | 16.4% | 20.6% |
| CPU, heavy at 2,000 tok/s | 6.3% | 7.2% | 8.1% | 11.2% | 43.9% | 61.4% |
| Frames dropped | 0% | 0% | 0% | 0% | 0% | 0% |

One laptop run overlapped with a short compile on the same machine. The medians above do not show an effect.

## The reference harnesses as installed

For scale, the two reference harnesses were installed from the npm registry into scratch space and started on the laptop: Pi 1.5.2 on Node 26.13, and Opencode 1.19.5 as its self-contained executable. Each ran with an empty home directory, so each showed whatever it shows on a first run. These are complete products and the prototypes above are not, so this is a statement of where the bar is today, not a like-for-like comparison.

| | Pi 1.5.2 | Opencode 1.19.5 | Rust prototype (`rust-lines`), same laptop |
|---|---|---|---|
| `--version` | 175 ms | 371 ms | under 1 ms |
| Interactive: first output | 223 ms | 526 ms | 1.3 ms to a complete first frame |
| Interactive: first screen finished drawing | 284 ms | 540 ms | 1.3 ms |
| Resident memory, idle after 4 seconds | 124 MB | 614 MB | 2.6 MB |
| What is installed | 154 MB of packages, plus a Node runtime | a 214 MB executable | 0.8 MB |

**Method.** `--version` with hyperfine: 20 runs after 3 warm-ups, median. Interactive: each tool was started under a 120 by 40 pseudo-terminal by a small script that answered cursor-position and device-attribute queries, timed the first byte of output and the end of the first burst of drawing (the last output before 300 ms of silence), and summed the resident memory of the process tree after 4 seconds. 5 runs after one warm-up, median. Warm file cache.

**Caveats.** One machine. First-run screens, not a session in a project. Resident memory counts the pages of the executable that have been touched, which is part of why the larger executable reads higher. Neither tool was configured or tuned.

## What the numbers say

1. **Smoothness is not where the languages differ.** With events merged per frame and a small live region, the TypeScript prototype keeps 60 frames a second at any token rate tested, with no dropped frames. Frame time is a few milliseconds at worst.
2. **Start-up, memory and CPU are where they differ, by one to two orders of magnitude.** 1 ms against 51 to 98 ms to the first frame. 2 to 5 MB against 50 to 180 MB. 1 to 4% of a core against 8 to 40% while streaming. The TypeScript memory also grows with the session, by about 95 MB over a minute of fast streaming.
3. **Key echo is 5 to 20 times faster in Rust**, and stays under half a millisecond while streaming. The TypeScript prototype is still within what people perceive as instant, except at the 99th percentile under heavy load on the slower machine (14 ms).
4. **Shipping a TypeScript front end means shipping a runtime**: 110 to 150 MB against under 1 MB of UI code.
5. **Talking to the core over JSON lines costs almost nothing.** `rust-inline-rpc` against `rust-inline`: the same latency, 1.5 MB more memory (the second process), 2 points more CPU at the highest rate. A separate front end is viable on performance grounds; its costs are elsewhere.
6. **A line-based renderer of our own beats the cell-grid framework** on frame cost (about 5 times), on bytes in the heavy case, and on flicker safety, in about 60 lines.

## What was not measured

- **Native Windows.** The harness runs on Linux. It compiles for Windows (checked with `cargo check` for the Windows target), so the same measurements can be taken there under ConPTY. How a real Windows Terminal window looks is the manual checklist in [windows-terminal-checklist.md](windows-terminal-checklist.md).
- **A real terminal emulator's paint.** The harness models the terminal; it does not photograph one. Flicker figures say what a terminal could paint, not what a given terminal did.
- **Cold file cache.** Start-up was measured warm. A cold start favours the small binary further.
- **A realistic application.** The prototypes are a few hundred lines. The real UI will cost more in both languages; the gaps come from the runtimes and should carry over.
