# After step 11: the process runner and the shell tool

Step 11 added process trees to `cortex-sandbox` (about 1,600 lines), the runner and the `shell` tool to `cortex-tools` (about 4,700 lines with their unit tests, and 2,400 of integration and system tests), and child processes for tests to `cortex-testkit`. It adds no third-party package to the workspace: `libc` becomes a direct dependency and was in every build already, and `windows-sys` gains API families. It is the first step whose tests are mostly real processes: about fifty system tests on each OS are its own (52 on Linux and 48 on Windows as merged). Measured on the step's pull request on 2026-11-13. Its first run was on the base that step 8 left:

| Job | Step 5, first run | Step 7, first run | Step 11, first run |
|---|---|---|---|
| `lint-linux` | 62 s | 26 s | 32 s |
| `test-linux` | 51 s | 45 s | 52 s |
| `lint-windows` | 115 s | 58 s | 55 s |
| `test-windows` | 99 s | 93 s | 105 s |
| **Wall time, Linux** | 62 s | 45 s | 52 s |
| **Wall time, Windows** | 115 s | 93 s | 105 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs | 11.2 s (clippy 9.5 s) | 26.0 s (build tests 21.3 s, run 4.6 s) | 17.9 s (clippy) | 61.1 s (build tests 40.3 s, run 20.8 s) |
| Test time against the tier budgets | | tier 1: 7.6 s of 20 s. Tier 2: 6.4 s of 40 s. Tier 3: 2.4 s of 60 s | | tier 1: 11.2 s of 20 s. Tier 2: 11.4 s of 40 s. Tier 3: 33.0 s of 90 s |

- **The system tier is where this step shows, and only on Windows:** 33 s of its 90 s, where it was under 3 s. On Linux the same tests take 2.4 s together. The difference is what a shell costs to start: bash starts in a millisecond, and PowerShell 7 and Windows PowerShell in a few hundred, so a test that runs four commands in PowerShell takes one to two seconds. No test passed its 10 s mark. The set for `cmd.exe` takes under a second in all.
- **Every Windows test passed on the first run, and one failed on the second,** which was the fault of the test's own helper and is the lesson of the step: a child that reported to its test and exited at once could lose its report, because Windows may throw away what a process has sent, and the other side has not yet read, when the process ends. The child now waits for the test to answer before it goes on. The run that failed took its whole limit, 30 s, which is what the limit is for.
- **The Windows code ran here for the first time anywhere.** It had only been checked for the Windows target on the desktop. Job Objects with a suspended start, the Ctrl-Break event, cancelling a read, and the three shells' quoting all worked as written. Two things about the hosted runner were open until then, and both hold: a test process has a console to send a Ctrl-Break through, and PowerShell 7 (7.6.9), Windows PowerShell 5.1, `cmd.exe` and Git Bash are all there. A test that finds a shell missing fails in CI, so a green job means every shell ran.
- **Nothing waits.** The time limit of a command, the grace between two stages of stopping and the grace for what a command left running are waits on the runner's timer, which a test ends by hand. A child process is seen to start and to die through a loopback connection. The system tier, of which the step's tests are nearly all, takes 0.9 s on the desktop.
- **A process test is run a few hundred times before it is trusted.** One run of the suite passed, on both OSes, with four races in it: three in tests, and one in the code. Running the three crates' tests 60 to 250 times over on the desktop, with every core busy, found them: a harness killed in the instant after it started a command left a tree the guard had not been told of (3 runs in 60), which is now closed in the code ([decision 0030](../../decisions/0030-process-tree-control-in-cortex-sandbox.md)); a test counted a background process that `dash` gives an empty input, so that it exits at once; a test interrupted a child before the child had printed the word the test then looked for; and a test read a file before the process writing it had written all of it. After the fixes, 250 runs passed without a failure. A test that starts a process, waits for it, and then asserts on something a third party does is the shape to distrust: wait for the very thing the assertion is about.
- **Breaking the code fails the tests.** Forty-five rules of the step were each broken on purpose, one at a time, and the tests run: every one failed at least one test. The list is in the step's report.
- **What the hosted runners measured of starting a command,** in the unoptimised `ci` build, 30 runs: on Windows, `cmd.exe` alone 6.7 ms, and 16.1 ms through the runner; on Linux, bash alone 1.5 ms, and 5.6 ms through the runner. The Linux figure is what led to reading less of the process table ([performance-budgets.md](../../performance-budgets.md)): the desktop, with thirty processes, had hidden what a machine with a few hundred shows.
- **The clean build of `cortex-tools` alone,** on the desktop pinned to 4 cores: 14.1 s in the `dev` profile and 8.1 s in `release`, with 54 packages, against 13.7 to 13.8 s and 8.4 s after step 6. `cortex-sandbox` alone: 8.6 s and 5.0 s, with 27 packages, nearly all of it `cortex-base` and what that needs. No other session's run was active: the desktop's test lock lets one run at a time.

## Windows CI health, 2026-11-13

A job after wave 5 looked at the Windows checks: a test of `shell::cmd` stopped at 60 s in about one run in seven, the unit tier over its budget and the integration tier at it. What was found in step 11's code is in [answered.md](answered.md). This is what was measured.

**The timeouts.** Five of about 35 runs of the blocking workflow on 2026-11-13 lost one `shell::cmd` test to its 60 s limit, a different test each time.

| | Runs | What happened |
|---|---|---|
| The `cmd.exe` tests alone, repeated (`windows-try` with `count`) | 150 repeats of 8 tests (run 43429640301) | No failure. |
| All of `cortex-tools`, repeated | 40 repeats (run 43430875016) | No failure. |
| The whole workspace, four times in one job, nothing changed | run 43431467457 | The same test stopped at 60 s in the first two. |
| The whole workspace with a trace printed as it happened | 24 runs in four jobs | No failure: printing hid it. |
| The whole workspace with the trace kept in memory | run 43435383403 | One hang caught, with the cause in its trace. |
| A command started, waited for, and the job asked at once, 120 times, on a quiet runner | six rounds in run 43435383403 | The job still listed the command 18, 49, 51, 52, 55 and 62 times. |
| The new system test (40 exits) on the old count, 30 repeats | run 43437537149 | Failed 6 times. |
| The same on the fix, 30 repeats | run 43437508068 | Passed every time: 1,200 exits. |

**The tiers.** Hosted Windows runners come in speeds. The fastest starts a test process in 6 ms and the slowest in 12, and a tier's total follows: the same commit gave 17.0 and 22.7 s for the unit tier on two of them. So a run is compared with a run on a runner of the same speed, told by its fastest tests.

| Windows tier, test time | Before | After | Runner |
|---|---|---|---|
| Unit, before step 13a merged (1,009 tests) | 17.0 s | 11.5 s | fast (6 ms a process) |
| Integration, the same (848 tests) | 28.2 s | 19.5 s | fast |
| Unit, the same commit before, on slower runners | 21.9, 22.7 s | | slow (10 to 12 ms) |
| Integration, the same | 38.4, 40.6 s | | slow |
| Unit, with step 13a (1,084 tests), temporary files on `C:` | | 22.0, 22.0 s | slow, one job |
| Unit, the same, temporary files on `D:` | | 20.9, 20.4 s | the same job |
| Integration, with step 13a (907 tests), temporary files on `C:` | | 43.5, 41.8 s | the same job |
| Integration, the same, temporary files on `D:` | | 39.3, 39.6 s | the same job |
| System, the same, `C:` then `D:` | | 67.3, 63.1 s, then 61.4, 61.3 s | the same job |

The pull request's own run, on a slow runner (10 ms a process): 19.7 s, 39.1 s and 63.4 s for the three tiers, with the job at 2 min 36 s.

Step 13a's own five runs, before any of this, gave 19.3 to 28.2 s for the unit tier and 35.0 to 52.0 s for the integration tier.

- **What was changed:** the heaviest exhaustive tests check a sample on a pull request and the whole space in the nightly run; the heaviest properties run 40 to 50% of the cases they did, on a pull request and, multiplied by fifty, in the nightly run; the decoder replays cut each stream at half as many places; `serde_json` and `sha2` are optimised in the `ci` profile; two tests made more files than they needed; the scripted server no longer has its own wake-up connection refused, which cost one test 2.0 s in half the runs; and the Windows test job's temporary directory is on the workspace's disk ([testing-and-ci.md](../../testing-and-ci.md)). No test was removed.
- **The unit tier's floor is the start of a process.** nextest runs each test in a process of its own. On the slowest runners that is 12 ms a test: 13 s of the unit tier's 20 for 1,084 tests, before any test does anything, and 1.2 s more for every hundred tests added. After the cuts no unit test takes more than 0.25 s there, and the tier is at 20.4 to 20.9 s. **On the slowest runners the 20 s budget cannot be met by making tests faster, and 80% of it not on any but the fastest.**
- **The integration tier is inside its budget and not at 80% of it on the slowest runners:** 39.3 and 39.6 s of 40 s there, about 23 s on the fastest. Its floor is the same start of a process, 13.6 s for 907 tests. What is left above it is spread: the decoder replays (7.7 s), the session log's tests (5.7 s), the tools on real files, and `cortex-core`'s machines. One lever was measured and not taken: `cortex-llm` at `opt-level = 1` in the `ci` profile runs its tests four times as fast on the desktop, about 7 s of this tier on a slow runner, and adds to the build of every pull request that changes it.
- **Both were put to the owner,** who decided on the same day to keep the budgets and hold them to each tier's time above the start of its processes ([answered.md](answered.md), and "Budgets above the start of a process" below).
- **What a `cortex-core` test costs,** which step 13a asked: not the product's start. Starting a core takes 0.3 to 2 ms on a Windows runner and `session/new` 9.5 to 12 ms; the owner-only access lists take 1.2 ms for the first two directories of a process and 0.3 ms after. A test's 0.11 s was a machine of its own (a home directory, configuration, the built-in catalog read in an unoptimised build), a turn, and its removal, all in the temporary directory on the slow disk: half of it went with the move to `D:`.

## Budgets above the start of a process, 2026-11-13

The tier report now takes off each test the cost of starting its process, measured by the run, and holds the budgets to what is left ([testing-and-ci.md](../../testing-and-ci.md), "Budgets are enforced"). The figures of this job's runs, measured again that way:

| Run | Start cost | Unit, raw / above the start | Integration | System |
|---|---|---|---|---|
| 10ae61a, fast Windows runner (run 43428712691) | 6 ms | 17.0 / 11.0 s | 28.2 / 23.1 s | 39.2 / 38.8 s |
| The same tree, slow Windows runner (run 43428151586) | 11 ms | 22.7 / 11.6 s | 40.6 / 31.3 s | 45.6 / 44.9 s |
| 10ae61a, Linux (run 43428712691) | 5 ms | 12.9 / 7.8 s | 21.2 / 16.9 s | 20.6 / 20.2 s |
| PR 31's last code run, slow Windows runner (run 43441400989) | 11 ms | 19.7 / 7.8 s | 39.1 / 29.1 s | 63.4 / 62.5 s |
| Four runs of one tree in one job, slow Windows runner | 11 ms | 20.4 to 22.0 / 8.5 to 10.1 s | 39.3 to 43.5 / 29.3 to 33.5 s | 61.3 to 67.3 / 60.4 to 66.4 s |

- **The unit tier now reads alike on a fast and a slow runner** (11.0 and 11.6 s for one tree), and is at about 40 to 60% of its budget on any runner. **The integration tier does not read alike** (23.1 and 31.3 s): a slow runner is slower at the work too. It is at about 70 to 85% of its budget on the slow runners and 60% on a fast one.
- **The budgets now leave more room than they did.** Held to the raw total, the unit tier was at or over its budget on the slow runners; above the start it is at about half. A test that makes a tier slow is seen as before, since every second above the start counts; what is not counted is the 6 to 12 ms a test that no test can do anything about.
- The new tier report is measured on this branch's own pull request when it runs; its figures are added here then.
