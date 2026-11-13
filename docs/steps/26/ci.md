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

- **Stress.** `cargo xtask stress -p cortex-sandbox -p cortex-tools -p cortex-core --count 50` before the review's fixes: 575 tests, seeds 0 to 49, every test passed every time. After them, with `cortex-session` too: 694 tests, every test passed every time. No flake of the signal tests of `cortex-cli` was seen in this step's runs.
- **Breaks.** 40 rules broken on purpose, one at a time ([`mutations/step-26.breaks`](../../../mutations/step-26.breaks)). All 40 are caught. On the way: one break survived because its test changed a file small enough for its content to be compared, which hid the change time (the test now keeps no content); three no longer did what their list said after the review's fixes (a break that edited a value the assembly now overwrites; a test that depended on the order of a directory listing; an undo that removed a planted link before it restored the file under it, which hid the write through the link), and each was corrected; one did not compile and was rewritten.
- **The review.** An independent review on Opus, before the first push, found eleven defects and four groups of tests that could not fail: [answered.md](answered.md).

## On the pull request

To be filled from the pull request's runs.
