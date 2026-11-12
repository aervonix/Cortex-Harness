# Build plan

The ordered steps from an empty repo to a first usable release, and beyond. Each step is sized for one focused session that has only these docs and its own prompt.

The plan serves the [product vision](vision.md). The table after the owner's decisions says which features are in the first usable release and which come later, with reasons.

## The owner's decisions

The owner answered the open questions on **2026-11-12**. This section is the record of each answer. What is still outstanding is listed at the end of it.

### Where the vision pulls against the speed budgets

All ten resolutions were **accepted as recommended on 2026-11-12** and are no longer provisional.

| # | Conflict | Resolution, accepted 2026-11-12 | Recorded in |
|---|---|---|---|
| C1 | **Extension sandboxing against binary size, build time and "no C code".** Self-written, sandboxed, hot-loaded extensions need an engine in the binary. | An embedded JavaScript engine (QuickJS), the one accepted C dependency: 1.1 MB and 8.6 s of release build. Extension scripts are confined by the engine on every OS. Lua is the fallback if the Windows build gives trouble. | [Decision 0005](decisions/0005-extension-mechanism.md) |
| C2 | **The repository map against start-up, memory, binary size and build time.** | Approximate outlines from small pure-Rust extractors, indexed in the background under caps, cached on disk, never held in memory, never on the start-up path. | [Decision 0022](decisions/0022-repository-map.md) |
| C3 | **Parallel worktrees (attempts, Coordinator teams) against disk, CPU and time.** | Never automatic. An estimate first. A cap on concurrent commands at half the cores. Hard budgets. Not in the first release. | [Decision 0023](decisions/0023-run-engine-after-first-release.md) |
| C4 | **Local statistics storage against start-up and footprint.** | Append-only files and a small aggregate, written off-thread, loaded on first use. No database. | [Decision 0021](decisions/0021-local-statistics.md) |
| C5 | **Snapshots for undo against time and disk on large trees.** | A per-turn snapshot of the files the file tools changed, always on, plus detection and best-effort capture of changes made by shell commands. Whole-workspace snapshots are not in the first release. | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md) |
| C6 | **Starting tools early against correct retries.** | Side-effect-free calls start the moment their arguments finish. Side-effecting calls start when the response ends. | [architecture/15-efficiency.md](architecture/15-efficiency.md) |
| C7 | **Automatic verification against wall-clock time.** | The project's checks run at the end of a turn that changed files, with time limits, at most two rounds, and a mode switch. Off until the project's commands are known. Speculative verification, adopted the same day, gives this stage a head start and does not change it. | [architecture/15-efficiency.md](architecture/15-efficiency.md) |
| C8 | **Background compaction against tokens.** | On, on the fast model, from 70% of the context limit. | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md) |
| C9 | **The size of the first release against time to release.** | The vision's features stay in the first release. Preview builds are published at the two alpha milestones. The five ideas adopted the same day are placed after the release, so they do not add to it. | This page |
| C10 | **CI time against a growing workspace.** | The mitigation order in [testing-and-ci.md](testing-and-ci.md). Step 2 replaces the estimates with measurements. | [testing-and-ci.md](testing-and-ci.md) |

### Product and design choices

| # | Question | The owner's answer, 2026-11-12 | Status |
|---|---|---|---|
| 1 | TUI language | **Rust**, in the main binary. | Accepted. **Still provisional** until the owner runs the Windows Terminal checklist, which must happen before step 20. [Decision 0001](decisions/0001-tui-language.md) |
| 2 | Inline or full screen by default | **Inline.** Full screen is an option. | Accepted. **Still provisional** with question 1, for the same reason. [Decision 0002](decisions/0002-tui-rendering.md) |
| 3 | Panes | **Stacked panels** above the editor. Side by side only in the full-screen presenter on wide windows. | Accepted. |
| 4 | Look and feel | **As drafted** in [architecture/09-tui.md](architecture/09-tui.md): glyphs, wording, density, default keys, slash-command names, and the accent with its lighter tint. | Accepted. |
| 5 | Command and directory names | **Changed.** The command is `cortex`, **plus a short alias, `cx`**, that runs the same program on both OSes. The alias ships as a link to the one executable. Step 2 checks both names for clashes and reports. | Accepted. [Decision 0013](decisions/0013-naming.md) |
| 6 | How far to go on sandboxing | **Linux sandbox in the first release. Windows AppContainer after it.** | Accepted. [Decision 0009](decisions/0009-sandboxing.md) |
| 7 | How much the harness learns without asking | **Observed facts become active with a notice. Anything a model inferred waits for review.** | Accepted. |
| 8 | Experimenting with per-model variants | **Opt-in only.** Variants are compared offline with the evaluation engine. | Accepted. |
| 9 | Extension language | **JavaScript on QuickJS, with Lua as the fallback.** | Accepted. [Decision 0005](decisions/0005-extension-mechanism.md) |
| 10 | Coordinator on Windows before its sandbox exists | **Supervised level only**, until the Windows sandbox exists. The same rule now holds for single sessions' leases. | Accepted. |
| 11 | Sign-in flows | **API keys only in the first release.** | Accepted. |
| 12 | Credential storage on Linux | **An owner-only file**, with the keychain behind a build feature. | Accepted. [Decision 0018](decisions/0018-credential-storage.md) |
| 13 | Providers in the first release | **Anthropic and OpenAI-compatible. Google and OpenAI Responses right after**: they are steps 35 and 36, the first steps after the release. | Accepted. |
| 14 | Release targets | **Linux x86-64 and arm64 (static), Windows x86-64.** | Accepted. |
| 15 | Publishing crates | **Not published.** The `cortex-` prefix stays. | Accepted. |
| 16 | Edit matching | **Exact, plus one whitespace-tolerant tier.** | Accepted. [Decision 0020](decisions/0020-edit-matching.md) |
| 17 | The further ideas | **Changed. All five are adopted:** checked memory, corrections become evaluations, autonomy leases, the context profiler, speculative verification. Each is placed after the step it depends on (steps 37, 42, 43, 44 and 47). Two conditions came with them: leases and the Coordinator's levels are one model ([decision 0026](decisions/0026-one-autonomy-model.md)), and speculative verification works with end-of-turn verification ([architecture/15-efficiency.md](architecture/15-efficiency.md)). | Accepted. [ideas.md](ideas.md) |
| 18 | Spike sources | **Keep the archive of the spikes until the Windows Terminal checklist has been run.** It is outside this repo, on the owner's machine. | Accepted. |
| 19 | Checking for updates | **No automatic check.** `cortex update check` asks on request. | Accepted. |

### Added on 2026-11-12: the `/usage` command

The owner added a feature that had been held back: usage and cost per provider, model and project over time, limits left, and usage recorded by other coding agents on the same machine. It is specified in [architecture/18-usage.md](architecture/18-usage.md) and [decision 0025](decisions/0025-usage-view.md). Our own numbers are in the first release (steps 16 and 24). The readers for other tools are step 45.

### Still outstanding

| Item | Who | When |
|---|---|---|
| **The Windows Terminal checklist** ([measurements/windows-terminal-checklist.md](measurements/windows-terminal-checklist.md)). Decisions 0001 and 0002 stay provisional until it is run. | The owner | Before step 20 |
| **Clash check for `cortex` and `cx`**, with a report to the owner. | Step 2 | Before anything is published |
| **QuickJS with the Windows toolchain.** If it does not build cleanly, the step switches to Lua, as agreed. | Step 28, in its first hour | |
| **Usage endpoints.** The brief for `/usage` allows "documented usage endpoints" and also says nothing is sent anywhere. The design calls no endpoint. If the owner wants allowance figures that only an endpoint gives, it would be an explicit, off-by-default refresh per provider. | The owner | Any time; nothing waits on it |
| **The name of the default level.** Making one autonomy model renamed the default from `standard` to `supervised`, so that the word means the same for a session and for a team. If the owner prefers `standard`, it can be the display name; nothing else changes. | The owner | Before step 12 |
| **MCP scope in the first release** ([decision 0010](decisions/0010-mcp-in-first-release.md)): tools only, through search. It was not among the questions answered. Work proceeds as drafted. | The owner | Before step 30 |
| **Real CI timings.** The estimate for a cold-cache Windows run is over 5 minutes. | Step 2 measures | |

## Features: first release or later

| Feature | First usable release | Later | Reason for "later" |
|---|---|---|---|
| Project profile | Observed commands and flaky tests, user entries, review, in the prompt (step 17) | A session-end pass that proposes entries | It spends tokens every session. It should follow evidence that proposals are good. |
| Project memory | `remember` proposals with source and expiry; the `check` field stored and shown (step 17) | **Checked memory:** running the checks (step 43) | It needs the verification runner and handling of unreliable checks. |
| Per-model formats and prompt style | Variant points, catalog defaults, overrides, outcomes recorded (step 16) | Choice from measured success rates (step 48) | It needs statistics that only exist after use, a second edit format, and the evaluation engine to compare fairly. |
| Model routing within a budget | Roles for utility calls, budgets (step 16) | Delegated sub-tasks on the fast model (step 50) | Needs sub-agents. |
| Modes and profiles | Yes (steps 9, 12) | | |
| **Autonomy leases** | The lease type, the standing lease every session has, and the levels `read-only`, `supervised`, `autonomous`, `unrestricted` (step 12) | Task leases and the `guided` level (step 37) | They need the sandbox, budgets, the profile and verification to exist, and they are the base the Coordinator is built on. |
| Hot-reload | Yes (steps 9, 24, 29) | | |
| Self-written extensions | Yes (step 29) | | |
| Repository map | Yes, approximate (step 18) | Parser-accurate outlines | Conflict C2. |
| Changed-only re-reads | Yes (step 6) | | |
| Compressed tool output, full output a key away | Structural compression, the viewer (steps 19, 24) | Model-written summaries of long output (step 52) | Costs tokens. Wait for routing data. |
| Cache-ordered prompt | Yes (steps 14, 15) | | |
| Live tokens, cost, cache hit rate | Yes (steps 16, 24) | | |
| **`/usage`** | Our own usage and cost per provider, model and project, for the session, today, the week, the month and all time; limits left from response headers; `cortex usage` (steps 16, 24) | Usage recorded by Claude Code and Codex, then Opencode, Pi and others (step 45) | Reading other tools' formats is separate work with its own fixtures, and nothing else depends on it. |
| **Context profiler** | The by-category `/context` view, and the per-request record it needs (steps 15, 16, 24) | The full view, cache-miss explanations and suggestions (step 44) | Presentation over data the first release already keeps. |
| Tools start when arguments finish; parallel tools | Yes (step 7) | Side-effecting calls started early | Conflict C6. |
| Prefetch of likely reads | Reads started early, a result cache (step 7) | Prediction from history (step 52) | Needs statistics, and a wrong guess costs tokens. |
| Background compaction | Yes (step 15) | | |
| Start-up, memory, rendering | Budgets enforced in CI (steps 20, 33) | | |
| Smart permissions | Yes, with the learned safe list (steps 12, 17) | | |
| Automatic verification | Configured and learned commands, end of turn (step 19) | Affected-test selection per ecosystem (step 52) | Per-ecosystem work that should follow real use. |
| **Speculative verification** | No. End-of-turn verification is the gate it builds on (step 19). | Step 42 | It needs the workspace manager for checks that write, and statistics to show it pays. It ships off by default. |
| Live, editable plan | Yes (steps 10, 24) | | |
| Parallel attempts | No | Steps 38, 39 | Decision 0023: it multiplies whatever one session does, so one session comes first. |
| Replay and evaluation | Recording from the first release (step 8) | Step 46 | It needs recordings, which start with the release. |
| **Corrections become evaluations** | Sessions record how each turn ended, including undone and rewound (step 8) | Step 47 | It is built on replay. |
| The Coordinator | The lease, protocol and log pieces (steps 3, 8, 12, 27) | Steps 40, 41, after task leases and the orchestration layer | It is built on both, and on a solid single session. |
| Autonomy default with snapshots | Yes (steps 10, 12) | Whole-workspace snapshots | Conflict C5. |
| Adaptive panels, accent colour | Plan, review, usage, project panels (step 24) | Attempts, Employees and escalations panels | They belong to features that come later. |
| Compatibility readers | Instruction files (15), Skills (28), MCP configurations (30) | | |
| Local-only statistics | Yes (step 16) | | |
| More providers | Anthropic (14), OpenAI-compatible (31) | OpenAI Responses (35), Google (36) | The owner's answer to question 13: right after the release. |
| Sign-in beyond API keys | The layer (step 9) | Provider-offered flows (step 53) | Each provider's terms must be checked first. |
| Onboarding | Yes (step 25) | | |
| Windows and Linux shells | Yes (step 11) | | |
| The command and its alias | `cortex` and `cx` (steps 2, 13, 34) | | |
| Branching time-travel, re-attachable sessions | The log tree, snapshots, sockets (steps 8, 27) | As features | Not headlines. |

## Rules for every step

These hold in every step. A step's prompt may add to them.

1. **Original work.** The reference harnesses may be studied to understand problems. Nothing is copied from them: no code, structure, names, prompt text, UI or configuration format. No dependency that is a port or extract of their code.
2. **Speed and efficiency are priorities**, in the product and in the tooling. The blocking pull-request checks finish in under 5 minutes per OS on GitHub-hosted runners. Anything slower goes in a non-blocking tier.
3. **The owner's name and email never appear** in commits, trailers, docs, code, configuration or fixtures. No absolute home-directory paths. Use `~` or repo-relative paths. Commits use the repo-local identity.
4. **Everything fresh.** Nothing is carried over from earlier attempts.
5. **The brand is Cortex-Harness.** Crates use the `cortex-` prefix.
6. **The docs are the specification.** Read [README.md](README.md), [vision.md](vision.md), the architecture pages your step names, and the decisions they cite before writing code. If the code must differ from a doc, change the doc in the same change. If a decision changes, add or amend a decision record.
7. **No new dependency without an entry** in `docs/dependencies.md` saying why, what else was considered and what it costs.
8. **Linux and Windows together.** A step is not done until its tests pass on both in CI.
9. **No paid API in CI.** Tests use the scripted provider and recorded fixtures.
10. **Stay inside the step.** "Must not change" lists what a step may not touch. Needed changes outside the step are reported, not made.
11. **Conflicts are raised.** Where a feature cannot meet a budget, say so in the step report with a recommendation. Do not cut the feature or loosen the budget quietly.

**Done, for every step, means:** the done criteria below are met; formatting, lints and all blocking tests pass on both OSes; the blocking checks are still within budget; public items are documented; `docs/` matches the code; and the step's report lists what was built, what was measured and anything left open.

**Sizes:** S is under half a session. M is one focused session. L is one long session and should not be combined with anything else.

## Milestones

| Milestone | After step | What exists |
|---|---|---|
| Skeleton | 2 | A workspace that builds, lints and tests on both OSes in CI within the time budget. |
| Headless alpha (preview build) | 15 | `cortex -p` runs real tasks against Anthropic with the built-in tools, permissions, sessions, snapshots and compaction. |
| Adaptive core | 19 | Statistics and usage, the project profile and memory, the repository map, verification. |
| Interactive alpha (preview build) | 25 | The TUI with panels, `/usage` and onboarding, on Linux and Windows. |
| **First usable release** | 34 | Sandbox on Linux, RPC server, extensions the harness can write, MCP, a second provider protocol, budgets enforced, release binaries under both command names. |
| More providers | 36 | OpenAI Responses and Google Gemini. |
| Task leases | 37 | One autonomy model in use: scoped, time- and cost-limited leases for a single session. |
| Orchestration | 39 | The orchestration layer and parallel attempts. |
| Coordinator | 41 | A Manager session running a team under a team lease. |
| Sharper tools | 45 | Speculative verification, checked memory, the context profiler, other tools' usage in `/usage`. |
| Evaluation | 48 | Replay, evaluation, corrections as cases, and per-model choices from measurements. |
| Breadth | 49 to 56 | Windows sandbox, sub-agents, more tools, sign-in, network transport, macOS. |
| Review | 57 | Ranked findings from a whole-repo review. |

After the release, three tracks touch different crates and can run at the same time in separate sessions: providers (35, 36), autonomy and orchestration (37 to 41), and the single-session features (43, 44, 45).

## Dependency graph

```
2 ─► 3 ─► 4 ─┬─► 5 ─┬─► 7 ───────────────────┐
             │      └─► 14 ─► 31              │
             ├─► 6 ─┬─► 7                     │
             │      ├─► 10 (needs 8)          ├─► 13 ─► 15 ─► 16 ─┬─► 17 ─► 19
             │      ├─► 11 ─► 26              │                   └─► 18
             │      └─► 12 (needs 9) ─────────┤
             ├─► 8 ───────────────────────────┤
             ├─► 9 ───────────────────────────┘
             └─► 20 ─┬─► 21 ─┐
                     └─► 22 ─┴─► 23 (needs 13) ─► 24 (needs 19) ─► 25
13 ─► 27          26, 12 ─► 28 ─► 29 (needs 24)          28 ─► 30
25, 26 to 31 ─► 32 ─► 33 ─► 34

after the release:
31 ─► 35, 36
12, 16, 24, 26 ─► 37 ─────────┐
34 ─► 38 ─► 39 ───────────────┴─► 40 ─► 41
      38 ─► 42 (needs 19)     38 ─► 46 ─► 47
      38 ─► 50                      46 ─► 48 ─► 52
17, 19 ─► 43     15, 16, 24 ─► 44     16, 24 ─► 45
26 ─► 49     34 ─► 51     30 ─► 53     29, 26 ─► 54     27, 41 ─► 55     34 ─► 56
everything ─► 57
```

Steps that can run at the same time in separate sessions are marked in each step. They touch different crates.

---

## Step 2: Repo skeleton

- **Goal and scope.** The Cargo workspace with every first-release crate from [architecture/01-workspace.md](architecture/01-workspace.md) as an empty, compiling crate with one trivial test. `rust-toolchain.toml`, workspace lints, `rustfmt.toml`, `clippy.toml` with the disallowed-methods list, `deny.toml`, `.gitignore`, `.gitattributes`, the profiles. A test that checks the crate dependency rules from `cargo metadata`. The `xtask` crate with stub commands. The test tiers wired to nextest profiles. CI for Linux and Windows as in [testing-and-ci.md](testing-and-ci.md), with caching. `README.md`, `CONTRIBUTING.md`, `SECURITY.md`, `docs/dependencies.md`, an `AGENTS.md` for this repo that states the rules above. **Both command names:** the binary is `cortex`, and `xtask` creates the `cx` alias as a link beside it for local runs ([decision 0013](decisions/0013-naming.md)). **The clash check:** look for existing tools and packages named `cortex` or `cx`, as that decision lists, and report what is found to the owner. **The logo:** add the supplied logo under `assets/`, and a dark-on-light version for the README and docs, because the original is near-white and disappears on light backgrounds; the README picks between them by colour scheme. The licence file exists and stays. Settle the repo name question with the owner.
- **Depends on.** Step 1 (these docs).
- **Done when.**
  - `cargo build`, `cargo nextest run`, `cargo clippy` and `cargo deny check` pass locally and in CI on both OSes.
  - CI wall time is recorded for a cold cache and a warm cache on both OSes, and the blocking checks are under 5 minutes in all four cases. The numbers replace the estimates in [testing-and-ci.md](testing-and-ci.md).
  - The dependency-rule test fails when a forbidden edge is added (shown in the step report).
  - A check fails the build if a file contains an absolute home-directory path.
  - The README shows the right logo on light and on dark backgrounds.
  - The stub binary gives the same output under both names (a test on both OSes).
  - The clash report for `cortex` and `cx` is in the step report, with a recommendation if anything serious was found.
- **Must not change.** Anything under `docs/architecture/` or `docs/decisions/` except to correct an error the step report names. The licence.
- **Size.** M. **Parallel.** No.

## Step 3: Protocol types

- **Goal and scope.** `cortex-protocol`: the `Op`, `Event`, item, message, part, usage, stop-reason and error-code types from [architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md), [03-agent-loop.md](architecture/03-agent-loop.md) and [04-providers.md](architecture/04-providers.md), including client roles, plan, usage (session and report) and project operations, and the wire form of levels and leases. JSON-RPC envelopes. `xtask schema` generating JSON Schema and TypeScript declarations into `docs/protocol/`.
- **Depends on.** 2.
- **Done when.** Round-trip tests for every type. Unknown fields and enum values tolerated where the versioning rules say so. The schema test fails when the checked-in schema is stale. One golden transcript exists. The crate depends only on serde and serde_json in a normal build, and its clean build time is recorded.
- **Must not change.** Other crates.
- **Size.** M. **Parallel.** No.

## Step 4: Base primitives

- **Goal and scope.** `cortex-base`: error kinds, `Secret`, ids, clock and RNG traits with fakes, the cancellation token tree, workspace paths with the Windows rules, text width, wrapping and truncation helpers, atomic file writes, the newline-delimited JSON-RPC reader and writer, the modification-time check used for hot-reload.
- **Depends on.** 3.
- **Done when.** Property tests for path normalisation and truncation. Windows-only tests for case, device names, UNC rejection and replace-while-locked. A test proves `Secret` cannot be printed or serialised. The framing reader passes every chunk split and both line endings.
- **Must not change.** `cortex-protocol` types, except agreed additions.
- **Size.** M. **Parallel.** No. Steps 5, 6, 8, 9 and 20 start after it.

## Step 5: Provider abstraction and test kit

- **Goal and scope.** `cortex-llm` without any real protocol: request and event types, the `Protocol`, `StreamDecoder`, `Transport`, `ModelClient` and `AuthMethod` traits, the SSE parser, the retry layer, usage normalisation, the catalog loader with a first `catalog/models.toml` including variant defaults, the HTTP transport. `cortex-testkit`: the scripted provider, the scripted SSE server, fake clock helpers.
- **Depends on.** 4.
- **Done when.** The SSE parser passes fixtures split at every byte. The retry layer is tested against the fake clock for every row of the classification table, including server waits and the maximum wait. The scripted provider can express text, reasoning, tool calls, usage, mid-stream errors, stalls and aborts. The real transport is tested against the scripted SSE server, including cancellation and the idle timeout. The HTTP client's creation cost is measured and it is created lazily.
- **Must not change.** `cortex-protocol` beyond agreed additions.
- **Size.** L. **Parallel.** With 6, 8, 9 and 20.

## Step 6: Tool contract and read-only tools

- **Goal and scope.** In `cortex-tools`: the `Tool` trait, `Plan`, effects, concurrency classes, results, the output policy with structural compression and artifacts (through a content-store trait), file snapshots. The tools `read` (with changed-only re-reads), `list`, `search`. The dynamic-tool registry with `tool_search` and `tool_call`.
- **Depends on.** 4.
- **Done when.** Contract tests for every row of each tool's section in [architecture/05-tools.md](architecture/05-tools.md). Re-read tests: unchanged, small change (a diff), large change, `fresh`. Property tests for the output policy: never cuts inside a line, always says what was cut, the artifact holds everything. Search over a large generated tree within a recorded time and cancellable. `tool_call` validates arguments and never changes the declared tool list (test). Windows path cases pass.
- **Must not change.** `cortex-base` and `cortex-protocol` beyond agreed additions.
- **Size.** L. **Parallel.** With 5, 8, 9 and 20.

## Step 7: Agent loop

- **Goal and scope.** `cortex-agent`: the turn state machine, streaming, early start of side-effect-free calls with a result cache, the tool scheduler, steering and queueing, plan-edit notices, retries, stop reasons, loop guards, limits as a ledger charged to the session's lease, interruption, statistics events through a trait, a verification stage behind a trait (stubbed), several sessions per core. Generic over the model client, tool set, policy (stub) and session store (in-memory). Context assembly is a stub until step 15.
- **Depends on.** 5, 6.
- **Done when.** Every state transition has a test with the scripted provider and fake tools. Tests cover parallel read-only calls, same-path serialisation, exclusive calls, results in call order, exactly one result per started call. Early start: a read begins before the response ends (asserted by event order), and a failed response that is retried reuses or discards early results correctly with no side effect having run. Interruption in each state leaves a history a following request accepts. The whole suite runs in under 5 seconds.
- **Must not change.** The trait shapes from steps 5 and 6 without recording it.
- **Size.** L. **Parallel.** With 8 to 11, 14 and 20.

## Step 8: Session store

- **Goal and scope.** `cortex-session`: the JSONL log, entry kinds including `base`, `outcome` and the reserved `escalation` and `decision`, the tree and active branch, durability settings, streaming read, tail repair, resume, rewind, fork, artifacts, the snapshot (checkpoint) store, the index. Recording of the workspace's base state in the background.
- **Depends on.** 4.
- **Done when.** A log cut at every byte of its last entry opens cleanly. Dangling tool calls are repaired on resume. Unknown kinds and fields are skipped. A failed write leaves memory and disk in agreement. A turn's snapshot restores exactly, including files that did not exist. The base state is recorded for a git and a non-git workspace without delaying the first prompt. Opening 10,000 entries and listing 1,000 sessions meet the budgets. Files are owner-only on both OSes.
- **Must not change.** `cortex-protocol` beyond agreed additions.
- **Size.** M to L. **Parallel.** With 5 to 7, 9 and 20.

## Step 9: Configuration and credentials

- **Goal and scope.** `cortex-config`: layers, merging, profiles, provenance, validation, locks, the trust classification of keys, schema generation, reload with the "old configuration stays on error" rule, the credential store for both OSes behind the `AuthMethod` layer, the state-directory files, discovery of other tools' files (reported, not yet acted on).
- **Depends on.** 4.
- **Done when.** A table-driven test covers every merge rule, layer order and profile. Every key is classified for project trust (test). Provenance output is snapshot-tested. A reload with an invalid file keeps the old configuration. Credentials round-trip on both OSes, the file store is owner-only, and concurrent writers do not corrupt it. Loading configuration meets its start-up time.
- **Must not change.** Other crates.
- **Size.** M to L. **Parallel.** With 5 to 8 and 20.

## Step 10: Editing tools and the plan

- **Goal and scope.** `write`, `edit`, `plan`, `remember` (storing proposals through a trait until step 17), with the staleness guard, the two matching tiers, line-ending and byte-order-mark preservation, atomic writes and per-turn snapshots.
- **Depends on.** 6, 8.
- **Done when.** Property tests for `edit`: a failed call leaves the file byte-identical; applying then reversing restores the original; mixed and CRLF endings survive; overlapping and ambiguous edits are refused. The tolerant tier is always reported. Staleness is tested. Undo of a turn restores every file the turn's tools changed. A user's edit to the plan reaches the model as a notice and the next tool result returns the current plan. Windows: locked-file behaviour.
- **Must not change.** The `Tool` trait.
- **Size.** L. **Parallel.** With 7, 11, 12 and 14.

## Step 11: Shell tool and process runner

- **Goal and scope.** The process runner: process groups and Job Objects, pipes, merged output, the bounded buffer and artifact spill, timeouts, staged termination, clean-up. The `shell` tool. Shell selection per OS, with PowerShell 7, Windows PowerShell and `cmd.exe` each handled explicitly. Detection of files changed by a command. A launcher hook that step 26 fills.
- **Depends on.** 6.
- **Done when.** On both OSes a command with a grandchild that ignores the polite interrupt is gone after cancellation and after timeout, and children do not survive the harness being killed. Each Windows shell has quoting, merged-output and exit-status tests. Large-output tests cover streaming, head-and-tail and the artifact. Background children holding the pipes are detected. Changed-file detection is tested with and without git. Process start overhead is measured.
- **Must not change.** The `Tool` trait and the output policy.
- **Size.** L. **Parallel.** With 7, 10, 12 and 14.

## Step 12: Permissions

- **Goal and scope.** `cortex-policy`: the one autonomy model from [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md). The ladder of levels, with `read-only`, `supervised`, `autonomous` and `unrestricted` usable now and `guided` defined for step 37. The `Lease` type with its grant, stop conditions, expiry and grantor, `narrow` and `is_within`, and the standing lease every session holds. Rules and their evaluation, built-in deny rules, the three safe lists, command analysis that fails closed (choosing the parser as that page describes), the read-only command table, approvals at three scopes, project trust with hash pinning.
- **Depends on.** 6, 9.
- **Done when.** A table of commands with expected outcomes is tested, including every bypass class in [security.md](security.md). Property tests: adding an `allow` never changes a `deny`; `narrow` is always within both inputs; no sequence of narrowing widens a lease; a narrowed lease never has a higher level, a later expiry or a larger budget than its parent. `autonomous` is refused where the sandbox does not enforce. Projects can tighten and cannot loosen without trust. A changed definition is disabled until re-approved. `cmd.exe` and PowerShell cases run on Windows.
- **Must not change.** The `Plan` and effect types beyond agreed additions.
- **Size.** L. **Parallel.** With 7, 10, 11 and 14.

## Step 13: Core assembly and headless mode

- **Goal and scope.** `cortex-core`: wire the loop, tools, sessions, configuration and policy into a `Core` that takes `Op` and emits `Event`, with approvals as requests to the front end and client roles. `cortex-cli`: argument parsing, mode selection, headless mode with the three output formats and the exit-code table. Help and usage text print whichever name the program was started with, `cortex` or `cx`. Subcommands for sessions, configuration, permissions and credentials.
- **Depends on.** 7 to 12.
- **Done when.** Process-level tests run the real binary against the scripted SSE server for every exit code. `jsonl` output validates against the generated schema. Standard output carries only the result in every format. Signals behave as documented on both OSes. `cortex --version` and a trivial headless run meet the start-up budget, under both names.
- **Must not change.** Crate APIs from earlier steps without recording it.
- **Size.** L. **Parallel.** No.

## Step 14: Anthropic protocol

- **Goal and scope.** The `anthropic-messages` protocol: requests with cache hints, reasoning controls and tool definitions; the stream decoder; error classification; usage; raw-block replay. Begin by re-reading the provider's current documentation and updating [architecture/04-providers.md](architecture/04-providers.md) where it has moved. Catalog entries. `xtask record` for scrubbed fixtures, run by hand.
- **Depends on.** 5.
- **Done when.** Request snapshots for plain text, tools, reasoning, cache markers, a multi-round tool conversation, and a model switch that drops reasoning. Decoder fixtures for text, reasoning with signature, one and several tool calls, a server-side tool block, an unknown event, an error after a 200, and every stop reason, replayed at every chunk boundary. The prefix-stability test passes over a recorded conversation. The classification table is tested row by row. A live smoke test exists and is not in CI.
- **Must not change.** The traits from step 5 without recording it. Nothing outside `cortex-llm`, the catalog and the test kit.
- **Size.** L. **Parallel.** With 6 to 12 and 20.

## Step 15: Context management

- **Goal and scope.** In `cortex-agent`: system prompt assembly, instruction files (`AGENTS.md` and `CLAUDE.md`) with discovery, nested files and change notices, context epochs, budgeting, compaction with carried state, background compaction on the fast role, the overflow fallback, and the per-request context record (token estimates by source, and where the cached prefix first differed on a miss) passed to the statistics trait. The core instructions text is written here, by us.
- **Depends on.** 13, 14.
- **Done when.** The prefix check holds across a long scripted session with steering, notices and tool rounds, and fails in a test that edits history on purpose. Compaction never splits a tool round, carries the plan and file lists exactly, and can be undone. Background compaction: the session does not stop when the summary is ready in time, replayed turns carry no reasoning, and the fallback waits. Budget and instruction-file tests as listed in [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md). By hand with the real provider: a session that compacts in the background and continues, with cache-read tokens reported in each epoch.
- **Must not change.** The session log format beyond agreed additions.
- **Size.** L. **Parallel.** With 20 to 22.

*Milestone: headless alpha. Publish a preview build.*

## Step 16: Statistics, roles and variants

- **Goal and scope.** `cortex-stats`: events, monthly files, aggregates, retention, the `cortex stats` commands. Model roles (`main`, `fast`, `judge`) with utility calls routed to `fast`. Budgets per session and per day. Variant points with catalog defaults and overrides, fixed per epoch and recorded with every outcome. The usage figures (cache hit rate included) in `usage.updated`. **Usage history for `/usage`** ([architecture/18-usage.md](architecture/18-usage.md)): request events carry the project key, the billing kind and a cost fixed when recorded; the aggregates hold usage buckets by local day, provider, model, project and billing kind, folding into month buckets after 400 days; the range and grouping query; the latest limits each provider reported, with the time seen; `cortex usage` and `usage/report`. The per-request context record is stored.
- **Depends on.** 15.
- **Done when.** Recording never blocks the loop (a test with a stalled writer). Nothing is read at start-up (the start-up file-access test). Aggregates rebuild to the same values from the raw files. Disabling statistics leaves every feature working on defaults. A variant override changes the request snapshot and is recorded. Usage ranges are right across day, week, month and time-zone boundaries (fake clock), all-time totals survive the pruning of raw files, and a cost already recorded does not change when catalog prices change. A limit past its reset time is reported as unknown, not as full. `cortex usage --range month` meets its budget and makes no network connection.
- **Must not change.** The loop's behaviour. Provider adapters' snapshots, except through a variant.
- **Size.** M. **Parallel.** With 18 and 20 to 22.

## Step 17: Project profile and memory

- **Goal and scope.** `cortex-knowledge`: entries with source, basis, status, expiry and the optional `check` field (stored and shown here, run in step 43); the shared and private stores; the command observer; the `remember` path; rendering the profile section inside its token budget; the learned safe list feeding `cortex-policy`; the `project/*` operations and `cortex project` commands.
- **Depends on.** 16.
- **Done when.** A command observed succeeding twice becomes an active entry with a notice and can be undone. A test seen to flip becomes a flaky entry. Proposed entries never reach the prompt before acceptance. Expired entries drop out. The profile section is stable within an epoch. An accepted command no longer asks; a rejected one does. Both files survive hand edits, including invalid ones (reported, old state kept).
- **Must not change.** Instruction files. The permission engine's evaluation rules.
- **Size.** L. **Parallel.** With 18 and 20 to 22.

## Step 18: Repository map

- **Goal and scope.** `cortex-index`: the outline extractors for the first set of languages and the fallback, the on-disk cache, ranking, the map renderer, the caps, background indexing, the `outline` tool, the map in the first user message of an epoch or as a notice.
- **Depends on.** 15.
- **Done when.** Snapshot tests of outlines for each language from fixture files. A warm start re-scans only changed files (asserted by counting). Caps hold on a generated tree of 100,000 files. The start-up file-access test still passes. The map is byte-stable within an epoch. Memory after indexing returns to the idle level. The budgets for warm and cold indexing are met.
- **Must not change.** `cortex-tools` beyond adding the `outline` tool.
- **Size.** L. **Parallel.** With 16, 17 and 20 to 22.

## Step 19: Verification

- **Goal and scope.** The verification stage of the loop: choosing checks from configuration and the profile, running them in the sandbox hook with time limits, feeding failures back within the round limit, flaky-test handling, the modes, `/verify` and `verify/run`. Compression of build and test output that keeps failures.
- **Depends on.** 17.
- **Done when.** Scripted tests: pass completes the turn; fail feeds back and continues; the round limit ends the turn with the failing state reported; a check over its time limit is stopped and noted; a known flaky test is retried once. Verification is off when no check is known. Interrupting during verification leaves a valid history.
- **Must not change.** The loop's other states.
- **Size.** M. **Parallel.** With 20 to 23.

*Milestone: adaptive core.*

## Step 20: Terminal layer and renderer

- **Goal and scope.** `cortex-term`: raw mode and restore, input decoding, capability detection with overrides (including the background-colour query used for the accent tint and focus events used for hot-reload), the styled-line model, the line renderer, the inline presenter, the frame scheduler, sanitising. In the test kit: the virtual terminal and pseudo-terminal driver. `xtask perf` measuring start to first frame, frame cost, input latency and the flicker properties, by the method in [measurements/tui-language-spike.md](measurements/tui-language-spike.md).
- **Depends on.** 4. **Before it starts,** the owner runs the Windows Terminal checklist ([measurements/windows-terminal-checklist.md](measurements/windows-terminal-checklist.md)) and its result is recorded in decisions 0001 and 0002. If the result reopens either decision, this step waits.
- **Done when.** Presenter tests assert, for every frame: one write, synchronized-update brackets, no clear-then-redraw, nothing above the live region touched, and the right screen after commits and height changes. A demo binary streaming fake content meets the frame-time and input-latency budgets. The terminal is restored after exit, panic and signals. Pseudo-terminal tests pass on Windows.
- **Must not change.** No dependency on any core crate.
- **Size.** L. **Parallel.** With 5 to 19.

## Step 21: Editor

- **Goal and scope.** In `cortex-tui`: the multi-line editor, the key map and actions, history, paste handling, completion menus, the external-editor hand-off.
- **Depends on.** 20.
- **Done when.** Every default binding has a key → action → state test. Grapheme tests for combining marks, wide characters and emoji sequences. Large pastes are text and shown as a chip. Key-to-frame latency meets the budget while a fake stream runs.
- **Must not change.** `cortex-term`'s public API without recording it.
- **Size.** L. **Parallel.** With 22.

## Step 22: Transcript rendering

- **Goal and scope.** In `cortex-tui`: view functions for every element in [architecture/09-tui.md](architecture/09-tui.md): streaming markdown with the commit rules, code blocks with optional highlighting, tool chips, output, diffs, notices, errors, reasoning, the status line, themes with the accent and its tint, glyph sets, density.
- **Depends on.** 20, 3.
- **Done when.** Snapshots for each element at 60, 80 and 120 columns, both glyph sets, with and without colour, on light and dark backgrounds. Feeding a document token by token commits exactly the lines that feeding it at once produces. Markdown stress cases pass. Highlighting never delays a frame. The accent tint meets its contrast target against sample backgrounds (computed in a test). Frame cost with a 30-line live region meets the budget.
- **Must not change.** `cortex-term`'s public API without recording it.
- **Size.** L. **Parallel.** With 21.

## Step 23: The interactive application

- **Goal and scope.** Wire `cortex-tui` to `Core`: state from events, operations from actions, the approval, question and trust dialogs, slash commands, pickers, the viewer, steering and queueing, interactive mode in `cortex-cli`.
- **Depends on.** 13, 21, 22.
- **Done when.** End-to-end tests under a pseudo-terminal on both OSes with the scripted provider: a full turn with tools, an approval, an interruption mid-stream and mid-tool, a steer, a queue, a resume, an undo. Every slash command then available has a test. Start to first frame, idle memory and streaming budgets are measured with the real application.
- **Must not change.** `cortex-core`'s operations and events, except through the protocol with a schema diff.
- **Size.** L. **Parallel.** No.

## Step 24: Panels and live views

- **Goal and scope.** The panel mechanism and the first-release panels: plan (with editing), review, usage, project. The usage panel has its session side and its history side: usage and cost per provider and model for today, the week, the month and all time, grouped by project on request, with limits left and each cost's basis ([architecture/18-usage.md](architecture/18-usage.md)). `/context` shows the by-category breakdown. Hot-reload wired end to end for themes, key bindings and settings. The `/usage`, `/plan`, `/project`, `/profile`, `/verify` and `/reload` commands.
- **Depends on.** 19, 23.
- **Done when.** Each panel opens and closes on its conditions (state tests). Plan edits from the panel reach the model. Editing a theme or key-binding file takes effect on the next focus event or key press without a restart, and an invalid file leaves the old one in force. Several panels never exceed the live region's limit. Snapshots at three widths. The usage panel opens within one frame with a year of generated history, shows "not available" for a provider that reports no limit, and marks any estimate as an estimate.
- **Must not change.** View functions from step 22.
- **Size.** L. **Parallel.** With 26 to 31.

## Step 25: Full screen, session UI and onboarding

- **Goal and scope.** The full-screen presenter with scrolling, re-wrapping and search, and panels beside the transcript on wide windows. Rewind, undo and fork in the UI. The first-run flow: provider and key, import of what other tools left, project trust and command detection, mode choice. Terminal quirks found so far, each with an override.
- **Depends on.** 24.
- **Done when.** The same view snapshots render under both presenters. Resize tests for both. A first run in an empty home directory reaches a working session through the flow (pseudo-terminal test), and every step can be skipped. Memory stays within budget over a long scripted session in both presenters. **The Windows Terminal checklist is run again, this time with the real TUI, and its results are added to decision 0001.**
- **Must not change.** The inline presenter's output.
- **Size.** L. **Parallel.** With 26 to 31.

*Milestone: interactive alpha. Publish a preview build.*

## Step 26: Linux sandbox

- **Goal and scope.** `cortex-sandbox`: the policy type, the launcher subcommand, Landlock and seccomp, detection and status, protected-path checks after commands, leaving the sandbox as an approval, the `autonomous` mode. Windows gets the Job Object limits and the "sandbox off" status.
- **Depends on.** 11, 12.
- **Done when.** On Linux: writes outside the workspace fail, connections fail, the workspace and the private temporary directory are writable, a protected-path change is detected and restorable. A kernel without Landlock is simulated: commands are unconfined, the status says so, and `autonomous` is refused. The blocked operation is named in the result. Launcher overhead is within budget.
- **Must not change.** The shell tool's contract.
- **Size.** L. **Parallel.** With 23 to 25, 27 and 31.

## Step 27: RPC server

- **Goal and scope.** `cortex-rpc`: `cortex serve --stdio` and `--socket`, the handshake with roles, every first-release method and event, server-to-client requests with supervisor routing, several clients per session, backpressure, socket permissions and peer checks, `cortex attach`.
- **Depends on.** 13.
- **Done when.** The conformance test runs the same scenarios in-process and over stdio and compares event sequences. A slow client gets merged and dropped deltas with `item.resync`, bounded memory, and no stall of the model stream. With a supervisor attached, approvals route to it first, it cannot answer beyond a test lease, and a user client's answer wins. A session survives its client disconnecting and can be re-attached. Golden transcripts still parse. The decoder passes a fixed-length fuzzing run in the non-blocking tier. A minimal TypeScript client using only the generated declarations runs a turn.
- **Must not change.** `cortex-core` behaviour.
- **Size.** L. **Parallel.** With 23 to 26 and 31.

## Step 28: Extensions I

- **Goal and scope.** `cortex-ext`: custom commands, skills from the three folder conventions, hooks as commands, the script runtime with the host functions in [architecture/12-extensions.md](architecture/12-extensions.md), manifests and capability checks, per-extension limits, the registration cache, tools through the dynamic-tool registry, trust pinning, the management commands, three example extensions. **Start with a one-hour check that the chosen engine builds and passes its tests on the Windows runner; if it does not, switch to the fallback language and record it.**
- **Depends on.** 12, 13, 26.
- **Done when.** A script cannot reach files, processes or the network except through host functions (tests that try). Every host function is refused outside the manifest and then outside policy. Memory and time limits stop a runaway script without affecting the session. Hook tests for every event and decision, including failure, timeout and combination. Extensions add nothing to start-up until used (the start-up test, and the engine not being initialised). The engine's measured cost in binary size and build time matches decision 0005 within tolerance, or the decision is updated. Identical behaviour on Windows.
- **Must not change.** The permission engine's rules. The loop, except through its hook points.
- **Size.** L. **Parallel.** With 24, 25, 27 and 31.

## Step 29: Extensions II: self-written extensions

- **Goal and scope.** The `propose_extension` tool, the pre-review checks (manifest lint, parse, dry-run registration, tests with stubbed host calls), the review dialog, immediate loading, the diff flow for changes, `/extend`, hot-reload of extensions end to end.
- **Depends on.** 28, 24.
- **Done when.** Scripted end to end: the agent proposes an extension, the checks run, the user approves, and the new tool is callable in the same session without the declared tool list changing. Nothing is written or loaded before approval. A broad capability is flagged. A hand edit disables the extension until reviewed. A proposal with a syntax error or a failing test comes back to the agent with the reason.
- **Must not change.** The script runtime's host functions.
- **Size.** M to L. **Parallel.** With 30 and 31.

## Step 30: MCP client

- **Goal and scope.** `cortex-mcp`: local and remote servers with static credentials, both protocol eras, tools through the dynamic-tool registry, direct exposure, per-tool hashing and approval, lifecycle, reading the project's `.mcp.json` and importing other clients' files, the management commands. Begin by re-reading the current specification.
- **Depends on.** 28.
- **Done when.** Tests against in-process fake servers of both eras: list, search, call, cancellation, change notification, crash and restart. The request prefix is unchanged when servers connect, disconnect or change tools. A changed tool definition needs approval again. A project's `.mcp.json` yields proposed servers that run only after trust and approval. Output limits apply. The build without the `mcp` feature works and the size difference is recorded.
- **Must not change.** The `Tool` trait and the output policy.
- **Size.** L. **Parallel.** With 29 and 31.

## Step 31: OpenAI-compatible protocol

- **Goal and scope.** The `openai-chat` protocol: requests, the stream decoder with index-keyed tool calls, usage in the stream, reasoning fields under their various names, error classification, capability flags and provider entries for OpenAI, OpenRouter and local servers, discovery of a local server's loaded context size. Re-read current documentation first.
- **Depends on.** 5, 14.
- **Done when.** The same fixture categories as step 14, from at least two different compatible servers. A session switches between an Anthropic model and a compatible one and back, with reasoning dropped and everything else kept. Any change the second protocol forced on the neutral types is listed in the report.
- **Must not change.** The Anthropic adapter's snapshots.
- **Size.** L. **Parallel.** With 16 to 30.

## Step 32: Hardening pass

- **Goal and scope.** Run every row of [security.md](security.md), sections 1 to 7, against the code and add any missing test, together with the rows of sections 9 and 10 that already apply: the lease type and levels, and a usage view that makes no network connection. Fuzz the decoders, the framing reader and the script host's argument passing for a fixed time. Fix what is found.
- **Depends on.** 25 to 31.
- **Done when.** Each mitigation has a named test. Findings are fixed or listed with a reason.
- **Must not change.** Features.
- **Size.** M. **Parallel.** No.

## Step 33: Performance pass

- **Goal and scope.** Measure every budget in [performance-budgets.md](performance-budgets.md) with the real application on both OSes. Fix what misses. Wire the performance tier into CI with stored baselines. Add the binary-size check.
- **Depends on.** 32.
- **Done when.** Each budget has a measured value, a method anyone can re-run with `xtask perf`, and an automatic check where the budget page says so. A before-and-after table is in the report.
- **Must not change.** Behaviour.
- **Size.** M. **Parallel.** No.

## Step 34: Release engineering and first usable release

- **Goal and scope.** The `dist` profile, a release workflow that builds, checksums and publishes binaries from a tag, both command names as [decision 0013](decisions/0013-naming.md) specifies (`cx` as a link in the Linux archives, an install script that creates it on Windows, `cortex alias install` and `remove`, never overwriting another program's `cx`), installation instructions, a user guide generated in part from the configuration schema and command tables, a migration page for people coming from other agents, a changelog.
- **Depends on.** 33.
- **Done when.** A tagged dry run produces working binaries for every target, each started and smoke-tested by the workflow under both names. Release jobs do not run on pull requests. Binary sizes are within budget. The guide covers installation, first run, configuration, permissions, sessions, the project profile, extensions and MCP.
- **Must not change.** Behaviour.
- **Size.** M to L. **Parallel.** No.

*Milestone: first usable release.*

---

## After the first release

Each step begins by re-reading the current external documentation for its subject.

Three tracks start here and touch different crates, so they can run in separate sessions at the same time: more providers (35, 36), autonomy and orchestration (37 to 41), and features for a single session (43, 44, 45).

### Step 35: OpenAI Responses protocol

- **Goal and scope.** The `openai-responses` protocol, used statelessly: requests, the stream decoder, encrypted reasoning items stored and replayed as raw blocks, tool calls, usage, rate-limit headers, error classification, catalog entries.
- **Depends on.** 31.
- **Done when.** The same fixture categories as step 14. A session switches between all three protocols and back with reasoning dropped and everything else kept. The prefix-stability test passes over a recorded conversation. Nothing is stored on the provider's side (asserted from the request snapshots). Limits reported by the provider appear in `/usage`.
- **Must not change.** The other adapters' snapshots. The neutral types, except agreed additions listed in the report.
- **Size.** M to L. **Parallel.** With 36 to 45.

### Step 36: Google Gemini protocol

- **Goal and scope.** Google's models. Choose between Google's two APIs from the documentation at that time and record the choice. Function calls, thought signatures replayed as raw blocks, the ordering rules for function responses, safety-block stop reasons, usage, catalog entries. Record what, if anything, the API reports about limits; where it reports nothing, `/usage` shows "not available".
- **Depends on.** 31.
- **Done when.** The same fixture categories as step 14. Parallel function calls round-trip with the ordering the API requires. A blocked response becomes a clear stop reason, not an empty message. A session switches between Google and another provider and back.
- **Must not change.** The other adapters' snapshots. The neutral types, except agreed additions listed in the report.
- **Size.** L. **Parallel.** With 35 and 37 to 45.

### Step 37: Autonomy leases

- **Goal and scope.** Task leases for a single session, on the lease type from step 12, and the `guided` level ([architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md), [decision 0026](decisions/0026-one-autonomy-model.md)). Granting from a sentence or a preset: the model may draft terms, the harness shows the exact terms it will enforce, and a user-role client confirms. Enforcement of scope, budget, time and stop conditions. Stopping and reporting at the lease's edge, with fall-back to the standing lease. Extending and revoking. The status-line countdown, `/lease`, the `lease/*` methods, a headless flag. Leases as the permission model for unattended runs.
- **Depends on.** 12, 16, 19, 24, 26.
- **Done when.**
  - Scripted sessions for each level behave as the level table says, row by row.
  - The terms on the confirmation screen are rendered from the lease value, and a test shows that changing the value changes the screen.
  - Only a `user` client can grant, extend or revoke; a model's tool call cannot, and neither can a `supervisor` or `observer` client (one test each).
  - Each stop condition ends the lease at a safe boundary with a report: budget, time, repeated verification failure, an action outside the scope.
  - At the edge the session stops; it does not issue a series of approval prompts.
  - `guided` and `autonomous` are refused where the sandbox is not enforcing.
  - Every row of section 9 of [security.md](security.md) has a test.
- **Must not change.** The permission engine's evaluation rules. The standing lease's behaviour, which is what the first release shipped.
- **Size.** L. **Parallel.** With 35, 36, 38, 39 and 43 to 45.

### Step 38: Orchestration layer

- **Goal and scope.** `cortex-runs`: the workspace manager (worktrees and directory copies, set-up command, clean-up), the session supervisor with the stuck and runaway watchdog, session handles that hide in-process and RPC sessions, messaging, the shared budget ledger and rate-limit state, the cap on concurrent commands, the judges, results in the statistics. A child session's lease is always its parent's, narrowed. [architecture/16-run-engine.md](architecture/16-run-engine.md).
- **Depends on.** 34.
- **Done when.** Workspaces are created and removed, including after a simulated crash. Two supervised sessions share one ledger and stop at its cap. A stuck scripted session is paused by the watchdog, not by a model. The command cap holds. A handle works identically for an in-process session and one over stdio.
- **Must not change.** `cortex-core`, except additions through the protocol.
- **Size.** L. **Parallel.** With 35 to 37 and 43 to 45.

### Step 39: Parallel attempts

- **Goal and scope.** The attempts flow: variants, the estimate, the batch, the attempts panel, ranking, picking and applying the winner as one snapshot, `cortex attempts`, the `run/*` methods. Each attempt runs under a lease narrowed from the session's.
- **Depends on.** 38.
- **Done when.** A scripted batch of three attempts runs, is judged by checks, and the picked diff is applied and can be undone. A changed workspace produces a shown conflict, never a forced apply. Deleting a failing test to pass is flagged. Budgets and the estimate are tested.
- **Must not change.** The orchestration layer's interfaces without recording it.
- **Size.** L. **Parallel.** With 35 to 37 and 43 to 45.

### Step 40: Coordinator I

- **Goal and scope.** `cortex-coordinator`: the team lease and sub-leases by narrowing, the team rows of the level table, the Manager's tools, approval routing, the escalation queue and decision log as session records, the hard limits (budget ledger, harness-run git with no forcing path, worktree confinement, the main-branch rule, sensitive paths), review packets, integration into a branch and opening a pull request. It uses the lease machinery of step 37 and adds no permission type of its own. [architecture/17-coordinator.md](architecture/17-coordinator.md).
- **Depends on.** 37, 39.
- **Done when.** Scripted teams at each level behave as the level table says, row by row. An Employee can never be given more than the Manager holds (property test over the spawn path). A Manager instructed by a scripted "Employee report" to exceed a hard limit cannot (one test per limit). An approval beyond the Manager's lease reaches the user. Every solo decision appears in the log with the clause that allowed it. Ending the team lease ends every sub-lease. Where the sandbox is not enforcing, only the supervised level starts. Section 8 of [security.md](security.md) has a test per row.
- **Must not change.** The permission engine and the lease type. The orchestration layer's interfaces without recording it.
- **Size.** L. **Parallel.** No.

### Step 41: Coordinator II

- **Goal and scope.** The team-lease confirmation screen, the Employees and escalations panels, attaching to an Employee, summaries as work lands, stopping a team, `cortex team` commands and the `team/*` methods, the resource estimate.
- **Depends on.** 40.
- **Done when.** End to end under a pseudo-terminal with scripted models: start a team, answer an escalation, attach to an Employee and send it a message, see the Manager told of the intervention, integrate, stop. Memory per idle Employee is measured against its budget.
- **Must not change.** Coordinator behaviour from step 40.
- **Size.** L. **Parallel.** With 42 to 46.

### Step 42: Speculative verification

- **Goal and scope.** A head start for the end-of-turn verification stage, exactly as specified in [architecture/15-efficiency.md](architecture/15-efficiency.md): the state key, starting a check after edits go quiet, cancelling on a new edit, the look-up at the end of the turn, serving a result when the model runs the same check itself, running write-heavy checks in an isolated copy, the one-run limit, the `verify.speculative` setting, and the used, waited-on and wasted counts in the statistics.
- **Depends on.** 19, 38.
- **Done when.**
  - The end-of-turn stage's behaviour is unchanged with the feature off, and its outcomes are identical with it on (the same scripted turns, both ways).
  - A result is never used for a state it did not run against: tests change one byte after the run and expect a fresh run.
  - Nothing is added to the conversation that the model did not ask for.
  - A new edit cancels the running check and its whole process tree.
  - Never more than one speculative run. It is off by default, and `auto` turns it on only above the configured hit rate.
  - Measured on a fixture project with a slow check: the wait at the end of a turn with and without the feature, in the report.
- **Must not change.** The verification stage's contract: what is checked, the round limit, when failures reach the model.
- **Size.** L. **Parallel.** With 41 and 43 to 46.

### Step 43: Checked memory

- **Goal and scope.** Running the checks that project-memory entries can carry ([architecture/14-adaptation.md](architecture/14-adaptation.md)): the three check forms, the rules for when a check is due, running in the background inside the sandbox with no writes and no network, updating or disabling the entry, the notice to the user, dropping checks that flip, the model's optional `check` argument to `remember`, checks in the review panel.
- **Depends on.** 17, 19.
- **Done when.** A fact whose check fails is disabled, leaves the prompt at the next epoch, and the user sees why and can restore it. A passing check moves the expiry. A check that flips twice is dropped and the entry returns to plain expiry. A `command` check that is not read-only cannot run without the user's approval. Nothing runs at start-up (the start-up test). Checks stop when the session ends.
- **Must not change.** The memory file format from step 17. The prompt section's stability within an epoch.
- **Size.** M. **Parallel.** With 35 to 42, 44 and 45.

### Step 44: Context profiler

- **Goal and scope.** The full `/context` view ([architecture/15-efficiency.md](architecture/15-efficiency.md)): where the tokens went by source and item, including repeated reads; why a request missed the cache, from the prefix check; and the first set of suggestion rules, each with an estimated saving. A headless form, `cortex context --session <id>`.
- **Depends on.** 15, 16, 24.
- **Done when.** Each suggestion rule has tests for when it speaks and when it stays silent, and its estimate is within a stated tolerance on fixture sessions. A scripted cache miss is explained with the right cause. Attribution sums to the request's token count within the estimator's error. The view opens from the record without re-tokenising the session.
- **Must not change.** The per-request record's format from the first release, except by a versioned addition.
- **Size.** M. **Parallel.** With 35 to 43 and 45.

### Step 45: Usage from other tools

- **Goal and scope.** `cortex-usage` ([architecture/18-usage.md](architecture/18-usage.md)): the source trait, detection, opt-in, the background indexer and per-source cache, the separate and merged views with provenance and cost basis, `cortex usage sources` and `forget`, `usage/sources`. Adapters for **Claude Code and Codex first**, then Opencode and Pi. Each adapter starts by examining what the current version of the tool writes, and records the versions it handles. Adapters are our own code, written from the files on disk and public documentation.
- **Depends on.** 16, 24.
- **Done when.**
  - Each adapter passes its fixtures: small made-up example files, one set per supported version, plus damaged and unknown-version examples. No real session data is in the repo (the static check covers the fixture folders).
  - An unknown version yields "cannot read this version" and "not available", never a crash and never numbers.
  - No text from an input file reaches the output or the cache (asserted).
  - Nothing is read before a source is enabled, and nothing is ever written in another tool's directory (a test with a read-only directory, and one that watches for writes).
  - No network connection is made by the view or the indexer (a test with the network denied).
  - Reading the same files twice changes no total.
  - `/usage` still opens within a frame with a large generated history, and indexing stays within its memory budget.
  - Every row of section 10 of [security.md](security.md) has a test.
- **Must not change.** `cortex-stats` and our own numbers. Other tools' files.
- **Size.** L. May be split: the framework with the first two adapters, then the next two. **Parallel.** With 35 to 44.

### Step 46: Replay and evaluation

- **Goal and scope.** Building tasks and bases from recorded sessions, replaying with a variant, the agreement judge, `cortex eval` over a selection of tasks and variants under a budget, reports, results into the statistics.
- **Depends on.** 38.
- **Done when.** A recorded session replays from its base state with a different scripted model and is judged. An evaluation over a small matrix produces a report and stops at its budget. Results are readable by the variant chooser.
- **Must not change.** Session recording, except agreed additions.
- **Size.** L. **Parallel.** With 40 to 45.

### Step 47: Corrections become evaluations

- **Goal and scope.** Evaluation cases mined from the user's own corrections ([architecture/16-run-engine.md](architecture/16-run-engine.md)): detecting the correction signals, the case index, keeping referenced sessions from pruning, the three judging bases with the basis always shown, `cortex eval --cases corrections`, `cortex eval cases` to list, tag and delete.
- **Depends on.** 46.
- **Done when.** Each correction signal produces a case in a scripted session, and an ordinary turn produces none. A case replays from its base and is judged by checks where they exist, by agreement otherwise, by the model judge last, and the report names which. Deleting a case releases its session for pruning. Capture adds no measurable time to a turn.
- **Must not change.** The session log format. The replay engine's interfaces without recording it.
- **Size.** M. **Parallel.** With 48 to 56.

### Step 48: Measured per-model choices

- **Goal and scope.** A second edit format (`patch`). The measured choice of variants from statistics and evaluation results, including results on correction cases, with the rules in [architecture/14-adaptation.md](architecture/14-adaptation.md). The status view that explains each choice.
- **Depends on.** 46. Better after 47.
- **Done when.** With seeded statistics the chooser picks the better variant, only at an epoch boundary, and explains why. Without enough data it uses the default. Exploration happens only when enabled.
- **Must not change.** The `replace` edit format's contract.
- **Size.** L. **Parallel.** With 47 and 49 to 56.

### Step 49: Windows sandbox

- **Goal and scope.** First a spike, outside the repo, on the three unknowns in [decisions/0009-sandboxing.md](decisions/0009-sandboxing.md). Then the AppContainer backend in `cortex-sandbox`: the launcher, granting and revoking access to the workspace, network off, clean-up after a crash, status reporting. It enables the `guided` and `autonomous` levels on Windows, for single sessions and for Coordinator teams.
- **Depends on.** 26.
- **Done when.** On the Windows runner: writes outside the workspace fail, connections fail, the workspace is writable, common developer tools (a compiler, a package manager, git) still run, and access entries left by a killed harness are removed on the next start. Launcher overhead meets the Windows budget. If the spike shows the backend is not workable, the step stops with a report and a recommendation, and changes nothing.
- **Must not change.** The sandbox policy type. The Linux backend.
- **Size.** L, possibly two sessions. **Parallel.** With 35 to 48.

### Step 50: Sub-agents and delegated sub-tasks

- **Goal and scope.** A `task` tool that runs a child session through the orchestration layer under a narrowed lease and a budget, returning a summary and artifacts. Routing of search and log-reading sub-tasks to the `fast` role where the statistics say it is safe.
- **Depends on.** 38.
- **Done when.** A child can never hold more than its parent (property test over the spawn path). A child's tokens are charged to the parent's ledger. Interrupting the parent stops the child. The child's transcript can be opened from the parent's. Routing is off without data and can be disabled.
- **Must not change.** The orchestration layer's interfaces without recording it. The permission engine.
- **Size.** L. **Parallel.** With 40 to 49.

### Step 51: Fetch, web search and images

- **Goal and scope.** A `fetch` tool with the defences in [security.md](security.md): address checks after name resolution, redirect limits, size and time limits, content converted to text and marked as untrusted. Provider-side web search where a provider offers it. Image input from files and the clipboard, resized before sending.
- **Depends on.** 34.
- **Done when.** Requests to loopback, link-local and private addresses are refused, including through redirects and name-resolution tricks (tests with a local server). Fetched text is delimited as untrusted in the request. Images over the limits are resized, and the token estimate accounts for them. Network use follows the permission rules.
- **Must not change.** The output policy. The sandbox's network rule for commands.
- **Size.** M to L. **Parallel.** With 35 to 50.

### Step 52: Deeper efficiency

- **Goal and scope.** Affected-test selection for the main ecosystems. Model-written summaries of long output on the `fast` role. Prefetch predicted from statistics. Each is measured with the evaluation engine before it is on by default. May be split into three sessions.
- **Depends on.** 48.
- **Done when.** For each feature: an evaluation report shows what it saves and what it costs, a switch turns it off, and the default follows the report. Affected-test selection never hides a failing test that the full run would have found, on the fixture projects.
- **Must not change.** The loop's states. The verification stage's contract.
- **Size.** L. **Parallel.** With 49 to 51.

### Step 53: Sign-in flows and MCP OAuth

- **Goal and scope.** The login routine: browser hand-off with a loopback redirect, and a paste-the-code fallback for remote shells. Token storage and refresh behind the `AuthMethod` layer. One provider-offered sign-in flow at a time, each only after its terms have been checked and recorded. OAuth for remote MCP servers with the same routine. Usage on a subscription is recorded with that billing kind, so `/usage` shows it as an API-equivalent estimate.
- **Depends on.** 30.
- **Done when.** Flows are tested against a local fake authorisation server: success, denial, timeout, refresh, revocation. Tokens are stored as credentials and never logged. The loopback listener accepts one request with the expected state and closes. Each real provider flow has a recorded terms check in its decision record.
- **Must not change.** API-key behaviour.
- **Size.** L. **Parallel.** With 35 to 52.

### Step 54: Sharing extensions and background processes

- **Goal and scope.** Adding an extension from a git URL at a pinned commit, with the same review and approval as a self-written one. A handle-based variant of the shell tool for long-lived processes (start, read output, send input, stop), with clean-up when the session ends.
- **Depends on.** 29, 26.
- **Done when.** An added extension is fetched at the pinned commit, shown for review, and disabled if its content changes. Nothing from the fetched repository runs during installation. A background process is listed, readable, stoppable, confined like any command, and gone after the session ends or the harness is killed.
- **Must not change.** The script runtime's host functions. The shell tool's contract.
- **Size.** M. **Parallel.** With 35 to 53.

### Step 55: Network transport and clients

- **Goal and scope.** The WebSocket transport for the RPC server, with token and origin rules. A TypeScript client package generated from the schema. An adapter for the Agent Client Protocol. Employees on another machine over SSH, using the stdio transport. May be split.
- **Depends on.** 27, 41.
- **Done when.** The conformance scenarios pass over WebSocket. A connection without the token, or from a disallowed origin, is refused. The server binds to loopback unless told otherwise, and says so loudly when it is not. The TypeScript client runs the conformance scenarios. A remote Employee behaves like a local one in the Coordinator tests, with its lease enforced on the remote side.
- **Must not change.** The protocol, except by a versioned addition with a schema diff.
- **Size.** L, may be split. **Parallel.** With 42 to 54.

### Step 56: macOS

- **Goal and scope.** Only if the owner makes macOS a target: a Seatbelt sandbox backend, Keychain credential storage, terminal checks, release binaries for both architectures under both command names, and CI.
- **Depends on.** 34.
- **Done when.** The whole test suite passes on a macOS runner. The sandbox tests from step 26 pass with the macOS backend. The blocking checks on macOS are within the 5-minute limit, or run in a non-blocking tier if the owner prefers.
- **Must not change.** Linux and Windows behaviour.
- **Size.** M to L. **Parallel.** With 35 to 55.

---

## Step 57: Whole-repo review

The last step. It reports and stops. It changes no code.

- **Scope.**
  - **Bugs:** read every crate for logic errors, with attention to cancellation, error paths, platform differences and the invariants in [architecture/00-overview.md](architecture/00-overview.md).
  - **Security:** check every row of [security.md](security.md) against the code and its tests.
  - **Performance:** measure every budget before and after the review period and report both. Profile start-up and a long streaming session.
  - **Dead code and duplication:** unused items, features and configuration keys; logic repeated across crates.
  - **Unused dependencies:** run the unused-dependency tools; compare `docs/dependencies.md` with what is linked; report crate count and size against the budget.
  - **Slow tests:** the slowest tests per crate and whether they are in the right tier; the blocking checks' time against the 5-minute limit.
  - **Stale docs:** every page under `docs/` against the code; generated artefacts against their sources.
  - **Test gaps:** behaviours in the docs with no test; platform-specific code tested on only one OS.
- **Depends on.** Everything built so far. It can also be run before the first release at the owner's request.
- **Done when.** A report lists findings ranked by severity, each with its location, evidence, and a suggested fix sized S, M or L. Performance numbers are given before and after. Nothing is changed.
- **Must not change.** Anything.
- **Size.** L. **Parallel.** No.
