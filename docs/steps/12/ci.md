# After step 12: the permission engine

Step 12 added `cortex-policy` (about 9,200 lines of source, 2,500 of unit tests and 4,700 of integration and system tests: 91 unit and 69 integration tests, one of them a measurement run by hand), the turn that ends as `denied` to `cortex-agent`, the project key in stored case to `cortex-base`, and the gate on what a base state keeps to `cortex-session`. It adds no package to the workspace. Measured on the step's pull request on 2026-11-13, rebased onto main with steps 9, 10 and 14 in it. "Main" is main's own run at that commit:

| Job | Main | Step 12, first run | Step 12, second run | Step 12, third run |
|---|---|---|---|---|
| `lint-linux` | 31 s | 31 s | 37 s | 30 s |
| `test-linux` | 69 s | 50 s, failed | 70 s | 48 s |
| `lint-windows` | 74 s | 53 s | 55 s | 64 s |
| `test-windows` | 90 s | 89 s, failed | 130 s | 111 s |
| **Wall time, Linux** | 69 s | 50 s | 70 s | 48 s |
| **Wall time, Windows** | 90 s | 89 s | 130 s | 111 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (main) | 8.9 s (clippy 7.5 s) | 36.4 s (build tests 29.2 s, run 7.2 s) | 22.1 s (clippy) | 47.2 s (build tests 29.2 s, run 18.0 s) |
| The same (second run) | 15.5 s (clippy 13.2 s) | 39.5 s (build tests 29.5 s, run 10.0 s) | 16.0 s (clippy) | 84.4 s (build tests 52.8 s, run 31.6 s) |
| The same (third run) | 9.4 s (clippy 8.0 s) | 28.6 s (build tests 22.4 s, run 6.1 s) | 16.7 s (clippy) | 65.9 s (build tests 42.0 s, run 23.9 s) |
| Test time against the tier budgets (main) | | tier 1: 8.8 s of 20 s. Tier 2: 14.3 s of 40 s. Tier 3: 2.9 s of 60 s | | tier 1: 10.2 s of 20 s. Tier 2: 15.6 s of 40 s. Tier 3: 25.8 s of 90 s |
| The same (second run) | | tier 1: 16.8 s of 20 s. Tier 2: 14.2 s of 40 s. Tier 3: 6.2 s of 60 s | | tier 1: **26.3 s of 20 s, a warning.** Tier 2: 29.7 s of 40 s. Tier 3: 40.6 s of 90 s |
| The same (third run) | | tier 1: 7.9 s of 20 s. Tier 2: 11.5 s of 40 s. Tier 3: 3.1 s of 60 s | | tier 1: 14.4 s of 20 s. Tier 2: 21.6 s of 40 s. Tier 3: 33.7 s of 90 s |

- **The first run failed on both OSes in one test, which was doing its job.** The comparison of the PowerShell tokenizer with PowerShell's own parser had never run: neither the laptop nor the desktop has PowerShell. On its first run it found that a parameter's name ends at the first `.` or `:`, so that `-Path.5` is two words to PowerShell and was one to the analysis ([architecture/06-permissions-and-sandbox.md](../../architecture/06-permissions-and-sandbox.md), "Command analysis as built"). Every other test that ran for the first time on Windows passed: PowerShell and `cmd.exe` commands decided on the real file system, and the directory marked case-sensitive.
- **The hosted Windows runner can make a case-sensitive directory.** `fsutil.exe file setCaseSensitiveInfo <dir> enable` works there, and the two tests that need it fail in CI if it does not, so they are not skipped unnoticed.
- **The second run put the unit tier over its budget on Windows,** at 26.3 s of 20 s. About a third of it was this crate's properties of rules and leases, at three thousand cases each: every case compiles rule sets, in a build that does not optimise. They now run a few hundred cases on a pull request, and fifty times that in the nightly run. In CI's profile on the desktop the crate's unit tests went from 3.5 s to 0.9 s, and the third run has the tier at 14.4 s on Windows. Step 14's lesson held and was not applied widely enough: the properties had been timed in CI's profile before the push, one by one, and each was well under its own limit. **A tier's total is measured before the push as well as each test.**
- **Windows varies from run to run by more than this step costs.** The integration tier was 29.7 s in the second run and 21.6 s in the third with almost the same tests, and Linux's unit tier 16.8 s and 7.9 s. Against main, the third run is 4 s more in the unit tier on Windows, 6 s in the integration tier and 8 s in the system tier, where the step's tests start PowerShell and make real directories. CI's log has no time for single tests under two seconds, so the step's share on Windows is an estimate from the desktop's proportions. A line for each tier's slowest tests in `xtask`'s budget report would settle such questions; it is outside this step.
- **One test of the test kit is slow on Windows in every run** (`server::tests::it_can_hold_the_whole_response_back_and_is_let_go_when_dropped`, 2.0 s), and one of `cortex-llm`'s transport tests was in the first. Neither is this step's.
- **The tests were run two to four hundred times over on the desktop,** the engine's and the loop's. That found a test of this step that failed three runs in ten: the loop picks among ready events at random, and what a stopped turn did depended on which it saw first. The fix was in the loop ([architecture/03-agent-loop.md](../../architecture/03-agent-loop.md), "Tool rounds"): the turn now stops at the refused call in the model's order.
- **Breaking the code fails the tests.** 158 rules of the step were each broken on purpose, one at a time, on a copy of the tree. On the first pass over the first version, 111 of 113 were caught. After the review's fixes 154 of 158 were, and the last four got tests with the order of events fixed; a fifth turned up when the properties were cut to fewer cases, and got a test that changes each field of a lease in turn. Every one of the 158 now fails at least one test.

# The fixes to step 12

The fixes changed `cortex-policy` (the command analysis, the engine, trust and approvals, leases), `cortex-agent` (the order of calls behind a question), and `cortex-session` (a base state's record). They also added `mutations/step-12-fixes.breaks`. The times below were measured on pull request 29 on 2026-11-13, rebased onto main at `e6c4ac3`. "Main" is main's own run at that commit.

| Job | Main, `e6c4ac3` | First run | Second run | Second run, Windows tests again |
|---|---|---|---|---|
| `lint-linux` | 45 s | 31 s | 45 s | |
| `test-linux` | 77 s | 74 s | 67 s | |
| `lint-windows` | 54 s | 66 s | 69 s | |
| `test-windows` | 150 s | 202 s, failed | 162 s, failed | 145 s |

| | `test-linux` | `test-windows` |
|---|---|---|
| The job's command once `xtask` runs (main) | 43.1 s (build tests 31.8 s, run 11.3 s) | 89.2 s (build tests 56.3 s, run 32.9 s) |
| The same (second run; on Windows, run again) | 37.5 s (build tests 28.6 s, run 8.9 s) | 97.9 s (build tests 64.4 s, run 33.4 s) |
| Test time against the tier budgets (main) | tier 1: 11.5 s of 20 s. Tier 2: 22.5 s of 40 s. Tier 3: 7.3 s of 60 s | tier 1: **21.9 s of 20 s, a warning.** Tier 2: 38.4 s of 40 s. Tier 3: 43.5 s of 90 s |
| The same (second run; on Windows, run again) | tier 1: 9.5 s of 20 s. Tier 2: 16.6 s of 40 s. Tier 3: 6.3 s of 60 s | tier 1: **22.8 s of 20 s, a warning.** Tier 2: 39.7 s of 40 s. Tier 3: 45.7 s of 90 s |
| Of which `cortex-policy` (main, then this) | 2.1 s, then 2.6 s | 5.8 s, then 11.9 s |

- **What the fixes add is 6 s on Windows, nearly all in starting PowerShell.**
  - The new argument test runs one script through `powershell`, `pwsh`, and `pwsh` in Legacy mode, and compares what a program receives with the analysis's words. It is 4.2 s of the system tier, about 1.4 s per start of PowerShell. Finding 2 of the review asked for it.
  - The test of a directory of 1,001 files, the bound of the listing, is 0.5 s.
  - The test of the final review's findings is 0.5 s.
  - On Linux the crate's tests went from 2.1 s to 2.6 s.
- **The Windows unit tier is over its budget on main too,** at 21.9 s of 20 s, and was 15.6 s and 24.5 s in this step's other two runs of almost the same tests. The fixes add nothing to the unit tier that rises above that spread. The integration tier on Windows is at 99% of its budget, where main is at 96%; the fixes add 1.4 s there. **Recommendation:** the Windows unit tier is a case for the tooling's slowest-test report, and not something this step can settle. The policy crate's share of it is 2.6 s.
- **The first run failed on Windows in a test of this step, which was doing its job.** The argument test found that PowerShell 7 passes a bare `~\x` to a native program as the home directory, where Windows PowerShell passes it on as written. The engine judged such a word only as home. It now judges a PowerShell word that begins with `~` both ways, quoted or not ([architecture/06-permissions-and-sandbox.md](../../architecture/06-permissions-and-sandbox.md), rule 6).
- **A `cmd.exe` test of step 11 stopped at 60 s in each of the first two runs.** It was `shell::cmd::sys_output_is_merged_in_order` the first time and `shell::cmd::sys_hostile_arguments_arrive_unchanged` the second, as the tooling step recorded on main ([../tooling-w5/ci.md](../tooling-w5/ci.md)). Run again, the job passed. The 60 s is in the system tier of those runs.
- **The checks before the push, on the desktop:**
  - `cargo xtask ci` passed with 1,932 tests;
  - `cargo xtask mutate step-12 step-12-fixes` ran all 105 breaks, and each did what its list expects;
  - `cargo xtask stress -p cortex-agent --count 50` passed every test under every seed.
