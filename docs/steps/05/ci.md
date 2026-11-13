# After step 5: the provider abstraction and the test kit

Step 5 filled `cortex-llm` and the first parts of `cortex-testkit`: about 12,000 lines, more than half of them tests, 173 tests in the two crates. It brings in the largest dependency the core has, the HTTP and TLS stack: a build of `cortex-llm` has 109 packages where `cortex-base` had 15, and `ring` is the first C code in the workspace. Only the main branch writes caches, so the pull request built all of it from source. Measured on the step's pull request on 2026-11-12, on its first run, when the main branch was still at step 4:

| Job | Step 4, second run | Step 5, first run | Step 5, second run, on top of step 6 |
|---|---|---|---|
| `lint-linux` | 31 s | 62 s | 49 s |
| `test-linux` | 28 s | 51 s | 49 s |
| `lint-windows` | 53 s | 115 s | 111 s |
| `test-windows` | 67 s | 99 s | 106 s |
| **Wall time, Linux** | 31 s | 62 s | 49 s |
| **Wall time, Windows** | 67 s | 115 s | 111 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs | 28.4 s (clippy 27.3 s) | 26.7 s (build tests 25.0 s, run 1.6 s) | 68.4 s (clippy) | 56.8 s (build tests 53.5 s, run 3.3 s) |
| Test time against the tier budgets | | tier 1: 3.8 s of 20 s. Tier 2: 0.9 s of 40 s. Tier 3: 0.3 s of 60 s | | tier 1: 7.7 s of 20 s. Tier 2: 1.7 s of 40 s. Tier 3: 0.6 s of 90 s |

The second run came after the pull request was rebased onto step 6, which had merged meanwhile. The breakdown is of the first run.

- **The HTTP stack from source costs about 20 s on Linux and 45 to 55 s on Windows, in each job.** That is the whole of the increase: nearly a hundred packages, with `ring`'s C and assembly among them, where the test jobs build them unoptimised (the `ci` profile) and the lint jobs check them. The slowest job, `lint-windows`, took 1 min 55 s of its 5 minutes. Once the main branch has written caches that hold these crates, a pull request that leaves the lockfile alone will not build them.
- **This is the cold-cache case the projection further down was written for,** for about a third of the planned set by packages, and it is well inside the budget. The projection allowed 60 to 82 s on Windows for the whole set.
- **The tests themselves add almost nothing.** The 173 tests of the two crates take 1.0 s together on the desktop. Every wait in them is on the fake clock: the retry layer's backoff, a ten-minute cooldown, the 90-second idle limit. The ten that use real sockets (the HTTP client against the scripted SSE server, in-process) take turns with the server through gates, and take about 55 ms each, which is more than half of that second.
- **One case is left out on Windows,** in a test of the real transport: a connection to a port nothing listens on. Windows tries a refused loopback connection again for about two seconds before it reports it, which is longer than a test may take. The test's other cases (a server that hangs up without answering, a body cut off, a URL that is not one) run on both.
- **`lint-windows` cannot be checked ahead of CI for the HTTP stack.** The desktop can lint Windows-only code for crates with no C code, and `ring` has some. `cortex-llm` has no code of its own that is compiled only for Windows, and the test kit, which has none either, was linted for the Windows target before the first push.
- **The clean build of `cortex-llm` alone,** on the desktop pinned to 4 cores: 20.8 to 21.2 s in the `dev` profile with the `http` feature and 8.1 s without it; 12.7 s and 5.6 s in `release`. The `dev` figure is long because that profile optimises third-party crates, which is right at a desk, where they are built once.
- **The release build** has 125 crates on Linux where step 4 had 34, and 156 with what steps 6 and 9 added at the same time: `cortex-core` turns on the `http` feature, so the binary's build now has the HTTP and TLS stack. The binary is unchanged at 0.28 MB, because nothing in it calls the client yet.
- **The static Linux targets needed a C compiler, and the workflows now install one.** The `main` and `nightly` workflows build the published profile for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, and until this step no crate in it had C code. Run by hand on the step's branch, `main` failed in `size-linux`: `ring`'s build looked for `x86_64-linux-musl-gcc` and the runner image has none. `size-windows` passed, in 2 min 2 s. Both workflows now install `musl-tools` in the jobs that build a musl target, which is the note step 3 left ("that will need another look when the first C code arrives"). The run opened an issue, as a failed run should; it is closed with the fix. The script engine of step 28 will need the same compiler and finds it there.
