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
| **5. Live** | Smoke tests against real providers. Fixture recording. | By hand: `cargo xtask live` and `cargo xtask record`, since step 14. No workflow runs them yet. Never on pull requests. | No | |
| **6. Soak** | Fuzzing of decoders and parsers, long scripted sessions for memory growth, release builds for every target. | Nightly. | No | |

Tiers 1 to 3 are separated by where the test lives and by nextest filters, not by cargo features:

- unit tests are `#[cfg(test)]` modules in the crate;
- integration tests are one test binary per crate, `tests/it/main.rs`, with modules inside it;
- system tests live in `cortex-cli/tests/system/` and in `cortex-testkit`-driven binaries, and their names start with `sys_`.

**One integration binary per crate.** Every extra test binary is another link step, and linking is the slow part on Windows. Doc tests are turned off (`doctest = false`); examples that must compile are ordinary tests.

**No real waiting in tests.** This is rule 12 of the [build plan](build-plan.md).

- Timeouts, retries, backoff, cooldowns, debouncing and anything else that depends on time passing are tested with time that is paused or simulated: the async runtime's test clock, which jumps to the next timer as soon as every task is idle, or the fake clock for code that takes time from the clock trait in `cortex-base`. A test of a 30-second timeout takes microseconds. The fake clock is defined in `cortex-base`, beside the trait, because that crate tests itself with it and cannot depend on the test kit; the test kit re-exports it. Its `sleep` returns at once, moves its time forward and records what was asked, so blocking code that waits and retries (the replace of a locked file) is tested for how long it would have waited.
- Production code never reads the system clock or sleeps directly. It goes through the runtime's timers or the clock trait, which is what makes the rule possible to follow.
- **Async code that also needs the date waits on a timer trait.** The retry layer of `cortex-llm` waits between attempts and also says when the next attempt is and when a cooldown ends, so it needs waits and dates from one clock. It takes its waits from a `Timer` trait and its dates from the clock trait. The harness gives it the runtime's timer. A test gives it the test kit's `FakeTimer`, made from the same `FakeClock`, in one of two kinds: **automatic**, where a wait finishes when it is first polled and moves the clock by its length, so code that waits and goes on runs straight through and the test reads back what it waited for; and **manual**, where a wait finishes only when the test moves the clock past its end, for a time limit that must not pass until the test says so. Neither needs an async runtime: the test kit's `block_on` runs such code on the test's own thread.
- **Two sides of a test take turns through a gate, not a pause.** Where things must happen in an order (the scripted server has seen the client hang up; a scripted response may go on), one side opens a `Gate` and the other waits for it. A blocking wait on a gate takes a bound, so that a mistake is a failed test and not a hung one.
- **Wait for a gate. Do not assert that it is open,** unless your own thread is the one that opens it. Another thread opens its gate a moment after the thing the gate marks: the scripted server writes a piece and then marks it, so the piece can reach the test first. An assertion that the gate is already open passes almost every time and fails under load, which is how step 5 put a test on the main branch that the nightly run then failed. Asserting that a gate is still **closed** is sound only for a gate the test itself opens.
- **Never rely on closing a socket from one thread to wake a read that is waiting in another.** Linux wakes it. Windows does not promise to, and a test that joins the reading thread then hangs there, some of the time. A thread that reads a socket and must be stoppable gives its read a short timeout and asks, each time it runs out, whether it has been told to stop. That is how the scripted server stops (`crates/cortex-testkit/src/server.rs`): the interval bounds how long stopping can take, and no test waits on it.
- A test never calls `std::thread::sleep`. Clippy's disallowed-methods list bans it everywhere, tests included.
- Tier 3 cannot pause time, because the code under test is another process. It waits on a condition with a deadline (output seen, file present, process gone), never on a fixed delay, and the helper for that is in `cortex-testkit`.
- **A process test's children are the test binary itself, run again** (`cortex_testkit::children`, since step 11). A test binary has one entry test, which passes at once under the test runner, and becomes a child when a test starts the binary again with a child's specification: one that stays until it is killed, one that starts a grandchild, one that the polite interrupt does not stop, one that exits with a code, one that prints its arguments back. Each child connects to the test's `Roll` on the loopback interface and keeps the connection until it dies. So "the process is up" is a connection that arrives, and "the process is gone" is that connection closed by the system: neither is a guess at how long something takes. No helper program is built, so there is no second link step, and no test runs `sleep`.
- **The process runner waits on a timer trait,** like the retry layer. Its tests give it a timer whose waits end when the test ends them, by their length: the time limit of a call, the grace between two stages of stopping, the grace for what a command left running. A test of a two-minute time limit takes as long as the processes take to die. A call that does not end by itself runs on the test's thread while a director on a second thread waits for children and ends waits, and a director that fails lets every wait go, so that a failed assertion is a failed test and not a hung one.

**Budgets are enforced.** This is rule 13 of the build plan.

| Tier | A test is reported as slow at | A test is stopped, and fails, at | The tier's tests together |
|---|---|---|---|
| 1. Unit | 2 s | 10 s | 20 s |
| 2. Integration | 2 s | 10 s | 40 s |
| 3. System | 10 s | 60 s | 60 s on Linux, 90 s on Windows |

- The limits on one test are nextest's `slow-timeout` and `terminate-after`, set per tier in `.config/nextest.toml`. They hold in every profile, at a desk as in CI, except `properties`, the nightly run with fifty times the property cases, whose limits are scaled for it ("Property tests", below).
- The tier totals are the sum of the tests' own times, not the wall time of the run. `cargo xtask ci test` reads them from nextest's JUnit report after the run, prints them with the share of the budget used, and writes them to the CI job's summary page.
- **A tier over its budget is a warning; a tier over one and a half times its budget is a failure.** Both are annotations on the pull request. The gap is there because hosted runners vary by tens of percent from one run to the next, and more on Windows (the same build took 60 to 82 s on four Windows runners in step 2). A failure at exactly the budget would turn a tier that is near its limit into red builds at random, which teaches people to re-run instead of to fix. The warning is the signal to act; the failure is for a tier that is over on any runner. The limits on a single test stay hard failures: they are several times what a healthy test takes.
- A test over its slow mark needs a reason or moves to tier 6.

**Property tests** use no property-testing crate. The pattern was set in step 4 (`crates/cortex-base/src/cases.rs`), and steps 6, 10 and 12 follow it. Since step 5 the helper every crate uses is `cortex_testkit::cases`. `cortex-base` keeps its own private copy for its unit tests, because it cannot depend on the test kit, which depends on it; a change to one is a change to both. `cortex-tools` had a third copy while step 6 was built beside the test kit; step 7 removed it, and the crate's unit tests use the shared one.

- A property is a function from a generated input to "fine" or "what was wrong". A generator is a function from a random-number generator to an input. `check(name, cases, generate, property)` runs the property on that many inputs.
- **Every run is the same.** Each case draws from the seeded generator of `cortex-base` with a seed made from the property's name and the case number. There is no randomness between runs or machines, so a property test cannot be flaky.
- **A failure prints the property, the seed, the case number and the input.** `CORTEX_PROPERTY_SEED=<seed>` runs that one case again, alone.
- **`CORTEX_PROPERTY_SCALE=<n>` multiplies every property's number of cases.** Pull requests run at 1, which keeps the properties of one crate to well under a second of the unit tier's 20. The `nightly` workflow runs tiers 1 and 2 at 50. The first time this was tried it found a case the default count had not (a wrong expectation in a wrapping test), which is the point of it. At 50 a property takes fifty times as long, so the nightly run has a nextest profile of its own, `properties`, whose limits on one test are scaled for it: slow at 60 s, stopped at 300 s. The limits pull requests run with are not changed. A property of pure logic is far from either (the slowest of step 4 takes 1.8 s at 50 on the desktop); one that does real file work for each case is not: the rewind property of `cortex-session`, a session and a workspace per case, takes 0.75 s at 1 and 32 s at 50 on the desktop, and passed the 10 s limit in the nightly run of 2026-11-13 (issue #19).
- **There is no shrinking,** so generators prefer small inputs, and an input is printed whole. In exchange the helper is 60 lines, adds no dependency and nothing to the build.
- **Small spaces are checked exhaustively** in addition, where that is possible: every sequence of up to six path steps from a small alphabet, every split of a byte stream into two and into three chunks.

What was weighed against it: proptest, which shrinks failing inputs and has a library of generators, and which would add about fifteen packages to every crate's test build, on both OSes, for the life of the project.

## Deterministic agent tests without a network

`cortex-testkit` provides three levels of fake, one for each kind of test.

| Fake | Replaces | Used for |
|---|---|---|
| **Scripted provider** | `ModelClient`. A script of model events with optional delays, errors, stalls and aborts, plus assertions on the requests it received. | Tier 2: every loop behaviour. Runs in microseconds. |
| **Scripted transport** | `Transport`. Returns recorded response bodies in chosen chunk sizes. | Tier 1: protocol adapters, from fixtures. Tier 2: the provider client and its retry layer. |
| **Scripted SSE server** | The provider's HTTP endpoint. A local listener that writes scripted chunks with controlled timing, stalls and disconnects. | The real HTTP client, timeouts, cancellation (tier 2, in-process, since step 5), and the real binary end to end (tier 3). |

All three were built in step 5, with two more pieces they need.

- **The scripted provider** builds a response from what a model does (text, reasoning, tool calls, usage, an end, a speed the provider served it at) and what goes wrong (an error part-way, a delay on a timer, a stall until cancelled, a wait for the test's gate, a stream that just stops). It hands over one event at a time and only when the reader asks, so the order a test sees is the order of the script.
- **The scripted SSE server** listens on the loopback interface only, on a port the system picks, with plain threads and no async runtime, so it can also serve another process. A reply is a script of steps: send these bytes, tell the test this point was reached, wait for the test, hold the connection until the client hangs up, drop it. That last-but-one step is how a test sees that a cancelled request really closed its connection.
- **A test protocol** (`TestProtocol`): a wire dialect small enough to write by hand in a test and complete enough to say everything a real one can, including the speed that served a request and a refusal that belongs to a speed tier. `cortex-llm` has no real protocol until the first provider step, and its client is tested with this one. It is not a model of any provider's API.
- **Fixture models** with every shape of speed tier: one that fails when it cannot be served and has a limit and a cache of its own, one the provider downgrades by itself, and a model with no tier.

**A crate's own unit tests cannot use test-kit types that are made of that crate's types.** The test kit depends on `cortex-llm`, so in `cortex-llm`'s unit-test build there are two copies of the crate, the one under test and the one the test kit was built against, and a `ModelEvent` of one is not a `ModelEvent` of the other. So `cortex-llm` tests its pure parts (the parser, the retry rules, the catalog, cost) as unit tests, and its client, which needs the scripted transport and the fake timer, in its integration-test binary. The parts of the test kit that involve only `cortex-base` types, such as the property helper, work in unit tests too.

**Fixtures.** Recorded provider responses live in the repo under each protocol's `fixtures/` directory: the raw response stream, the request that produced it, and a note. `xtask record` captures them from a real provider by hand, scrubs credentials and identifiers, and is never run in CI. Each provider step adds fixtures for every case in its checklist ([build-plan.md](build-plan.md)).

As built in step 14, for the first protocol:

- **Where.** `crates/cortex-llm/fixtures/<protocol>/<name>/`, with four files: `request.json` (the body that was sent), `response.sse` (the response body, byte for byte), `response.json` (the status and the headers that were kept) and `note.md` (what it shows, the model, and how it was made). `.gitattributes` keeps every file under a `fixtures` directory exactly as it is, line endings included.
- **Recorded, or written by hand.** A case that the real API produces on request is recorded. A case that cannot be provoked (an overload in the middle of a response, a refusal, a server-side tool, a stop reason from a newer API) is written by hand from the provider's documented format, and its note says so. Of Anthropic's 32 fixtures, 16 are recorded.
- **The requests are shared.** The recorder builds each request with the same function the offline tests use. So a fixture's `request.json` is a snapshot of a request the real API accepted, and a test fails when the builder stops writing it. A recorded conversation is rebuilt the same way: each later request from what the decoder made of the response before it, and compared, byte for byte, with what was sent.
- **Recording.** `cargo xtask record [SCENARIO…]` runs an ignored test of `cortex-llm` (`tests/it/anthropic/live.rs`), and `cargo xtask live` the smoke test beside it. They are there and not in `xtask` because `xtask` is built before every CI job, and the HTTP and TLS stack is the largest dependency the project has. No CI job runs ignored tests.
- **The key.** `CORTEX_LIVE_ANTHROPIC_KEY_FILE` names a file that holds the key and nothing else. The key is never an argument and never in the environment. Without the variable both commands say so and stop: they do not pass, and they do not skip.
- **What is taken out** (`cortex_testkit::fixture::Scrubber`). No request header is kept at all. Of the response headers only a short list is kept: the content type, `retry-after`, the request id and the rate limits of the model and of the fast tier. The ids the provider makes up (`msg_…`, `req_…`, `toolu_…`) are replaced by stand-ins, the same stand-in wherever the id appears, so that a tool result still names its call. The opaque payloads of reasoning blocks are replaced the same way: they are bound to the account that produced them. Every value of such a member that is not empty is replaced, whatever it looks like, and the member is also found where a block is quoted inside a string.
- **What a fixture still says about the account:** the size of its rate limits, which are the numbers in the headers that are kept. The tests of those headers read them. They name nobody, and they show which usage tier the recording account was on.
- **A recording that holds the key is refused.** Everything that is about to be written, and every line that is about to be printed, is searched for the key and for any twelve characters in a row of it. If it is there, nothing is written and nothing is printed. The refusal is tested with a made-up key, in the request, in the response, in a header that is kept and in the note.
- **What a run costs.** Each call prints what it used and what that cost at the catalog's prices, and a run stops before it has spent half a dollar. Recording all of Anthropic's fixtures cost about one and a half cents.

**Replays at every chunk boundary.** Stream decoders are tested by replaying each fixture split at every byte offset, which catches the bugs that only appear when a network read ends in the middle of a token.

- Each stream fixture is decoded whole, cut in two at every byte, one byte at a time, and in pieces of several fixed sizes. The events must be the same every time, and what they are is in a snapshot for each fixture.
- **The work is bounded,** as step 4 learned it must be. Cutting a stream of `n` bytes in two at every byte decodes `n` bytes `n` times, and the longest recorded stream is 10 KB. A stream is cut in two at every byte of its first 4 KB and at every eighth byte after that. The pass that delivers one byte at a time still puts a boundary at every byte of every stream. The 26 streams of step 14 take 0.7 s together on the desktop, the longest 0.7 s by itself.

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
| Paths, through the tools | Built in step 6 (`crates/cortex-tools/tests/it/paths_windows.rs`): `read`, `list` and `search` each accept both separators and show `/`; find a file asked for in another case and show its on-disk spelling, as one seen file; take an absolute path with a drive letter; and refuse, when the call is planned, device names, the device and extended-length forms, trailing dots and spaces, alternate data streams, the characters Windows does not allow, drive-relative and root-relative paths, and network paths (on the text, with a server name that cannot exist). A walk steps past a file whose name ends with a dot and one named `aux`, both made through an extended-length path, and says so. A path longer than 260 characters is read, listed and searched. Globs take either separator and ignore case. Ignore files with CRLF endings are honoured. |
| Shells | PowerShell 7, Windows PowerShell and `cmd.exe`: quoting, merged output, exit status, the model being told which shell it has. Git Bash where the runner has it. Built in step 11 (`crates/cortex-tools/tests/it/shell.rs`), as one set of tests run once for each shell: the two output streams arrive as one, in the order written; the exit status is the command's own, a program's when a program ran last, and 1 for a PowerShell cmdlet that failed; twenty-seven hostile texts (quotes, `&`, `\|`, `^`, `%VAR%`, `$(…)`, back-ticks, a line break, a trailing backslash, typographic quotes) arrive unchanged at a program that prints its arguments, and through PowerShell itself, and none of them runs; a command with quotes after backslashes reaches the shell as written; the environment has the settings that keep tools from asking; the working directory may have `&` in its name; the input is closed; 6,000 lines of output are cut to a head and a tail with all of it in the artifact; and the tool's description names the shell and the version the shell itself reported. Which shell is found, and the order of the fallbacks, is tested on both OSes for both (`shell_search.rs`). A command with a line break is refused for `cmd.exe`. |
| Line endings | CRLF and mixed files survive `edit` and `write` byte for byte outside the edited lines; `.gitattributes` keeps fixtures exact. Built in step 10, and run on both OSes because the rules are the same on both: the properties of the matcher (`crates/cortex-tools/src/matching/tests.rs`) and of the tools on real files (`crates/cortex-tools/tests/it/editing.rs`), which make their files byte by byte and use no fixture. On Windows only (`editing_windows.rs`): a new file is written with the line feeds the model gave, and a file with CRLF and a byte-order mark keeps both through an edit. |
| Process trees | A command with a grandchild is fully gone after cancellation, after timeout, and after the harness is killed (Job Objects). Built in step 11, for the tree itself (`crates/cortex-sandbox/tests/it/tree.rs`) and through the tool (`crates/cortex-tools/tests/it/process.rs`), with a grandchild that the polite interrupt does not stop (on Windows one with no console, which a console event never reaches): gone after the time limit, after cancellation, when the call's future is dropped, and when a harness that is a process of its own is killed. A process left in the background is stopped and reported, for each shell, whether it holds the command's output or not. |
| Files in use | Replacing a file another process holds open retries and then fails cleanly. Built in step 4 (`crates/cortex-base/tests/it/atomic.rs`): a file held with no sharing fails after seven attempts and half a second of waiting, all of it on the fake clock, with the file unchanged and no temporary file left; a file let go during the retries is replaced. Through the editing tools since step 10 (`crates/cortex-tools/tests/it/editing_windows.rs`): a file another program holds with no sharing cannot be read, so `edit` and `write` fail at once and wait for nothing; a file another program holds for reading is read, the replace is tried seven times over half a second of the fake clock, and the call fails with the file byte for byte as it was, no temporary file beside it, the session's table of seen files unchanged and the round's checkpoint holding the content it still has; a file let go during the retries is edited; a file with the read-only attribute is refused; and a file read under one spelling is edited under another case and the other separator. The open that follows no link is tested there against a real symbolic link to a file and to a directory, a real junction, and a file held with no sharing (`crates/cortex-base/tests/it/nofollow.rs`). |
| Terminal | ConPTY end-to-end tests of the TUI: key release events ignored, new-line keys, resize, restoration. |
| Script shims | A `.cmd`-shim MCP server and hook command start with arguments intact; unsafe arguments are refused. |
| Permissions on files | Session, credential and state files are readable only by the user. Built in step 9 (`crates/cortex-base/tests/it/private.rs`): a file made by `private::create_new`, a file left by an owner-only atomic write (new, and over one made by hand), and a directory made by `private::create_dir_all` each have the current user as owner and as the one entry of the access list, with full control and inheritance off, read back from the file system; a path longer than 260 characters works; a link under the name is not followed. One of them asks `icacls`, which shares no code with the module, for a second opinion, and is a system test because `icacls` is a process. The credentials file and its lock file are checked the same way (`crates/cortex-config/tests/it/credentials.rs`). Step 8 checks every file and directory a session leaves (the log, its lock, a cut line set aside, artifacts, the checkpoint store, a fork, the index and its lock) with `is_private`, on both OSes (`crates/cortex-session/tests/it/private.rs`). |
| Credentials | A key round-trips through the real Windows Credential Manager, under an entry name of the test's own that no real entry has, removed when the test ends, pass or fail (`crates/cortex-config/tests/it/credentials.rs`). |
| Configuration | Files with a byte-order mark and CRLF parse, and an error in one names the same line as it would without them. Built in step 9 (`crates/cortex-config/tests/it/loading.rs`). |
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
- **The generated files under `docs/protocol/` and `docs/configuration/` count as code.** They are checked against the Rust types by tests, so a pull request that touches them runs the tests, wherever else its changes are.

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

#### After step 5: the provider abstraction and the test kit

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

#### After step 6: the tool contract and the read-only tools

Step 6 filled `cortex-tools` (about 13,200 lines, half of them tests: 208 tests on Linux and 213 on Windows) and added a module to `cortex-base`. It is the first step to bring in heavy third-party crates: the search libraries, `similar`, `tokio` with one feature, and `sha2`, which makes 50 third-party packages in a build of `cortex-tools` where `cortex-base` had 15. Only the main branch writes caches, so the pull request's runs built all of them from source. Measured on the step's pull request on 2026-11-13:

| Job | Step 4, second run | Step 6, first run | Step 6, second run |
|---|---|---|---|
| `lint-linux` | 31 s | 36 s | 37 s |
| `test-linux` | 28 s | 50 s | 37 s |
| `lint-windows` | 53 s | 51 s | 59 s |
| `test-windows` | 67 s | 77 s | 58 s |
| **Wall time, Linux** | 31 s | 50 s | 37 s |
| **Wall time, Windows** | 67 s | 77 s | 59 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (first run) | 10.8 s (clippy 10.0 s) | 21.7 s (build tests 18.9 s, run 2.8 s) | 16.5 s (clippy) | 33.8 s (build tests 29.9 s, run 3.9 s) |
| Test time against the tier budgets (first run) | | tier 1: 5.5 s of 20 s. Tier 2: 3.0 s of 40 s. Tier 3: 0.4 s of 60 s | | tier 1: 7.5 s of 20 s. Tier 2: 5.5 s of 40 s. Tier 3: 0.6 s of 90 s |
| Test time against the tier budgets (second run) | | tier 1: 4.2 s of 20 s. Tier 2: 2.7 s of 40 s. Tier 3: 0.3 s of 60 s | | tier 1: 4.7 s of 20 s. Tier 2: 3.8 s of 40 s. Tier 3: 0.4 s of 90 s |

- **Fifty new packages, built with nothing cached, cost about 10 s on Linux and 20 s on Windows.** That is the `ci` profile doing what it was added for: unoptimised, the regex engine and the walker build in seconds. Once the main branch has written caches that hold them, a pull request that leaves the lockfile alone will not build them at all. The projection further down allowed 27 to 45 s on Linux and 60 to 82 s on Windows for the whole planned set from cold; this is a fifth of that set by packages.
- **The unit tier is at a quarter to a third of its budget on Windows** (4.7 and 7.5 s of 20 s). `cortex-tools` adds 105 unit tests that take 0.1 s together on the desktop: its properties run 150 to 300 cases each, which the nightly run multiplies by fifty (the slowest then takes about 2 s on the desktop).
- **The integration tier grew from 0.5 s to 5.5 s on Windows,** which is where real files are made: about a hundred tests, each with a workspace of its own. The largest make 1,000 to 1,500 small files (the walk that is cancelled part-way, the generated tree that is searched), and one writes a 12 MB file to read three lines from its end. None passed its 2 s mark.
- **The 100,000-file search is not in any tier.** Making the tree takes longer than a test may. It is an ignored test, run by hand in a release build ([performance-budgets.md](performance-budgets.md)).
- **The Windows path cases ran here for the first time through the tools,** and passed on the first run: both separators, case, device names, the device and extended-length forms, trailing dots and spaces, alternate data streams, drive-relative and root-relative paths, network paths, a path over 260 characters, and a walk past two names that only an extended-length path can make. From the second run on, a test that finds it cannot make what it needs fails in CI instead of passing with nothing checked, so a green job means every case ran.
- **The clean build of `cortex-tools` alone,** on the desktop pinned to 4 cores: 13.7 to 13.8 s in the `dev` profile and 8.4 s in `release`. `cortex-base` alone: 7.7 to 7.8 s and 5.1 to 5.5 s, where it was 7.1 to 7.5 s and 4.7 to 4.8 s before `sha2`.
- **The release build** has 69 crates on Linux, 35 more than step 4's 34: the search libraries bring 26 packages, `sha2` 8, `tokio` 2 and `similar` 1, less what they share. The binary is unchanged at 0.28 MB, because nothing in it calls `cortex-tools` yet.

#### After step 9: configuration and credentials

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
- **`xtask` did not grow.** The configuration's generated files are checked by a test in `cortex-config`, not by the `generated` step, so `xtask` does not build the configuration crate before every job ("Lints and formatting", below).
- **The concurrent-writers test is the slowest of the step:** four writers, each a store of its own, making 28 flushed writes of the credentials file between them. It takes 0.9 s on the desktop with six writers and four rounds, which is why it was cut to four and three. Everything else in the crate is under 0.1 s a test.
- **The clean build of `cortex-config` alone,** on the desktop pinned to 4 cores: 8.4 to 8.6 s in the `dev` profile and 5.8 s in `release` (two runs each), against 7.1 to 7.5 s and 4.7 to 4.8 s for `cortex-base` alone in step 4. Another session's run may have been active on the desktop at the time.
- **The release build** gains 6 packages on Linux (the `toml` family) and 13 on Windows (those, the wrapper's 5, and `windows-sys` with `windows-link`).

#### After step 8: the session store

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

#### After step 7: the agent loop

Step 7 filled `cortex-agent` (about 6,700 lines of source and 6,000 of integration tests: 31 unit and 118 integration tests) and removed the third copy of the property helper from `cortex-tools`. It adds no third-party package to the workspace: `tokio`, `serde_json` and `thiserror` were in the build already, and `cortex-agent` turns on two features of `tokio`, `rt` and `sync`. Measured on the step's pull request on 2026-11-13, rebased on the main branch after step 8:

| Job | Step 5, first run | Step 8, second run | Step 7, first run |
|---|---|---|---|
| `lint-linux` | 62 s | 39 s | 26 s |
| `test-linux` | 51 s | 46 s | 45 s |
| `lint-windows` | 115 s | 47 s | 58 s |
| `test-windows` | 99 s | 87 s | 93 s |
| **Wall time, Linux** | 62 s | 46 s | 45 s |
| **Wall time, Windows** | 115 s | 87 s | 93 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs | 7.2 s (clippy 6.2 s) | 20.6 s (build tests 16.9 s, run 3.6 s) | 17.4 s (clippy) | 50.2 s (build tests 41.6 s, run 8.6 s) |
| Test time against the tier budgets | | tier 1: 5.5 s of 20 s. Tier 2: 5.7 s of 40 s. Tier 3: 0.6 s of 60 s | | tier 1: 12.0 s of 20 s. Tier 2: 15.3 s of 40 s. Tier 3: 2.9 s of 90 s |

- **The step is inside the noise of the runners.** Every job is within eleven seconds of step 8's second run, on the slower OS too, and far under step 5's, which built the HTTP stack from source. The job passed on both OSes on its first run, with no test over its slow mark: 1,009 tests on Linux and 1,016 on Windows.
- **The loop's 149 tests take 0.43 s together on the desktop, run one at a time** (two runs: 0.42 and 0.44 s), 0.12 s on four threads and 0.06 s on all of them. The step's done criterion is under 5 seconds. No other session's run was active on the desktop for these.
- **Nothing in them waits.** A session runs on a single-threaded runtime, the provider is scripted, and a test waits for what it expects to see: an event, a line in the fake tools' log, a gate. The loop itself takes no time from the runtime: its one clock is the fake clock.
- **What takes the time is real threads, not the loop.** Planning a call goes through the blocking pool, because a plan may ask the file system where a path leads, so every test with a tool call crosses to another thread and back. The property over random rounds makes a session with a runtime of its own for each case. It runs 24 cases, 0.03 s on the desktop, so that the nightly run at fifty times the count (0.8 s for the whole crate) stays under the mark at which a test is reported as slow. At 40 cases it took 2.5 s there on a busy machine.
- **Two tests use the real `read` tool on real files,** to show that a result which is thrown away is forgotten by the table of seen files. Everything else uses fake tools that touch nothing.
- **A panic in the loop fails a test at once.** The loop contains its own panics and ends the turn. In a test that is a bug showing itself, so the test's event sink refuses a turn that ended that way, and the test process stops with the panic's message in place of waiting out the 10 s limit.
- **The Windows job ran nothing of this step that Linux did not.** `cortex-agent` has no code of its own for one OS. It was linted for the Windows target on the desktop before the first push.
- **The clean build of `cortex-agent` alone,** on the desktop pinned to 4 cores: 19.0 to 19.5 s in the `dev` profile and 12.4 to 12.7 s in `release` (two runs each), with 71 packages in the build, against 13.7 to 13.8 s and 8.4 s for `cortex-tools` alone after step 6. The difference is `cortex-llm` without its `http` feature, which `cortex-tools` does not build, and the crate itself.

#### After step 11: the process runner and the shell tool

Step 11 added process trees to `cortex-sandbox` (about 1,600 lines), the runner and the `shell` tool to `cortex-tools` (about 4,700 lines with their unit tests, and 2,400 of integration and system tests), and child processes for tests to `cortex-testkit`. It adds no third-party package to the workspace: `libc` becomes a direct dependency and was in every build already, and `windows-sys` gains API families. It is the first step whose tests are mostly real processes: about fifty system tests on each OS are its own (52 on Linux and 48 on Windows as merged). Measured on the step's pull request on 2026-11-13. Its first run was on the base that step 8 left:

| Job | Step 5, first run | Step 7, first run | Step 11, first run |
|---|---|---|---|
| `lint-linux` | 62 s | 26 s | 32 s |
| `test-linux` | 51 s | 45 s | 52 s |
| `lint-windows` | 115 s | 58 s | 55 s |
| `test-windows` | 99 s | 93 s | 105 s |
| **Wall time, Linux** | 62 s | 45 s | 52 s |
| **Wall time, Windows** | 115 s | 93 s | 105 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs | 11.2 s (clippy 9.5 s) | 26.0 s (build tests 21.3 s, run 4.6 s) | 17.9 s (clippy) | 61.1 s (build tests 40.3 s, run 20.8 s) |
| Test time against the tier budgets | | tier 1: 7.6 s of 20 s. Tier 2: 6.4 s of 40 s. Tier 3: 2.4 s of 60 s | | tier 1: 11.2 s of 20 s. Tier 2: 11.4 s of 40 s. Tier 3: 33.0 s of 90 s |

- **The system tier is where this step shows, and only on Windows:** 33 s of its 90 s, where it was under 3 s. On Linux the same tests take 2.4 s together. The difference is what a shell costs to start: bash starts in a millisecond, and PowerShell 7 and Windows PowerShell in a few hundred, so a test that runs four commands in PowerShell takes one to two seconds. No test passed its 10 s mark. The set for `cmd.exe` takes under a second in all.
- **Every Windows test passed on the first run, and one failed on the second,** which was the fault of the test's own helper and is the lesson of the step: a child that reported to its test and exited at once could lose its report, because Windows may throw away what a process has sent, and the other side has not yet read, when the process ends. The child now waits for the test to answer before it goes on. The run that failed took its whole limit, 30 s, which is what the limit is for.
- **The Windows code ran here for the first time anywhere.** It had only been checked for the Windows target on the desktop. Job Objects with a suspended start, the Ctrl-Break event, cancelling a read, and the three shells' quoting all worked as written. Two things about the hosted runner were open until then, and both hold: a test process has a console to send a Ctrl-Break through, and PowerShell 7 (7.6.9), Windows PowerShell 5.1, `cmd.exe` and Git Bash are all there. A test that finds a shell missing fails in CI, so a green job means every shell ran.
- **Nothing waits.** The time limit of a command, the grace between two stages of stopping and the grace for what a command left running are waits on the runner's timer, which a test ends by hand. A child process is seen to start and to die through a loopback connection. The system tier, of which the step's tests are nearly all, takes 0.9 s on the desktop.
- **A process test is run a few hundred times before it is trusted.** One run of the suite passed, on both OSes, with four races in it: three in tests, and one in the code. Running the three crates' tests 60 to 250 times over on the desktop, with every core busy, found them: a harness killed in the instant after it started a command left a tree the guard had not been told of (3 runs in 60), which is now closed in the code ([decision 0030](decisions/0030-process-tree-control-in-cortex-sandbox.md)); a test counted a background process that `dash` gives an empty input, so that it exits at once; a test interrupted a child before the child had printed the word the test then looked for; and a test read a file before the process writing it had written all of it. After the fixes, 250 runs passed without a failure. A test that starts a process, waits for it, and then asserts on something a third party does is the shape to distrust: wait for the very thing the assertion is about.
- **Breaking the code fails the tests.** Forty-five rules of the step were each broken on purpose, one at a time, and the tests run: every one failed at least one test. The list is in the step's report.
- **What the hosted runners measured of starting a command,** in the unoptimised `ci` build, 30 runs: on Windows, `cmd.exe` alone 6.7 ms, and 16.1 ms through the runner; on Linux, bash alone 1.5 ms, and 5.6 ms through the runner. The Linux figure is what led to reading less of the process table ([performance-budgets.md](performance-budgets.md)): the desktop, with thirty processes, had hidden what a machine with a few hundred shows.
- **The clean build of `cortex-tools` alone,** on the desktop pinned to 4 cores: 14.1 s in the `dev` profile and 8.1 s in `release`, with 54 packages, against 13.7 to 13.8 s and 8.4 s after step 6. `cortex-sandbox` alone: 8.6 s and 5.0 s, with 27 packages, nearly all of it `cortex-base` and what that needs. No other session's run was active: the desktop's test lock lets one run at a time.

#### After step 10: the editing tools and the plan

Step 10 added `write`, `edit`, `plan` and `remember` to `cortex-tools` (about 5,600 lines of source with their unit tests, and 2,400 of integration tests), the open that follows no link to `cortex-base`, a marker and a round number to the tool contract with their few lines in `cortex-agent`, and the first tests of `cortex-core`. It adds no third-party package to the workspace: `libc` becomes a direct dependency of `cortex-base` and was in every build already. 117 tests are its own. Measured on the step's pull request on 2026-11-13, on the base that step 11 left:

| Job | Step 7, first run | Step 11, first run | Step 10, first run | Step 10, second run |
|---|---|---|---|---|
| `lint-linux` | 26 s | 32 s | 32 s | 30 s |
| `test-linux` | 45 s | 52 s | 59 s | 50 s |
| `lint-windows` | 58 s | 55 s | 60 s | 48 s |
| `test-windows` | 93 s | 105 s | 132 s | 96 s |
| **Wall time, Linux** | 45 s | 52 s | 59 s | 50 s |
| **Wall time, Windows** | 93 s | 105 s | 132 s | 96 s |

| | `lint-linux` | `test-linux` | `lint-windows` | `test-windows` |
|---|---|---|---|---|
| The job's command once `xtask` runs (first run) | 10.1 s (clippy 8.8 s) | 30.0 s (build tests 24.6 s, run 5.4 s) | 18.8 s (clippy) | 83.4 s (build tests 54.7 s, run 28.7 s) |
| The same (second run) | clippy 8.9 s | 25.2 s (build tests 20.4 s, run 4.8 s) | 14.9 s (clippy) | 56.7 s (build tests 35.7 s, run 21.0 s) |
| Test time against the tier budgets (first run) | | tier 1: 8.0 s of 20 s. Tier 2: 9.1 s of 40 s. Tier 3: 2.4 s of 60 s | | tier 1: 15.8 s of 20 s. Tier 2: 24.2 s of 40 s. Tier 3: 42.5 s of 90 s |
| The same (second run) | | tier 1: 6.3 s of 20 s. Tier 2: 8.9 s of 40 s. Tier 3: 2.1 s of 60 s | | tier 1: 9.9 s of 20 s. Tier 2: 17.0 s of 40 s. Tier 3: 32.0 s of 90 s |

The two runs are of nearly the same tests on two machines (the second has two tests made lighter), and differ by a third on Windows: read every Windows number here with that in mind.

- **Every job passed on its first run, on both OSes,** and again on the second, with 1,274 tests on Linux and 1,267 on Windows, and none of the step's own over its slow mark. The one slow test of the first Windows run, at 2.0 s, is `cortex-session`'s test of an entry over the line limit, which this step does not touch.
- **The Windows-only code ran here for the first time anywhere,** and passed: the open that asks for a reparse point itself and reads its tag, against a real symbolic link to a file and to a directory and a real junction; `edit` and `write` against a file another program holds with no sharing, and against one it holds for reading, where the replace is tried seven times on the fake clock and the file is left byte for byte; a file let go during the retries; the read-only attribute; and a path in another case with the other separator. The module could only be type-checked for the Windows target before this run.
- **The integration tier on Windows is where the step shows:** 24.2 s and 17.0 s of 40 s in its two runs, where step 11's run had 11.4 s. The step's tests take 2.5 s together on the desktop. What they do is make, read, replace and compare real files, a few hundred times in the properties, and each of those costs several times as much on a Windows runner. The unit tier there was at 15.8 s of 20 s in the first run, which is the nearest any tier has been to its mark, and at 9.9 s in the second: the step's unit tests are properties of pure logic (0.4 s together on the desktop), and that tier has varied by a half between runs of the same tests before (8.6 s and 13.9 s in step 8). If it warns, the number of cases is the dial.
- **Nothing waits.** The retries of a replace are waits on the fake clock. The tests that swap a file for a FIFO run the call on a thread of their own and bound the wait for it, so that an open that waits is a failed test and not a hung one; none that passes waits that bound out.
- **The tests were run two hundred times over before the first push,** on the desktop with every core busy, as step 11 learned to: the five crates' whole suites, with no failure. One run of a mutation pass had shown one race first, in a test of this step (it told the plan that a notice was delivered a moment after the model's next call could already have run), which now holds the scripted response at a gate.
- **Breaking the code fails the tests.** Forty-two rules of the step were each broken on purpose, one at a time, and the tests run: every one failed at least one test. The list is in the step's report.
- **The property of undo runs few cases,** twelve, because each is a session, a workspace and a runtime of its own (about 40 ms a case on the desktop). The nightly run multiplies it by fifty.
- **The clean build of `cortex-tools` alone,** on the desktop pinned to 4 cores: 14.3 to 15.1 s in the dev profile and 8.5 to 8.8 s in release (two runs each), with 54 packages, against 14.1 s and 8.1 s after step 11. `cortex-base` alone: 7.9 to 8.8 s and 5.0 to 5.2 s, against 7.7 to 7.8 s and 5.1 to 5.5 s. Another session's run may have been active on the desktop.

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
| `nightly` (`.github/workflows/nightly.yml`) | Every night at 03:17 UTC, and by hand | `cargo xtask advisories` (`cargo deny check advisories`); `cargo xtask unused` (cargo-machete); `cargo xtask size --check` for every release target, each on a runner of its own architecture: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc`; and, since step 4, tiers 1 and 2 on Linux with fifty times the property-test cases ("Property tests", above); and, since step 14, coverage-guided fuzzing of the stream decoders for two minutes a target (`cargo xtask fuzz --seconds 120`). | Tier 6: a fuzz target for each later decoder, the long-session memory test. Tier 5 where a key is configured. |
| `release` | On a version tag | Not built yet. | Builds, checks and publishes binaries with checksums. |
| `live` | By hand | Not built yet. | Tier 5 and fixture recording. Uses a protected environment for secrets. |

- **The jobs call `cargo xtask`,** like the blocking ones, so the same commands run at a desk.
- **Fuzzing is two things.** The first is the nightly property run. Step 5 built the first decoder, the SSE parser, and fuzzes it with a property: the pieces of the format in any order, with bytes that are not text among them, cut at random, must never panic and must give the same events however they are cut. Pull requests run 400 such inputs and the nightly run 20,000. Step 14 added the same for the Anthropic decoder, from the pieces of real streams with some of them damaged. That is random testing: it does not follow coverage.
- **The second, since step 14, is coverage-guided fuzzing,** in the `fuzz` job of the nightly workflow.
  - **The targets** are a package of their own, `fuzz/`, outside the workspace, with its own lockfile: the fuzzer needs a nightly compiler and a C++ compiler for its runtime, and neither belongs in the blocking checks or in what ships. There is one target for the SSE parser and one for each provider's decoder; so far, Anthropic's.
  - **What a target checks** is written as an ordinary function (`fuzz/src/lib.rs`), which a test on the stable toolchain also calls. The first two bytes of an input decide where the rest is cut, so the fuzzer explores the cuts as well as the content. For both targets: no input panics, and the same bytes give the same result however they are cut. For the decoder also: a response ends at most once and nothing follows its end; a block ends only after it started; and every block the decoder stores is one JSON object, so that it can be sent back.
  - **The corpus** each run starts from is the recorded fixtures, each once whole and once in small pieces. Nothing the fuzzer finds is kept between runs yet.
  - **Running it.** `cargo xtask fuzz [--seconds N] [TARGET…]`, which needs a nightly toolchain and `cargo-fuzz`. An input that fails is kept under `fuzz/artifacts/`, and as an artifact of the nightly run, and `cargo +nightly fuzz run <target> <file>` runs it again.
  - **The nightly compiler is not pinned.** A new nightly can break the job without any change here. It blocks nothing, and it opens an issue like any other failure.
  - **The package's lockfile is its own,** and the nightly advisory check, which reads the workspace's lockfile, does not see it. What it adds to the workspace's packages is the fuzzer's runtime and the few crates that brings.
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
- `unsafe` is denied by default and allowed per module in `cortex-sandbox` and `cortex-term`, where the OS interfaces need it, and in one Windows-only module of `cortex-base`, `private::windows`, which creates files with an access list of their own and reads a file's reparse tag ([decision 0029](decisions/0029-owner-only-files-on-windows.md)). Every block has a safety comment. A new place needs a decision record.
- Public items are documented (`missing_docs`).
- Clippy runs on Linux and on Windows, so that code compiled only for Windows is linted.

**The repo rules** are the checks no off-the-shelf tool does. `cargo xtask rules` runs them, and a test runs them again on both OSes:

- no file contains an absolute home-directory path (every file is scanned, binary files included);
- the crate dependency rules of [architecture/01-workspace.md](architecture/01-workspace.md), read from `cargo metadata`, and the manifest conventions (the `cortex-` prefix, `publish = false`, shared lints, no doc tests, one integration-test binary);
- `cortex-protocol` depends on serde and serde_json and nothing else, and on schemars only as an optional dependency behind its `schema` feature;
- `docs/dependencies.md` agrees with the manifests;
- workflow actions are pinned by commit hash; every workflow's token is read-only; a workflow that runs on pull requests uses no secret and can write nothing; and the only write permission anywhere is `issues: write` on the `report` job of a workflow that does not run on pull requests ("Not blocking", above).

`cargo xtask check` also has a `generated` step: the protocol's JSON Schema and TypeScript declarations in `docs/protocol/` must be what the types in `cortex-protocol` generate. A test makes the same comparison on both OSes. `cargo xtask schema` regenerates them.

**The configuration's generated files** are `docs/configuration/schema.json` and `docs/configuration/reference.md`, generated from the settings declared in `cortex-config`. They are checked by a test in that crate, on both OSes, and not by the `generated` step: `xtask` is built before every CI job does anything, and checking them there would make it build the configuration crate and its TOML parser each time. `cargo xtask gen` writes them, by running that test in its writing mode.

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
| `schema` | Regenerates the protocol's JSON Schema and TypeScript declarations in `docs/protocol/`. `schema --check` only compares. |
| `gen` | Regenerates every generated file: what `schema` writes, and the configuration schema and reference in `docs/configuration/`. |
| `size`, `unused`, `advisories` | What the non-blocking workflows run: the `dist` build with its size and crate count (`--check` fails over the budget, `--target` names the target), cargo-machete, and `cargo deny check advisories`. |
