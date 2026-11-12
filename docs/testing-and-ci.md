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

**No sleeping in tests.** Time is the fake clock. A lint bans `std::thread::sleep` and real timers in tiers 1 and 2. Tier 3 waits on conditions with deadlines, never on fixed delays.

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

Three jobs in parallel. Wall time is the slowest job.

| Job | Runner | Steps |
|---|---|---|
| `lint` | `ubuntu-latest` | `cargo fmt --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo deny check licenses bans sources`; the repo-rule checks. |
| `test-linux` | `ubuntu-latest` | `cargo nextest run --workspace --locked --profile ci` (tiers 1 to 3). |
| `test-windows` | `windows-latest` | The same. |

Settings common to all jobs:

- `--locked`, `CARGO_INCREMENTAL=0`, no debug info in test builds (`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`).
- The toolchain version comes from `rust-toolchain.toml`.
- Dependency caching with `Swatinem/rust-cache`, keyed by OS, job and lockfile. Only the main branch writes the cache, so pull requests read a cache that is always consistent.
- `cargo-nextest` and `cargo-deny` are installed as prebuilt binaries, not compiled.
- Superseded runs on the same branch are cancelled.
- Actions are pinned by commit hash. The workflow token has read-only permissions.

**nextest configuration** (`.config/nextest.toml`):

- profile `ci`: fail fast off, a slow-test warning at 10 s, termination at 60 s, JUnit output for the summary;
- a test group that limits pseudo-terminal and process tests to two at a time, so they do not starve each other on a 4-core runner;
- no retries, except tests explicitly marked as timing-sensitive, which get one retry and are reported when it is used;
- profile `fast` for local use: tiers 1 and 2 only.

### Where the five minutes go

Hosted runners for public repositories have 4 cores and 16 GB. The measured cost of building the planned third-party dependency set from clean, in a dev profile on 4 pinned desktop cores, is 15 s for the core set and 27 s with the MCP SDK, keychain and highlighter included; the script engine, which is C code, adds about 3 s ([measurements/dependency-costs.md](measurements/dependency-costs.md)). The desktop's cores are roughly twice as fast as a hosted runner's, and Windows runners are slower again. From that:

| Phase | Linux, warm cache | Linux, cold cache | Windows, warm cache | Windows, cold cache |
|---|---|---|---|---|
| Checkout, toolchain, tools | 0:25 | 0:25 | 0:40 | 0:40 |
| Cache restore | 0:15 | 0:00 | 0:30 | 0:00 |
| Build third-party dependencies | 0:00 | 1:00 | 0:00 | 2:00 |
| Build workspace crates and test binaries | 0:50 | 0:50 | 1:30 | 1:30 |
| Run tiers 1 to 3 | 0:45 | 0:45 | 1:15 | 1:15 |
| **Total (estimate)** | **2:15** | **3:00** | **3:55** | **5:25** |

These are estimates from measurements on other hardware. **Step 2 measures all four cases on real runners and replaces this table.**

**The risk is the cold-cache Windows run**, which happens when the lockfile changes. The mitigations, in the order they are applied:

1. Keep the dependency tree small. This is why every dependency has a measured cost and an owner crate.
2. Only the main branch writes the cache, and a scheduled job on the main branch keeps it warm. A pull request that does not touch the lockfile never builds dependencies.
3. Group dependency updates into one weekly pull request, so cold runs are rare and expected.
4. One integration-test binary per crate, to cut link steps.
5. Use the faster linker on Windows for test builds if step 2 measures a gain.
6. If the Windows job is still over budget on a warm cache: split it in two with nextest's partitioning, building once and sharing the test archive.
7. If a cold-cache Windows run is still over 5 minutes after all of this, that case is reported to the owner for a decision. The limit is not relaxed quietly.

**What grows the time, and the rule for each.**

| Growth | Rule |
|---|---|
| More tests | Tier budgets above. A test over 2 s in tiers 1 and 2, or over 10 s in tier 3, needs a reason or moves to tier 6. The final review step lists the slowest tests. |
| More crates | Each new crate needs one of the reasons in [architecture/01-workspace.md](architecture/01-workspace.md). |
| More dependencies | The dependency policy, and the crate-count check below. |
| The script engine's C code | Built once and cached. Its measured build time is in decision 0005. |

### Not blocking

| Workflow | When | What |
|---|---|---|
| `main` | After each merge to the main branch | Tier 4 performance checks on both OSes with stored baselines; a release-profile build and the binary-size check; writes the caches. |
| `nightly` | Nightly | Tier 6: fuzzing for a fixed time, long-session memory test, release builds for every target, `cargo deny check advisories`, unused-dependency check, tier 5 where a key is configured. |
| `release` | On a version tag | Builds, checks and publishes binaries with checksums. |
| `live` | By hand | Tier 5 and fixture recording. Uses a protected environment for secrets. |

A failure in a non-blocking workflow opens or updates an issue. It does not block merging, and it is not ignored: the performance and size checks have owners named in the workflow.

## Lints and formatting

- `rustfmt` with a small checked-in configuration.
- Clippy with warnings as errors, a workspace lint table, and three project-specific rules through `clippy.toml`:
  - blocking calls (`std::fs`, `std::process`, `std::thread::sleep`) are disallowed in the async crates;
  - `unwrap` and `expect` are disallowed outside tests, except where a comment gives the invariant;
  - `println!` and `eprintln!` are disallowed outside `cortex-cli`, so nothing can write to standard output by accident.
- `unsafe` is denied by default and allowed per module in `cortex-sandbox` and `cortex-term`, where the OS interfaces need it, with a safety comment on each block.

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

| Action | Budget | How |
|---|---|---|
| `cargo check` after an edit in a leaf crate | Under 2 s | Small crates; heavy dependencies in leaf crates. |
| Rebuild and relink the binary after an edit | Under 5 s | A tiny binary crate. Measured floor for relinking the full dependency set: 0.4 s. |
| Tiers 1 and 2 | Under 30 s | `cargo nextest run --profile fast`. |
| Everything that blocks a pull request | Under 3 min | `cargo xtask ci` runs exactly what CI runs. |
