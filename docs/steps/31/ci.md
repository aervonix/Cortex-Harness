# After step 31: the OpenAI-compatible protocol

Step 31 added the `openai-chat` protocol to `cortex-llm` (about 4,300 lines of source with their unit tests, and 7,100 of integration tests: 20 unit, 120 integration and 2 system tests, and one more per provider that is run by hand), 69 fixtures and 76 snapshots, a fuzz target, and the scrubbing of identifiers by name and by shape to `cortex-testkit`. It adds no package to the workspace. Measured on the step's pull request on 2026-11-13, on main with steps 9, 10, 12 and 14 in it. "Main" is main's own run at the last commit before the step that changed code; step 14's column is its third run, the one after its tests were cut down, from [its record](../14/ci.md):

| Job | Step 14, third run | Main | Step 31, first run |
|---|---|---|---|
| `lint-linux` | 36 s | 43 s | 43 s |
| `test-linux` | 48 s | 58 s | 55 s |
| `lint-windows` | 59 s | 75 s | 65 s |
| `test-windows` | 91 s | 96 s | 94 s |
| **Wall time, Linux** | 48 s | 58 s | 55 s |
| **Wall time, Windows** | 91 s | 96 s | 94 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs, main | 12.4 s (clippy) | 29.2 s (build tests 22.6 s, run 6.6 s) | 21.1 s (clippy) | 54.5 s (build tests 34.9 s, run 19.6 s) |
| The same, step 31 | 13.5 s (clippy) | 32.8 s (build tests 24.4 s, run 8.4 s) | 22.2 s (clippy) | 55.9 s (build tests 35.7 s, run 20.2 s) |
| Test time against the tier budgets, main | | tier 1: 7.7 s of 20 s. Tier 2: 10.9 s of 40 s. Tier 3: 5.7 s of 60 s | | tier 1: 12.4 s of 20 s. Tier 2: 17.8 s of 40 s. Tier 3: 27.1 s of 90 s |
| The same, step 31 | | tier 1: 8.1 s of 20 s. Tier 2: 17.8 s of 40 s. Tier 3: 5.0 s of 60 s | | tier 1: 10.7 s of 20 s. Tier 2: 21.9 s of 40 s. Tier 3: 26.8 s of 90 s |

- **Every job passed on its first run, on both OSes.** The step has no code that is compiled only for Windows. What ran there for the first time is its fixtures as checked out on Windows, its snapshots, and the client's tests of a server that sends only keep-alives, which use a local socket and paused time.
- **The integration tier grew by 6.9 s on Linux and 4.1 s on Windows,** to 44% and 55% of its budget. It is the same family of tests that put step 14 over: each stream delivered in pieces and cut in two. This step measured them in CI's own profile before the push, as step 14's record asks, and bounded them before they were ever pushed: one test for each server, a sample of cuts that shrinks as a stream grows, and only the first 16,000 bytes of a long stream a byte at a time. In CI's profile on the desktop the protocol's 119 integration tests take 4.8 s of test time between them, against 3.3 s for the Anthropic protocol's. The nightly run cuts every stream at every byte.
- **The jobs' wall time did not grow.** The test build is 1 to 2 s longer and the run 1 to 2 s longer; the difference between two runs of the same commit is larger than that.
- **Breaking the code fails the tests.** Eighty-seven rules of the step were each broken on purpose, one at a time, in a copy, and the two crates' tests run. Every break failed at least one test. Two were not caught when first tried and got tests: a second stop reason that replaces the first, and a made-up call id that is the same for every call. A third, keep-alives waited on for ever, made its test hang instead of failing; the test now fails at once.
- **The fuzz target** `openai_chat_decoder`, on the desktop: 281,585 inputs in 91 s, 3,094 a second, no failing input. The nightly workflow runs it with the others.
- **The tests that use a socket or the clock** were run 200 times in a row on the desktop before the push, and passed each time.
- **Not measured:** a clean build of `cortex-llm` alone. Three other sessions were building on the desktop throughout, and a number taken there would say more about them than about this step.
