# After step 6: the tool contract and the read-only tools

Step 6 filled `cortex-tools` (about 13,200 lines, half of them tests: 208 tests on Linux and 213 on Windows) and added a module to `cortex-base`. It is the first step to bring in heavy third-party crates: the search libraries, `similar`, `tokio` with one feature, and `sha2`, which makes 50 third-party packages in a build of `cortex-tools` where `cortex-base` had 15. Only the main branch writes caches, so the pull request's runs built all of them from source. Measured on the step's pull request on 2026-11-13:

| Job | Step 4, second run | Step 6, first run | Step 6, second run |
|---|---|---|---|
| `lint-linux` | 31 s | 36 s | 37 s |
| `test-linux` | 28 s | 50 s | 37 s |
| `lint-windows` | 53 s | 51 s | 59 s |
| `test-windows` | 67 s | 77 s | 58 s |
| **Wall time, Linux** | 31 s | 50 s | 37 s |
| **Wall time, Windows** | 67 s | 77 s | 59 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (first run) | 10.8 s (clippy 10.0 s) | 21.7 s (build tests 18.9 s, run 2.8 s) | 16.5 s (clippy) | 33.8 s (build tests 29.9 s, run 3.9 s) |
| Test time against the tier budgets (first run) | | tier 1: 5.5 s of 20 s. Tier 2: 3.0 s of 40 s. Tier 3: 0.4 s of 60 s | | tier 1: 7.5 s of 20 s. Tier 2: 5.5 s of 40 s. Tier 3: 0.6 s of 90 s |
| Test time against the tier budgets (second run) | | tier 1: 4.2 s of 20 s. Tier 2: 2.7 s of 40 s. Tier 3: 0.3 s of 60 s | | tier 1: 4.7 s of 20 s. Tier 2: 3.8 s of 40 s. Tier 3: 0.4 s of 90 s |

- **Fifty new packages, built with nothing cached, cost about 10 s on Linux and 20 s on Windows.** That is the `ci` profile doing what it was added for: unoptimised, the regex engine and the walker build in seconds. Once the main branch has written caches that hold them, a pull request that leaves the lockfile alone will not build them at all. The projection further down allowed 27 to 45 s on Linux and 60 to 82 s on Windows for the whole planned set from cold; this is a fifth of that set by packages.
- **The unit tier is at a quarter to a third of its budget on Windows** (4.7 and 7.5 s of 20 s). `cortex-tools` adds 105 unit tests that take 0.1 s together on the desktop: its properties run 150 to 300 cases each, which the nightly run multiplies by fifty (the slowest then takes about 2 s on the desktop).
- **The integration tier grew from 0.5 s to 5.5 s on Windows,** which is where real files are made: about a hundred tests, each with a workspace of its own. The largest make 1,000 to 1,500 small files (the walk that is cancelled part-way, the generated tree that is searched), and one writes a 12 MB file to read three lines from its end. None passed its 2 s mark.
- **The 100,000-file search is not in any tier.** Making the tree takes longer than a test may. It is an ignored test, run by hand in a release build ([performance-budgets.md](../../performance-budgets.md)).
- **The Windows path cases ran here for the first time through the tools,** and passed on the first run: both separators, case, device names, the device and extended-length forms, trailing dots and spaces, alternate data streams, drive-relative and root-relative paths, network paths, a path over 260 characters, and a walk past two names that only an extended-length path can make. From the second run on, a test that finds it cannot make what it needs fails in CI instead of passing with nothing checked, so a green job means every case ran.
- **The clean build of `cortex-tools` alone,** on the desktop pinned to 4 cores: 13.7 to 13.8 s in the `dev` profile and 8.4 s in `release`. `cortex-base` alone: 7.7 to 7.8 s and 5.1 to 5.5 s, where it was 7.1 to 7.5 s and 4.7 to 4.8 s before `sha2`.
- **The release build** has 69 crates on Linux, 35 more than step 4's 34: the search libraries bring 26 packages, `sha2` 8, `tokio` 2 and `similar` 1, less what they share. The binary is unchanged at 0.28 MB, because nothing in it calls `cortex-tools` yet.
