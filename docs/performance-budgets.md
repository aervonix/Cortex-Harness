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
| Headless: process start to request handed to the network stack (warm connection excluded) | ≤ 15 ms | ≤ 40 ms | ≤ 100 ms | Building the HTTP client: 1.2 ms warm, 45 ms on a cold file cache. Measured again in step 5 through the real transport (`HttpTransport` in `cortex-llm`), release build, ten runs on the desktop: 1.06 to 1.13 ms warm, and 44.6 ms on the first run after the build. The client is created on first use; making the transport takes under a microsecond. Reading the built-in catalog: 18 to 24 µs, on first use. |

| Loading configuration: the managed, user, project and local files, a profile, the environment and the command line, checked and merged | ≤ 1 ms | ≤ 3 ms | ≤ 10 ms | Step 9, desktop, release build, files in the page cache, median of 2,000 loads: 127 µs for four files with a profile, three environment and command-line settings, 41 effective settings and four rules (slowest 0.5 ms); 4.7 µs when none of the four files exists. It is part of each start-up budget above, not an addition to them: under 1% of the headless budget. Runners are not measured yet. |

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
| Starting a shell command, harness overhead including the sandbox launcher | ≤ 5 ms on Linux, ≤ 30 ms on Windows | Measured in step 11 (`perf_starting_a_command` in `crates/cortex-tools/tests/it/perf.rs`). **Linux,** on the desktop, release build, 200 runs of a command that does nothing, medians: bash alone, started with the standard library, 0.84 ms. Through the runner (a process tree, one pipe, two threads, the count of what was left running), 1.02 ms: 0.18 ms of overhead, the same with the guard told of the tree. Through the whole tool (plan, run, result), 0.24 ms of overhead. The count of what a command left running reads the process table, and on a machine with a few hundred processes reading all of it was most of the overhead (about 4 ms on a hosted runner, in an unoptimised build); it now reads only the processes with a higher id than the command's, for a command under a second old. **Windows,** on a hosted runner, in the unoptimised `ci` build, 30 runs: `cmd.exe` alone 6.7 ms, through the runner 16.1 ms: 9.5 ms of overhead, most of it the suspended start and the pass over the thread list that lets the process run. A release build on a desk is not measured. The launcher is step 26's to add. **The look for changed files is not counted here:** this budget is for starting the command, and the look has the row below. Asking a shell for its version, once a session: 0.5 ms for bash, 149 ms for PowerShell 7 on the runner. |
| The look for what a shell command changed, one before the command and one after it | Not part of the budget for starting a command (the owner, 2026-11-13). Its own cap: ≤ 500 ms a look, after which the look is given up and the result says it did not finish | Measured in step 11, by the same test. In a workspace of 500 files the two looks together add 0.8 ms on Linux without git (a walk of modification times) and 2.4 ms with it (two answers of `git status`); on the Windows runner 5 ms and 73 ms. It grows with the repository, and is skipped for a call the permission engine marks read-only. |
| Changing a file with `edit` or `write`, harness time | No budget set: the model's own time for the call is seconds. Bounded by the 16 MiB limit on the files these tools change | Measured in step 10 (`perf_editing_and_undoing_a_file` in `crates/cortex-core/tests/it/undo.rs`): the real tools against a real session's checkpoint store, on the desktop, release build, medians. One edit in the middle of a file of 10 KiB: 0.15 ms; of 1 MiB: 3.2 ms; of 16 MiB, the limit: 49 ms. That is the whole call: open, read, hash, match, hand the bytes to the checkpoint store, write the new file beside the old and rename it, hash what was left. **The flush to disk is on top,** and for a small file it is nearly all of the call: about 15 ms a write on the desktop's disk, so 15.6 ms, 22 ms and 113 ms for the three sizes. The tools flush because these are the user's files. The end of the round adds 0.01 to 0.03 ms, because the tool told the checkpoint what it left and nothing is hashed again. Undoing the turn: 14 ms, 19 ms and 72 ms, of which the flush of the restored file is about 14. Reading the same file with `read`: 0.08 ms, 1.4 ms and 11 ms. So hashing is not what a call spends its time on, and the limit, not a separate cap on hashing, is what bounds it: a file the tools will not change is refused on its length, before a byte of it is read. Another session's run may have been active on the desktop. Windows is not measured. |
| Hot-reload check | ≤ 1 ms | By design: a few `stat` calls. Step 4 measured the check itself (`cortex-base`, `watch`) on the desktop, release build, files in the page cache: 3.2 µs for ten watched files. Resolving a path, measured the same way: 6.5 µs for an existing file nine directories below the file system's root, 7.1 µs for the same file through a link, and 5 to 6 µs for a file that does not exist yet. The resolver looks at every directory on the way without following it, which is one `lstat` a level. Windows is not measured. |

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
| Release binary, Linux arm64, `dist` profile, stripped | ≤ 16 MB | The same limit as Linux x86-64, until it is measured with real code. The skeleton with the protocol crate: 0.38 MB on both. |
| Release binary, Windows x86-64 | ≤ 18 MB | Not measured. Windows binaries of the same code are usually somewhat larger. |
| Crates in a release build | Recorded, not capped. A rise of more than five in one change needs a reason. | Core set: 191. With the optional parts: about 310. Recorded by the `main` workflow since step 3: 30 (19 of ours and 11 third-party), with a `dist` binary of 0.38 MB on Linux and 0.12 MB on Windows. |

The size budget is where the vision's features meet the footprint goal most directly: the script engine, the MCP SDK and the highlighter are each a megabyte or more. Their measured costs are in the decision records, and the ones that are optional have cargo features so their cost can always be seen.

**How.** `ls -l` on the stripped `dist` binary.

**Checked.** Main (a release build is too slow for the blocking checks): `cargo xtask size --check`, in the `main` workflow after each merge and in `nightly` for every release target. Each of the three release targets has a row above.

## Build and test times

| Budget | Limit | Measured basis |
|---|---|---|
| Blocking pull-request checks, wall time per OS | < 5 min | Measured in step 2 for the skeleton: 20 to 29 s on Linux and 37 to 39 s on Windows, with a cold or a warm cache. In step 3, with the protocol crate and a cold cache for the new dependencies: 39 to 48 s on Linux and 78 to 94 s on Windows. The tables are in [testing-and-ci.md](testing-and-ci.md). |
| Clean build of the workspace in the `ci` profile, Linux runner | ≤ 2 min | Measured in step 2: the planned third-party set alone takes 27 to 45 s. In the `dev` profile, which optimises dependencies, it takes 129 s; that is why CI has a profile of its own. |
| Clean build of the workspace in the `ci` profile, Windows runner | ≤ 3.5 min | Measured in step 2: the planned third-party set alone takes 60 to 82 s, after about 10 s of fetching sources (26 to 36 s before cargo's files were moved off the runner's system disk). In the `dev` profile it takes 259 s. |
| Clean release build (`release` profile), 4 desktop cores | ≤ 3 min | Core dependency set: 37 s. With the optional parts: 76 s. The script engine adds about 9 s. |
| `cargo check` after an edit in a leaf crate, desktop | ≤ 2 s | |
| Rebuild and relink the binary after an edit, desktop | ≤ 5 s | Relinking the full dependency set with a trivial change: 0.4 s on the desktop, 0.54 s on a Linux runner, 1.5 s on a Windows runner. |
| No-op build | ≤ 0.5 s | 0.13 s with the full dependency set. 0.03 s for the skeleton. |
| Tiers 1 and 2 test time | ≤ 60 s on a runner, ≤ 30 s on a desktop | |
| Tier 3 test time | ≤ 60 s Linux runner, ≤ 90 s Windows runner | |

**How.** CI job timings, and `cargo build --timings` for the breakdown.

**Checked.** PR: the job fails at 5 minutes by its timeout, which makes the limit real. Main records the phase timings so a trend is visible before the limit is hit.

## Data paths

| Budget | Limit | Measured basis |
|---|---|---|
| Open a session with 10,000 entries | ≤ 50 ms | Measured in step 8 on the desktop (release build, the file in the page cache, medians of 15): a 12.1 MB log of prompts, answers with a paragraph and a tool call, results of 1.6 KB and checkpoints opens in 10.9 ms, and resumes (with its lock and repair check) in 11.2 ms. Reading the active branch's 9,000 items for a request's history takes 27 ms more, 39 ms in all. Opening keeps only each entry's envelope and where its line is; the rest is read when asked for. |
| List 1,000 sessions | ≤ 10 ms | Measured in step 8 on the desktop: 0.72 ms (median of 30, slowest 0.95 ms). It reads the index and makes one directory listing, which is how a session the index does not have yet is found. The three session measurements run with `cargo test --release -p cortex-session --test it perf_ -- --ignored --nocapture`. |
| Append one entry, including the flush | ≤ 2 ms on an SSD | Measured in step 8 on a laptop running Linux directly on its NVMe SSD (ext4, release build, three runs of 200 appends): 0.48 to 0.49 ms median, 0.54 to 0.59 ms at the 95th percentile. Without the flush the same append takes 3 µs, so the time is the flush (`fdatasync`). On the desktop, whose Linux runs in a WSL 2 virtual machine, the same test took 4.6 ms on the virtual disk that sits on its NVMe drive and 7.7 ms on a USB SSD: the virtual disk's flush, not the harness. |
| Search a 100,000-file tree for a literal | ≤ 1 s on a desktop | Measured in step 6: 43 ms on the desktop with every core (the walker uses 12 threads), 81 ms pinned to 4 cores, 239 ms on one thread. The tree is 100,000 files of a kilobyte each in 1,000 directories, with the literal in one file in a hundred, read from a warm file cache. A pattern that matches all 1.2 million lines takes 143 ms, because only the matches a result can show are kept. Run it with `cargo test --release -p cortex-tools --test it perf_ -- --ignored --nocapture`. |
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
