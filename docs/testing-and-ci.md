# Testing and CI

The test suite and CI are designed for speed from the first commit. The rule that shapes everything here: **the blocking pull-request checks finish in under 5 minutes per OS on GitHub-hosted runners.** Anything slower runs in a tier that does not block.

## Test tiers

| Tier | What | Where it runs | Blocking? | Time budget |
|---|---|---|---|---|
| **0. Static** | Formatting, lints, licence and source checks, generated-file freshness, repo rules (no home-directory paths, dependency list in sync, crate dependency rules). | Every pull request. | Yes | Under 2 min on a warm cache. |
| **1. Unit** | Pure logic inside each crate: parsers, decoders, mappers, policy evaluation, text layout, view functions. Property tests. Snapshot tests. | Every pull request, both OSes. | Yes | The whole tier in under 20 s of test time. |
| **2. Integration** | Several crates together, in-process, with the scripted provider, fake tools or real tools on a temporary workspace, the fake clock and the virtual terminal. The agent loop, sessions, context, permissions, the RPC conformance scenarios, the TUI against the virtual terminal. | Every pull request, both OSes. | Yes | Under 40 s of test time. |
| **3. System** | The real binary as a process: headless runs against the scripted SSE server, the TUI under a pseudo-terminal, process-tree and sandbox behaviour, extension scripts and MCP fake servers. | Every pull request, both OSes. | Yes | Under 60 s of test time on Linux, 90 s on Windows. |
| **4. Performance** | The budgets in [performance-budgets.md](performance-budgets.md). | Main branch after merge, nightly, and on request. | No | 10 min. |
| **5. Live** | Smoke tests against real providers. Fixture recording. | By hand, and nightly where a key is configured. Never on pull requests. | No | |
| **6. Soak** | Fuzzing of decoders and parsers, long scripted sessions for memory growth, release builds for every target. | Nightly. | No | |

Tiers 1 to 3 are separated by where the test lives and by nextest filters, not by cargo features:

- unit tests are `#[cfg(test)]` modules in the crate;
- integration tests are one test binary per crate, `tests/it/main.rs`, with modules inside it;
- system tests live in `cortex-cli/tests/system/` and in `cortex-testkit`-driven binaries, and their names start with `sys_`.

**One integration binary per crate.** Every extra test binary is another link step, and linking is the slow part on Windows. Doc tests are turned off (`doctest = false`); examples that must compile are ordinary tests.

**No real waiting in tests.** This is rule 12 of the [build plan](build-plan.md).

- Timeouts, retries, backoff, cooldowns, debouncing and anything else that depends on time passing are tested with time that is paused or simulated: the async runtime's test clock, which jumps to the next timer as soon as every task is idle, or the fake clock for code that takes time from the clock trait in `cortex-base`. A test of a 30-second timeout takes microseconds. The fake clock is defined in `cortex-base`, beside the trait, because that crate tests itself with it and cannot depend on the test kit; the test kit re-exports it. Its `sleep` returns at once, moves its time forward and records what was asked, so blocking code that waits and retries (the replace of a locked file) is tested for how long it would have waited.
- Production code never reads the system clock or sleeps directly. It goes through the runtime's timers or the clock trait, which is what makes the rule possible to follow.
- A test never calls `std::thread::sleep`. Clippy's disallowed-methods list bans it everywhere, tests included.
- Tier 3 cannot pause time, because the code under test is another process. It waits on a condition with a deadline (output seen, file present, process gone), never on a fixed delay, and the helper for that is in `cortex-testkit`.

**Budgets are enforced.** This is rule 13 of the build plan.

| Tier | A test is reported as slow at | A test is stopped, and fails, at | The tier's tests together |
|---|---|---|---|
| 1. Unit | 2 s | 10 s | 20 s |
| 2. Integration | 2 s | 10 s | 40 s |
| 3. System | 10 s | 60 s | 60 s on Linux, 90 s on Windows |

- The limits on one test are nextest's `slow-timeout` and `terminate-after`, set per tier in `.config/nextest.toml`. They hold in every profile, at a desk as in CI.
- The tier totals are the sum of the tests' own times, not the wall time of the run. `cargo xtask ci test` reads them from nextest's JUnit report after the run, prints them with the share of the budget used, and writes them to the CI job's summary page.
- **A tier over its budget is a warning; a tier over one and a half times its budget is a failure.** Both are annotations on the pull request. The gap is there because hosted runners vary by tens of percent from one run to the next, and more on Windows (the same build took 60 to 82 s on four Windows runners in step 2). A failure at exactly the budget would turn a tier that is near its limit into red builds at random, which teaches people to re-run instead of to fix. The warning is the signal to act; the failure is for a tier that is over on any runner. The limits on a single test stay hard failures: they are several times what a healthy test takes.
- A test over its slow mark needs a reason or moves to tier 6.

**Property tests** use no property-testing crate. The pattern was set in step 4 (`crates/cortex-base/src/cases.rs`), and steps 6, 10 and 12 follow it. The test kit takes the helper over when it is built, so that every crate uses one copy.

- A property is a function from a generated input to "fine" or "what was wrong". A generator is a function from a random-number generator to an input. `check(name, cases, generate, property)` runs the property on that many inputs.
- **Every run is the same.** Each case draws from the seeded generator of `cortex-base` with a seed made from the property's name and the case number. There is no randomness between runs or machines, so a property test cannot be flaky.
- **A failure prints the property, the seed, the case number and the input.** `CORTEX_PROPERTY_SEED=<seed>` runs that one case again, alone.
- **`CORTEX_PROPERTY_SCALE=<n>` multiplies every property's number of cases.** Pull requests run at 1, which keeps the properties of one crate to well under a second of the unit tier's 20. The `nightly` workflow runs tiers 1 and 2 at 50. The first time this was tried it found a case the default count had not (a wrong expectation in a wrapping test), which is the point of it. The limit on one test still holds at any scale: at 50 the slowest property of step 4 takes 1.8 s on the desktop, and at 300 it passes the 10 s limit.
- **There is no shrinking,** so generators prefer small inputs, and an input is printed whole. In exchange the helper is 60 lines, adds no dependency and nothing to the build.
- **Small spaces are checked exhaustively** in addition, where that is possible: every sequence of up to six path steps from a small alphabet, every split of a byte stream into two and into three chunks.

What was weighed against it: proptest, which shrinks failing inputs and has a library of generators, and which would add about fifteen packages to every crate's test build, on both OSes, for the life of the project.

## Deterministic agent tests without a network

`cortex-testkit` provides three levels of fake, one for each kind of test.

| Fake | Replaces | Used for |
|---|---|---|
| **Scripted provider** | `ModelClient`. A script of model events with optional delays, errors, stalls and aborts, plus assertions on the requests it received. | Tier 2: every loop behaviour. Runs in microseconds. |
| **Scripted transport** | `Transport`. Returns recorded response bodies in chosen chunk sizes. | Tier 1: protocol adapters, from fixtures. |
| **Scripted SSE server** | The provider's HTTP endpoint. A local listener that writes scripted chunks with controlled timing, stalls and disconnects. | Tier 3: the real HTTP client, timeouts, cancellation, and the real binary end to end. |

**Fixtures.** Recorded provider responses live in the repo under each protocol's `fixtures/` directory: the raw response stream, the request that produced it, and a note. `xtask record` captures them from a real provider by hand, scrubs credentials and identifiers, and is never run in CI. Each provider step adds fixtures for every case in its checklist ([build-plan.md](build-plan.md)).

**Replays at every chunk boundary.** Stream decoders are tested by replaying each fixture split at every byte offset, which catches the bugs that only appear when a network read ends in the middle of a token.

**Golden transcripts.** Sequences of protocol messages from released versions are kept and replayed against current types, to catch accidental breaking changes to the RPC protocol. The first one, written by hand in step 3, is `crates/cortex-protocol/tests/golden/v1-session.jsonl`: every method, every event type and every kind of item, part, delta, notice and plan edit of protocol version 1. Three tests use it: each line must read into its typed form and write back as the same JSON; the same must hold after a field no version knows is added to every object; and each line must be valid against the generated schema ([architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md)). A transcript is not edited to make a test pass. The types that travel in no message have golden values in a second file beside it, and a test fails, naming the type, when a type in the schema has a value in neither. An addition to a released version gets a new golden file.

**No paid API in CI.** Pull-request jobs have no secrets. The scripted fakes and fixtures cover everything that blocks.

**Usage records from other tools.** The adapters in [architecture/18-usage.md](architecture/18-usage.md) are tested only against small, made-up example files written for the purpose: one set per format version an adapter supports, plus damaged and unknown-version examples. Real session data from any tool is never used as a fixture and never committed. The static check that rejects home-directory paths covers the fixture folders, and an adapter test asserts that no text from the input reaches the output.

## Testing the TUI

Three levels, matching the tiers.

| Level | What is tested | How |
|---|---|---|
| **View functions** (tier 1) | That state renders to the right lines. | Pure functions from state, width and theme to styled lines. Snapshots as text with inline style markers, at 60, 80 and 120 columns, in both glyph sets, with and without colour. |
| **Presenters** (tier 2) | That frames reach the terminal correctly. | The presenter writes into a virtual terminal (a terminal-state parser from the test kit). Tests assert the resulting screen and scrollback, and the **no-flicker properties** for every frame: one write, inside a synchronized update, no clear-then-redraw, nothing above the live region touched, bytes per frame under a cap. |
| **The real binary** (tier 3) | That it all works in a terminal. | The binary runs under a pseudo-terminal (ConPTY on Windows), driven by scripted keys, with the scripted provider. Tests assert on the parsed screen, on terminal restoration after exit, panic and signals, and on input latency staying under a generous limit. |

The no-flicker properties and the latency checks use the method developed for the TUI language comparison ([measurements/tui-language-spike.md](measurements/tui-language-spike.md)).

Snapshot changes are reviewed like code. A change in a view snapshot is a change in what users see.

## Windows coverage

Every row is a test, or a group of tests, that runs on the Windows job.

| Area | What is checked |
|---|---|
| Paths | Both separators accepted; case-insensitive comparison; device names and UNC paths rejected; long paths; `~` expansion. Built in step 4 (`crates/cortex-base/tests/it/paths_windows.rs`), with these further cases: trailing dots and spaces and alternate data streams rejected; drive-relative and root-relative paths rejected; a directory link and a junction that leave the workspace resolve to their targets; a link to a network share is refused without the share being contacted; a short (8.3) name resolves to the same path as the long one. The junction is made by `mklink`, so that test is a system test. No test reaches for a share, not even the machine's own: that an allowed share passes is tested on the text. |
| Shells | PowerShell 7, Windows PowerShell and `cmd.exe`: quoting, merged output, exit status, the model being told which shell it has. Git Bash where the runner has it. |
| Line endings | CRLF and mixed files survive `edit` and `write` byte for byte outside the edited lines; `.gitattributes` keeps fixtures exact. |
| Process trees | A command with a grandchild is fully gone after cancellation, after timeout, and after the harness is killed (Job Objects). |
| Files in use | Replacing a file another process holds open retries and then fails cleanly. Built in step 4 (`crates/cortex-base/tests/it/atomic.rs`): a file held with no sharing fails after seven attempts and half a second of waiting, all of it on the fake clock, with the file unchanged and no temporary file left; a file let go during the retries is replaced. |
| Terminal | ConPTY end-to-end tests of the TUI: key release events ignored, new-line keys, resize, restoration. |
| Script shims | A `.cmd`-shim MCP server and hook command start with arguments intact; unsafe arguments are refused. |
| Permissions on files | Session, credential and state files are readable only by the user. |
| Configuration | Files with a byte-order mark and CRLF parse. |
| Extension scripts | The script engine builds and runs, and a script's host calls are denied exactly as on Linux. |
| Command names | The binary behaves identically as `cortex.exe` and `cx.exe`. The install script creates the alias as a hard link, falls back to a copy, and never overwrites another program's `cx`. |

What CI cannot check is how a real terminal window looks. That is the manual checklist in [measurements/windows-terminal-checklist.md](measurements/windows-terminal-checklist.md), run by the owner at the points the build plan names.

## CI layout

### Blocking, on every pull request

Four jobs in parallel, in `.github/workflows/ci.yml`. Wall time is the slowest job.

| Job | Runner | Runs |
|---|---|---|
| `lint-linux` | `ubuntu-latest` | `cargo xtask check`: `cargo fmt --all --check`; the repo rules; the generated files (the protocol's schema and TypeScript declarations are what the types generate); `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo deny check licenses bans sources`. |
| `lint-windows` | `windows-latest` | `cargo xtask check clippy`. Formatting, the repo rules and cargo-deny give the same answer on every OS and run once. Clippy runs on both, because code compiled only for Windows is otherwise never linted. |
| `test-linux` | `ubuntu-latest` | `cargo xtask ci test`: build the tests, run tiers 1 to 3 (`cargo nextest run --workspace --locked --cargo-profile ci --profile ci`), check the tier budgets. |
| `test-windows` | `windows-latest` | The same. |

The first draft of this page had three jobs. `lint-windows` was added in step 2: it runs beside the others, so it adds nothing to the wall time, and it closes a gap that would have opened as soon as the first Windows-only module was written.

**The jobs call `cargo xtask`.** What a job runs is defined in the `xtask` crate and nowhere else, so `cargo xtask ci` at a desk runs what CI runs. Every step of a command runs even when an earlier one failed, so one CI round trip reports everything that is wrong. Each step is timed, and the timings go to the log and to the job's summary page.

Settings common to all jobs:

- `--locked` and `CARGO_INCREMENTAL=0`.
- Each job has `timeout-minutes: 5`. That makes the budget a fact: a job that takes longer fails.
- The toolchain version comes from `rust-toolchain.toml` and is installed with `rustup toolchain install`. The version is pinned exactly, so a new stable release never changes the lints under a pull request. Installing it costs 8 to 9 s in every job, cache or no cache (11 to 13 s on Windows before `RUSTUP_HOME` was moved off the system disk).
- **Tests build in cargo's `ci` profile:** `dev`, with third-party crates unoptimised. The reason is in "Where the five minutes go". Our own crates keep line tables, so a failing test's backtrace has line numbers.
- Dependency caching with `Swatinem/rust-cache`. The key is made of the job, the OS, the toolchain version, the `CARGO_*` and `RUST*` environment, and a hash of `Cargo.lock`, every `Cargo.toml`, `rust-toolchain.toml` and `.cargo/config.toml`. When that hash has no exact match, the newest cache for the same job, OS and toolchain is restored instead, so **a lockfile change rebuilds the changed crates and what depends on them, not everything**. Only a toolchain bump, or caches that have expired, give a truly cold run.
- Only the main branch writes the cache, so pull requests read a cache that is always consistent and never push each other's out of the repository's 10 GB. A scheduled run of the same workflow, twice a week, keeps the caches from expiring.
- On Windows, `CARGO_HOME` and `RUSTUP_HOME` point at a folder on the workspace's disk, not at their default place on the system disk, which is several times slower at unpacking files (measured below).
- `cargo-nextest` and `cargo-deny` are installed as prebuilt, checksummed binaries, not compiled.
- A new push to a pull request cancels the run in progress.
- Actions are pinned by commit hash. The workflow token has read-only permissions. No secret is used. `cargo xtask rules` checks all three.

**Documentation-only pull requests skip the Rust build and tests.** Each job starts with a step (`.github/actions/changes`) that lists what the pull request changes. If every changed file is under `docs/` or ends in `.md`, the job's remaining steps are skipped and the job still reports success under its own name.

- The decision is made inside each job, not in a separate job that the others wait for. All four checks therefore always report, so they can be required by branch protection as they are, with no gate job, and a pull request that does touch code pays about a second for the question.
- **The repo rules still run on a documentation-only pull request, deliberately.** The check for home-directory paths and the check of `docs/dependencies.md` exist mostly for documentation, and skipping them on exactly the pull requests that write prose would open the hole they close. This compiles `xtask`, which is five small crates, and nothing else.
- Anything else in the change set (a manifest, a workflow, an asset, a source file) runs everything. So does every push to the main branch and every scheduled or manual run.
- **The generated protocol files under `docs/protocol/` count as code.** They are checked against the Rust types by a test, so a pull request that touches them runs the tests, wherever else its changes are.

**nextest configuration** (`.config/nextest.toml`):

- the per-test limits of each tier, from the table under "Test tiers", in every profile;
- profile `ci`: fail fast off, quiet on success, JUnit output for the tier budgets;
- a test group that limits pseudo-terminal and process tests (everything named `sys_…`) to two at a time, so they do not starve each other on a 4-core runner;
- no retries, except tests in a module named `timing_sensitive`, which get one retry and are reported as flaky when it is used;
- profile `fast` for local use: tiers 1 and 2 only.

### Where the five minutes go

Everything in this section was measured on GitHub-hosted runners on 2026-11-12, in step 2. Both runner types report 4 cores. Times are from the job and step timestamps the API reports, to the second, and from timers in the measurement scripts.

**Hosted runners vary.** The same clean build took 27, 29, 42 and 45 s on four Linux runners and 60, 75, 77 and 82 s on four Windows runners on the same afternoon. Read every number here as "about", and compare two settings only when they were measured in the same job on the same machine, which is how the comparisons below were made unless they say otherwise.

#### The skeleton, as merged in step 2

The workspace has 21 crates with no code to speak of, 22 test binaries and five small third-party crates. Wall time per OS is the slower of that OS's two jobs.

| Case | Linux | Windows |
|---|---|---|
| Cold cache: the pull request's own runs, which have no cache to read | 20 to 29 s | 37 to 39 s. Before cargo's and rustup's files were moved off `C:` (below): 47 to 59 s, and 74 s once |
| Warm cache: a scratch branch running the same jobs with cache writing on, second run | 20 s | 42 s, measured before that move |
| Documentation-only pull request | 16 s | 14 s |

All four cases are far inside 5 minutes, and cold and warm are the same within noise, because there is almost nothing to cache yet. The warm case can only be measured on the real workflow once the main branch has written its first caches, which happens when step 2 is merged; the first pull request after that is the check.

The job breakdown, for the last cold run of step 2's pull request:

| Phase | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| Start the job, check out, ask what changed | 2 s | 4 s | 9 s | 9 s |
| Install the pinned toolchain | 8 s | 14 s | 9 s | 8 s |
| Look for a cache (there is none yet) | 0 s | 1 s | 2 s | 3 s |
| Install cargo-deny or cargo-nextest | 1 s | 1 s | not needed | 2 s |
| Build `xtask`, then run the job's command | 5 s | 6 s | 11 s | 14 s |
| of which the command's own steps | fmt 0.1 s, rules 0.1 s, clippy 1.9 s, deny 0.3 s | build tests 2.7 s, run tests 0.3 s | clippy 3.8 s | build tests 4.2 s, run tests 0.6 s |
| Finish the job | 1 s | 1 s | 3 s | 1 s |
| **Job total** | **20 s** | **29 s** | **37 s** | **39 s** |

The steps are rounded to the second, so a column does not add up exactly to its total. The toolchain install in `test-linux` took 14 s in this run and 8 to 9 s in the others.

Most of a skeleton job is fixed cost: starting the job, checking out, installing the pinned toolchain and looking for a cache. That fixed cost is about 15 s on Linux and 20 to 25 s on Windows, and it is the floor under every future run.

#### After step 3: the protocol crate

Step 3 added the first real code (`cortex-protocol`, about 200 types) and the first third-party crates in a shipped crate: serde with its derive macro, and serde_json. `xtask` now depends on the protocol crate with its `schema` feature, which adds schemars. What a workspace build compiles went from 5 third-party packages to 17. Measured on the step's pull request on 2026-11-12, two runs, both with nothing to read from the cache for the new crates, because only the main branch writes caches:

| Job | Step 2, last cold run | Step 3, first run | Step 3, second run |
|---|---|---|---|
| `lint-linux` | 20 s | 30 s | 46 s |
| `test-linux` | 29 s | 39 s | 48 s |
| `lint-windows` | 37 s | 78 s | 85 s |
| `test-windows` | 39 s | 66 s | 94 s |
| **Wall time, Linux** | 29 s | 39 s | 48 s |
| **Wall time, Windows** | 39 s | 78 s | 94 s |

All far inside 5 minutes. The two runs differ by as much as the step added, which is the runner variance described above. Where the added time is:

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| Building `xtask` before it can run anything | 13 and 19 s | 15 and 19 s | 40 and 43 s | 25 and 39 s |
| The job's command once `xtask` runs | 4 and 7 s (clippy 4.0 and 6.4 s) | 9 and 11 s (build tests 9.1 and 10.8 s, run 0.3 and 0.4 s) | 11 s (clippy 10.5 and 11.2 s) | 15 and 24 s (build tests 14.2 and 22.9 s, run 0.7 and 1.0 s) |
| Test time against the tier budgets | | tier 1: 0.4 s of 20 s. Tier 3: 0.1 s of 60 s | | tier 1: 1.0 s of 20 s. Tier 3: 0.4 s of 90 s |

- **Most of the increase is `xtask` itself.** In step 2 it built in about 5 s on Linux and 10 s on Windows. It now compiles the procedural-macro stack (syn, serde's derive, schemars' derive) and the protocol crate before any job can start its real work, and it does so in every job. That is the price of generating the schema inside `xtask`.
- **In the test jobs that stack is built twice:** once for `xtask`, in the `dev` profile, and once for the tests, in the `ci` profile. Building `xtask` in the `ci` profile in those jobs would share it. It was not done in step 3 because it changes how the blocking jobs start on Windows, where the running `xtask` executable is locked, for a saving that only a cold cache sees.
- **A warm cache should remove nearly all of it,** because every one of these crates is then restored and not built. That can only be measured on the first pull request after step 3 is merged, when the main branch has written caches that include them. If the warm numbers are not close to step 2's, the `ci`-profile change above is the first thing to try.
- **A documentation-only pull request** still builds `xtask` to run the repo rules, so its 16 s on Linux becomes about 30 s on a cold cache.

The clean build of the protocol crate alone, measured on the desktop pinned to 4 cores as in [measurements/dependency-costs.md](measurements/dependency-costs.md): 7.1 s in the `dev` profile (two runs: 7.13 and 7.12 s) and 4.9 s in `release`, of which the crate itself is 0.5 s and the rest is serde's derive macro and what it depends on. With the `schema` feature: 9.2 to 9.3 s. A normal build has 11 third-party packages, and the feature adds 6.

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

#### After step 4: the base primitives

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

#### The planned third-party set

The skeleton cannot show what a cold cache will cost once the dependencies arrive, which is the risk this page was written around. So step 2 also built a throwaway program that depends on the whole third-party set planned for the first release (the async runtime, the HTTP and TLS stack, serde, the terminal library, the search libraries, the MCP SDK, the highlighter, the script engine, the test-kit libraries, and the Windows-only and Linux-only crates: 283 packages in its lockfile) on both runners. The program was on a scratch branch and is not in the repository. It uses each library enough to link it.

| Measurement | Linux | Windows |
|---|---|---|
| Fetch the sources (download and unpack) | 2 to 3 s | 26 to 37 s |
| Clean build, dependencies at `opt-level = 1` (the `dev` profile) | 129 s | 259 s |
| Clean build, dependencies at `opt-level = 0` (the `ci` profile) | 27 to 45 s | 60 to 82 s |
| Relink the one binary after touching its source | 0.4 to 0.5 s | 1.2 to 1.9 s |
| Build directory | 0.5 to 0.6 GB | 0.65 to 0.72 GB |
| The cache `rust-cache` stores for it | 209 MB | 220 MB |
| Restore that cache | 3 to 4 s | 26 s |
| Unpack the sources again after a restore (the cache holds the `.crate` files, not the unpacked trees) | 2 s | 18 s |
| Build after a restore | under 1 s | 5 s |

What follows from it:

- **Optimised dependencies do not fit a cold Windows run.** With the `dev` profile's `opt-level = 1`, Windows spends about 290 s fetching and building dependencies before it compiles a line of ours. Unoptimised, it spends about 110 s. That is why the test jobs build in a profile of their own, `ci`. The `dev` profile is unchanged, because at a desk the dependencies are built once and kept.
- **Line tables are nearly free.** In one job per OS, with dependencies unoptimised: no debug info anywhere, 29 to 30 s on Linux and 75 to 82 s on Windows; line tables for the top crate only, 29 s and 75 s; line tables for every crate, which is more than `ci` asks for, 32 s and 83 s. Relink times did not change. So `ci` keeps line tables for our own crates.
- **The faster linker does not pay on Windows.** With `rust-lld` in place of the default linker, in the same job: a clean build took 274 s against 259 s, and a relink 1.3 s against 1.5 to 1.7 s. For the skeleton's 22 test binaries, 2.9 s against 3.8 s. Linking is not where the time goes, so the default linker stays. (Mitigation 7 below is closed.)
- **The script engine builds on Windows.** QuickJS compiled with the MSVC toolchain on the hosted runner and ran a script. Step 28 still confirms it in the real crate.
- **A warm Windows run paid about 45 s before it built anything of ours:** 26 s to restore the cache and 18 s to unpack the sources. The second figure is what the next section cuts to 9 s.

#### Where cargo's files live on a Windows runner

A hosted Windows runner has two disks. The workspace is checked out on `D:`, and cargo's home directory, where crate sources are downloaded and unpacked, is on `C:` by default. Unpacking a few hundred crates is tens of thousands of small files, and that is slow on `C:`. Measured in one job, two rounds each, with the planned set (283 packages):

| Where `CARGO_HOME` is | Download and unpack | Unpack only (what a warm run does) | Set-up cost |
|---|---|---|---|
| `C:`, the default | 26 to 30 s | 23 to 27 s | none |
| `C:`, in a folder excluded from antivirus scanning | 24 to 25 s | 24 to 33 s | 0.1 s |
| The workspace's disk, `D:` | 10 s | 9 s | none |
| A Dev Drive (a ReFS volume in a virtual disk on `D:`, trusted) | 8 to 9 s | 7 to 8 s | 9 s to create, and 1.5 s to copy the checkout onto it |

- **The Windows jobs set `CARGO_HOME` and `RUSTUP_HOME` to a folder on the workspace's disk.** It costs nothing and saves 15 to 20 s on every Windows run that has dependencies to unpack, warm or cold.
- **A Dev Drive is not used.** The owner asked for it to be measured, with the workspace, `CARGO_HOME` and the build directory on it. In one job, alternating the default disks and the Dev Drive twice: a clean build of the planned set took 75 and 76 s on the default disks and 76 and 77 s on the Dev Drive; a relink 1.6 to 1.7 s on both; the skeleton's 22 test binaries 7.9 and 8.0 s against 8.0 and 9.2 s; restoring a cache archive 3.1 and 3.0 s against 2.7 and 3.0 s. Compiling and linking are not held back by the file system here. The one thing the Dev Drive speeds up is unpacking sources, and the plain `D:` disk gets all but about 2 s of that without the 10 s of set-up. Net, the Dev Drive is slower.
- Real-time antivirus scanning is already off on the hosted image (`RealTimeProtectionEnabled: False`), which is why excluding a folder changes nothing and why a Dev Drive's "performance mode" has little to give.

#### After a lockfile change: `rust-cache` alone, or with `sccache`

The owner asked whether a per-crate compile cache (`sccache` with the GitHub Actions cache as its store) would shorten the run after a lockfile change. The premise was that a lockfile change throws the dependency cache away. It does not: `rust-cache` restores the newest cache for the previous lockfile, and cargo rebuilds the crates that changed and the crates that depend on them. The measurement, with the planned set in the `ci` profile, each cell one job (so different machines; differences of a few seconds are noise):

| Run | Linux, `rust-cache` | Linux, with `sccache` | Windows, `rust-cache` | Windows, with `sccache` |
|---|---|---|---|---|
| Everything cold | 45 s | 61 s | 88 s | 88 s |
| One small crate added that nothing else depends on: first push | 1.8 s | 2.2 s | 18.2 s | 19.8 s |
| The same: second push | 1.8 s | 1.9 s | 18.0 s | 19.2 s |
| A patch bump of `bytes`, which 19 crates of the set are rebuilt for: first push | 14.9 s | 17.3 s | 39.4 s | 40.1 s |
| The same: second push | 11.2 s | 8.0 s | 36.3 s | 28.4 s |

The times are the build step, which on Windows includes about 18 s of unpacking sources on `C:` (these runs were made before `CARGO_HOME` was moved). "First push" is the first run of a pull request that changes the lockfile; it reads the main branch's cache and, like every pull request, cannot write it. "Second push" is a later run of the same pull request.

- **`sccache` is not used.** On the first push after a lockfile change it has nothing to offer: the crates that must be rebuilt have new inputs, so its cache has never seen them. It was 0.4 to 2.4 s slower there. It helps only from the second push of a pull request that bumps a crate much of the graph depends on, by about 3 s on Linux and 8 s on Windows.
- What it would cost: a third-party action in the blocking workflow, a network request per compiled crate, cache entries written from pull requests (against the rule that only the main branch writes caches), and a slower cold build on the Linux runner that measured it.
- The warm case was not slowed by it: with every dependency fresh, cargo starts no compiler and the wrapper is not called.

#### Projection for the first release

Not measured: put together from the measured parts above, for a pull request once the planned dependencies are in. The parts for our own code are estimates, because that code does not exist yet.

| Phase | Linux, warm | Linux, cold | Windows, warm | Windows, cold | Basis |
|---|---|---|---|---|---|
| Start the job, check out, install the toolchain and the tools | 0:15 | 0:15 | 0:25 | 0:25 | Measured |
| Restore the cache | 0:04 | 0:00 | 0:26 | 0:00 | Measured |
| Download or unpack the sources | 0:02 | 0:03 | 0:09 | 0:10 | Measured, with `CARGO_HOME` on the workspace's disk |
| Build third-party dependencies, `ci` profile | 0:00 | 0:27 to 0:45 | 0:05 | 1:00 to 1:22 | Measured |
| Build workspace crates and test binaries | 0:50 | 0:50 | 1:30 | 1:30 | Estimate, unchanged from the first draft |
| Run tiers 1 to 3 | 0:45 | 0:45 | 1:15 | 1:15 | Estimate, unchanged; the tier budgets cap it |
| **Total** | **about 2:00** | **about 2:20 to 2:40** | **about 3:50** | **about 4:20 to 4:45** | |

With the `dev` profile's optimised dependencies the cold Windows column would be about 7:40. In the `ci` profile it is inside 5 minutes, with little room, and it rests on two estimates. The steps that add the first heavy dependencies (5, 6 and 14) should look at the job summary's timings when they land, and take mitigation 8 below before the estimate is tested the hard way.

**The risk is still the cold-cache Windows run,** and it is now a rare one: a toolchain bump, or expired caches. The mitigations, in the order they are applied, with what step 2 found:

1. Keep the dependency tree small. This is why every dependency has a measured cost and an owner crate.
2. Only the main branch writes the cache, and a scheduled job on the main branch keeps it warm. A pull request that does not touch the lockfile never builds dependencies. **In place.**
3. A lockfile change restores the previous lockfile's cache and rebuilds only what changed. **In place** (it is how `rust-cache` works), and measured above.
4. Build dependencies unoptimised in CI. **In place:** the `ci` profile. It is the largest single saving found.
5. Group dependency updates into one weekly pull request, and bump the toolchain in a pull request of its own, so cold runs are rare and expected.
6. One integration-test binary per crate, to cut link steps. **Checked by `cargo xtask rules`.**
7. The faster linker on Windows. **Measured: no gain.** Not used.
8. If the Windows job is still over budget on a warm cache: split it in two with nextest's partitioning, building once and sharing the test archive.
9. If a cold-cache Windows run is still over 5 minutes after all of this, that case is reported to the owner for a decision. The limit is not relaxed quietly.

**What grows the time, and the rule for each.**

| Growth | Rule |
|---|---|
| More tests | The per-test limits and tier budgets under "Test tiers", which CI enforces. The final review step lists the slowest tests. |
| More crates | Each new crate needs one of the reasons in [architecture/01-workspace.md](architecture/01-workspace.md). |
| More dependencies | The dependency policy, and the crate-count check below. |
| The script engine's C code | Built once and cached. Its measured build time is in decision 0005. |
| More Windows jobs | Windows runners are slower to start a job than Linux ones, and when more than about four Windows jobs of this repository were running at once, further ones waited minutes for a runner. Queue time is outside a job's 5 minutes but inside what a person waits. Keep the blocking workflow at two Windows jobs. |

### Not blocking

`main` and `nightly` exist in their first form since step 3 of the [build plan](build-plan.md), with the checks that need no product code. Later steps add their tiers to them. `release` and `live` arrive with the steps that need them. The blocking workflow also runs on every push to the main branch and twice a week on a schedule: those runs are what write the caches and keep them from expiring.

| Workflow | When | What it runs now | What later steps add |
|---|---|---|---|
| `main` (`.github/workflows/main.yml`) | After each merge to the main branch, and by hand | `cargo xtask size --check` for Linux x86-64 and Windows x86-64: a build of the published profile (`dist`), the binary's size checked against its budget ([performance-budgets.md](performance-budgets.md)), and the number of crates in the build. Each run keeps its record as an artifact, and shows it on its summary page. | Tier 4 performance checks on both OSes with stored baselines (step 20), and a comparison of the size and the crate count with the run before. |
| `nightly` (`.github/workflows/nightly.yml`) | Every night at 03:17 UTC, and by hand | `cargo xtask advisories` (`cargo deny check advisories`); `cargo xtask unused` (cargo-machete); `cargo xtask size --check` for every release target, each on a runner of its own architecture: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc`; and, since step 4, tiers 1 and 2 on Linux with fifty times the property-test cases ("Property tests", above). | Tier 6: fuzzing for a fixed time (from step 5), the long-session memory test. Tier 5 where a key is configured. |
| `release` | On a version tag | Not built yet. | Builds, checks and publishes binaries with checksums. |
| `live` | By hand | Not built yet. | Tier 5 and fixture recording. Uses a protected environment for secrets. |

- **The jobs call `cargo xtask`,** like the blocking ones, so the same commands run at a desk.
- **No build cache.** Nobody waits for these runs, and the blocking workflow's caches keep the repository's whole 10 GB.
- **One Windows job each.** Hosted Windows runners queue when a repository starts many of them ("What grows the time", above).

**A failure opens or updates an issue.** It does not block merging, and it is not ignored. Each workflow ends with a `report` job that runs only when another job failed. It looks for an open issue titled "The `<workflow>` workflow failed", with the label `ci-failure`. If there is none it opens one; if there is one it adds a comment. Either way the text names the run, the jobs that did not succeed, the trigger and the commit. A person closes the issue when the cause is fixed, and the next failure opens a new one. The steps are in `.github/actions/report-failure`.

**Only the `report` job can write, and only issues.** Every workflow's token is read-only at the top level. A workflow that runs on pull requests can write nothing and uses no secret. In a workflow that does not, the job named `report` may have `issues: write`, and nothing else may have any write permission: the job that holds a token that can write runs no code from the build, and the jobs that build and run code hold a token that cannot write. `cargo xtask rules` checks all of it.

**Testing the report.** Both workflows take a `force_failure` input when started by hand, which makes the first job fail at once. `gh workflow run nightly.yml --ref <branch> -f force_failure=true` exercises the whole path without waiting for a real failure. Close the issue it opens. Step 3 did this for both workflows from a scratch branch: each forced run opened its issue within seconds of failing, and a second failure of `nightly` was added to the open issue as a comment.

## Lints and formatting

- `rustfmt` with a small checked-in configuration.
- Clippy with warnings as errors, a workspace lint table, and three project-specific rules through `clippy.toml`:
  - blocking calls (`std::fs`, `std::process`, `std::thread::sleep`) are disallowed. The lists are on in every crate. Code that is meant to block, because it runs on the blocking pool or the whole crate is synchronous, opts out for a module or the crate with an `allow` that gives the reason;
  - `unwrap` and `expect` are disallowed outside tests, except where an attribute gives the invariant;
  - `println!` and `eprintln!` are disallowed outside `cortex-cli` and `xtask`, so nothing can write to standard output by accident.
- An `allow` or `expect` attribute must carry a `reason`. `todo!`, `unimplemented!`, `dbg!` and `std::process::exit` are warnings, and so errors in CI.
- `unsafe` is denied by default and allowed per module in `cortex-sandbox` and `cortex-term`, where the OS interfaces need it, with a safety comment on each block.
- Public items are documented (`missing_docs`).
- Clippy runs on Linux and on Windows, so that code compiled only for Windows is linted.

**The repo rules** are the checks no off-the-shelf tool does. `cargo xtask rules` runs them, and a test runs them again on both OSes:

- no file contains an absolute home-directory path (every file is scanned, binary files included);
- the crate dependency rules of [architecture/01-workspace.md](architecture/01-workspace.md), read from `cargo metadata`, and the manifest conventions (the `cortex-` prefix, `publish = false`, shared lints, no doc tests, one integration-test binary);
- `cortex-protocol` depends on serde and serde_json and nothing else, and on schemars only as an optional dependency behind its `schema` feature;
- `docs/dependencies.md` agrees with the manifests;
- workflow actions are pinned by commit hash; every workflow's token is read-only; a workflow that runs on pull requests uses no secret and can write nothing; and the only write permission anywhere is `issues: write` on the `report` job of a workflow that does not run on pull requests ("Not blocking", above).

`cargo xtask check` also has a `generated` step: the protocol's JSON Schema and TypeScript declarations in `docs/protocol/` must be what the types in `cortex-protocol` generate. A test makes the same comparison on both OSes. `cargo xtask schema` regenerates them.

## Dependency hygiene

- **Licences.** `cargo deny` allows: MIT, Apache-2.0 (with or without the LLVM exception), BSD-2-Clause, BSD-3-Clause, ISC, Zlib, 0BSD, BSL-1.0, CC0-1.0, Unlicense, Unicode-3.0, Unicode-DFS-2016, and CDLA-Permissive-2.0 (the certificate data). Everything else fails the check. The planned dependency set was checked against this list when these docs were written: all of it passes. The one copyleft licence seen anywhere in the candidates (MPL-2.0, through one of the scripting engines that was not chosen) is not in the planned set.
- **Sources.** crates.io only. No git dependencies.
- **Bans.** OpenSSL bindings, native-TLS, and anything that needs CMake or NASM. Duplicate versions of the same crate are warnings that the weekly update pull request is expected to clear.
- **Advisories.** Checked nightly and on every lockfile change. A new advisory against an unchanged lockfile must not block unrelated pull requests, so it is not in the blocking set.
- **Unused dependencies.** Checked nightly, with cargo-machete. It reads the sources and needs no build, so it is a heuristic: a dependency used only in a way it cannot see is declared in the manifest, under `[package.metadata.cargo-machete]`, with the reason. **For now it looks at third-party crates only.** The skeleton declares the edges between workspace crates before the code that uses them exists, so the workspace's own crates are on an ignore list in the root `Cargo.toml`. A name comes off the list when the crates that depend on it have code, and the list goes once the first release's crates are built.
- **The dependency list.** `docs/dependencies.md` names every direct dependency, its owner crate, its purpose and its measured cost. A tier 0 check fails when it and the manifests disagree.
- **Crate count.** The number of crates in a release build is recorded on the main branch by the `main` workflow (every crate cargo compiles for the binary, ours included, with build dependencies and without development ones). An increase of more than five in one change needs a line in the pull request saying why.
- **Updates.** One grouped pull request a week.
- **The lockfile is committed.**

## Local development loop

The same budgets apply at a desk.

| Action | Budget | How | Measured for the skeleton, step 2 |
|---|---|---|---|
| `cargo check` after an edit in a leaf crate | Under 2 s | Small crates; heavy dependencies in leaf crates. | 0.04 s |
| Rebuild and relink the binary after an edit | Under 5 s | A tiny binary crate. Measured floor for relinking the full dependency set: 0.4 s on the desktop, 0.5 s on a Linux runner, 1.2 to 1.9 s on a Windows runner. | 0.1 s |
| No-op build | Under 0.5 s | | 0.03 s |
| Tiers 1 and 2 | Under 30 s | `cargo xtask test --profile fast`. | 0.2 s |
| The static checks | | `cargo xtask check`. | 0.3 s |
| Everything that blocks a pull request | Under 3 min | `cargo xtask ci` runs exactly what CI runs. | 0.7 s warm; 3.6 s from an empty build directory on 4 cores |

The measurements are from the desktop the earlier spikes used, with warm build directories unless it says otherwise. The crates are empty, so these are floors: what the tooling itself costs before there is any code.

The commands, all through `cargo xtask` (or `./x`):

| Command | Runs |
|---|---|
| `check` | The static checks: `fmt`, `rules`, `generated`, `clippy`, `deny`. All of them run even when one fails, and the time of each is printed. |
| `test` | All tests. Arguments go to nextest: `--profile fast` for tiers 1 and 2. |
| `ci` | What the blocking jobs run: `check`, then the tests in the `ci` profiles, then the tier budgets. |
| `schema` | Regenerates the protocol's JSON Schema and TypeScript declarations in `docs/protocol/`. `schema --check` only compares. `gen` regenerates every generated file, which today is the same thing. |
| `size`, `unused`, `advisories` | What the non-blocking workflows run: the `dist` build with its size and crate count (`--check` fails over the budget, `--target` names the target), cargo-machete, and `cargo deny check advisories`. |
