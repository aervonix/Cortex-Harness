# After step 26: the Linux sandbox

Step 26 built the Linux sandbox in `cortex-sandbox` (the policy and its wire form, detection, the launcher's two sides, Landlock and seccomp behind the feature `backends`, what the model is told, the look at protected paths, and the Job Object's limits on Windows) and wired it into `cortex-core`, `cortex-cli`, `cortex-policy`, `cortex-tools` and `cortex-agent`. It adds two direct dependencies, `landlock` and `seccompiler`, and four packages to a Linux build: those two, `enumflags2` and its derive.

## The kernels

Measured on 2026-11-13, with a one-job probe workflow run from a scratch branch that was deleted afterwards, and on the two desks.

| Where | Kernel | Landlock | The sandbox |
|---|---|---|---|
| Hosted `ubuntu-24.04`, x86-64 | 7.0.0-1017-azure | 8 | enforces |
| Hosted `ubuntu-24.04`, arm64 | 7.0.0-1017-azure | 8 | enforces |
| Hosted `ubuntu-22.04` | 6.8.0-1070-azure | 4 | enforces, with the gap on signals |
| The desktop (WSL 2, Ubuntu 24.04) | 6.18.58.1-microsoft-standard-WSL2 | 7 | enforces |
| The laptop | 7.0.0-42-generic | 8 | enforces |

Every one has Landlock in its active list of security modules, and seccomp filters. The blocking jobs run on `ubuntu-latest`; the tests of confinement fail there, rather than skip, if the sandbox does not enforce (`CI` set), so the image cannot change under them unseen.

## At a desk

On the desktop, through `rx`, in cargo's `ci` profile (`cargo xtask ci`), with no other `rx` run active, on main at `f24c001` with this step on top:

| Tier | Tests | Time | Budget | Used | Of which this step's tests |
|---|---|---|---|---|---|
| 1, unit | 1,164 | 7.4 s | 20 s | 25% | 26 tests, 0.14 s |
| 2, integration | 914 | 16.3 s | 40 s | 36% | 6 tests, 0.05 s |
| 3, system | 131 | 4.5 s | 60 s | 7% | 21 tests, 0.33 s |

None of this step's tests is among the five slowest of a tier. The tests that start processes through the launcher take 5 to 40 ms each.

- **Starting a command through the launcher** (release build, 200 runs, quiet machine, Landlock 7): 0.78 ms over starting it directly, 0.84 ms in a first run while a build was ending ([performance-budgets.md](../../performance-budgets.md)).
- **The look at protected paths** (release build, this repository with its build directory, 3,278 directories): 51.1 ms a look as first written, 23.6 ms once entry types were read from the listing instead of asked for one by one; a command has two ([outstanding/the-cost-of-the-look-at-protected-paths.md](../../outstanding/the-cost-of-the-look-at-protected-paths.md)).
- **The two new dependencies,** by the method of [measurements/dependency-costs.md](../../measurements/dependency-costs.md): a program using both builds in 1.6 s (dev) and 2.8 s (release) on 4 cores against 0.3 s and 1.2 s for an empty one, and is 43.9 KB larger.

## That the tests bite

- **Stress.** `cargo xtask stress -p cortex-sandbox -p cortex-tools -p cortex-core --count 50` before the review's fixes: 575 tests, seeds 0 to 49, every test passed every time. After them, with `cortex-session` too: 694 tests, every test passed every time. After the second review's fixes, the same four crates: 701 tests, every test passed every time. No flake of the signal tests of `cortex-cli` was seen in this step's runs.
- **Breaks.** 54 rules broken on purpose, one at a time ([`mutations/step-26.breaks`](../../../mutations/step-26.breaks)). All 54 are caught: 40 before the second review, 13 for its fixes, and 1 after the rebase onto the step 13a fixes (a tool's preview passed on by the guard round every tool), run again in full on the rebased branch. On the way: one break survived because its test changed a file small enough for its content to be compared, which hid the change time (the test now keeps no content); three no longer did what their list said after the review's fixes (a break that edited a value the assembly now overwrites; a test that depended on the order of a directory listing; an undo that removed a planted link before it restored the file under it, which hid the write through the link), and each was corrected; one did not compile and was rewritten.
- **The reviews.** An independent review on Opus, before the first push, found eleven defects and four groups of tests that could not fail; a second, of the fixes, found two more ways round the sandbox, a regression and five smaller defects: [answered.md](answered.md). After the second round of fixes four breaks no longer found their code (a rewritten note, the network filter, the faked call), and were updated to it.

## On the pull request

Pull request 38, on main at `f24c001`. "Main" is main's own run at that commit; "PR" is the run of `2b75746`, the first with all four jobs green.

| Job | Main | PR |
|---|---|---|
| `lint-linux` | 37 s | 41 s |
| `test-linux` | 80 s | 80 s |
| `lint-windows` | 59 s | 71 s |
| `test-windows` | 161 s | 161 s |

| | Main | PR | This step's tests in the PR run |
|---|---|---|---|
| Linux, tier 1 (unit) | 10.5 s, 4.9 s above start: 24% of 20 s | 10.3 s, 4.5 s above start: 23% | 26 tests, 0.17 s |
| Linux, tier 2 (integration) | 18.1 s, 13.5 s: 34% of 40 s | 17.6 s, 13.0 s: 32% | 6 tests, 0.09 s |
| Linux, tier 3 (system) | 10.3 s, 9.7 s: 16% of 60 s | 11.2 s, 10.5 s: 18% | 21 tests, 0.62 s |
| Windows, tier 1 (unit) | 19.7 s, 8.4 s above start: 42% of 20 s | 21.9 s, 9.3 s: 46% | 20 tests, 0.34 s |
| Windows, tier 2 (integration) | 36.1 s, 27.0 s: 67% of 40 s | 41.5 s, 31.4 s: 79% | 5 tests, 0.47 s |
| Windows, tier 3 (system) | 71.0 s, 70.0 s: 78% of 90 s | 76.5 s, 75.3 s: 84% | 2 tests, 0.22 s |

- **The blocking checks stay under five minutes on both OSes,** and every tier is inside its budget, counted above the start of the tests' processes.
- **What this step adds is about a second on each OS:** 0.9 s of test time on Linux and 1.0 s on Windows. The Windows tiers moved by more than that between main's run and this one (a test's process started in 10 ms in one and 11 ms in the other), which is the runners' speed, not the step ([testing-and-ci.md](../../testing-and-ci.md)). Windows runs none of the tests of confinement: there the sandbox is off, and its tests are of the state a session reports and of the Job Object's limits.
- **`ubuntu-latest` enforces.** Every test of confinement passed in the Linux job, where a kernel without Landlock would have failed them, so the image `ubuntu-latest` resolves to has Landlock 3 or newer; the probe of `ubuntu-24.04` found 8.
- **The first runs failed three times, each in this step's tests, none in its code:**
  - `windows-try` before the pull request: a Job Object reads its memory limit back in whole pages, so a test that compared it with the limit asked for failed by 1,024 bytes (the limit is now rounded down to 4 KiB); and the check for the harness's own files in a write root compared Windows' extended spelling of a path with the plain one, and never matched (it now compares resolved paths).
  - `lint-windows`: a test module of `cortex-session` imported, on every system, what only its Unix test uses.
  - `test-linux`: outside the sandbox, `unshare` of a user namespace answers `EINVAL` in a process with more than one thread, as the test binary's helper is on the runner; the control now asks only that the kernel does not refuse with `EPERM` by itself.

## Size and start-up

The post-merge size check (`main` workflow), run on the branch at `2b75746` and on main at `f24c001`:

| | Main | This step | Budget |
|---|---|---|---|
| Linux binary (musl, release) | 11.22 MB | 11.39 MB (+180 KB) | 16 MB |
| Windows binary | 13.06 MB | 13.25 MB (+192 KB) | 18 MB |
| Linux `cortex --version`, median | 0.63 ms | 0.50 ms | 15 ms |
| Linux headless `cortex -p`, to its request, median | 10.18 ms | 7.77 ms | 40 ms |
| Windows `cortex --version`, median | 6.69 ms | 9.19 ms | 50 ms |
| Windows headless `cortex -p`, to its request, median | 24.34 ms | 35.50 ms | 100 ms |

- **The size is the two backends' code** (`landlock`, `seccompiler` and `enumflags2`, Linux only) and this step's own; on Windows, the Job Object limits and the sandbox's state.
- **Start-up is not this step's to move:** nothing it adds runs before the first request but the detection of Landlock (one system call, on Linux) and the state's computation. Both OSes moved by more than that between the two runs, in opposite directions, and on Windows `--version`, which runs none of it, moved as much as the headless run: the runners' speed.
