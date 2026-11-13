# After step 13b: the command line and headless mode

Step 13b built `cortex-cli`: the command line, headless mode with its three output formats and its exit codes, the subcommands, signals and the hidden guard subcommand, with 20 unit tests (21 on Windows), 24 system tests through the real binary (23 on Windows) and 2 live tests, ignored. It added the subcommands' functions and the shell's version cache to `cortex-core` (`manage`, `shell_cache`), the reduction of tools for a strict variant to `cortex-llm` (`strict`), the headless records to `cortex-protocol`, the schema validator to `cortex-testkit`, and `cargo xtask startup`. It adds one direct dependency, `lexopt`, and two packages to a Unix build through Tokio's `signal` (`signal-hook-registry`, `errno`).

## At a desk

The desktop, through `rx`; another session shares it, and whether it was building at the time is not known.

| | |
|---|---|
| `cargo xtask ci`, tier 1 (unit) | 7.5 s of 20 s |
| `cargo xtask ci`, tier 2 (integration) | 19.7 s of 40 s |
| `cargo xtask ci`, tier 3 (system) | 3.7 s of 60 s |
| `cortex-cli`'s own tests | 0.5 s of test time: 21 unit tests, 24 system tests |

## On the pull request

Pull request 33, last run at `4fa9174` on 2026-11-13, rebased on main at `7620319`. "Main" is main's own run at `b852e3a`, the last before the step's rebase whose runners started a test's process at the same speed on Linux (4 ms); its Windows runner was slower (11 ms against 8 ms), so the two Windows columns are not the same machine's.

| Job | Main, `b852e3a` | Pull request, `4fa9174` |
|---|---|---|
| `lint-linux` | 52 s | 48 s |
| `test-linux` | 68 s | 56 s |
| `lint-windows` | 74 s | 67 s |
| `test-windows` | 162 s | 138 s |

| Time above process start, against the budget | Linux, main | Linux, the PR | Windows, main (11 ms) | Windows, the PR (8 ms) |
|---|---|---|---|---|
| Tier 1, unit (20 s) | 3.8 s | 3.4 s | 9.8 s | 7.6 s |
| Tier 2, integration (40 s) | 15.7 s | 7.1 s | 29.8 s | 21.6 s |
| Tier 3, system (60 s on Linux, 90 s on Windows) | 5.5 s | 4.7 s | 64.6 s | 57.1 s |

**What this step's own tests add**, as the sum of their times in the pull request's last run:

| | Linux | Windows |
|---|---|---|
| Tier 1: `cortex-cli`'s unit tests, `cortex-llm`'s `strict`, `cortex-core`'s `shell_cache`, `xtask`'s `startup` | 0.1 s | 0.2 s |
| Tier 2: the trust test of `cortex-core` | 0.02 s | 0.1 s |
| Tier 3: `cortex-cli`'s system tests | 1.6 s | 9.1 s |

- **Before the shell's version was kept, the same system tests were 11.1 s on a fast Windows runner and the tier 85.3 s of 90 s on a slow one** (a warning, 95%): every headless run of a test asked PowerShell its version. One run in between, with the version asked on the side but not yet waited for at shutdown, was over its budget (99.9 s of 90 s, a warning) because one test waited 30 s for a version that a short run never kept; the fix is in the last commit.
- **The blocking checks stay under five minutes on both OSes:** 56 s on Linux and 138 s on Windows in the last run.

## Start-up, release build

### On the runners

`cargo xtask startup` in the `main` workflow's release job (the `dist` binary), started by hand on the branch. Median of 30 runs, after one that is not counted; the 95th percentile in brackets.

| What | Linux runner | Windows runner, first measurement | Windows runner, with the version kept | Budget, Linux / Windows |
|---|---:|---:|---:|---:|
| `cortex --version` | 0.37 ms (0.43) | 8.01 ms (9.13) | 4.63 ms (5.12) | 15 / 50 ms |
| `cx --version` | 0.36 ms (0.42) | 11.08 ms (12.21) | 5.87 ms (6.06) | 15 / 50 ms |
| headless `cortex -p`, to its request | 8.58 ms (10.83) | **211.69 ms (237.53)** | 32.14 ms (60.48) | 40 / 100 ms |
| headless `cx -p`, to its request | 8.49 ms (10.78) | **213.72 ms (227.27)** | 34.10 ms (55.75) | 40 / 100 ms |
| breakdown: with `--no-session` | 6.08 ms | 201.94 ms | 17.41 ms | |
| breakdown: with a shell that starts fast (`sh`, `cmd`) | 8.81 ms | 32.07 ms | 20.50 ms | |
| breakdown: with both | 6.12 ms | 18.86 ms | 9.17 ms | |

- **What the Windows time went to:** about 180 ms of 212 was the session asking PowerShell 7 its version before its first request. The version is now kept in the state directory ([answered.md](answered.md)). The run that is not counted is the one that asks; the 30 after it read the answer.
- **What is left on Windows,** 32 ms: about 15 ms is the session log (owner-only files, decision 0029) and about 9 ms is the process and the core.
- **Linux** has room to spare on its runner. The desktop's headless run, 12.6 ms against 15 ms, is closer: 9.4 ms of it is the session log there, against 2.5 ms on the runner, which suggests the desktop's disk or the other session's load; it was not looked into further.

### At a desk

`cargo xtask startup target/release/cortex --budget desktop`: 30 runs each after one that is not counted, the binary under its own name and under `cx`, a hard link beside it.

| What | Median | 95th percentile | Budget |
|---|---:|---:|---:|
| `cortex --version` | 0.56 ms | 0.71 ms | 5 ms |
| `cx --version` | 0.54 ms | 0.62 ms | 5 ms |
| headless `cortex -p`, to its request | 12.77 ms | 13.47 ms | 15 ms |
| headless `cx -p`, to its request | 12.74 ms | 13.70 ms | 15 ms |

## Breaks

`cargo xtask mutate step-13b` (`mutations/step-13b.breaks`): 32 breaks: 22 written with the code, 7 with the tests the independent review asked for, and 3 with the shell's version cache. Each is caught by the tests its list names. No break covers `Core::shutdown` waiting for a shell still being asked: only Windows shows the difference (bash answers before a short run ends), where the system test `sys_the_shells_version_is_kept_for_the_runs_after_the_first` failed without it. One, `strict-open-object-kept`, is caught by a unit test alone: the one open object in today's toolbox is in a tool that Anthropic's limit on optional parameters already keeps out of strict mode, so no request shows the difference.

## The live runs

| Run | Result | Spend |
|---|---|---|
| `cargo xtask live` (Anthropic), first | The smoke test passed; the headless run was refused by Anthropic: `maxItems` in a strict tool | $0.00036, and nothing for the refused request |
| The headless run, twice more while it was fixed | Refused: an object left open; 25 optional parameters of a limit of 24 | nothing |
| The headless run, fixed | Completed: "forty-two", 2 requests, 1 tool call | $0.00063 |
| `cargo xtask live deepseek` | The smoke test, and the headless run with the whole toolbox: completed | $0.00089 and $0.00113 |
| `cargo xtask live` (Anthropic), last | Both passed | $0.00020 and $0.00012 |
| **All of them** | | **about $0.004** |

## The independent review

One review, on Opus, of the whole diff against the spec, before the first push. It found no high-severity bug. What it found, and what became of it:

| Finding | Severity | What was done |
|---|---|---|
| `--deny`, `--trust-project`, `--resume`, `--fork` and `--quiet` had no test; the second Ctrl-C's test would pass for a binary that exits on it | medium | Tests for each, and the second Ctrl-C's run must write its record; five breaks |
| Signals were dropped once the core thread was done, so a reader that stopped reading held the process; and not taken while the writer waited for one | medium-low | Signals are watched until the last line is written, and any signal exits once the core is done; a blocked write still takes them and interrupts the turn. Not tested: [outstanding](../../outstanding/headless-details-left-open.md) |
| The strict reduction removed a fixed list of keywords, so an MCP tool with `oneOf` would be sent as strict and refused | medium-low | An allow-list: a schema with any keyword its variant is not known to take, a list of schemas for `items`, or recursion is sent whole and not strict; OpenAI's root `anyOf` too. Unit tests and a break |
| Standard input that is not UTF-8 failed the run | low-medium | Read with U+FFFD; a test and a break |
| Trust cannot be approved for a profile a project defines | low-medium | [Outstanding](../../outstanding/trust-of-a-profile-a-project-defines.md) |
| The console's close event may end the process before its record | low | Said in the headless page; [outstanding](../../outstanding/headless-details-left-open.md) |
| A repository's rules and a model's arguments reached the terminal with their control characters | low | Escaped everywhere text reaches the terminal; a unit test and a break |
| The runtime page's "within two seconds" and "cancel the session token"; `--quiet` printed a bad log level; no record on a core-thread panic | low | The pages say what is built; `--quiet` silences it; a panic writes an `internal` record. A command line that cannot be read writes none, and the page says so |
| Subcommands panicked on a closed pipe; `auth login` took any provider name and cut a long key silently | low | Output that ends quietly; an unknown provider and an over-long key are refused; a test and a break |
| Early notices lost on a failure before the turn; an interrupted record's `error` differs by timing; `files_changed` trusts any tool's payload; `--continue` takes the newest created | low | [Outstanding](../../outstanding/headless-details-left-open.md), and [one for `--continue`](../../outstanding/continue-chooses-the-newest-session.md) |

## After the merge: the signal test that failed on main, 2026-11-13

`main` at `d104f4a`, `ci` run 43453239341, `test-linux`: `signals::sys_ctrl_c_interrupts_the_turn_and_exits_130` stopped at 30 s on its `hang_until_closed` gate. The job's re-run passed. The cause and the fix are in [answered.md](answered.md).

**At a desk** (the desktop, through `rx`). "Loaded" is the signal tests pinned with `taskset` to two cores that busy loops also use, four loops or eight.

| Run | Before the fix | After it |
|---|---|---|
| `cargo xtask stress -p cortex-cli --count 200`, `signals::` only | 200 of 200 passed | 300 of 300, with `cortex-testkit`'s `server::` tests beside them |
| Loaded, four loops | 1 failed of 200: the SIGTERM test, at `signals.rs:90` | |
| Loaded, eight loops | 800 of 800 passed | 1000 of 1000 passed |
| A 300 ms pause before the response's head, the Linux signal tests | both failed every time, the server's write a `BrokenPipe` | both passed, in 0.37 s, with the signal tests as they were on `main` |

The failure is rare by itself, so the pause is the evidence that tells the cases apart, not the counts. `cargo xtask mutate step-13b/server-misses-an-early-hang-up`: caught by `server::tests::a_client_that_hangs_up_before_the_hold_still_opens_its_gate`.

**On the pull request**, at `5d315c2`: `lint-linux` 46 s, `test-linux` 68 s, `lint-windows` 67 s, `test-windows` 142 s. The tiers, as time above process start against the budget: on Linux (4 ms a start), 5.5 s, 12.4 s and 11.9 s; on Windows (9 ms, a slow runner), 8.1 s, 24.4 s and 62.2 s of 90 s. The change adds one unit test, and takes nothing from the signal tests' time but the wait it removes.

**`windows-try`** with `crate=cortex-cli`, `filter=signals::`, `count=200` (run 43456142065): the Windows signal tests, Ctrl-Break's among them, passed 200 times out of 200.
