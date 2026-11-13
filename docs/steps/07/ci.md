# After step 7: the agent loop

Step 7 filled `cortex-agent` (about 6,700 lines of source and 6,000 of integration tests: 31 unit and 118 integration tests) and removed the third copy of the property helper from `cortex-tools`. It adds no third-party package to the workspace: `tokio`, `serde_json` and `thiserror` were in the build already, and `cortex-agent` turns on two features of `tokio`, `rt` and `sync`. Measured on the step's pull request on 2026-11-13, rebased on the main branch after step 8:

| Job | Step 5, first run | Step 8, second run | Step 7, first run |
|---|---|---|---|
| `lint-linux` | 62 s | 39 s | 26 s |
| `test-linux` | 51 s | 46 s | 45 s |
| `lint-windows` | 115 s | 47 s | 58 s |
| `test-windows` | 99 s | 87 s | 93 s |
| **Wall time, Linux** | 62 s | 46 s | 45 s |
| **Wall time, Windows** | 115 s | 87 s | 93 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs | 7.2 s (clippy 6.2 s) | 20.6 s (build tests 16.9 s, run 3.6 s) | 17.4 s (clippy) | 50.2 s (build tests 41.6 s, run 8.6 s) |
| Test time against the tier budgets | | tier 1: 5.5 s of 20 s. Tier 2: 5.7 s of 40 s. Tier 3: 0.6 s of 60 s | | tier 1: 12.0 s of 20 s. Tier 2: 15.3 s of 40 s. Tier 3: 2.9 s of 90 s |

- **The step is inside the noise of the runners.** Every job is within eleven seconds of step 8's second run, on the slower OS too, and far under step 5's, which built the HTTP stack from source. The job passed on both OSes on its first run, with no test over its slow mark: 1,009 tests on Linux and 1,016 on Windows.
- **The loop's 149 tests take 0.43 s together on the desktop, run one at a time** (two runs: 0.42 and 0.44 s), 0.12 s on four threads and 0.06 s on all of them. The step's done criterion is under 5 seconds. No other session's run was active on the desktop for these.
- **Nothing in them waits.** A session runs on a single-threaded runtime, the provider is scripted, and a test waits for what it expects to see: an event, a line in the fake tools' log, a gate. The loop itself takes no time from the runtime: its one clock is the fake clock.
- **What takes the time is real threads, not the loop.** Planning a call goes through the blocking pool, because a plan may ask the file system where a path leads, so every test with a tool call crosses to another thread and back. The property over random rounds makes a session with a runtime of its own for each case. It runs 24 cases, 0.03 s on the desktop, so that the nightly run at fifty times the count (0.8 s for the whole crate) stays under the mark at which a test is reported as slow. At 40 cases it took 2.5 s there on a busy machine.
- **Two tests use the real `read` tool on real files,** to show that a result which is thrown away is forgotten by the table of seen files. Everything else uses fake tools that touch nothing.
- **A panic in the loop fails a test at once.** The loop contains its own panics and ends the turn. In a test that is a bug showing itself, so the test's event sink refuses a turn that ended that way, and the test process stops with the panic's message in place of waiting out the 10 s limit.
- **The Windows job ran nothing of this step that Linux did not.** `cortex-agent` has no code of its own for one OS. It was linted for the Windows target on the desktop before the first push.
- **The clean build of `cortex-agent` alone,** on the desktop pinned to 4 cores: 19.0 to 19.5 s in the `dev` profile and 12.4 to 12.7 s in `release` (two runs each), with 71 packages in the build, against 13.7 to 13.8 s and 8.4 s for `cortex-tools` alone after step 6. The difference is `cortex-llm` without its `http` feature, which `cortex-tools` does not build, and the crate itself.
