# After step 8: the session store

Step 8 filled `cortex-session` (about 8,800 lines, a third of them tests: 30 unit and 37 integration tests, two of them system tests that start git, and three measurements that run by hand), added the project key to `cortex-base` and a setting to `cortex-config`. It brings no new third-party crate: `serde`, `serde_json` and `thiserror` were in the build already. Measured on the step's pull request on 2026-11-13, rebased on the main branch after steps 6 and 9:

| Job | Step 9, second run | Step 8, first run | Step 8, second run |
|---|---|---|---|
| `lint-linux` | 30 s | 31 s | 39 s |
| `test-linux` | 42 s | 39 s | 46 s |
| `lint-windows` | 52 s | 50 s | 47 s |
| `test-windows` | 53 s | 72 s | 87 s |
| **Wall time, Linux** | 42 s | 39 s | 46 s |
| **Wall time, Windows** | 53 s | 72 s | 87 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (first run) | 6.8 s (clippy 5.8 s) | 18.8 s (build tests 15.3 s, run 3.5 s) | 14.9 s (clippy) | 32.7 s (build tests 26.9 s, run 5.8 s) |
| The same (second run) | 7.3 s (clippy 6.2 s) | 23.1 s (build tests 19.1 s, run 4.0 s) | 10.2 s (clippy) | 44.7 s (build tests 36.4 s, run 8.2 s) |
| Test time against the tier budgets (first run) | | tier 1: 5.4 s of 20 s. Tier 2: 5.4 s of 40 s. Tier 3: 0.5 s of 60 s | | tier 1: 8.6 s of 20 s. Tier 2: 9.9 s of 40 s. Tier 3: 2.0 s of 90 s |
| The same (second run) | | tier 1: 7.2 s of 20 s. Tier 2: 5.8 s of 40 s. Tier 3: 0.7 s of 60 s | | tier 1: 13.9 s of 20 s. Tier 2: 12.4 s of 40 s. Tier 3: 2.8 s of 90 s |

- **Every job is within a minute and a half,** the slower OS included, and the step added no package to any build.
- **One test passed its 2 s mark on Windows in the first run:** the test that cuts a log at every byte of its last entry, at 2.3 s, where it takes 0.2 s on the desktop. Most of it was deleting and making the session's directory again for each cut, which costs far more on Windows. It is now three tests, one for each kind of last entry, and keeps its directory. The second run's slow test was the test kit's held-back response test (2.0 s), which this step does not touch.
- **The unit tier on Windows varies by a half between runs** (8.6 s and 13.9 s), as in step 4. The step's own property of failed writes made a real session for each of its cases; it now runs on the fake disk alone, and takes 0.05 s on the desktop for 150 cases where 60 took 0.7 s.
- **Every Windows-specific path of the store ran here first, and passed on the first run:** the private files of a session (the log, its lock, a cut line set aside, artifacts, the checkpoint store, a fork's hard links, the index) read back with `is_private`; the lock that refuses a second writer; cutting a log back on a file opened for writing; and the base state of a real git repository whose patch applies to a fresh clone.
- **The clean build of `cortex-session` alone,** on the desktop pinned to 4 cores: 8.8 to 8.9 s in the `dev` profile and 7.1 to 7.2 s in `release` (two runs each), against 7.7 to 7.8 s and 5.1 to 5.5 s for `cortex-base` alone after step 6. The crate itself is the difference: no package is added. No other session's run was active.
