# After step 4: the base primitives

Step 4 filled `cortex-base`: about 10,500 lines, more than half of them tests, 173 tests on Linux and 168 on Windows, and three small third-party crates (`thiserror`, `unicode-width`, `unicode-segmentation`). It is the first pull request to read caches that the main branch wrote, so the dependencies of step 3 were restored and not built. Measured on the step's pull request on 2026-11-12:

| Job | Step 3, last run of its pull request | Step 4, first run | Step 4, second run |
|---|---|---|---|
| `lint-linux` | 43 s | 26 s | 31 s |
| `test-linux` | 37 s | 33 s | 28 s |
| `lint-windows` | 56 s | 60 s | 53 s |
| `test-windows` | 94 s | 50 s | 67 s |
| **Wall time, Linux** | 43 s | 33 s | 31 s |
| **Wall time, Windows** | 94 s | 60 s | 67 s |

The step added code and tests and the jobs did not get slower, because the warm cache took away more than the step added. That is the answer to the question step 3 left ("A warm cache should remove nearly all of it"): it does. Every run was on a different machine, so differences of ten seconds mean nothing.

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (first run) | 5.7 s (clippy 5.2 s) | 9.7 s (build tests 7.9 s, run 1.8 s) | 12.2 s (clippy) | 12.6 s (build tests 10.4 s, run 2.2 s) |
| Test time against the tier budgets (first run, second run) | | tier 1: 5.2 and 4.2 s of 20 s. Tier 2: 0.3 s of 40 s. Tier 3: 0.3 and 0.2 s of 60 s | | tier 1: 5.6 and 10.2 s of 20 s. Tier 2: 0.5 and 0.7 s of 40 s. Tier 3: 0.4 and 0.7 s of 90 s |

- **A tier's total varies by a factor of two between Windows runs,** for the same tests: 5.6 s and then 10.2 s. The total is the sum of each test's own time, and tests run four at a time, so a test's time includes what it lost to its neighbours. Run one at a time on the desktop, the 173 tests of `cortex-base` take 1.1 s together.
- **One test was over its slow mark on the second Windows run,** at 3.1 s: the framing test that tries every split of a stream into three chunks, whose work grows with the square of the stream's length. It and the every-split test of the golden transcript were cut down after that run (a shorter stream, and one kilobyte of the transcript where there were four), which took away about half of the crate's test time. The numbers above are from before the cut. The lesson for later steps: an exhaustive test over a stream should say how long the stream may be.
- **The number of property cases is the other dial** if this tier fills up. The nightly run is where the large counts belong.
- **The Windows-only tests ran here for the first time,** and every one of them passed on the first run: case, device names, alternate data streams, network paths, a directory link, a junction (the one system test the step adds), short names, a path longer than 260 characters, and the replace of a locked file. The one failure of the first run was a test of time conversion that assumed a clock finer than Windows keeps (100 ns), and was a mistake in the test.
- **The clean build of `cortex-base` alone,** on the desktop pinned to 4 cores: 7.1 to 7.5 s in the `dev` profile and 4.7 to 4.8 s in `release` (two runs each), against 7.1 s and 4.9 s for `cortex-protocol` alone in step 3. The crate itself is 0.5 s. What it adds to the protocol crate's build is `thiserror`'s derive macro (1.05 s, built beside serde's) and the two Unicode crates (0.26 s together). A normal build has 15 third-party packages, 4 more than before.
- **The release build** has 34 crates, 4 more than step 3's 30. The binary is unchanged, because nothing in it calls `cortex-base` yet.
