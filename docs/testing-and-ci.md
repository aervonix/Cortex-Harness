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

- Timeouts, retries, backoff, cooldowns, debouncing and anything else that depends on time passing are tested with time that is paused or simulated: the async runtime's test clock, which jumps to the next timer as soon as every task is idle, or the fake clock from `cortex-testkit` for code that takes time from the clock trait in `cortex-base`. A test of a 30-second timeout takes microseconds.
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

## Deterministic agent tests without a network

`cortex-testkit` provides three levels of fake, one for each kind of test.

| Fake | Replaces | Used for |
|---|---|---|
| **Scripted provider** | `ModelClient`. A script of model events with optional delays, errors, stalls and aborts, plus assertions on the requests it received. | Tier 2: every loop behaviour. Runs in microseconds. |
| **Scripted transport** | `Transport`. Returns recorded response bodies in chosen chunk sizes. | Tier 1: protocol adapters, from fixtures. |
| **Scripted SSE server** | The provider's HTTP endpoint. A local listener that writes scripted chunks with controlled timing, stalls and disconnects. | Tier 3: the real HTTP client, timeouts, cancellation, and the real binary end to end. |

**Fixtures.** Recorded provider responses live in the repo under each protocol's `fixtures/` directory: the raw response stream, the request that produced it, and a note. `xtask record` captures them from a real provider by hand, scrubs credentials and identifiers, and is never run in CI. Each provider step adds fixtures for every case in its checklist ([build-plan.md](build-plan.md)).

**Replays at every chunk boundary.** Stream decoders are tested by replaying each fixture split at every byte offset, which catches the bugs that only appear when a network read ends in the middle of a token.

**Golden transcripts.** Sequences of protocol messages from released versions are kept and replayed against current types, to catch accidental breaking changes to the RPC protocol.

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
| Paths | Both separators accepted; case-insensitive comparison; device names and UNC paths rejected; long paths; `~` expansion. |
| Shells | PowerShell 7, Windows PowerShell and `cmd.exe`: quoting, merged output, exit status, the model being told which shell it has. Git Bash where the runner has it. |
| Line endings | CRLF and mixed files survive `edit` and `write` byte for byte outside the edited lines; `.gitattributes` keeps fixtures exact. |
| Process trees | A command with a grandchild is fully gone after cancellation, after timeout, and after the harness is killed (Job Objects). |
| Files in use | Replacing a file another process holds open retries and then fails cleanly. |
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
| `lint-linux` | `ubuntu-latest` | `cargo xtask check`: `cargo fmt --all --check`; the repo rules; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo deny check licenses bans sources`. |
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

None of these exists yet. Each arrives with the step that first has something for it to run. The blocking workflow also runs on every push to the main branch and twice a week on a schedule: those runs are what write the caches and keep them from expiring.

| Workflow | When | What |
|---|---|---|
| `main` | After each merge to the main branch | Tier 4 performance checks on both OSes with stored baselines; a release-profile build and the binary-size check. |
| `nightly` | Nightly | Tier 6: fuzzing for a fixed time, long-session memory test, release builds for every target, `cargo deny check advisories`, unused-dependency check, tier 5 where a key is configured. |
| `release` | On a version tag | Builds, checks and publishes binaries with checksums. |
| `live` | By hand | Tier 5 and fixture recording. Uses a protected environment for secrets. |

A failure in a non-blocking workflow opens or updates an issue. It does not block merging, and it is not ignored: the performance and size checks have owners named in the workflow.

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
- `docs/dependencies.md` agrees with the manifests;
- workflow actions are pinned by commit hash, and the blocking workflow uses no secret and no token that can write.

## Dependency hygiene

- **Licences.** `cargo deny` allows: MIT, Apache-2.0 (with or without the LLVM exception), BSD-2-Clause, BSD-3-Clause, ISC, Zlib, 0BSD, BSL-1.0, CC0-1.0, Unlicense, Unicode-3.0, Unicode-DFS-2016, and CDLA-Permissive-2.0 (the certificate data). Everything else fails the check. The planned dependency set was checked against this list when these docs were written: all of it passes. The one copyleft licence seen anywhere in the candidates (MPL-2.0, through one of the scripting engines that was not chosen) is not in the planned set.
- **Sources.** crates.io only. No git dependencies.
- **Bans.** OpenSSL bindings, native-TLS, and anything that needs CMake or NASM. Duplicate versions of the same crate are warnings that the weekly update pull request is expected to clear.
- **Advisories.** Checked nightly and on every lockfile change. A new advisory against an unchanged lockfile must not block unrelated pull requests, so it is not in the blocking set.
- **Unused dependencies.** Checked nightly.
- **The dependency list.** `docs/dependencies.md` names every direct dependency, its owner crate, its purpose and its measured cost. A tier 0 check fails when it and the manifests disagree.
- **Crate count.** The number of crates in a release build is recorded on the main branch. An increase of more than five in one change needs a line in the pull request saying why.
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
| `check` | The static checks: `fmt`, `rules`, `clippy`, `deny`. All of them run even when one fails, and the time of each is printed. |
| `test` | All tests. Arguments go to nextest: `--profile fast` for tiers 1 and 2. |
| `ci` | What the blocking jobs run: `check`, then the tests in the `ci` profiles, then the tier budgets. |
