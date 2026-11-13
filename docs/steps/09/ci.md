# After step 9: configuration and credentials

Step 9 filled `cortex-config` (about 10,900 lines, more than half of them tests: 123 tests) and added owner-only files to `cortex-base` (about 1,300 lines with their tests). Its third-party crates are small: the `toml` family, 6 packages, and for Windows only the Credential Manager wrapper, 5 packages, with `windows-sys`. Measured on the step's pull request on 2026-11-13. Its first run was on the base that step 4 left, so it built this step's crates from source and nothing of step 6's. Its second was after a rebase onto step 6, whose crates the main branch had cached by then:

| Job | Step 4, second run | Step 6, second run | Step 9, first run | Step 9, second run (on step 6) |
|---|---|---|---|---|
| `lint-linux` | 31 s | 37 s | 24 s | 30 s |
| `test-linux` | 28 s | 37 s | 39 s | 42 s |
| `lint-windows` | 53 s | 59 s | 50 s | 52 s |
| `test-windows` | 67 s | 58 s | 60 s | 53 s |
| **Wall time, Linux** | 31 s | 37 s | 39 s | 42 s |
| **Wall time, Windows** | 67 s | 59 s | 60 s | 53 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (first run) | 5.0 s (clippy 4.4 s) | 13.3 s (build tests 11.6 s, run 1.6 s) | 11.5 s (clippy) | 21.7 s (build tests 19.3 s, run 2.4 s) |
| The same (second run, with step 6's crates and tests) | 8.4 s (clippy 7.4 s) | 17.8 s (build tests 14.8 s, run 3.0 s) | 12.8 s (clippy) | 21.9 s (build tests 18.0 s, run 3.8 s) |
| Test time against the tier budgets (first run) | | tier 1: 4.3 s of 20 s. Tier 2: 0.7 s of 40 s. Tier 3: 0.3 s of 60 s | | tier 1: 5.1 s of 20 s. Tier 2: 1.7 s of 40 s. Tier 3: 0.5 s of 90 s |
| Test time against the tier budgets (second run) | | tier 1: 6.0 s of 20 s. Tier 2: 3.5 s of 40 s. Tier 3: 0.4 s of 60 s | | tier 1: 6.8 s of 20 s. Tier 2: 5.4 s of 40 s. Tier 3: 0.7 s of 90 s |

- **The step is inside the noise of the runners.** It added about 4 s of test build on Linux and 9 s on Windows over step 4's first run, with nothing of its own in the cache, and every job is within ten seconds of step 4's. On top of step 6, with 631 tests on the Windows job where step 6 had 421, the jobs are within five seconds of step 6's second run, and the slower OS finishes in under a minute.
- **The Windows-only code ran here for the first time anywhere,** and every test passed on the first run, and again on the second: 412 tests on the Windows job, then 631 with step 6's, none failed and none flaky. That is the access list of a private file, a private directory and an owner-only atomic write, read back with the current user alone and inheritance off; the same asked of `icacls`; a path over 260 characters; a link that is not followed; and a key through the real Credential Manager under an entry name of the test's own. The module could only be type-checked for the Windows target before this run, so two things about Windows itself were open until it: that the descriptor string is accepted as written, and that the "protected" flag survives the creation of a file. Both hold.
- **`xtask` did not grow.** The configuration's generated files are checked by a test in `cortex-config`, not by the `generated` step, so `xtask` does not build the configuration crate before every job ([testing-and-ci.md](../../testing-and-ci.md), "Lints and formatting").
- **The concurrent-writers test is the slowest of the step:** four writers, each a store of its own, making 28 flushed writes of the credentials file between them. It takes 0.9 s on the desktop with six writers and four rounds, which is why it was cut to four and three. Everything else in the crate is under 0.1 s a test.
- **The clean build of `cortex-config` alone,** on the desktop pinned to 4 cores: 8.4 to 8.6 s in the `dev` profile and 5.8 s in `release` (two runs each), against 7.1 to 7.5 s and 4.7 to 4.8 s for `cortex-base` alone in step 4. Another session's run may have been active on the desktop at the time.
- **The release build** gains 6 packages on Linux (the `toml` family) and 13 on Windows (those, the wrapper's 5, and `windows-sys` with `windows-link`).
