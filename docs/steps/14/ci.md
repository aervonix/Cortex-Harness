# After step 14: the Anthropic protocol

Step 14 added the `anthropic-messages` protocol to `cortex-llm` (about 2,900 lines of source with their unit tests, and 5,100 of integration tests: 18 unit and 84 integration tests of its own, four of which are run by hand), the fixture format and scrubber to `cortex-testkit`, 32 fixtures and 41 snapshots. It adds no package to the workspace: `serde_json` gains a feature. Measured on the step's pull request on 2026-11-13, on the base that step 11 left. Step 10 merged after these three runs; the branch was then rebased onto it, and that run's times are in the step's report:

| Job | Step 11, first run | Step 14, first run | Step 14, second run | Step 14, third run |
|---|---|---|---|---|
| `lint-linux` | 32 s | 41 s | 29 s | 36 s |
| `test-linux` | 52 s | 68 s | 49 s | 48 s |
| `lint-windows` | 55 s | 65 s | 58 s | 59 s |
| `test-windows` | 105 s | 134 s | 139 s | 91 s |
| **Wall time, Linux** | 52 s | 68 s | 49 s | 48 s |
| **Wall time, Windows** | 105 s | 134 s | 139 s | 91 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (second run) | 9.1 s (clippy 7.9 s) | 26.7 s (build tests 17.5 s, run 9.2 s) | 17.5 s (clippy) | 84.6 s (build tests 51.3 s, run 33.3 s) |
| The same (third run) | 11.9 s (clippy 10.3 s) | 25.1 s (build tests 21.0 s, run 4.0 s) | 20.2 s (clippy) | 51.1 s (build tests 32.2 s, run 18.9 s) |
| Test time against the tier budgets (second run) | | tier 1: 4.7 s of 20 s. Tier 2: 28.6 s of 40 s. Tier 3: 1.8 s of 60 s | | tier 1: 13.6 s of 20 s. Tier 2: **51.6 s of 40 s, a warning.** Tier 3: 38.0 s of 90 s |
| The same (third run) | | tier 1: 5.0 s of 20 s. Tier 2: 7.9 s of 40 s. Tier 3: 1.7 s of 60 s | | tier 1: 10.4 s of 20 s. Tier 2: 14.5 s of 40 s. Tier 3: 28.3 s of 90 s |

- **The step's first two runs put the integration tier over its budget on Windows,** and near it on Linux: 51.6 s of 40 s, and 28.6 s, where step 11 had left 11.4 s and 6.4 s. It was one family of tests: the decoder's fixtures, each cut in two at every byte. That is 200 MB of decoding, and CI's test build does not optimise the JSON reader, which is where the time goes. On the desktop, whose ordinary build does optimise it, the same tests took 0.7 s of wall time, so nothing looked wrong before the push. The lesson of step 4, again, with a second half: **a test whose work grows with the square of its input is measured in CI's own profile before it is pushed** (`cargo nextest run --cargo-profile ci`).
- **The third run is after the cut** ([testing-and-ci.md](../../testing-and-ci.md), "Replays at every chunk boundary"): on a pull request each stream is delivered a byte at a time and cut in two at a sample of places, and the nightly run cuts every stream at every byte. The integration tier is back at 7.9 s on Linux and 14.5 s on Windows, a second or three above step 11.
- **Every job passed on its first run, on both OSes.** The step has no code of its own that is compiled only for Windows. What ran there for the first time is the fixtures as checked out on Windows (kept byte for byte by `.gitattributes`), the snapshots, and the fixture files written to and read back from the system's temporary directory.
- **Build time is as before.** A clean build of `cortex-llm` alone, on the desktop pinned to 4 cores: 20.9 s in the `dev` profile with the `http` feature and 9.2 s without it; 13.8 s and 6.3 s in `release`. After step 5: 20.8 to 21.2 s and 8.1 s; 12.7 s and 5.6 s. The crate grew by about a second, all of it the crate's own code. One run each; three sessions share the desktop, and whether another was building at the time is not known.
- **Breaking the code fails the tests.** Seventy-four rules of the step were each broken on purpose, one at a time, and the two crates' tests run. Seventy-three breaks failed at least one test. The other removes a check that the next line makes again, and changes nothing that can be observed. Three of the first sixty-one were not caught at first, and got tests: the lookback at exactly one position over, another protocol under the same provider's name, and a rate limit for a tier that has no limit of its own.
- **The nightly workflow, run by hand on the branch.** Its first run failed in the new `fuzz` job, which could not build: the prebuilt `cargo-fuzz` is a static binary, `cargo fuzz` builds for the target it was itself built for, and the sanitizer cannot use that target. `cargo xtask fuzz` now names the compiler's own target. The run opened an issue, as a failed run should, and it is closed with the fix. The second run passed:

| Job | Time | |
|---|---|---|
| `fuzz` | 5 min 55 s | Two minutes for each target, after building them. `sse_parser`: 238,853 inputs, 1,973 a second. `anthropic_decoder`: 191,095 inputs, 1,579 a second, 3,367 coverage points reached. Neither found a failing input. |
| `properties` | 1 min 56 s | Before the full every-byte replays were moved into it. |
| `release-linux-x86_64`, `release-linux-arm64`, `release-windows-x86_64` | 1 min 33 s, 1 min 34 s, 2 min 55 s | |
| `dependencies` | 36 s | |
