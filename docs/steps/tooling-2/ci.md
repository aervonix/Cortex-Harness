# After the second tooling step: tier comparisons, the try workflow, the timeout report and parallel breaks

This step added tools, not product code: about 3,500 lines to `xtask`, the `watch` module of `cortex-testkit` and the names of its gates, the toolbox fixtures and their tests in `cortex-llm` and `cortex-cli`, a probe in `cortex-policy`'s tests, a workflow that blocks nothing, and 24 breaks. No dependency was added. The tables below were made with the step's own `cargo xtask ci-times` and `cargo xtask ci --tiers --compare`.

## On the pull request

Pull request 37, last run at `eebe85c` on 2026-11-13, against the main branch's run at `f24c001`, the step's base.

| Job | Main, `f24c001` | First run, `e02f1af` | Last run, `eebe85c` |
|---|---|---|---|
| `lint-linux` | 37 s | 37 s | 44 s |
| `test-linux` | 80 s | 64 s | 84 s |
| `lint-windows` | 59 s | 68 s | 75 s |
| `test-windows` | 161 s | 168 s | 167 s |

| Time above process start, against the budget | Linux, main (5 ms) | Linux, last run (3 ms) | Windows, main (10 ms) | Windows, last run (11 ms) |
|---|---|---|---|---|
| Tier 1, unit (20 s) | 4.9 s | 2.5 s | 8.4 s | 11.5 s |
| Tier 2, integration (40 s) | 13.5 s | 7.3 s | 27.0 s | 33.0 s |
| Tier 3, system (60 s on Linux, 90 s on Windows) | 9.7 s | 7.9 s | 70.0 s | 79.8 s |

- **What the comparison found and the step fixed.** On the first run the Windows unit tier was 4.9 s over main's, and `--compare` put 3.5 s of it on one new test, `watch::tests::a_wait_is_reported_while_it_lasts`: it built the whole timeout report three times, and on Windows the report's process tree is asked of PowerShell. The test now reads the waits alone, and the tree only on Linux.
- **What the step's own tests add.** The three new `openai-chat` streams add about 0.8 s to the Windows integration tier (`openai_chat::decode::at_every_cut::*`) and 0.3 s on Linux; Anthropic's adds a few hundredths. The toolbox check of `cortex-cli` runs the binary four times, about 1 s of the Linux system tier, and is skipped on Windows. The tests of the timeout report and of the race pause are ignored, and run nightly.
- **What is the runner's.** The rest of the Windows change is spread over tests this step did not touch, most of them PowerShell's (`cortex-tools`' shell and process tests, `cortex-core`'s trust tests, `cortex-policy`'s comparison with PowerShell's parser), on a runner that started a test in 11 ms against main's 10 ms; against the run before main's, on an 8 ms runner, the same tree is 22.6 s over in tier 3, all in such tests. Linux, on a faster runner than main's, is lower in every tier. The timeout report's thread runs PowerShell only for a test at four fifths of its limit, and no test of either run came near one. A comparison on one runner, both trees in one job, is what would settle it; the `try` workflow can run it once it is on the main branch.
- **The Windows tiers are near their budgets** in this run (83% and 89%) as they were in main's (68% and 78%) and are not over them.

## The try workflow, before it was on the main branch

A dispatched workflow can be started on a branch only once it is on the main branch. To check it before, a scratch branch carried a copy of `try.yml` that also ran on a push, as `startup` in the `dist` profile on both OSes (run 43473610962; the branch was deleted afterwards). Both jobs passed: Linux in 2 min 33 s and Windows in 3 min 39 s, each building `dist` from scratch. Median of 30 runs, 95th percentile in brackets:

| What | Linux runner | Windows runner | Budget, Linux / Windows |
|---|---:|---:|---:|
| `cortex --version` | 0.92 ms (0.98) | 5.11 ms (5.40) | 15 / 50 ms |
| `cx --version` | 0.89 ms (0.95) | 5.47 ms (5.73) | 15 / 50 ms |
| headless `cortex -p`, to its request | 6.42 ms (29.29) | 14.08 ms (14.52) | 40 / 100 ms |
| headless `cx -p`, to its request | 6.32 ms (79.82) | 14.18 ms (15.09) | 40 / 100 ms |
| breakdown: with `--no-session` | 5.32 ms | 11.21 ms | |
| breakdown: with a shell that starts fast (`sh`, `cmd`) | 6.45 ms | 13.96 ms | |
| breakdown: with both | 5.02 ms | 10.46 ms | |

## At a desk

The desktop, through `rx`, shared with other sessions.

| | |
|---|---|
| `cargo xtask ci`, tiers 1 / 2 / 3 above the start | 4.3 s / 18.3 s / 4.0 s |
| `cargo xtask mutate`, 24 breaks of `step-13a` in `assemble.rs`, copies built | 43.5 s with one job, 15.8 s with four |
| `cargo xtask mutate tooling-2 --jobs 4`, 24 breaks | all caught |
| `cargo xtask stress -p xtask -p cortex-testkit --count 200` | every test passed every time (110 s) |
| `cargo xtask stress -p cortex-testkit --count 40 --cores 2 --load 4`, the watch's own process tests | every test passed every time |
| The toolbox recordings, four providers | $0.0022 |
