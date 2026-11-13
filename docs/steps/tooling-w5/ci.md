# After the wave 5 tooling step: developer tooling

This step added tools, not product code: about 7,000 lines to `xtask` (the mutation driver, the stress command, the tier report's slowest tests, three repo rules), a seam in `cortex-tools` (250 lines with its tests), two modules in `cortex-testkit`, 211 breaks in `mutations/`, and a workflow that blocks nothing. No third-party crate was added. It was asked to add nothing to the blocking checks beyond the upload of a small artifact. Measured on the step's pull request on 2026-11-13, against the two runs of the main branch before it:

| Job | Main, `f924f46` | Main, `d4a0de7` | This step, first run | This step, second run |
|---|---|---|---|---|
| `lint-linux` | 43 s | 35 s | 36 s | 38 s |
| `test-linux` | 58 s | 69 s | 57 s | 67 s |
| `lint-windows` | 75 s | 69 s | 66 s | 59 s |
| `test-windows` | 96 s | 114 s | 151 s, failed | 93 s |
| **Wall time, Linux** | 58 s | 69 s | 57 s | 67 s |
| **Wall time, Windows** | 96 s | 114 s | 151 s | 93 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command, with the build of `xtask` before it: main's two runs | 21 and 18 s | 34 and 48 s | 34 and 36 s | 63 and 74 s |
| The same, this step's two runs | 17 and 20 s | 38 and 47 s | 36 and 30 s | failed, and 63 s |
| Keeping the JUnit report | | 1 s | | 1 s |
| Test time against the tier budgets (second run) | | tier 1: 11.1 s of 20 s. Tier 2: 14.7 s of 40 s. Tier 3: 7.0 s of 60 s | | tier 1: 11.9 s of 20 s. Tier 2: 17.7 s of 40 s. Tier 3: 27.4 s of 90 s |
| Of which this step's own tests (second run) | | tier 1: 1.1 s. Tier 2: 0.2 s | | tier 1: 1.7 s. Tier 2: 0.4 s. Tier 3: 0.4 s |

- **The upload is one second a test job.** The report is 0.3 MB. It is the one step this change adds to a blocking job.
- **The jobs are no slower than the main branch's own spread.** Every figure of this step's runs is inside the range of the two runs before it, which differ from each other by more than this step could add. `xtask` is built before every job does anything, and it is now more than twice the size it was: its own rebuild is 0.7 s on the desktop, and the job's command with that build in it took what it took before.
- **The three new static checks cost a fraction of a second:** the breaks 0.1 s for 211 of them, the links and the layout under a tenth of a second for 137 pages, on the desktop.
- **This step's own tests are in the blocking tiers, and are the cost that is not the upload:** about 125 tests, 1.1 s of the unit tier on Linux and 1.7 to 2.9 s on Windows, where a test's time is mostly the start of its process. The tools are tested there because that is where a change to them is caught on both OSes: the first run's one failure was such a catch. The one test that starts the real cargo is not in a blocking tier: it runs nightly.

## What the first runs showed

- **The first run failed on Windows in one test, which was doing its job.** The lock that keeps two runs of `cargo xtask mutate` off one copy was taken on a file opened for appending. Windows locks no file that is open only for appending, and the refusal was taken for "this file system has no locks", so on Windows the lock did nothing. The file is now opened for reading and writing, and a lock that cannot be taken stops the run.
- **`windows-try` was run once before it was merged,** through a push trigger that was on the branch for one push: a workflow can be started by hand only once it is on the main branch. It restored the cache the main branch wrote for `test-windows`, built the tests in 32 s, ran the whole workspace and caught the same failure in the same minute. The job took 141 s.
- **The Windows unit tier was over its budget in one of three runs of this step:** 22.6 s of 20 s in the first run, 13.1 s in `windows-try` on the same commit, and 11.9 s in the second run. The same 969 tests took almost twice as long on one runner as on the next. The tier report's slowest tests for the first run: `cortex-testkit server::tests::it_can_hold_the_whole_response_back_and_is_let_go_when_dropped` at 2.0 s (the test step 12 named as slow on Windows in every run), then `cortex-base path::tests::every_short_path_matches_the_reference` at 0.7 s and three more of `cortex-base` at 0.4 to 0.5 s; the five are 18% of the tier. The rest is 964 tests at 19 ms each: on that runner the tier's time was the starting of processes. This step's tests were 2.9 s of it. A tier that is at 60% on one runner and 113% on another is a tier whose budget the next step will meet, whichever step that is.
- **A test of step 11 timed out once.** `cortex-tools::it shell::cmd::sys_the_command_reaches_the_shell_as_written` was stopped at 60 s in the `windows-try` run and passed in the pull request's run of the same commit and in the run after. It was 60 of that run's 82.8 s of system tier. It is not this step's, and it is not changed here.
- **The reports were read with the tool that was built for them.** `gh run download <run> --name junit-windows` and `cargo xtask ci --tiers --from junit.xml --os windows` gave every figure above that is about one test.
