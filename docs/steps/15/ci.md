# After step 15: context management

Step 15 added context management to `cortex-agent` and wired it into `cortex-core`. That is about 8,700 lines of source and tests over 52 files in the crates, with 78 new tests (76 on Windows, where two are Unix-only), and no new dependency. Measured on the step's pull request (#43) on 2026-11-14, against the main branch's run at `714db6e`, the base the branch was rebased on:

| Job | Main, `714db6e` (run 43580906780) | This step, `0d051d3` (run 43582887347) |
|---|---|---|
| `lint-linux` | 48 s | 50 s |
| `test-linux` | 75 s | 84 s |
| `lint-windows` | 70 s | 79 s |
| `test-windows` | 141 s | 170 s |

| Time above process start, against the budget | Linux, main (4 ms) | Linux, this step (4 ms) | Windows, main (8 ms) | Windows, this step (10 ms) |
|---|---|---|---|---|
| Tier 1, unit (20 s) | 4.1 s | 4.7 s | 8.7 s | 11.5 s |
| Tier 2, integration (40 s) | 12.9 s | 14.3 s | 24.1 s | 37.7 s |
| Tier 3, system (60 s on Linux, 90 s on Windows) | 11.5 s | 9.4 s | 63.1 s | 75.7 s |

## What this step adds to each tier

**The two Windows runs above are not comparable.** This step's run started its test processes at 10 ms, and main's at 8 ms. The crates this step does not touch grew too: `cortex-llm` by 2.9 s of tier 2, `cortex-tools` by 2.3 s, `cortex-session` by 1.7 s. So the branch was measured against `main` on one runner, with the `try` workflow's A/B (run 43583320616: both trees built once, five rounds in turn, medians):

| Windows, one runner (11 ms) | Main | This step | Added | Budget |
|---|---|---|---|---|
| Tier 1, unit | 9.2 s | 10.3 s | +1.1 s | 20 s |
| Tier 2, integration | 32.6 s | 36.2 s | +3.6 s | 40 s |
| Tier 3, system | 73.2 s | 74.3 s | +1.1 s | 90 s |

**Of the 3.6 s in tier 2:**
- **The step's new tests, about 1 s.** Each test's time above its start, in the pull request's run:
  - Windows: 0.15 s of tier 1 (37 tests) and 1.08 s of tier 2 (39 tests);
  - Linux: 0.04 s and 0.34 s.
  - Tier 3 gains no test.
- **The core's existing integration tests, 1.3 s.** In the A/B, each is a median of 14 ms slower (84 tests). Every session now builds the prompt, reads the instruction files, and makes the per-request context record. The agent's existing tests use instruction files in memory and grew by a median of 1 ms.
  - Not the cause, measured on the desktop in CI's profile: the instruction files read from disk, and the debug build's prefix check. The suite took the same time with each turned off.
  - Where the rest goes is not found yet.
- **One test, 0.46 s:** `approvals::an_approval_for_a_change_to_a_file_shows_the_change_within_a_bound`. Its history holds a 600 KB write, and the debug build's prefix check renders and compares the whole request on each of its requests: 60 ms on the desktop, about 0.3 s on a hosted runner.

**Windows tier 2 is now at 36.2 s of its 40 s budget on a runner where main's is 32.6 s.** A step that adds integration tests will cross the warning on such a runner. Two places to look, in order:
- what the per-request context work costs in a debug build (the record's hashes and sizes, and the prefix check's rendering of the whole request, which could be compared message by message instead);
- the main branch's own growth.

The Linux tiers are far from their budgets.

## What the runs showed

- **Every job passed on the first run of the pull request.** A Windows `try` run of the agent's and the core's context, instruction-file and redaction tests passed before it (run 43582427387: 73 tests; the two link tests are Unix-only).
- **A desk run once put tier 2 over its budget.** In one run of `cargo xtask ci`, 46 of the agent's context tests and 10 of its denial tests (they run next to each other) each took 3.09 s. The next run, and every run after it, had none slower than 0.6 s. The same tests take 0.05 s alone. It looked like a stall of the desktop, not a wait in the code. Nothing like it showed on the runners.
