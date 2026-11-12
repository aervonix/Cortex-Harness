# Performance budgets

Numbers the product and the tooling must meet, how each is measured, and where each is checked automatically. They exist because "fast" is the first goal of the project and the second pillar of the [vision](vision.md), and a goal without a number erodes one reasonable-looking change at a time.

## How to read the budgets

- **Reference machines.** "Desktop" is the owner's desktop class of machine (a current 8-performance-core x86-64 CPU, Linux). "Runner" is a GitHub-hosted 4-core runner. Budgets are given for both where the difference matters, because the desktop number is what users feel and the runner number is what CI can check.
- **Measured basis.** Each budget lists what the throwaway prototypes measured, so it is clear how much room the real product has. The prototypes do far less than the product will. The budget is the limit, not the target.
- **Checks.** "PR" means a blocking pull-request check. "Main" means the non-blocking performance tier that runs after every merge and nightly, compares with stored baselines, and opens an issue on a miss. Hosted runners are noisy, so runner checks use generous limits and medians of several runs; they catch regressions of tens of percent, not single percents. `cargo xtask perf` runs the same measurements locally for exact work.
- **A miss is a bug.** A change that breaks a budget is fixed or reverted, or the budget is changed in this file with a reason, in the same pull request. Budgets are not loosened quietly.

## Start-up

| Budget | Desktop | Linux runner | Windows runner | Measured basis |
|---|---|---|---|---|
| `cortex --version`, start to exit | ≤ 5 ms | ≤ 15 ms | ≤ 50 ms | An empty Rust binary and a binary with the planned dependencies start in the same time, within noise ([measurements/dependency-costs.md](measurements/dependency-costs.md)). |
| TUI: process start to first complete frame | ≤ 20 ms | ≤ 50 ms | ≤ 120 ms | Prototype: 0.8 to 1.0 ms (desktop), 1.3 to 1.6 ms (laptop). The TypeScript prototypes: 51 and 98 ms (desktop), 64 and 131 ms (laptop). |
| Headless: process start to request handed to the network stack (warm connection excluded) | ≤ 15 ms | ≤ 40 ms | ≤ 100 ms | Building the HTTP client: 1.2 ms warm, 45 ms on a cold file cache. |

**How.** A pseudo-terminal harness records the time from spawning the process to the first frame that shows the editor and status line. Thirty runs, median and 95th percentile. The alias `cx` is the same file under a second name ([decisions/0013-naming.md](decisions/0013-naming.md)), so it has the same budget; the measurement is taken under both names.

**Rules that protect it.**

- Nothing on the start-up path reads the network, the session index, the statistics, the repository index or the model catalog's optional overlays.
- The HTTP client, the script engine, MCP servers and the repository index start in the background after the first frame.
- Terminal capability queries never block the first frame.
- A test fails if a start-up trace shows a file opened outside an allow-list before the first frame.

**Checked.** Main (both OSes). A coarse PR check: the system tier fails if first frame exceeds five times the runner budget.

## Memory

| Budget | Limit | Measured basis |
|---|---|---|
| Idle, TUI open with a new session | ≤ 25 MB resident | Prototype TUI: 2.0 to 2.7 MB. A process holding the async runtime and an HTTP client: 6.4 MB. TypeScript prototypes: 51 and 85 MB. |
| After a 200-turn session, inline presenter | ≤ 40 MB resident | Prototype over a 60-second session at 2,000 tokens per second: +1 to +2 MB (it keeps nothing it has committed). TypeScript prototypes: about +95 MB at peak. |
| After a 200-turn session, full-screen presenter | ≤ 80 MB resident | Prototype: +2.2 MB over the same 60-second session. |
| A command that prints 1 GB | ≤ 8 MB above idle | By design: bounded buffers, output spilled to a file. |
| Repository index | 0 held in memory between uses | By design: built from the disk cache on demand. |
| Each loaded extension script, in ordinary use | ≤ 4 MB | Script engines measured at about 1 MB per runtime ([measurements/extension-mechanisms.md](measurements/extension-mechanisms.md)). Separately, the enforced limit on one runtime is 32 MB by default and 64 MB at most ([architecture/12-extensions.md](architecture/12-extensions.md)). |
| Each additional session in the same process (a run, or an Employee), idle | ≤ 10 MB | By design: a session holds its current context and little else. A team of five Employees adds about 50 MB. To be measured when the orchestration layer is built. |

**How.** Resident set size of the process, sampled at idle and as a peak during a scripted session. Child processes are measured separately and are not part of these budgets.

**Checked.** Main. The long-session test is in the nightly tier.

## Latency the harness adds

The model's own latency is not ours. What the harness adds to it is.

| Budget | Limit | Measured basis |
|---|---|---|
| Prompt submitted → request handed to the network stack, with a 200,000-token history | ≤ 10 ms (95th percentile) | To be measured in step 15. Serialising a request of that size is a few milliseconds. |
| First response byte received → first frame that shows it | ≤ 20 ms (95th percentile) | Prototype: median 8.1 to 8.6 ms, 99th percentile 16.6 to 16.9 ms. It is bounded by the 60 frames-per-second cap. |
| Connection set-up on the first request of a session | 0, when the session has been open for 300 ms | By design: the connection is opened ahead in the background. |
| Tool call arguments complete → read-only tool started | ≤ 1 ms | By design: planned as the block ends. |
| Key press → frame that shows it, idle | ≤ 2 ms (99th percentile) | Prototype: 0.1 to 0.3 ms. TypeScript prototypes: 1.6 to 2.4 ms median, with single keys up to 13 ms. |
| Key press → frame that shows it, while streaming at 2,000 tokens per second | ≤ 4 ms (99th percentile) | Prototype: 0.2 to 0.5 ms. TypeScript prototypes: 1.6 to 5.1 ms. |
| Starting a shell command, harness overhead including the sandbox launcher | ≤ 5 ms on Linux, ≤ 30 ms on Windows | To be measured in steps 11 and 26. |
| Hot-reload check | ≤ 1 ms | By design: a few `stat` calls. |

**How.** Span timings from the tracing layer for the first two. The pseudo-terminal harness for key latency: it writes a key and records when the screen shows it.

**Checked.** Main. Key latency also has a coarse PR check in the system tier.

## Rendering under fast streaming

At 2,000 tokens per second, which is faster than any current hosted model.

| Budget | Limit | Measured basis |
|---|---|---|
| Frame cost (build the frame and write it), normal live region | ≤ 1 ms (99th percentile) | Prototypes: 0.04 to 0.08 ms with our own line renderer, 0.14 to 0.28 ms on a cell-grid framework. TypeScript: 1.4 to 1.8 ms. |
| Frame cost with a 30-line live region | ≤ 2 ms (99th percentile) | Prototypes: 0.04 to 0.06 ms with our own line renderer, 0.27 to 0.32 ms on a cell-grid framework. TypeScript: 3.2 to 4.8 ms. |
| Frames dropped at 60 per second | 0 (interval 99th percentile ≤ 18 ms) | Prototype: 16.8 to 17.1 ms. TypeScript: 17.2 to 22.9 ms. |
| Bytes written per frame, normal live region | median ≤ 1 KiB, maximum ≤ 4 KiB | Prototype with our own line renderer: 228 bytes median at 400 tokens per second, 820 at 2,000. A frame under 4 KiB reaches the terminal in one read, so it cannot tear even without synchronized updates. |
| Flicker | Every frame is one write inside a synchronized update. No frame clears and redraws. | Asserted as properties, not measured as a number. |
| CPU while streaming at 400 tokens per second | ≤ 5% of one core | Prototype: 1.4 to 2.1%. TypeScript: 8 to 11%. |
| CPU when idle | 0: no timers, no wake-ups | Prototype: 0.00% over 2 s. |

**How.** The pseudo-terminal harness replays the recorded output through a terminal-state parser, frame by frame. The frame cost comes from the renderer's own timing.

**Checked.** The flicker properties: PR (tier 2, on every frame of every presenter test). The timings: Main.

## Binary size

| Budget | Limit | Measured basis |
|---|---|---|
| Release binary, Linux x86-64, `dist` profile, stripped | ≤ 16 MB | A binary with the core dependency set: 7.0 MB. With the MCP SDK, keychain and highlighter: 12.0 MB. The script engine adds 1.1 MB, unwinding on panic 0.2 to 0.3 MB, and our own code comes on top. Measured with thin LTO; fat LTO is about 7% smaller. The TypeScript alternative ships a 110 to 150 MB runtime. |
| Release binary, Windows x86-64 | ≤ 18 MB | Not measured. Windows binaries of the same code are usually somewhat larger. |
| Crates in a release build | Recorded, not capped. A rise of more than five in one change needs a reason. | Core set: 191. With the optional parts: about 310. |

The size budget is where the vision's features meet the footprint goal most directly: the script engine, the MCP SDK and the highlighter are each a megabyte or more. Their measured costs are in the decision records, and the ones that are optional have cargo features so their cost can always be seen.

**How.** `ls -l` on the stripped `dist` binary.

**Checked.** Main (a release build is too slow for the blocking checks).

## Build and test times

| Budget | Limit | Measured basis |
|---|---|---|
| Blocking pull-request checks, wall time per OS | < 5 min | See the phase table in [testing-and-ci.md](testing-and-ci.md). |
| Clean dev build of the workspace, Linux runner | ≤ 2 min | Third-party dependencies alone: 15 to 27 s on 4 desktop cores, plus about 3 s for the script engine; roughly double on a runner. |
| Clean dev build of the workspace, Windows runner | ≤ 3.5 min | Not measured. Step 2 measures it. |
| Clean release build (`release` profile), 4 desktop cores | ≤ 3 min | Core dependency set: 37 s. With the optional parts: 76 s. The script engine adds about 9 s. |
| `cargo check` after an edit in a leaf crate, desktop | ≤ 2 s | |
| Rebuild and relink the binary after an edit, desktop | ≤ 5 s | Relinking the full dependency set with a trivial change: 0.4 s. |
| No-op build | ≤ 0.5 s | 0.13 s. |
| Tiers 1 and 2 test time | ≤ 60 s on a runner, ≤ 30 s on a desktop | |
| Tier 3 test time | ≤ 60 s Linux runner, ≤ 90 s Windows runner | |

**How.** CI job timings, and `cargo build --timings` for the breakdown.

**Checked.** PR: the job fails at 5 minutes by its timeout, which makes the limit real. Main records the phase timings so a trend is visible before the limit is hit.

## Data paths

| Budget | Limit | Measured basis |
|---|---|---|
| Open a session with 10,000 entries | ≤ 50 ms | To be measured in step 8. |
| List 1,000 sessions | ≤ 10 ms | To be measured in step 8. Reads the index only. |
| Append one entry, including the flush | ≤ 2 ms on an SSD | To be measured in step 8. |
| Search a 100,000-file tree for a literal | ≤ 1 s on a desktop | To be measured in step 6. |
| Warm repository map (cached outlines, few changes) | ≤ 150 ms | To be measured in the index step. |
| Cold repository index, 20,000 files | ≤ 2 s of background time per pass | To be measured in the index step. |
| Open the usage view, any range, with a year of history | ≤ 16 ms (one frame) from the aggregates | To be measured in step 24. It reads kilobytes and scans no raw history. |
| `cortex usage --range month` | The headless start-up budget plus ≤ 5 ms | To be measured in step 16. |
| Indexing other tools' usage records | Background only, one low-priority thread, ≤ 50 MB extra memory, paused while a turn streams. Never at start-up. | To be measured in step 45. |
| Take a per-turn snapshot | Proportional to the files the turn changed; nothing otherwise | By design. |
| Record a statistics event | ≤ 10 µs on the calling thread | By design: a queue push. |
| One tool call through `tool_call` to an extension script | ≤ 0.1 ms of overhead | Script engines measured at 4 to 45 µs per call. |

**Checked.** Main.

## What is deliberately not budgeted

- **Model latency and tokens per second.** They belong to the provider. The harness shows them.
- **Verification time.** It is the project's own build and test time. It has limits the user sets ([architecture/15-efficiency.md](architecture/15-efficiency.md)), not a budget here. Speculative verification spends CPU early to save waiting: one run at a time, off until local statistics show it pays.
- **Parallel attempts and Coordinator teams.** They multiply the project's build cost by design, and run only on request with an estimate first. What the harness controls is how many commands run at once (by default no more than half the machine's cores) and the spend ([architecture/16-run-engine.md](architecture/16-run-engine.md), [architecture/17-coordinator.md](architecture/17-coordinator.md)).

## Summary of automatic checks

| Check | Tier | Blocks a pull request? |
|---|---|---|
| Blocking checks under 5 minutes | Job timeout | Yes |
| Flicker properties on every presenter frame | 2 | Yes |
| First frame and key latency, coarse limits | 3 | Yes |
| No unexpected file access before the first frame | 2 | Yes |
| Start-up, memory, latency, rendering, data-path timings against baselines | 4 | No. Opens an issue. |
| Binary size and crate count | 4 | No. Opens an issue. |
| Long-session memory growth | 6 | No. Opens an issue. |
