# After the third tooling step: the nightly's fix, wrapped traits, the pre-flight, A/B runs and combine

This step added tools, not product code: about 3,500 lines to `xtask` (the pre-flight, two Rust-source rules and a workflow rule, `ab`, `combine`, `try`'s filters and A/B mode, `partial-landlock`, `ci-times`'s reading of artifacts), the test kit's `rerun` helper, the default bodies taken out of three traits in `cortex-tools` and `cortex-agent`, a nightly job on Ubuntu 22.04, and 49 breaks. No dependency was added. The tables were made with `cargo xtask ci-times`.

## On the pull request

Pull request 42, on `main` at `fbd69d6`. "Main" is main's own run at `fbd69d6`, which `ci-times` chose as the latest successful `ci` run on `main` that kept both reports.

| Job | Main, run 43575494327 | The PR at `590f0f5`, run 43575877543 |
|---|---|---|
| `lint-linux` | 40 s | 41 s |
| `test-linux` | 79 s | 60 s |
| `lint-windows` | 63 s | 58 s |
| `test-windows` | 136 s | 135 s |

| Time above process start, against the budget | Linux, main (3 ms) | Linux, the PR (4 ms) | Windows, main (8 ms) | Windows, the PR (7 ms) |
|---|---|---|---|---|
| Tier 1, unit (20 s) | 6.3 s | 3.4 s | 8.5 s | 8.1 s |
| Tier 2, integration (40 s) | 14.0 s | 9.5 s | 24.4 s | 24.7 s |
| Tier 3, system (60 s, 90 s on Windows) | 14.6 s | 7.5 s | 56.4 s | 55.8 s |

The Linux differences are the runner's: the PR's runner was faster at everything, as the A/A run below shows the step costs nothing measurable there. The static checks the step adds to:

- **`rules`**: 0.6 s on the desktop with the two Rust-source rules and without them, measured by taking the call out. The first form of the rules lexed every Rust file and took it to 1.1 s; only the files whose text calls a starter, or may declare a test module in a file of its own, are lexed now.
- **`breaks`**: 0.2 s on the desktop for the 643 breaks, the pre-flight included, which is what the step took for 609 before it. Its first form lexed two whole files for each break and took 1.3 s.
- **combine's tests** start git about forty times each: under 0.1 s each on Linux, and ignored on Windows, where the nightly's Windows job runs them.

## Two trees on one runner

The A/B mode, `try -f against=REF`, run on the hosted runners on 2026-11-14 (median of the rounds, each side above one start cost):

| Run | Unit | Integration | System |
|---|---|---|---|
| Linux, the branch at `1c03131` against itself, 4 rounds (run 43578388231) | +0.0 s (3.7 / 3.8) | +0.0 s (10.8 / 10.7) | −0.1 s (7.7 / 7.8) |
| Windows, the same, 4 rounds (run 43578384382) | +0.5 s (7.3 / 6.8) | +0.8 s (24.2 / 23.4) | +0.9 s (55.0 / 54.1) |
| Windows, `cortex-base` with a deliberately slow test against without it, 5 rounds (run 43578386325) | +0.0 s | **+0.4 s** | +0.0 s |

- **The run against itself is within noise.** On Linux every tier is within 0.1 s. On Windows the head is 0.5 to 0.9 s slower in each tier, inside what one side moves from round to round there (its unit tier 7.3 to 9.4 s, integration 24.5 to 27.8 s, system 53.6 to 61.5 s across four rounds); one round of the head was slow in every tier.
- **The slow test shows as itself:** "+0.402 s cortex-base::it deliberately_slow_for_the_ab_demonstration (new test)" on Windows (+0.400 s on Linux, run 43577565727), and no other test moved more than 3 ms.
- **One start cost for both sides.** The first Linux run against itself (run 43577563557) held each side to its own start cost, 5 ms and 6 ms, and showed its unit tier 0.8 s slower than itself: a millisecond over 1,315 tests. Both sides run on one machine, so they now share one, from both sides' tests together.

## What the runs found on the way

- **Every Windows `try test` run before this step ran into the Git Bash problem.** Run from `try`'s Git Bash step, with `target/try` made first by its `mkdir`, the Windows tests of a file held with no sharing failed where they pass in the blocking job: `main`'s own `try` failed `cortex-base`'s `nofollow::windows::a_file_held_without_sharing_is_reported_as_in_use` (run 43577463663), and `windows-try`, which runs in PowerShell, passed it (run 43578038486). Which of the two (the shell or the `mkdir`) was not separated. `try`'s Windows step now runs in PowerShell, keeps its log in the runner's temporary folder, and makes nothing under `target`; the three Windows and Linux runs above passed with it.
- **An A/B base on Linux** was first refused as inside the head (`std::path::absolute` keeps `..` there), and once let through, its build directory `…/repo/../ab-base/target` put a Unix socket's path past its 108 bytes. The path is read without its dots for both.
- **The step's own edit made the nightly workflow unreadable** (`run: … --ignored combine::`), which the first dispatch showed; and its first `partial-landlock` job built no confinement test, without `cortex-sandbox`'s `backends` feature.

## The nightly

The nightly workflow dispatched on the branch at `8bf1922` (run 43576703817):

- **`properties` passed,** with the real-cargo test of `mutate` that failed on 2026-11-14 (issue #40).
- **The Windows release job passed,** with combine's tests, which are ignored in the blocking Windows job.
- **`partial-landlock`** ran 66 tests of `cortex-sandbox` on Ubuntu 22.04, the confinement tests among them, and printed "Landlock 4 does not scope signals: the gap the status names": the confinement tests do not fail with the partial Landlock. One unit test of the protected-paths watch does, `protect::tests::a_change_that_puts_the_modification_time_back_is_still_found`, which the fixes to step 26 take. Until then the job is not among those the nightly's `report` job reports.

## Breaks and stress

- `cargo xtask mutate tooling-3 --jobs 4`: all 49 of the list's breaks do what it expects. The first full run flagged two (the pre-flight's nesting case and the working directory of a rerun), both missing checks in the tests, now fixed.
- `step-13a-fixes/session-store-starts-git-by-name`, which could only survive, is caught by the new session test.
- `cargo xtask stress -p cortex-testkit --count 50` on the desktop: 80 tests, every one passed every time.
