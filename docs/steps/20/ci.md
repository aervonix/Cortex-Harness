# After step 20: the terminal layer

Step 20 built `cortex-term` (the styled-line model, the line renderer, sanitising, input decoding with Windows Terminal's win32 input mode, capability detection, the inline presenter, the frame scheduler, and raw mode with the terminal put back on every way out), the test kit's virtual terminal and pseudo-terminal driver, the demo, and `cargo xtask perf`. It adds no third-party package.

## The Windows-only rules, broken by hand

Two rules live in code that only a Windows build compiles, so `cargo xtask mutate` cannot break them. Each was broken on a scratch branch and run on a hosted Windows runner with `windows-try` (`-f crate=cortex-term -f filter=sys_`), on 2026-11-13.

| Break | What failed |
|---|---|
| Virtual-terminal processing not turned on (`our_output_mode` without `ENABLE_VIRTUAL_TERMINAL_PROCESSING`) | All three system tests then: the demo's first frame never showed under ConPTY, which had been started with the mode off as the old console host has it. |
| The console's input mode not put back (`restore_modes` without its input half) | All three system tests then: the shell child reported `restored: NO`, input mode `1f7` before and `3e8` after, on the normal exit, the panic and the Ctrl+Break event alike. |

## On the pull request

Pull request 36, on `main` at `f24c001`. "Main" is main's own run at `f24c001`. Hosted runners differ in speed by up to a factor of two, told by the cost of starting a test's process that the tier report prints.

| Job | Main, `f24c001` | The PR at `c453310` | The PR at `3172648` |
|---|---|---|---|
| `lint-linux` | 37 s | 38 s | 54 s |
| `test-linux` | 80 s | 86 s | 63 s |
| `lint-windows` | 59 s | 59 s | 73 s |
| `test-windows` | 161 s | 140 s | 163 s |

| Time above process start, against the budget | Linux, main (5 ms) | Linux, `c453310` (5 ms) | Windows, main (10 ms) | Windows, `c453310` (8 ms) | Windows, `3172648` (10 ms) |
|---|---|---|---|---|---|
| Tier 1, unit (20 s) | 4.9 s | 5.6 s | 8.4 s | 7.5 s | 10.9 s |
| Tier 2, integration (40 s) | 13.5 s | 13.3 s | 27.0 s | 24.1 s | 33.4 s |
| Tier 3, system (60 s, 90 s on Windows) | 9.7 s | 15.3 s | 70.0 s | 59.4 s | 75.7 s |

The tiers move with the runner and with other crates' slowest tests (on Linux at `c453310` one PowerShell comparison of `cortex-policy` took 8.7 s of the system tier's 15.3). What this step's own tests take, raw, from the JUnit reports of `3172648`:

| | Linux | Windows |
|---|---|---|
| `cortex-term`, unit (60 tests) | 0.69 s | 1.44 s |
| `cortex-term`, integration (6 tests) | 0.45 s | 0.86 s |
| `cortex-term`, system (5 tests on Linux, 4 on Windows) | 0.16 s | 0.47 s |
| `cortex-testkit`, its new unit tests (11) | under 0.1 s | under 0.2 s |

The integration tier on Windows was 33.4 s of 40 at `3172648`, against 27.0 s on main's run and 24.1 s at `c453310`. The step adds 0.9 s of it. The rest is that runner: every crate's integration tests took 10 to 25% longer there than in main's run (`cortex-llm` 12.3 s against 10.6, `cortex-tools` 8.1 against 6.5), with the same start cost of 10 ms a test, which the start cost alone does not show. The tier is near its budget on a slow runner, as it was before the step.

## Stress and breaks

- `cargo xtask stress -p cortex-term -p cortex-testkit --count 200` on the desktop, at `3172648`: 143 tests, every one passed every time. An earlier run, at the first fixes after the review, found the Ctrl+Z test failing 70 times in 200: it waited for a state that the output can pass through within one read. The test now checks what was written, in order.
- `cargo xtask mutate step-20`: 33 breaks, every one caught by the tests its list names. In one full run `key-release-kept` was reported as not caught by its own test; run again alone it was caught, and the test fails with the break made by hand. The cause was not found.
