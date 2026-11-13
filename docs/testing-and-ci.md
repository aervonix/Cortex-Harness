# Testing and CI

The test suite and CI are designed for speed from the first commit. The rule that shapes everything here: **the blocking pull-request checks finish in under 5 minutes per OS on GitHub-hosted runners.** Anything slower runs in a tier that does not block.

## Test tiers

| Tier | What | Where it runs | Blocking? | Time budget |
|---|---|---|---|---|
| **0. Static** | Formatting, lints, licence and source checks, generated-file freshness, repo rules (no home-directory paths, dependency list in sync, crate dependency rules). | Every pull request. | Yes | Under 2 min on a warm cache. |
| **1. Unit** | Pure logic inside each crate: parsers, decoders, mappers, policy evaluation, text layout, view functions. Property tests. Snapshot tests. | Every pull request, both OSes. | Yes | The whole tier in under 20 s of test time above the start of its processes ("Budgets are enforced", below). |
| **2. Integration** | Several crates together, in-process, with the scripted provider, fake tools or real tools on a temporary workspace, the fake clock and the virtual terminal. The agent loop, sessions, context, permissions, the RPC conformance scenarios, the TUI against the virtual terminal. | Every pull request, both OSes. | Yes | Under 40 s of test time above the start of its processes. |
| **3. System** | The real binary as a process: headless runs against the scripted SSE server, the TUI under a pseudo-terminal, process-tree and sandbox behaviour, extension scripts and MCP fake servers. | Every pull request, both OSes. | Yes | Under 60 s of test time above the start of its processes on Linux, 90 s on Windows. |
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
- **A call that should end by itself is given a bound.** The shell tool's tests run such a call with nobody to end a wait for it. If the runner then waits for something that is not there (a grace for a process the command did not leave), the wait never ends on a hand-driven timer, and the test used to be stopped a minute later with nothing to read. The bench now lets every wait go after its limit and fails the test with the waits the runner was in.
- **The process runner waits on a timer trait,** like the retry layer. Its tests give it a timer whose waits end when the test ends them, by their length: the time limit of a call, the grace between two stages of stopping, the grace for what a command left running. A test of a two-minute time limit takes as long as the processes take to die. A call that does not end by itself runs on the test's thread while a director on a second thread waits for children and ends waits, and a director that fails lets every wait go, so that a failed assertion is a failed test and not a hung one.

**Budgets are enforced.** This is rule 13 of the build plan.

| Tier | A test is reported as slow at | A test is stopped, and fails, at | The tier's tests together |
|---|---|---|---|
| 1. Unit | 2 s | 10 s | 20 s |
| 2. Integration | 2 s | 10 s | 40 s |
| 3. System | 10 s | 60 s | 60 s on Linux, 90 s on Windows |

- The limits on one test are nextest's `slow-timeout` and `terminate-after`, set per tier in `.config/nextest.toml`. They hold in every profile, at a desk as in CI, except `properties`, the nightly run with fifty times the property cases, whose limits are scaled for it ("Property tests", below).
- The tier totals are the sum of the tests' own times, not the wall time of the run. `cargo xtask ci test` reads them from nextest's JUnit report after the run, prints them with the share of the budget used, and writes them to the CI job's summary page.
- **A tier is held to its budget above the start of its tests' processes** (the owner's decision of 2026-11-13). nextest runs each test in a process of its own, and starting one costs about 5 ms on a hosted Linux runner and 6 to 12 ms on a Windows one, by the runner's speed. For a thousand unit tests that was 6 to 12 s of the unit tier's 20 before any test did anything, so the raw total told which runner a run drew more than how its tests did: one tree gave 17.0 s on a fast runner and 22.7 s on a slow one.
  - **The start cost is measured by each run:** the time that 1% of its unit and integration tests were quicker than. The quickest tests of a run do next to nothing, so their time is the start of a process. Not the single quickest, which one lucky test moves by a millisecond and with it the whole tier: in one job that ran one tree twice with each of two settings, the unit tier's figure moved by 1.1 s between the two runs of one setting with the quickest test as the floor, where the 1st percentile and the raw totals did not move. System tests are left out of the measure: they start processes of their own. A dedicated empty test in each binary was weighed against it: it would be one sample where the percentile stands on about eighteen, and a test every binary must keep.
  - **A run of a few crates, or of a filter's tests, measures its own start,** and its quickest tests may do real work: the report says so when it has fewer than 500 unit and integration tests to measure on. CI runs the whole workspace.
  - **Each test's time above that cost is added up,** a test that took less counting as nothing, and the warning at the budget and the failure at one and a half times apply to that sum. The same on both OSes: on Linux it takes about 5 s off a unit tier of 13.
  - **The report prints both totals** and the cost it took off: `process start: 11 ms a test (the 1st percentile of the run's 1857 unit and integration tests' times)`, then for each tier its tests, its time, its time above the start, its budget and the share of it used.
  - **What it does not even out.** A slow runner is slower at the tests' work as well as at starting them. On the same tree the unit tier read 11.0 s above the start on a fast runner and 11.6 s on a slow one, within noise, where the raw totals were a third apart; the integration tier read 23.1 and 31.3 s, 28.2 and 40.6 s raw. Its tests do far more than start, and the budget is right to see that. The two runs are kept as fixtures, `xtask/fixtures/tiers/windows-fast-runner.xml` and `windows-slow-runner.xml`, and a test holds the figures.
- **The report lists each tier's slowest tests,** five of them in CI, with the share of the tier's time they are. A CI log shows no time for a test under two seconds, and a tier's total is made of hundreds of those: this is where to look when a tier grows. "A CI run's test times", below, has how to get the rest.
- **A tier over its budget is a warning; a tier over one and a half times its budget is a failure.** Both are annotations on the pull request. The gap is there because hosted runners vary by tens of percent from one run to the next, and more on Windows (the same build took 60 to 82 s on four Windows runners in step 2). A failure at exactly the budget would turn a tier that is near its limit into red builds at random, which teaches people to re-run instead of to fix. The warning is the signal to act; the failure is for a tier that is over on any runner. The limits on a single test stay hard failures: they are several times what a healthy test takes.
- A test over its slow mark needs a reason or moves to tier 6.

**Property tests** use no property-testing crate. The pattern was set in step 4 (`crates/cortex-base/src/cases.rs`), and steps 6, 10 and 12 follow it. Since step 5 the helper every crate uses is `cortex_testkit::cases`. `cortex-base` keeps its own private copy for its unit tests, because it cannot depend on the test kit, which depends on it; a change to one is a change to both. `cortex-tools` had a third copy while step 6 was built beside the test kit; step 7 removed it, and the crate's unit tests use the shared one.

- A property is a function from a generated input to "fine" or "what was wrong". A generator is a function from a random-number generator to an input. `check(name, cases, generate, property)` runs the property on that many inputs.
- **Every run is the same.** Each case draws from the seeded generator of `cortex-base` with a seed made from the property's name and the case number. There is no randomness between runs or machines, so a property test cannot be flaky.
- **A failure prints the property, the seed, the case number and the input.** `CORTEX_PROPERTY_SEED=<seed>` runs that one case again, alone.
- **`CORTEX_PROPERTY_SCALE=<n>` multiplies every property's number of cases.** Pull requests run at 1, which keeps the properties of one crate to well under a second of the unit tier's 20. The `nightly` workflow runs tiers 1 and 2 at 50. The first time this was tried it found a case the default count had not (a wrong expectation in a wrapping test), which is the point of it. At 50 a property takes fifty times as long, so the nightly run has a nextest profile of its own, `properties`, whose limits on one test are scaled for it: slow at 60 s, stopped at 300 s. The limits pull requests run with are not changed. A property of pure logic is far from either (the slowest of step 4 takes 1.8 s at 50 on the desktop); one that does real file work for each case is not: the rewind property of `cortex-session`, a session and a workspace per case, takes 0.75 s at 1 and 32 s at 50 on the desktop, and passed the 10 s limit in the nightly run of 2026-11-13 (issue #19).
- **There is no shrinking,** so generators prefer small inputs, and an input is printed whole. In exchange the helper is 60 lines, adds no dependency and nothing to the build.
- **Small spaces are checked exhaustively** in addition, where that is possible, in the nightly run and by a sample on a pull request (the next point): every sequence of up to six path steps from a small alphabet, every split of a byte stream into two and into three chunks.
- **An exhaustive test that takes a noticeable part of a second checks a sample on a pull request and all of it in the nightly run.** It asks `cases::exhaustive()`, which holds when `CORTEX_PROPERTY_SCALE` is above 1. Its own comment says what the sample is, and the sample must reach every kind of case: path sequences of up to five steps, the calendar's days from 1600 to 2399 with the two ends of the range, every third second cut of a stream. A property's number of cases is cut the same way when it takes a tenth of a second on a Windows runner. The nightly run multiplies whatever the number is by fifty, so a cut there is a cut of the nightly run too, by the same share: the properties cut on 2026-11-13 run 40 to 50% of the cases they did, in both.

**A comparison with the real thing is a property too.** Where the code models another program, the test asks that program. Step 12's command analysis says "this line is these commands with these arguments", and a rule is matched against that, so `crates/cortex-policy/tests/it/differential.rs` generates lines, keeps the ones the analysis models, and compares: bash and `sh` are run with each command replaced by a function that prints its arguments, and PowerShell is asked to parse each line with its own parser. They are system tests, because they start a shell, and `CORTEX_PROPERTY_SCALE` multiplies their lines like any property's cases. On its first run the comparison found that dash reads `20<file` as an argument and a redirection where bash reads a descriptor. A shell that is missing skips its half at a desk and fails it in CI.

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

**A directory of a test's own** is `cortex_testkit::Scratch`, made with the `scratch!` macro: a new, empty directory under the one Cargo gives the crate's integration tests, with helpers to write files and to make a git repository whose configuration is the test's own and not the machine's. It is removed when the test ends, and left in place when the test fails. A test that needs "no repository above this directory" asks for `Scratch::outside`, in the system's temporary directory. The tests of `cortex-config`, `cortex-policy`, `cortex-session` and `cortex-agent` use it. The ones that still have their own are listed in [steps/tooling-w5/answered.md](steps/tooling-w5/answered.md).

**A crate's own unit tests cannot use test-kit types that are made of that crate's types.** The test kit depends on `cortex-llm`, so in `cortex-llm`'s unit-test build there are two copies of the crate, the one under test and the one the test kit was built against, and a `ModelEvent` of one is not a `ModelEvent` of the other. So `cortex-llm` tests its pure parts (the parser, the retry rules, the catalog, cost) as unit tests, and its client, which needs the scripted transport and the fake timer, in its integration-test binary. The parts of the test kit that involve only `cortex-base` types, such as the property helper, work in unit tests too.

**Fixtures.** Recorded provider responses live in the repo under each protocol's `fixtures/` directory: the raw response stream, the request that produced it, and a note. `xtask record` captures them from a real provider by hand, scrubs credentials and identifiers, and is never run in CI. Each provider step adds fixtures for every case in its checklist ([build-plan.md](build-plan.md)).

As built in step 14, for the first protocol:

- **Where.** `crates/cortex-llm/fixtures/<protocol>/<name>/`, with four files: `request.json` (the body that was sent), `response.sse` (the response body, byte for byte), `response.json` (the status and the headers that were kept) and `note.md` (what it shows, the model, and how it was made). `.gitattributes` keeps every file under a `fixtures` directory exactly as it is, line endings included.
- **Recorded, or written by hand.** A case that the real API produces on request is recorded. A case that cannot be provoked (an overload in the middle of a response, a refusal, a server-side tool, a stop reason from a newer API) is written by hand from the provider's documented format, and its note says so. Of Anthropic's 32 fixtures, 16 are recorded. Of the 69 of `openai-chat`, 53 are recorded, from OpenRouter, DeepSeek and Venice.ai; the 16 written by hand are OpenAI's seven and the local servers' five, for which there was no key and no server, and four cases that cannot be provoked (two failures in the middle of a response, a stop reason, and a fast tier's refusal). A fixture written by hand because its server was not there says so in the first line of its note, so that a recording can replace it.
- **The requests are shared.** The recorder builds each request with the same function the offline tests use. So a fixture's `request.json` is a snapshot of a request the real API accepted, and a test fails when the builder stops writing it. A recorded conversation is rebuilt the same way: each later request from what the decoder made of the response before it, and compared, byte for byte, with what was sent.
- **Recording.** `cargo xtask record [--provider PROVIDER] [SCENARIO…]` runs an ignored test of `cortex-llm` (the `live` module of the protocol's tests), and `cargo xtask live [PROVIDER]` the smoke test beside it. The provider is `anthropic` when none is named, or `openrouter`, `deepseek` or `venice`. What the live tests of every protocol share (the transport that keeps what passes through it, the key, the spend limit) is `tests/it/live.rs`. They are there and not in `xtask` because `xtask` is built before every CI job, and the HTTP and TLS stack is the largest dependency the project has. No CI job runs ignored tests.
- **The key.** Each provider has a variable that names a file holding its key and nothing else: `CORTEX_LIVE_ANTHROPIC_KEY_FILE`, `CORTEX_LIVE_OPENROUTER_KEY_FILE`, `CORTEX_LIVE_DEEPSEEK_KEY_FILE`, `CORTEX_LIVE_VENICE_KEY_FILE`. The key is never an argument and never in the environment. Without the variable both commands say so and stop: they do not pass, and they do not skip.
- **What is taken out** (`cortex_testkit::fixture::Scrubber`). No request header is kept at all. Of the response headers only a short list is kept: the content type, `retry-after`, the request id and the rate limits of the model and of the fast tier. **The size of each rate limit is made up:** every `…-limit` is replaced by a round number chosen by what the limit counts (1,000 requests, 1,000,000 tokens, 100,000 output tokens), and every `…-remaining` by the same share of it, so that "all of it", "none of it" and "one request used" still read as they did. The ids the provider makes up (`msg_…`, `req_…`, `toolu_…`) are replaced by stand-ins, the same stand-in wherever the id appears, so that a tool result still names its call. The opaque payloads of reasoning blocks are replaced the same way: they are bound to the account that produced them. Every value of such a member that is not empty is replaced, whatever it looks like, and the member is also found where a block is quoted inside a string.
- **The servers of `openai-chat` write identifiers every which way** (a prefix and a hyphen, a bare UUID, a call id with underscores in it), so a recording of one finds them by where they are: the value of every `id` and `tool_call_id` member is an identifier, and is replaced wherever it appears afterwards, in any later exchange too. Of their headers the six `x-ratelimit-…` ones and `x-request-id` are kept, with the sizes made up in the same way. A UUID is an identifier wherever it is: one provider quotes its request id inside the message of a failure, where no member names it. The value of a request-id header is one whatever it looks like. **A stand-in is never replaced again:** a recording that quotes an earlier fixture (the conversation a second provider is handed) keeps that fixture's stand-ins.
- **A fixture says nothing about the account that recorded it.** The sizes of its rate limits would show its usage tier, which is why they are made up. A test checks that every limit in every fixture is the value the scrubber writes.
- **A recording that holds the key is refused.** Everything that is about to be written, and every line that is about to be printed, is searched for the key and for any twelve characters in a row of it. If it is there, nothing is written and nothing is printed. The refusal is tested with a made-up key, in the request, in the response, in a header that is kept and in the note.
- **What a run costs.** Each call prints what it used and what that cost at the catalog's prices, and a run stops before it has spent half a dollar. Recording all of Anthropic's fixtures cost about one and a half cents. Where a response states its own cost (OpenRouter, Venice.ai), that is the figure counted, and a run ends by asking the provider's usage endpoint how far its own figure moved; only the difference is printed. Recording the `openai-chat` fixtures cost under a cent for all three providers together.

**Replays at every chunk boundary.** Stream decoders are tested by replaying each fixture split at every byte offset, which catches the bugs that only appear when a network read ends in the middle of a token.

- Each stream fixture is decoded whole, one byte at a time, in pieces of several fixed sizes, and cut in two. The events must be the same every time, and what they are is in a snapshot for each fixture.
- **The work is bounded, and divided between two places,** as step 4 learned it must be. Cutting a stream of `n` bytes in two at every byte decodes `n` bytes `n` times, in a build that does not optimise the JSON reader. For the 26 streams of step 14 that is 200 MB of decoding. On the step's first CI run it took 22 s of test time on Linux and about 40 s on Windows, and put the integration tier over its budget there (51.6 s of 40 s, a warning). So:
  - **on every pull request,** each stream is delivered one byte at a time, which puts a boundary at every byte of it, and in pieces of two other sizes; and it is cut in two at each of its first 64 bytes and at about fifty places spread over the rest (a hundred places and seven sizes until 2026-11-13, when the Windows integration tier was at its budget);
  - **in the nightly run,** which already multiplies the property cases and has time limits of its own, each stream is cut in two at every byte, and delivered in pieces of seven other sizes.
- **The streams of `openai-chat` are larger**: one records 104 kB for 385 tokens, because the server sends a chunk of 300 bytes for each token of reasoning. So for them the number of cuts goes down as a stream gets longer (between 8 and 50 on a pull request), a stream over 16,000 bytes is cut in two at about 4,000 places in the nightly run and not at every byte, and one test replays all of a server's streams: a process for each stream cost more than the decoding did. Measured in CI's own build profile on the build machine before the push, the sum of the protocol's test times was 2.8 s against 2.4 s for Anthropic's (3.8 s before the streams of a server shared a test).

**Golden transcripts.** Sequences of protocol messages from released versions are kept and replayed against current types, to catch accidental breaking changes to the RPC protocol. The first one, written by hand in step 3, is `crates/cortex-protocol/tests/golden/v1-session.jsonl`: every method, every event type and every kind of item, part, delta, notice and plan edit of protocol version 1. Three tests use it: each line must read into its typed form and write back as the same JSON; the same must hold after a field no version knows is added to every object; and each line must be valid against the generated schema ([architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md)). A transcript is not edited to make a test pass. The types that travel in no message have golden values in a second file beside it, and a test fails, naming the type, when a type in the schema has a value in neither. An addition to a released version gets a new golden file.

**No paid API in CI.** Pull-request jobs have no secrets. The scripted fakes and fixtures cover everything that blocks.

**Usage records from other tools.** The adapters in [architecture/18-usage.md](architecture/18-usage.md) are tested only against small, made-up example files written for the purpose: one set per format version an adapter supports, plus damaged and unknown-version examples. Real session data from any tool is never used as a fixture and never committed. The static check that rejects home-directory paths covers the fixture folders, and an adapter test asserts that no text from the input reaches the output.

## Showing that the tests bite

A test that passes says little until it has been seen to fail. Two things are checked before a step is reported, with a command for each: that a test does not pass only because of the order things happened to run in, and that it fails when the rule it tests is broken.

### A seeded order of events

**Where the order came from.** The loop plans each tool call, and bounds each result, on the blocking pool (`cortex_tools::run_blocking`). Those are real threads, and they finish when the machine lets them: a call's plan arrives before or after the response's next event, and one call's plan before or after another's. The loop itself looks at what is ready in a fixed order and picks nothing at random. A test of step 12 failed three runs in ten for this reason, and a break of the code was once reported as caught by a test that had only lost that race.

**What a test of the loop does now.** `cortex-agent`'s rig (`crates/cortex-agent/tests/it/main.rs`, `World`) puts that work on the test's own thread, at moments a seeded generator decides. The session already runs on a single-threaded runtime on that thread, so with the blocking work there too, **one seed is one order of events, on every machine.** That holds for work that goes through `run_blocking`, which is all the loop has. A thread that a test or a tool starts by itself is still the machine's to schedule. And work given to `run_blocking` must not wait for another task of the same runtime (a gate, a channel): on the pool it would be let go, and on the test's only thread it never is.

- **The seam** is `cortex_tools::InlineBlocking`, behind the `test-hooks` feature of `cortex-tools`. While one is alive, `run_blocking` called on the thread that made it runs its work on that thread, after a number of turns of the runtime drawn from the seeded generator (none to eight, mostly few). Work whose token fires before its turn comes is not run, and the caller gets "cancelled", as it does from the pool.
- **Only tests have it.** A dev-dependency turns the feature on, and `cargo xtask rules` fails when anything else does. A build of the binary by itself (`cargo build -p cortex-cli`, the `dist` build) resolves `cortex-tools` with no feature, and the released binary has none of it. A build of the whole workspace with its tests (`cargo nextest run --workspace`) resolves it with the feature, for every crate in that build, so the binary the system tests start has the branch compiled in. It does nothing there: the switch is a thread-local that only an `InlineBlocking` sets, and only a test makes one.
- **The seed** comes from `cortex_testkit::seed::for_this_test()`. An ordinary run uses 0, every time. `CORTEX_TEST_SEED=<n>` fixes it. In a run of `cargo xtask stress`, repeat `n` uses the first seed plus `n`.
- **A failed test shows its seed.** The rig prints `test seed 17: run this test again in the same order with CORTEX_TEST_SEED=17`, and the test runner shows a test's output when it fails.
- **A test must pass under every seed.** It asserts on what the loop promises (the results are recorded in the model's order, a refused call stops the turn at that call), and where it needs one order it makes it, with a gate. It does not assert on an order the seed decides.

When the rig was seeded, every test of `cortex-agent` passed under seeds 0 to 299: none depended on an order.

**Other rigs.** A crate whose tests drive the loop or the tools on a single-threaded runtime (the core's, from step 13) takes the same two lines: `cortex-tools` with `test-hooks` in its dev-dependencies, and an `InlineBlocking::seeded(cortex_testkit::seed::for_this_test())` kept for the life of the test. A test whose order is the operating system's to decide (processes, sockets, threads of its own) cannot be seeded, and is repeated instead.

### Running the tests many times over

`cargo xtask stress -p <crate>… [--count N] [--first-seed S]` runs the crates' tests `N` times over (100 when not given), and reports each test that failed on some repeat:

```text
1 test(s), 40 time(s) each, with seeds 0 to 39.

FAILED 20 of 40 times: cortex-agent::it order::stress_fixture_expects_the_first_sessions_call_to_start_first
  seeds: 0, 1, 2, 6, 7, 8, 11, 14, 16, 17, 18, 22, and 8 more
  again: CORTEX_TEST_SEED=0 cargo xtask test -p cortex-agent -E 'test(=order::stress_fixture_expects_the_first_sessions_call_to_start_first)'
```

That is the command's own test case, a test that is wrong on purpose (below). Seed 0 fails it every time and seed 3 passes it every time.

- The repeating is nextest's (`--stress-count`, in a nextest profile of its own, `stress`). The command sets the first seed, reads which repeat of which test failed from the JUnit report, and says how to run one failure again. Anything it does not know goes to nextest: a filter, `-E`, `--run-ignored`.
- **A test that passed only on its retry counts as failed for that repeat.** nextest gives a test in a `timing_sensitive` module one retry, and reports it as passed when the retry passes. A test that needs its retry some of the time is what this command looks for.
- **For a seeded rig** each repeat is another order, and the seed brings a failure back exactly. **For process and thread tests** each repeat is another roll of the machine's own dice, and the seed means nothing: a test that fails one time in two hundred there is still a test with a race in it.
- **Run it before reporting a step,** for `cortex-agent` and for every crate whose tests start processes or cross threads: `cargo xtask stress -p cortex-agent --count 300` takes 17 s on the desktop. Stress first, then break the code: a test that fails by chance makes a break look caught.
- **How it is tested.** Its reading of a report, on reports written for the purpose (unit tests). And by hand on the test above, `order::stress_fixture_…` in `cortex-agent`: it expects the first of two sessions' calls to start first, which nothing promises, so it fails under about half the seeds. It is ignored in every ordinary run; `cargo xtask stress -p cortex-agent --count 40 --run-ignored only stress_fixture` runs it.

### Breaking the code on purpose

A done criterion of the form "X is tested" is evidence only if the test fails when X is broken. Every step since step 5 has broken its own rules on purpose, one at a time, and listed which tests failed. The breaks are now data in the repository, and one command runs them.

**The lists** are `mutations/<name>.breaks`, one file for each step or crate, so that two steps adding breaks at the same time do not touch the same file. A break is a small change that breaks one rule, with the tests that must fail when it is made:

```text
[break] cancellation-not-seen-in-wake
rule: Cancellation is an input: a wait for the response or the round is ended by the turn being cancelled.
file: crates/cortex-agent/src/turn.rs
in: wake
find: cancelled.as_mut().poll(context).is_ready()
replace: cancelled.as_mut().poll(context).is_ready() && false
caught-by:
    transitions::an_interruption_is_taken_in_every_state_that_waits
```

That one is from `mutations/step-07.breaks`.

| Key | What it is |
|---|---|
| `[break] <name>` | Begins a break. Lower-case letters, digits and `-`. It is shown, and chosen, as `<file>/<name>`: `step-12/deny-beats-allow`. |
| `rule` | The rule the break breaks, in a sentence. Required. |
| `file` | The file to change, relative to the repository. Required. |
| `in` | The item the code is in: names separated by `::`, such as `Round::spawn`. An `impl` block answers to every name in its header. Optional. |
| `find` | The code to replace. Required. It must be there exactly once, in the item if one is named. |
| `replace` | What to put there. Required, and may be empty. |
| `occurrence` | Which match of `find` is meant, counted from 1, when there are several. Optional. |
| `packages` | The crates whose tests to run, separated by commas. Without it, the crate the file is in. |
| `caught-by` | The tests that must fail, one a line: a test's name as nextest prints it, or its binary and its name (`cortex-agent::it denials::…`). A `*` at the end stands for any ending. Without it, any failing test catches the break. |
| `expect: survives` | Marks a break that changes nothing a test can see, kept so that nobody tries it again. Its `rule` says why. |

A value is on the key's line, or on the indented lines after a key that ends its line. A line that begins with `#` is a comment. Anything else is an error with its line: an unknown key, a key given twice, a break without a required key.

**`find` is matched by token,** not as text. White space, comments and a comma before a closing bracket are not tokens, so however `rustfmt` lays the code out, the break still applies. An exact-text break stops applying the day a line is wrapped, and that is how breaks were lost in earlier steps.

**A break that does not apply is an error, never a result.** `cargo xtask check` has a step, `breaks`, that applies every break of every list to the tree in memory and fails when one cannot be made: its code is gone, or is there twice, or its replacement is the same code. It reads the files the breaks name and runs nothing (0.1 s for the 300 breaks there are), so it is in the blocking checks: a change that moves code out from under a break updates the break in the same pull request, or deletes it if the rule is gone. The message names the list and the line, and says which.

**`cargo xtask mutate [BREAK…]`** runs the breaks: all of them, or the ones named (a break, a whole list, or the end of a name).

1. Every break is applied in memory, as `check` does. Nothing is built if one fails.
2. **The work is done on a copy** of the tree under `target/mutate/`, with a build directory of its own. The working tree is only read. The copy is brought up to date at the start of each run, which also undoes a break that a killed run left in it. The first build of the copy is a whole build; after that each break rebuilds one crate.
3. **The tests are run once with nothing broken,** for each set of packages that a break runs. A test that fails by itself stops the run: it is no evidence about anything. A break is judged against the tests of exactly the build it runs in, because which tests there are depends on what is built together.
4. **Each break is compiled** (`cargo check`) before any break's tests are run, and the ones that do not compile are named.
5. **Each break's tests are run,** and the tests that failed are read from nextest's JUnit report (profile `mutate`), not from its terminal output.

What it says of each break:

| Verdict | Meaning | What to do |
|---|---|---|
| caught by N test(s) | At least one test failed, and every test of `caught-by` did. | Nothing. |
| survived, as its list expects | A break marked `expect: survives`, and no test failed. | Nothing. |
| NOT CAUGHT | No test failed. | Write the test. If nothing a test can see changes, mark the break `expect: survives`. |
| NOT CAUGHT BY ITS OWN TESTS | Some test failed, and one that `caught-by` names passed. | A test that used to catch this no longer does: find out why. |
| NAMES A TEST THAT DOES NOT EXIST | A `caught-by` entry matches no test. | The test was renamed or removed: correct the list. |
| CAUGHT, AND EXPECTED TO SURVIVE | A break marked `expect: survives` made a test fail. | Take the mark off and name the test. |
| DOES NOT COMPILE | The code does not build with the break made. | Correct `replace`. |
| COULD NOT BE JUDGED | The run failed and named no failed test. | Read the log it names. |

It exits 0 when every break did what its list expects, 1 when one did not, and 2 when its time ran out.

**It stops, and carries on.** Every result is written to `target/mutate/results.jsonl` as it is known, under a key made of the code (every file but the lists) and the break. A run does not start new work after its time budget (`--minutes N`, 5 when not given, 0 for none), and the next run starts where it stopped: run the same command until it exits 0 or 1.

- **The budget is on starting work, not on finishing it.** One piece of work, a break's build and tests, is seconds. The first build of the copy is a whole build of the crates named, and runs to its end whatever the budget. After it, a long list gives a shared build machine back every five minutes or so.
- A run that is killed loses at most the break it was in.
- **One run at a time on a copy.** A second run on the same copy is refused while the first holds it: it would bring the copy up to date under the first, which would then report a break as survived that was no longer made. The lock is the operating system's, so a killed run lets go of it. A copy that cannot be locked at all is not used.
- A change to the code forgets every result. A change to a list forgets only the breaks it changed.
- **A run that ended without a verdict is tried again by the next run:** a break that did not compile, or a test run that failed and named no failed test. The cause may have been the machine's. A break that did not compile is reported with the compiler's first errors. A test run that named no failed test keeps its whole output in a file of its own under `target/mutate/logs/`, which the message names.
- `--again` forgets the results of the breaks named, and runs them again. To carry on after the time ran out, leave it off: with it, the next run would forget what this one did. The command says so.
- `--report` prints what is known and runs nothing.

**What it cannot tell.**

- **A break in code the build does not compile is reported as not caught.** A break is made in the text, so it can be made in code that is only built on Windows, or behind a feature that is off, and the tests then pass because nothing changed. Run such a break where the code is built, and name in `packages` a crate whose build turns the feature on.
- **Which tests exist depends on what is built.** `cortex-tools` has tests of the seeded order that are only built when `cortex-agent`'s tests are in the same build, because that is what turns `test-hooks` on. A break that names them in `caught-by` names `packages: cortex-tools, cortex-agent`.
- **"Caught" with no `caught-by` is one failing test in one run.** A test that fails by chance would do. The tests with nothing broken are run once, not many times: stress the crate first, and name the tests a break should fail.
- **`find` passes over a trailing comma,** so `f(a)` also finds `f(a,)`, and `(x)` finds the tuple `(x,)`. Whether a break changes anything is judged with commas counted. A match never begins or ends inside an operator of two characters: `= 0` does not find the second half of `x == 0`.
- **A break's tests are run without the caller's test settings:** under seed 0, with the ordinary number of property cases, and writing no snapshot or generated file. A result is kept by the code and the break alone.

**Adding a step's breaks.** Write `mutations/step-NN.breaks` as the rules are built, run `cargo xtask mutate step-NN`, and put the verdicts in the step's report. A break that nothing catches is a missing test.

**The lists as seeded.** Wave 4's breaks were kept only in the reports of the sessions that made them. The lists of steps 7, 8, 9, 10, 12 and 14 were written again from the pull requests' descriptions, the code's own documentation and the tests: 101 breaks, 14 to 20 a step. Every one is caught, by the tests its list names. All 101 run in under five minutes on the desktop, about a second each once the copy is built. What running them showed is in [steps/tooling-w5/answered.md](steps/tooling-w5/answered.md). This step's own code has a list too, `mutations/tooling-w5.breaks`: 23 breaks of the seeded order, the scratch directory and the commands of `xtask`, written with the code. And `mutations/step-31.breaks` is the 87 breaks step 31's session kept, each as text before and after, converted by a script. All 87 apply and are caught. Six of them no longer applied in the session's own last run, which matched the text exactly: by token, they still find their code.

**How the command is tested.** With a runner that reads the copy and says which tests "fail" for what it finds: three breaks, two that are caught and one that nothing catches, and each verdict in the table (unit tests, in the blocking checks). And against the real cargo and nextest, on a small workspace kept for it (`xtask/fixtures/mutate`), with five breaks: two caught, one not, one expected to survive and one that does not compile. That test builds a workspace ten times over (0.8 s in all on the desktop, several times that on a hosted Windows runner), so it is ignored in the blocking checks. The nightly workflow runs it on Linux, and so does anyone who changes the driver: `cargo xtask test -p xtask --run-ignored only sys_the_real`.

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
| Permissions | Built in step 12. **PowerShell and `cmd.exe` commands** decided on the real file system (`crates/cortex-policy/tests/it/real_paths.rs`): a PowerShell command that only reads runs under any casing and either separator; a file that looks like a secret, a path a rule denies and a file outside the workspace are asked about under any casing; the spellings the resolver refuses (a trailing dot, an alternate data stream, a device name, the device and extended-length forms, a network share, drive-relative and root-relative paths, a provider path such as `Env:PATH`) are asked about, because what they name cannot be checked; more than one command, the call operator and an encoded command are asked about; a deny holds however the command is written. Every `cmd.exe` command is asked about, no allow rule applies to one, and a deny still does. **PowerShell's own parser** is asked about every generated line the analysis models (`differential.rs`). **Case:** an allow rule reaches the folder it names however the path is typed, and a folder that differs only in case, in a directory made case-sensitive with `fsutil`, is outside the scope and not granted by an allow for the other; the project key of one folder is the same under every casing, and two such folders have two keys (`crates/cortex-base/tests/it/paths_windows.rs`). **Files:** the stored approvals and the trust store are owner-only. |
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
| `lint-linux` | `ubuntu-latest` | `cargo xtask check`: `cargo fmt --all --check`; the repo rules; the break lists, each break of which must still apply ("Breaking the code on purpose"); the generated files (the protocol's schema and TypeScript declarations are what the types generate); `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo deny check licenses bans sources`. |
| `lint-windows` | `windows-latest` | `cargo xtask check clippy`. Formatting, the repo rules and cargo-deny give the same answer on every OS and run once. Clippy runs on both, because code compiled only for Windows is otherwise never linted. |
| `test-linux` | `ubuntu-latest` | `cargo xtask ci test`: build the tests, run tiers 1 to 3 (`cargo nextest run --workspace --locked --cargo-profile ci --profile ci`), check the tier budgets and list each tier's slowest tests. Then the JUnit report is kept as an artifact. |
| `test-windows` | `windows-latest` | The same. |

The first draft of this page had three jobs. `lint-windows` was added in step 2: it runs beside the others, so it adds nothing to the wall time, and it closes a gap that would have opened as soon as the first Windows-only module was written.

**The jobs call `cargo xtask`.** What a job runs is defined in the `xtask` crate and nowhere else, so `cargo xtask ci` at a desk runs what CI runs. Every step of a command runs even when an earlier one failed, so one CI round trip reports everything that is wrong. Each step is timed, and the timings go to the log and to the job's summary page.

Settings common to all jobs:

- `--locked` and `CARGO_INCREMENTAL=0`.
- Each job has `timeout-minutes: 5`. That makes the budget a fact: a job that takes longer fails.
- The toolchain version comes from `rust-toolchain.toml` and is installed with `rustup toolchain install`. The version is pinned exactly, so a new stable release never changes the lints under a pull request. Installing it costs 8 to 9 s in every job, cache or no cache (11 to 13 s on Windows before `RUSTUP_HOME` was moved off the system disk).
- **Tests build in cargo's `ci` profile:** `dev`, with third-party crates unoptimised. The reason is in "Where the five minutes go". Our own crates keep line tables, so a failing test's backtrace has line numbers. Two small third-party crates are optimised all the same, `serde_json` and `sha2`: the decoder replays and the session log's tests spend their time in the first, and every file a tool reads is hashed by the second. A third-party crate is built once and then served from the cache, so the exception costs a cold run a few seconds and a warm one nothing. Our own crates are not optimised there, though `cortex-llm` at `opt-level = 1` ran its tests four times as fast on the desktop: it is rebuilt by most pull requests, and the build is where a Windows job's time already goes.
- Dependency caching with `Swatinem/rust-cache`. The key is made of the job, the OS, the toolchain version, the `CARGO_*` and `RUST*` environment, and a hash of `Cargo.lock`, every `Cargo.toml`, `rust-toolchain.toml` and `.cargo/config.toml`. When that hash has no exact match, the newest cache for the same job, OS and toolchain is restored instead, so **a lockfile change rebuilds the changed crates and what depends on them, not everything**. Only a toolchain bump, or caches that have expired, give a truly cold run.
- Only the main branch writes the cache, so pull requests read a cache that is always consistent and never push each other's out of the repository's 10 GB. A scheduled run of the same workflow, twice a week, keeps the caches from expiring.
- On Windows, `CARGO_HOME` and `RUSTUP_HOME` point at a folder on the workspace's disk, not at their default place on the system disk, which is several times slower at unpacking files (measured below).
- **The Windows test job's temporary directory is on the workspace's disk too** (`TEMP` and `TMP`, since 2026-11-13). A test that needs a directory with no repository above it makes it in the system's temporary directory, which is on `C:` by default. Measured in one job, alternating twice: the 53 integration tests of `cortex-core` took 6.3 and 6.2 s with it on `C:` and 3.3 and 3.5 s on `D:`, and the three tiers together about 8 s less ([steps/11/ci.md](steps/11/ci.md)).
- `cargo-nextest` and `cargo-deny` are installed as prebuilt, checksummed binaries, not compiled.
- A new push to a pull request cancels the run in progress.
- Actions are pinned by commit hash. The workflow token has read-only permissions. No secret is used. `cargo xtask rules` checks all three.

**Documentation-only pull requests skip the Rust build and tests.** Each job starts with a step (`.github/actions/changes`) that lists what the pull request changes. If every changed file is under `docs/` or ends in `.md`, the job's remaining steps are skipped and the job still reports success under its own name.

- The decision is made inside each job, not in a separate job that the others wait for. All four checks therefore always report, so they can be required by branch protection as they are, with no gate job, and a pull request that does touch code pays about a second for the question.
- **The repo rules still run on a documentation-only pull request, deliberately.** The check for home-directory paths, the check of `docs/dependencies.md`, the check that every link inside the repository leads somewhere and the check of where the step records are exist mostly for documentation, and skipping them on exactly the pull requests that write prose would open the hole they close. This compiles `xtask`, which is five small crates, and nothing else.
- Anything else in the change set (a manifest, a workflow, an asset, a source file) runs everything. So does every push to the main branch and every scheduled or manual run.
- **The generated files under `docs/protocol/` and `docs/configuration/` count as code.** They are checked against the Rust types by tests, so a pull request that touches them runs the tests, wherever else its changes are.

**nextest configuration** (`.config/nextest.toml`):

- the per-test limits of each tier, from the table under "Test tiers", in every profile;
- profile `ci`: fail fast off, quiet on success, JUnit output for the tier budgets;
- profiles `mutate` and `stress`, for the two commands of "Showing that the tests bite": fail fast off, only failures printed, JUnit output;
- a test group that limits pseudo-terminal and process tests (everything named `sys_…`) to two at a time, so they do not starve each other on a 4-core runner;
- no retries, except tests in a module named `timing_sensitive`, which get one retry and are reported as flaky when it is used;
- profile `fast` for local use: tiers 1 and 2 only.

### A CI run's test times

Each test job keeps nextest's JUnit report as an artifact, `junit-linux` or `junit-windows`, for 14 days, whether the tests passed or not. It has the time of every test, which the log does not.

```text
gh run download <run id> --name junit-windows --dir junit
cargo xtask ci --tiers --from junit/junit.xml --os windows --top 20
```

prints the tier totals of that run against the Windows budgets, and its twenty slowest tests in each tier. Two runs' reports can be compared the same way. The upload is the one step these tools add to a blocking job. What they add in all (that step, three static checks and their own tests) is measured in [steps/tooling-w5/ci.md](steps/tooling-w5/ci.md).

**The same report at a desk.** `cargo xtask ci --tiers [-p CRATE]… [--top N] [-- NEXTEST ARGUMENT…]` builds and runs the tests as the blocking job does, in cargo's `ci` profile, and prints the same report with ten tests a tier. The profile is the point: a desk's own build optimises third-party crates and CI's does not, so a test that spends its time in one (a JSON reader, a regular-expression engine) is several times slower in CI than it looks. Steps 12 and 14 each put a Windows tier over its budget that way. A desk is still faster than a hosted runner, and Windows about twice as slow as Linux there: read the report for which tests the time is in, and for the tier's share of its budget with room to spare.

### A Windows run without a pull request

Code behind `#[cfg(windows)]` is built and run only on a Windows runner, and the blocking checks reach one only through a pull request. The `windows-try` workflow (`.github/workflows/windows-try.yml`) runs a crate's tests on a hosted Windows runner for any branch that has been pushed:

```text
git push origin <branch>
gh workflow run windows-try --ref <branch> -f crate=cortex-tools -f filter=shell::
ci-wait --workflow windows-try
```

- `crate` is one crate, or several separated by spaces or commas; empty is the whole workspace. `filter` is part of a test's name, as nextest takes it; empty is every test.
- **`count` repeats the tests:** `-f count=100` runs `cargo xtask stress --count 100` for the crates, in the blocking job's build, and reports each test that failed on some repeat. It needs a crate. That is how to learn how often a test fails on a Windows runner, before a fix and after it. **A test that passes every repeat there may still fail in the blocking job:** the repeats of one crate do not have the rest of the workspace's tests running beside them.
- It runs `cargo xtask ci --tiers` for them: the build and the run of the blocking `test-windows` job, then the tier report with the twenty slowest tests, and the JUnit report kept as `junit-windows-try`.
- It restores the build cache the main branch wrote for `test-windows` (its job has the same id and environment on purpose) and writes none. It has 20 minutes, not 5: it blocks nothing.
- `ci-wait`, or `gh run watch`, waits for it: it is a run on the branch's head commit like any other. `gh run list --workflow windows-try --branch <branch>` finds it.
- **A workflow can be started by hand only once it is on the main branch.** A branch made before this workflow was merged has to be rebased first.
- Code that needs no run can be checked at a desk without it: `cargo clippy -p <crate> --all-targets --target x86_64-pc-windows-msvc` lints Windows-only code on Linux, for a crate with no C code.

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

#### After each later step

From step 3 on, each step records its CI times, and what its first runs showed, in a file of its own: `steps/<step>/ci.md`, beginning with [steps/03/ci.md](steps/03/ci.md). The folders of [steps/](steps/README.md) sort in step order. A step adds nothing here.

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
| `nightly` (`.github/workflows/nightly.yml`) | Every night at 03:17 UTC, and by hand | `cargo xtask advisories` (`cargo deny check advisories`); `cargo xtask unused` (cargo-machete); `cargo xtask size --check` for every release target, each on a runner of its own architecture: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc`; and, since step 4, tiers 1 and 2 on Linux with fifty times the property-test cases ("Property tests", above); and, since step 14, coverage-guided fuzzing of the stream decoders for two minutes a target (`cargo xtask fuzz --seconds 120`); and `cargo xtask mutate` against the real cargo and nextest, on the small workspace kept for that ("Breaking the code on purpose"). | Tier 6: a fuzz target for each later decoder, the long-session memory test. Tier 5 where a key is configured. |
| `release` | On a version tag | Not built yet. | Builds, checks and publishes binaries with checksums. |
| `live` | By hand | Not built yet. | Tier 5 and fixture recording. Uses a protected environment for secrets. |

- **The jobs call `cargo xtask`,** like the blocking ones, so the same commands run at a desk.
- **Fuzzing is two things.** The first is the nightly property run. Step 5 built the first decoder, the SSE parser, and fuzzes it with a property: the pieces of the format in any order, with bytes that are not text among them, cut at random, must never panic and must give the same events however they are cut. Pull requests run 400 such inputs and the nightly run 20,000. Step 14 added the same for the Anthropic decoder, from the pieces of real streams with some of them damaged. That is random testing: it does not follow coverage.
- **The second, since step 14, is coverage-guided fuzzing,** in the `fuzz` job of the nightly workflow.
  - **The targets** are a package of their own, `fuzz/`, outside the workspace, with its own lockfile: the fuzzer needs a nightly compiler and a C++ compiler for its runtime, and neither belongs in the blocking checks or in what ships. There is one target for the SSE parser and one for each protocol's decoder: `anthropic_decoder` and, since step 31, `openai_chat_decoder`, whose first input byte chooses the kind of server (reasoning under a named member, reasoning in tags inside the text, a model that is asked for a speed tier or one that is not).
  - **What a target checks** is written as an ordinary function (`fuzz/src/lib.rs`), which a test on the stable toolchain also calls. The first two bytes of an input decide where the rest is cut, so the fuzzer explores the cuts as well as the content. For both targets: no input panics, and the same bytes give the same result however they are cut. For the decoder also: a response ends at most once and nothing follows its end; a block ends only after it started; and every block the decoder stores is one JSON object, so that it can be sent back.
  - **The corpus** each run starts from is the recorded fixtures, each once whole and once in small pieces. Nothing the fuzzer finds is kept between runs yet.
  - **Running it.** `cargo xtask fuzz [--seconds N] [TARGET…]`, which needs a nightly toolchain and `cargo-fuzz`. An input that fails is kept under `fuzz/artifacts/`, and as an artifact of the nightly run, and `cargo +nightly fuzz run <target> <file>` runs it again.
  - **The nightly compiler is pinned to a date,** in `fuzz/rust-toolchain.toml` and nowhere else, so that a new nightly cannot break the job without a change here. `cargo xtask fuzz` and the workflow both run inside `fuzz/`, where that file decides the toolchain. **To move it forward:** change the date in that file to a recent nightly, start the nightly workflow by hand on the branch (`gh workflow run nightly.yml --ref <branch>`), and merge when its `fuzz` job passes. Do it when cargo-fuzz or the fuzzer's runtime is updated, or every few months: the sanitizer and the coverage instrumentation are part of the compiler, and an old nightly gets none of their fixes.
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
- workflow actions are pinned by commit hash; every workflow's token is read-only; a workflow that runs on pull requests uses no secret and can write nothing; and the only write permission anywhere is `issues: write` on the `report` job of a workflow that does not run on pull requests ("Not blocking", above);
- a crate's `test-hooks` feature is turned on by a dev-dependency and in no other way: not by a normal or build dependency, not by `default` or another feature of the crate that has it, and not by a feature of another crate that passes it on ("A seeded order of events", above);
- **every link inside the repository leads somewhere:** a relative link in a Markdown file names a file or a directory that is there; a link with a `#fragment` names a heading the page has, by the name GitHub gives it (the text in lower case, without punctuation, a `-` for each space, and `-1`, `-2` for a second and third heading of the same name); and a `docs/….md` path written in prose, in configuration or in a comment of the code names a file that is there. In Rust source only what follows `//` on a line is read, on a line of its own or after code: a string there is as likely to be a test's made-up file. Links to other sites are not followed;
- **the step records and the open items are where the others expect them** ([steps/README.md](steps/README.md), [outstanding/README.md](outstanding/README.md)): a folder of `docs/steps/` is a step's number in two digits with its letter, or a name in lower-case words joined by hyphens; it holds `answered.md` and `ci.md` and nothing else; each begins with the heading its kind has, naming the step the folder is for; and a file of `docs/outstanding/` is named in hyphenated words and holds the table's header and one row.

`cargo xtask links` runs the last two alone. They read text files and ask cargo nothing, so they answer at once: under a tenth of a second for the 137 pages there are.

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
| `check` | The static checks: `fmt`, `rules`, `breaks`, `generated`, `clippy`, `deny`. All of them run even when one fails, and the time of each is printed. |
| `test` | All tests. Arguments go to nextest: `--profile fast` for tiers 1 and 2, `-p cortex-agent` for one crate, and then only that crate is built and run. |
| `ci` | What the blocking jobs run: `check`, then the tests in the `ci` profiles, then the tier budgets. |
| `ci --tiers` | The tests as CI builds them, and each tier's time and slowest tests ("A CI run's test times"). |
| `stress` | The tests of the crates named, many times over, each time under the next seed ("Running the tests many times over"). |
| `mutate` | The breaks of `mutations/*.breaks`, one at a time, on a copy of the tree ("Breaking the code on purpose"). |
| `links` | The part of `rules` that is about the docs: links inside the repository, and where the step records are. |
| `schema` | Regenerates the protocol's JSON Schema and TypeScript declarations in `docs/protocol/`. `schema --check` only compares. |
| `gen` | Regenerates every generated file: what `schema` writes, and the configuration schema and reference in `docs/configuration/`. |
| `size`, `unused`, `advisories` | What the non-blocking workflows run: the `dist` build with its size and crate count (`--check` fails over the budget, `--target` names the target), cargo-machete, and `cargo deny check advisories`. |
