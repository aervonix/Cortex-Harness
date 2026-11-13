# After step 10: the editing tools and the plan

Step 10 added `write`, `edit`, `plan` and `remember` to `cortex-tools` (about 5,600 lines of source with their unit tests, and 2,400 of integration tests), the open that follows no link to `cortex-base`, a marker and a round number to the tool contract with their few lines in `cortex-agent`, and the first tests of `cortex-core`. It adds no third-party package to the workspace: `libc` becomes a direct dependency of `cortex-base` and was in every build already. 117 tests are its own. Measured on the step's pull request on 2026-11-13, on the base that step 11 left:

| Job | Step 7, first run | Step 11, first run | Step 10, first run | Step 10, second run |
|---|---|---|---|---|
| `lint-linux` | 26 s | 32 s | 32 s | 30 s |
| `test-linux` | 45 s | 52 s | 59 s | 50 s |
| `lint-windows` | 58 s | 55 s | 60 s | 48 s |
| `test-windows` | 93 s | 105 s | 132 s | 96 s |
| **Wall time, Linux** | 45 s | 52 s | 59 s | 50 s |
| **Wall time, Windows** | 93 s | 105 s | 132 s | 96 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (first run) | 10.1 s (clippy 8.8 s) | 30.0 s (build tests 24.6 s, run 5.4 s) | 18.8 s (clippy) | 83.4 s (build tests 54.7 s, run 28.7 s) |
| The same (second run) | clippy 8.9 s | 25.2 s (build tests 20.4 s, run 4.8 s) | 14.9 s (clippy) | 56.7 s (build tests 35.7 s, run 21.0 s) |
| Test time against the tier budgets (first run) | | tier 1: 8.0 s of 20 s. Tier 2: 9.1 s of 40 s. Tier 3: 2.4 s of 60 s | | tier 1: 15.8 s of 20 s. Tier 2: 24.2 s of 40 s. Tier 3: 42.5 s of 90 s |
| The same (second run) | | tier 1: 6.3 s of 20 s. Tier 2: 8.9 s of 40 s. Tier 3: 2.1 s of 60 s | | tier 1: 9.9 s of 20 s. Tier 2: 17.0 s of 40 s. Tier 3: 32.0 s of 90 s |

The two runs are of nearly the same tests on two machines (the second has two tests made lighter), and differ by a third on Windows: read every Windows number here with that in mind.

- **Every job passed on its first run, on both OSes,** and again on the second, with 1,274 tests on Linux and 1,267 on Windows, and none of the step's own over its slow mark. The one slow test of the first Windows run, at 2.0 s, is `cortex-session`'s test of an entry over the line limit, which this step does not touch.
- **The Windows-only code ran here for the first time anywhere,** and passed: the open that asks for a reparse point itself and reads its tag, against a real symbolic link to a file and to a directory and a real junction; `edit` and `write` against a file another program holds with no sharing, and against one it holds for reading, where the replace is tried seven times on the fake clock and the file is left byte for byte; a file let go during the retries; the read-only attribute; and a path in another case with the other separator. The module could only be type-checked for the Windows target before this run.
- **The integration tier on Windows is where the step shows:** 24.2 s and 17.0 s of 40 s in its two runs, where step 11's run had 11.4 s. The step's tests take 2.5 s together on the desktop. What they do is make, read, replace and compare real files, a few hundred times in the properties, and each of those costs several times as much on a Windows runner. The unit tier there was at 15.8 s of 20 s in the first run, which is the nearest any tier has been to its mark, and at 9.9 s in the second: the step's unit tests are properties of pure logic (0.4 s together on the desktop), and that tier has varied by a half between runs of the same tests before (8.6 s and 13.9 s in step 8). If it warns, the number of cases is the dial.
- **Nothing waits.** The retries of a replace are waits on the fake clock. The tests that swap a file for a FIFO run the call on a thread of their own and bound the wait for it, so that an open that waits is a failed test and not a hung one; none that passes waits that bound out.
- **The tests were run two hundred times over before the first push,** on the desktop with every core busy, as step 11 learned to: the five crates' whole suites, with no failure. One run of a mutation pass had shown one race first, in a test of this step (it told the plan that a notice was delivered a moment after the model's next call could already have run), which now holds the scripted response at a gate.
- **Breaking the code fails the tests.** Forty-two rules of the step were each broken on purpose, one at a time, and the tests run: every one failed at least one test. The list is in the step's report.
- **The property of undo runs few cases,** twelve, because each is a session, a workspace and a runtime of its own (about 40 ms a case on the desktop). The nightly run multiplies it by fifty.
- **The clean build of `cortex-tools` alone,** on the desktop pinned to 4 cores: 14.3 to 15.1 s in the dev profile and 8.5 to 8.8 s in release (two runs each), with 54 packages, against 14.1 s and 8.1 s after step 11. `cortex-base` alone: 7.9 to 8.8 s and 5.0 to 5.2 s, against 7.7 to 7.8 s and 5.1 to 5.5 s. Another session's run may have been active on the desktop.
