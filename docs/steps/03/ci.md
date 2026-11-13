# After step 3: the protocol crate

Step 3 added the first real code (`cortex-protocol`, about 200 types) and the first third-party crates in a shipped crate: serde with its derive macro, and serde_json. `xtask` now depends on the protocol crate with its `schema` feature, which adds schemars. What a workspace build compiles went from 5 third-party packages to 17. Measured on the step's pull request on 2026-11-12, two runs, both with nothing to read from the cache for the new crates, because only the main branch writes caches:

| Job | Step 2, last cold run | Step 3, first run | Step 3, second run |
|---|---|---|---|
| `lint-linux` | 20 s | 30 s | 46 s |
| `test-linux` | 29 s | 39 s | 48 s |
| `lint-windows` | 37 s | 78 s | 85 s |
| `test-windows` | 39 s | 66 s | 94 s |
| **Wall time, Linux** | 29 s | 39 s | 48 s |
| **Wall time, Windows** | 39 s | 78 s | 94 s |

All far inside 5 minutes. The two runs differ by as much as the step added, which is the runner variance described in [testing-and-ci.md](../../testing-and-ci.md), "Where the five minutes go". Where the added time is:

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| Building `xtask` before it can run anything | 13 and 19 s | 15 and 19 s | 40 and 43 s | 25 and 39 s |
| The job's command once `xtask` runs | 4 and 7 s (clippy 4.0 and 6.4 s) | 9 and 11 s (build tests 9.1 and 10.8 s, run 0.3 and 0.4 s) | 11 s (clippy 10.5 and 11.2 s) | 15 and 24 s (build tests 14.2 and 22.9 s, run 0.7 and 1.0 s) |
| Test time against the tier budgets | | tier 1: 0.4 s of 20 s. Tier 3: 0.1 s of 60 s | | tier 1: 1.0 s of 20 s. Tier 3: 0.4 s of 90 s |

- **Most of the increase is `xtask` itself.** In step 2 it built in about 5 s on Linux and 10 s on Windows. It now compiles the procedural-macro stack (syn, serde's derive, schemars' derive) and the protocol crate before any job can start its real work, and it does so in every job. That is the price of generating the schema inside `xtask`.
- **In the test jobs that stack is built twice:** once for `xtask`, in the `dev` profile, and once for the tests, in the `ci` profile. Building `xtask` in the `ci` profile in those jobs would share it. It was not done in step 3 because it changes how the blocking jobs start on Windows, where the running `xtask` executable is locked, for a saving that only a cold cache sees.
- **A warm cache should remove nearly all of it,** because every one of these crates is then restored and not built. That can only be measured on the first pull request after step 3 is merged, when the main branch has written caches that include them. If the warm numbers are not close to step 2's, the `ci`-profile change above is the first thing to try.
- **A documentation-only pull request** still builds `xtask` to run the repo rules, so its 16 s on Linux becomes about 30 s on a cold cache.

The clean build of the protocol crate alone, measured on the desktop pinned to 4 cores as in [measurements/dependency-costs.md](../../measurements/dependency-costs.md): 7.1 s in the `dev` profile (two runs: 7.13 and 7.12 s) and 4.9 s in `release`, of which the crate itself is 0.5 s and the rest is serde's derive macro and what it depends on. With the `schema` feature: 9.2 to 9.3 s. A normal build has 11 third-party packages, and the feature adds 6.

**The non-blocking workflows' first runs**, the same day, from a scratch branch:

| Workflow | Job | Time | Of which the release build |
|---|---|---|---|
| `main` | `size-linux` (`x86_64-unknown-linux-musl`) | 53 s | 36 s |
| `main` | `size-windows` | 105 s | 81 s |
| `nightly` | `release-linux-x86_64` | 65 to 66 s | 45 to 46 s |
| `nightly` | `release-linux-arm64` (on an arm64 runner) | 54 s | 41 to 43 s |
| `nightly` | `release-windows-x86_64` | 78 to 106 s | 55 to 78 s |
| `nightly` | `dependencies` | about 30 s | advisories 17 s, unused dependencies 1 s |
| either | `report`, when something failed | 6 to 7 s | |

"The release build" includes building `xtask` first. The recorded sizes of the `dist` binary, which is still the skeleton plus the protocol crate: 393,912 bytes for Linux x86-64, 397,480 for Linux arm64, 126,976 for Windows, each with 30 crates in the build. The static Linux targets linked on the hosted runners with nothing installed beyond the Rust target; that will need another look when the first C code arrives (the script engine, step 28).

The first `nightly` run failed, usefully: the unused-dependency tool had been started in a way that made it read its own name as a directory. The `report` job opened an issue for it, and the second failure (the tool, now working, listing the skeleton's placeholder edges between workspace crates) was added to that issue as a comment. Both were fixed in the same pull request. Until the crates have code, the check ignores the workspace's own crates and looks at third-party ones only; the list is in the root `Cargo.toml`.
