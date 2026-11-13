# Build plan

The ordered steps from an empty repo to a first usable release, and beyond. Each step is sized for one focused session that has only these docs and its own prompt.

The plan serves the [product vision](vision.md). The table after the owner's decisions says which features are in the first usable release and which come later, with reasons.

## The owner's decisions

The owner answered the open questions on **2026-11-12**. This section is the record of each answer. What is still outstanding is listed at the end of it.

### Where the vision pulls against the speed budgets

All ten resolutions were **accepted as recommended on 2026-11-12** and are no longer provisional.

| # | Conflict | Resolution, accepted 2026-11-12 | Recorded in |
|---|---|---|---|
| C1 | **Extension sandboxing against binary size, build time and "no C code".** Self-written, sandboxed, hot-loaded extensions need an engine in the binary. | An embedded JavaScript engine (QuickJS): 1.1 MB and 8.6 s of release build. It is one of the two accepted dependencies with C code. The other is `ring`, the cryptography under TLS, accepted in [decision 0004](decisions/0004-http-and-tls.md) and in the build since step 5. Both need only the platform's own C compiler. Extension scripts are confined by the engine on every OS. Lua is the fallback if the Windows build gives trouble. | [Decision 0005](decisions/0005-extension-mechanism.md) |
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

### Answered after step 1, on 2026-11-12

Three items that step 1 left outstanding, and the repository's name. Step 2 recorded the answers.

| Question | The owner's answer, 2026-11-12 | Recorded in |
|---|---|---|
| **Usage endpoints.** The brief for `/usage` allows "documented usage endpoints" and also says nothing is sent anywhere. | **An opt-in refresh.** Besides response headers, the user may turn on, per provider, a refresh that calls that provider's documented usage endpoint with the user's own key. It is off by default and is never telemetry. Where neither source gives a figure, the view shows "not available". | [architecture/18-usage.md](architecture/18-usage.md), [decision 0025](decisions/0025-usage-view.md) |
| **The name of the default level.** Making one autonomy model renamed the default from `standard` to `supervised`. | **`supervised`.** `standard` is not used, as a level or as a display name. | [Decision 0026](decisions/0026-one-autonomy-model.md) |
| **MCP scope in the first release:** tools only, through search. | **Accepted as written.** No longer provisional. | [Decision 0010](decisions/0010-mcp-in-first-release.md) |
| **The repository's name.** | **`Cortex-Harness`**, public. Settled. | This page |
| **The name clash step 2 found.** Snowflake's coding-agent CLI installs a command named `cortex`; two vendor tools install `cx`. | **The names stay `cortex` and `cx`.** The "never overwrite another program's command" rule stays. | [Decision 0013](decisions/0013-naming.md), with the findings |
| **The details of the `/usage` refresh:** the setting `usage.refresh`, user-only, run only on request, never on a timer. | **Confirmed.** Built in steps 16 and 24, and with each later provider. | [architecture/18-usage.md](architecture/18-usage.md) |

### Added on 2026-11-12: speed tiers, fail-closed leases, and a `CLAUDE.md`

Two additions the owner made when step 3 began, and one decision from its review.

| Addition | What the owner asked for | Recorded in |
|---|---|---|
| **Speed tiers.** | Provider fast modes available in Cortex-Harness, starting with Anthropic's (Opus 5.5 and Fable 5.2, about 2.5 times faster for twice the price per token). Designed at the protocol level in step 3, so that the contract does not need reopening: a speed setting for the session and per role; the speed each request ran at in its usage record, priced at that speed's rate; events for a fallback to standard speed and for a cooldown; which speeds a model offers, in the model info. Other providers' tiers fit the same concept without their differences being flattened. **For the terminal UI:** the `/effort` picker shows a fast-mode toggle on `Tab`, with the hint `tab - fast mode`, only when the current model offers a fast speed. The UI is otherwise our own, not a copy of another harness's command or icon. | [decision 0027](decisions/0027-speed-tiers.md), which also lists the steps that build each part, the answer on utility roles from the step's review (they run at `standard` and do not inherit a fast session speed, unless set per role), and two details left for steps 5 and 13. [architecture/04-providers.md](architecture/04-providers.md), [architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md) |
| **Enforced values fail closed.** | Decided in the review of step 3. A lease, a grant, a rule or a hard limit with a field or a value a build does not know is refused whole, with its own error code. It is never ignored and never kept: a dropped restriction would widen a lease. Everything that only informs stays tolerant. | [decision 0028](decisions/0028-enforced-values-fail-closed.md), [architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md) |
| **A repo-root `CLAUDE.md`.** | One line that includes `AGENTS.md`, so that Claude Code sessions load the repository's rules by themselves. | The file. It is documentation: a pull request that changes only it skips the Rust build. |

### Answered in step 4, on 2026-11-12

Step 4 built `cortex-base`. These are the choices it put to the owner, each accepted as recommended.

| Question | The owner's answer, 2026-11-12 | Recorded in |
|---|---|---|
| **A dependency outside the planned set:** `unicode-segmentation`, for grapheme clusters. Width is counted per cluster, and a cut or a wrap must never split one. | **Approved**, with its measured cost in the dependency table. | [dependencies.md](dependencies.md), [architecture/01-workspace.md](architecture/01-workspace.md) |
| **Property tests without a property-testing crate:** a small helper on the seeded generator of `cortex-base`. | **Approved**, on two conditions, both met: a failure prints the seed and the input, and an environment variable multiplies the number of cases so that the nightly run checks many more than a pull request. Steps 6, 10 and 12 follow the pattern. | [testing-and-ci.md](testing-and-ci.md), "Property tests" |
| **Paths that could mean two things are refused, not tidied.** On Windows: device names, the device and extended-length forms, names that end with a dot or a space, alternate data streams, drive-relative and root-relative paths, and network shares that are not allowed in configuration. On every OS: a path that is not valid Unicode, and a link to something that does not exist. | **Approved.** The Windows refusals apply on Windows only. A path that is not Unicode is a refusal of that one path, which a directory walk steps past. Junctions are resolved like symbolic links, and a short name resolves to the long one. | [architecture/05-tools.md](architecture/05-tools.md), "Paths" |
| **An atomic write refuses a read-only target,** on both OSes. | **Approved.** The review of the step widened it, in the same spirit: a file the user may not write and anything that is not a regular file are refused too. | [architecture/05-tools.md](architecture/05-tools.md), "Atomic writes" |
| **The fake clock and the seeded generator live in `cortex-base`,** beside their traits, and the test kit re-exports them. | **Approved.** | [architecture/01-workspace.md](architecture/01-workspace.md), rule 6 |

### Answered in step 5, on 2026-11-12

Step 5 built `cortex-llm` without a protocol, and the first parts of the test kit. These are the choices it put to the owner.

| Question | The owner's answer, 2026-11-12 | Recorded in |
|---|---|---|
| **How long a speed tier's cooldown lasts, and what a "short" wait is.** [Decision 0027](decisions/0027-speed-tiers.md) left both to this step, with a recommendation. | **Accepted as recommended.** Falling back is on by default. A request waits in place for its tier for up to 20 seconds in all, and then falls back. A cooldown lasts until the time the provider gives, and 10 minutes when it gives none. All three are settings, defined in `cortex-llm`; step 9 names their keys. | [Decision 0027](decisions/0027-speed-tiers.md), "Settled and open"; [architecture/04-providers.md](architecture/04-providers.md), "Speed tiers" |
| **A shortest cooldown.** With none, a provider that keeps answering "in 25 seconds" could make a session change speed every half minute. | **Left out for now.** Step 14 decides, from Anthropic's real waits. The length of a cooldown is decided in one function, so a floor can be added without touching anything else. | [Decision 0027](decisions/0027-speed-tiers.md), "Settled and open" |
| **The limit on the wait for a response's first event.** The docs named the limit and gave it no number. | **60 seconds, and a provider's entry can set its own.** Right for a hosted provider. A local server can take longer to read a long prompt; step 31 revisits the default for those. | [architecture/03-agent-loop.md](architecture/03-agent-loop.md), "Retries"; [architecture/04-providers.md](architecture/04-providers.md), "Transport" |

### Answered in step 6, on 2026-11-12

Step 6 built the tool contract and the read-only tools in `cortex-tools`. These are the choices it put to the owner.

| Question | The owner's answer, 2026-11-12 | Recorded in |
|---|---|---|
| **A content hash, and where it lives.** A file snapshot records one, the version of a file the model last saw is kept under one, and step 8 needs the same for checkpoints and the project key. Nothing in the planned set provided it. | **`cortex_base::ContentHash`, an agreed addition to `cortex-base`, through the `sha2` crate** and not SHA-256 of our own: `read` hashes whole files, the processor's SHA instructions make that several times faster, and a widely reviewed implementation is better than one we maintain. The algorithm is named nowhere outside the module. The standard's examples are kept as a test. | [dependencies.md](dependencies.md), [architecture/05-tools.md](architecture/05-tools.md), "File snapshots" |
| **How a cut result names its artifact.** The first draft said "the artifact path". A path would be an absolute path under the user's home directory, outside the workspace. A made-up form such as `artifact:<id>` was proposed. | **A separate argument.** `artifact:a_123` is a legal file name on Linux, and a path that could mean two things is refused, not guessed at. `read` and `search` take either `path` or `artifact`, never both, and a cut result names the artifact's id. | [architecture/05-tools.md](architecture/05-tools.md), "One output policy" |
| **`tokio` in `cortex-tools`,** with the `rt` feature only, for the blocking pool. | **Fine**, as the workspace page planned. Each crate names the `tokio` features it uses. | [dependencies.md](dependencies.md) |

### Answered in step 7, on 2026-11-13

Step 7 built the agent loop in `cortex-agent`. These are the choices it put to the owner.

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **After an interrupt, do queued prompts start by themselves?** The loop's page said both "queued prompts stay queued" and "idle ends when a queued prompt exists". | **They are held.** An interrupt means stop. The queue is kept and the front end told (`session.updated` with the queue's length, and no `session.idle`). The next prompt the user sends runs first, and the held prompts follow it in order. | [architecture/03-agent-loop.md](architecture/03-agent-loop.md), "Steering and queued prompts" |
| **After a turn that failed or hit a limit, does the queue carry on?** The page said yes, whatever the outcome. | **Held, the same way.** Only a turn that completed starts the next queued prompt by itself: queued prompts are usually steps that assume the one before worked. A steering message that was never delivered is held with them. | The same |
| **What the loop needs from a session store,** while `cortex-session` was being built beside it. | **A small trait in `cortex-agent`, accepted as proposed:** it takes `&mut self` and is asynchronous; `open` returns the active branch's items and where the turn and item counters stand; `append` takes an item or a discarded request attempt; `end_turn` takes how the turn ended. A failed append fails the turn. While a session is open only the loop makes turn and item ids. Step 13 adapts `cortex-session` to it. | [architecture/03-agent-loop.md](architecture/03-agent-loop.md); `crates/cortex-agent/src/store.rs` |
| **Trait objects where the page said "generic over".** | **Fine:** the same testability, and less to compile. | [architecture/03-agent-loop.md](architecture/03-agent-loop.md) |
| **Two parts of "prefetch" that step 7 did not build:** starting a `read` when its `path` argument is complete, and a result cache across requests. | **Accepted as built.** A call starts when its block ends, so that arguments are parsed once. There is no cache across requests, because "unchanged" and a diff already answer a repeated read, and a cached copy would undo that. Warming a file named with `@` goes to step 24, with the editor's attachments. | [architecture/15-efficiency.md](architecture/15-efficiency.md), "Prefetch" |
| **A tool that changes the session** (`plan`, `remember`) would start early as specified, and **the turn outcome `denied`** is never produced. | **Both go to the steps that own them:** a marker on the plan, as an agreed addition to the tool contract, in step 10, which built it ("Answered in step 10", below); a way for the policy to end a turn in step 12, listed under "Still outstanding" until then. | This page |

### Answered in step 9, on 2026-11-12

Step 9 built `cortex-config`, and owner-only files on Windows in `cortex-base`. These are the choices it put to the owner, each accepted as recommended.

| Question | The owner's answer, 2026-11-12 | Recorded in |
|---|---|---|
| **Owner-only files on Windows.** Three options: rely on the per-user profile folder's rules, a helper in `cortex-sandbox`, or `cortex-base` itself with one module allowed `unsafe`. | **`cortex-base`,** in one Windows-only module, with `windows-sys`. The list is the current user only, like `0600`. | [Decision 0029](decisions/0029-owner-only-files-on-windows.md) |
| **`owner_only` over an existing file.** Step 4 kept an existing file's mode even with the option set. | **One meaning on both OSes:** with the option, the file the write leaves is owner-only, existing or not. It only ever tightens. | [Decision 0029](decisions/0029-owner-only-files-on-windows.md), [architecture/05-tools.md](architecture/05-tools.md), "Atomic writes" |
| **Where the credential layer ends.** `AuthMethod` is in `cortex-llm`, which `cortex-config` may not depend on. | **`cortex-config` owns the stores and the lookup order** and returns a `Secret` with its source. `cortex-core` hands it to the API-key `AuthMethod`. No new crate edge. | [Decision 0018](decisions/0018-credential-storage.md), [architecture/08-configuration.md](architecture/08-configuration.md) |
| **The Linux keychain.** The Secret Service backend cannot be run in the blocking CI. | **Not shipped untested.** The store selection, the feature name and a clear error are built. The backend is outstanding, below. | [Decision 0018](decisions/0018-credential-storage.md) |
| **The Credential Manager wrapper.** | **`windows-native-keyring-store` with `keyring-core`,** and local-machine persistence for stored keys. | [dependencies.md](dependencies.md) |

Added on 2026-11-13, with the fixes that followed the review of step 9:

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **Which keys raise cost.** Only a speed tier other than `standard` waited for trust; a project could still choose a dearer model, more reasoning, more output or more attempts. | **An untrusted repository must not raise what its user spends.** The model, for the main role and each other role, with a role's provider; the reasoning level, for the main role and each other role; `model.max_output`; and `retry.attempts` are "raises cost", whatever their value. They apply only once the project is trusted, and until then the user's own values apply. | [architecture/08-configuration.md](architecture/08-configuration.md), "Trust and what a project may set" |
| **Whose `.git` or `.cortex` counts for the workspace root on Windows.** An elevated process creates objects owned by the Administrators group, not the user. | **The current user's, or the Administrators group's** (`S-1-5-32-544`), read with the owner check `cortex-base` already has, so no new `unsafe`. Only an administrator can create an object the group owns. A link at `.git` is passed over on Windows and counts by its own owner on Linux. | [architecture/08-configuration.md](architecture/08-configuration.md), "Layers" |
| **The check for changed files and a link that appears after the load.** The check asks the file system, which follows links. | **The project's files are found again through the path resolver before each check**, and on any difference the configuration is reloaded through the resolver. The moment between that look and the check's own is a residual, listed for the whole-repo review. | [architecture/08-configuration.md](architecture/08-configuration.md), "Reloading without a restart" |
| **What is in force when trust is lowered while a file is broken.** Dropping the project's files would drop its `deny` rules and protected paths too. | **What an untrusted load would give:** the files in force are merged again with the project untrusted, so its restrictions and preferences stay and what needs trust waits. Losing a restriction would loosen what is in force, the opposite of what taking trust away is for. | [architecture/08-configuration.md](architecture/08-configuration.md), "Reloading without a restart" |
| **Secrets in a reserved section.** A section a later step declares holds anything until then, an MCP server's token included. | **Redacted by name until it is declared** (`env`, `headers`, and names with `token`, `secret`, `password`, `auth`, `credential` or the word `key`), **and the user part of any web address everywhere.** The step that declares a section gives its secret keys the redacted kind. | [architecture/08-configuration.md](architecture/08-configuration.md), "Credentials" |

### Answered in step 8, on 2026-11-13

Step 8 built `cortex-session`. These are the choices it put to the owner before building, each accepted as recommended unless the row says otherwise.

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **How the log records the active branch.** "The newest leaf" cannot say that a rewind or a branch switch happened, and the log is never edited. | **Accepted:** the first line is the root (`e_0000`); session records (`base`, `lease`, `meta`, `usage`, `outcome`, `escalation`, `decision`, `ext`) have `parent: null` and are outside the tree; a new kind, `rewind`, moves the active position, which is the last tree entry in the file; and an optional `turn` in the envelope. | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md), "The log" |
| **Durability's default.** Decision 0006 said "per turn"; 07 and 02-runtime said "per item". | **Per item**, with decision 0006 amended. The setting `session.durability` (`item`, `turn`, `never`) is **user only**: a project that could set `never` could cost the user their sessions. | [Decision 0006](decisions/0006-session-storage.md), [architecture/08-configuration.md](architecture/08-configuration.md) |
| **Redaction.** security.md promises a filter; step 8's scope did not name it. | **The hook now** (`Redactor`, applied before a line or an artifact is written), **the filter in step 13.** Provider raw blocks are not redacted; whether one that holds a secret is dropped is open for step 13. | [security.md](security.md), section 4 |
| **The project key.** | **In `cortex-base`, not `cortex-session`**, as an agreed addition: `AbsPath::project_key`, the protocol's `ProjectKey` made of 32 lowercase hexadecimal digits (128 bits) of the root's hash, no readable prefix. Policy, statistics, knowledge and sessions key data by it, so one definition beside the path code. | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md), "On-disk layout" |
| **The base state.** | **As proposed, with three changes:** project trust is an input, and an untrusted project gets only its commit and branch, because a repository's own configuration can make `git diff` run a program; likely secret files are never stored, trusted or not; and the git hardening flags are listed for later steps. | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md), "The base state"; [security.md](security.md) |
| **Fork, leases not understood, the cut tail.** | **Accepted:** a fork copies the branch and the base state, never leases, outcomes or metadata; any lease record not understood makes the lease in force a refusal; a cut last line is kept aside in a private `log.cut-<offset>` file, and only a resume, which holds the lock, repairs. | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md) |
| **A size cap for snapshots** (added by the coordinator). | **16 MiB**, chosen by the step: a file over it is recorded by hash and size only and cannot be put back, and a rewind names it first. | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md), "Snapshots and undo" |
| **Who makes turn and item ids** (asked by step 7). | **The session's owner, while it is open**, starting after `Log::last_ids`; the store makes item ids only when it repairs, which a resume does before it returns. | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md), "Writing" |

### Answered in step 11, on 2026-11-13

Step 11 built the process runner and the `shell` tool. These are the choices it put to the owner, each accepted as recommended: three before it was built and two with its report.

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **Where the operating-system calls for process trees live.** `unsafe` is denied in `cortex-tools`, and no crate with a safe API covers the Ctrl-Break event on Windows, or the session and the death signal on Linux. | **In `cortex-sandbox`,** which may already use `unsafe`, in one small module with a safe API, with a new edge `cortex-tools` to `cortex-sandbox`. The rule about where `unsafe` may be written does not change. | [Decision 0030](decisions/0030-process-tree-control-in-cortex-sandbox.md), [architecture/01-workspace.md](architecture/01-workspace.md) |
| **What dies on Linux when the harness is killed.** The kernel's death signal reaches the command itself and nothing it started. | **Both the death signal and a guard process:** one small helper for each harness, told every tree's process group, which kills what is left when the harness is gone. It runs in a session of its own and kills only what it was told. | [Decision 0030](decisions/0030-process-tree-control-in-cortex-sandbox.md) |
| **What becomes of processes a command leaves running.** The docs said the tool stops waiting and says what happened. | **The call's whole tree is killed when the call ends,** on both OSes, and the result says that the processes were stopped and that background processes are not supported yet. Long-lived background processes are step 54. | [architecture/05-tools.md](architecture/05-tools.md), "`shell`" |
| **Whether the look for changed files counts against the budget for starting a command.** Step 11 measured 0.2 ms of overhead for starting a command on Linux, against a budget of 5 ms, and 9.5 ms on a Windows runner, against 30 ms. The look for what the command changed is extra: 2.4 ms in a repository of 500 files on Linux and 73 ms on the Windows runner, growing with the repository. | **It does not.** The budget is for starting the command. The look is reported beside it, with its own cap of half a second a look. | [performance-budgets.md](performance-budgets.md), "Latency the harness adds" |
| **One list of the flags git is started with, and one timer.** `cortex-tools` and `cortex-session` each hold a list of the flags, and `cortex-tools` and `cortex-llm` each hold a `Timer` trait, because none of them can see the other. | **Both go in `cortex-base`,** as agreed additions. Step 13 makes the move, where those crates meet; until then each crate keeps its own. | Step 13 |

### Answered in step 10, on 2026-11-13

Step 10 built the editing tools, the plan and `remember` in `cortex-tools`. Two additions were agreed before it began, and it put three questions to the owner while it was built, each accepted as recommended.

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **A tool that changes the session must not start early** (outstanding since step 7). | **A marker on the plan, `Plan::changes_session`,** as an agreed addition to the tool contract. The loop reads it beside `has_side_effects`: such a call never starts before its response has ended. | [architecture/05-tools.md](architecture/05-tools.md), "The tool trait"; [architecture/03-agent-loop.md](architecture/03-agent-loop.md), "Tool rounds" |
| **Two session-changing calls in one response.** `plan` and `remember` are scheduled read-only, so two `plan` calls could run at once, and the one that finished last would stand. | **They conflict,** and run one at a time in the model's order. Beside any other call they run as read-only calls do. | The same |
| **A path swapped between its check and its open.** `DiskStore::put_file` looked at a path and then opened it, and the editing tools would have had the same window: a file swapped for a link is followed, one swapped for a FIFO is waited on for ever. | **Closed now, in `cortex-base`,** as an agreed addition: `open_regular`, an open that follows no link at the last name and waits for nothing (`O_NOFOLLOW` and `O_NONBLOCK` through the standard library's safe `custom_flags`, with the constants from `libc`; on Windows the reparse point opened as itself). `write_atomic` can be told not to write through a link. No `unsafe` for either. | [architecture/05-tools.md](architecture/05-tools.md), "What resolution does not promise"; [dependencies.md](dependencies.md) |
| **Telling a cloud placeholder from a link on Windows** needs a file's reparse tag, which the standard library does not report. | **One function in `cortex_base::private::windows`,** the module already allowed `unsafe`, with a safe API. A reparse point is refused unless its tag is on a short list of tags whose content a filter supplies; an unknown tag is refused. | [Decision 0029](decisions/0029-owner-only-files-on-windows.md), "Amended in step 10" |
| **What "exact match" means in a file with CRLF endings.** `read` shows lines without their endings, so an `old` copied from such a file has line feeds, and against the file's bytes every edit of more than one line would be a tolerant match. | **Both tiers match against the text as `read` shows it.** An `old` copied from `read` is exact and is not reported. New text takes the file's line ending, and in a file with both kinds, the ending of the lines it replaces. | [architecture/05-tools.md](architecture/05-tools.md), "`edit`"; [decision 0020](decisions/0020-edit-matching.md) |
| **A `plan` call that crosses a user's edit.** Each call replaces the whole list, so a call written before the model saw the edit would silently undo it. | **The user's edit wins.** The call replaces nothing, and its result says what the user changed and gives the current plan. | [architecture/05-tools.md](architecture/05-tools.md), "`plan`" |
| **Two `plan` calls in one response, both crossing an edit** (found by the step's independent review). The first is refused; but its result reaches the model only after the round, so the second must be refused too. The tool has to know that two calls belong to one response. | **A round number in the run context, `RunCx::round`,** set by the loop, which moves on only when a round's results have been recorded. An edit counts as shown from a later round, or when the core says its notice was delivered. With no round number a tool assumes nothing. | [architecture/05-tools.md](architecture/05-tools.md), "The tool trait", "`plan`"; [architecture/03-agent-loop.md](architecture/03-agent-loop.md), "Tool rounds" |
| **Files over the checkpoint store's limit.** A change to a file over 16 MiB could not be undone. | **Refused for now,** with the reason and another way to make the change. Asking the user in place of refusing, through the permission engine, is for steps 12 and 13 to pick up. | [architecture/05-tools.md](architecture/05-tools.md), "Checkpoints"; step 10, below |

### Answered in step 14, on 2026-11-13

Step 14 built the `anthropic-messages` protocol in `cortex-llm`. These are the choices it put to the owner, each accepted as recommended.

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **What a change of model does to reasoning blocks, inside one provider.** The providers page said: dropped, unless the catalog says the new model can read them. Anthropic's documentation now asks for the opposite, and on its current models a block is tied to everything before it, so leaving blocks out for one model makes the next request to the other model fail. | **A message made by the same provider through the same protocol is replayed raw whatever the model**, where the provider's entry says the server leaves out what a model cannot read, as Anthropic's does. A message from another provider or protocol gets the neutral projection: text and tool calls kept, reasoning and provider blocks dropped. The stricter rule, by the catalog's `reads_reasoning_from`, stays for a server whose entry does not say so. | [architecture/04-providers.md](architecture/04-providers.md), "The conversation model" |
| **A shortest cooldown** ([decision 0027](decisions/0027-speed-tiers.md), left open by step 5). | **One minute**, the window the provider's limits are counted in. A setting of the retry policy; its configuration key is step 13's to add. | [Decision 0027](decisions/0027-speed-tiers.md), "Settled in step 14" |
| **Two new error codes.** A spending cap that arrives as a 429 (`enforced_spend_limit_reached`), and a ramp limit (`ramp_limit_exceeded`). | **The spending cap is a hard stop:** never retried, never a speed tier's refusal, so no fallback and no cooldown, and the user sees it plainly. **The ramp limit is a rate limit**, with the server's wait; at fast speed it is the tier's own, because the provider keeps a separate ramp limit for fast mode. | [architecture/04-providers.md](architecture/04-providers.md), "The `anthropic-messages` protocol" |
| **Where the live test and the recorder live.** `xtask` could not hold them without building the HTTP and TLS stack before every CI job. | **Ignored tests of `cortex-llm`, started by `cargo xtask live` and `cargo xtask record`.** No CI job runs ignored tests, and that stays so. The key is read from a file named by `CORTEX_LIVE_ANTHROPIC_KEY_FILE`; without the variable they say so and stop. | [testing-and-ci.md](testing-and-ci.md), "Fixtures" |

### Answered in step 12, on 2026-11-13

Step 12 built `cortex-policy`. Three choices were made with the owner before it began, and the rest were put to the owner before anything was built on them. Each was accepted as recommended, the first with three tightenings.

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **The shell parser:** a pure-Rust parser crate, or a hand-written tokenizer. | **Hand-written allow-list tokenizers,** one for POSIX shells and one for PowerShell, and no crate. `cmd.exe` and shells the analysis does not know are not analysed, and a `deny` or `ask` rule still matches their raw line. Three tightenings: a word that begins with `=` and a `~` that is not the home directory are refused; `{` and `}` are refused anywhere in a word; and the read-only table knows, command by command, the options that write or run things, and fails closed on any it does not list. | [Decision 0031](decisions/0031-command-analysis-by-allow-list.md), with the measurements; [dependencies.md](dependencies.md), "Considered and not added" |
| **A denial that ends the turn.** | **A `Stop` answer on what the loop takes from the policy,** for a decision and for the answer to a question. The engine itself answers allow, ask or deny; what "ask" becomes when nobody can be asked is the front end's side. `--fail-on-denial` is step 13's flag. | [architecture/03-agent-loop.md](architecture/03-agent-loop.md), "Tool rounds" |
| **Case on Windows.** A directory can be marked case-sensitive, and there `App` and `app` are two folders. | **What holds back folds case, and what grants does not.** A `deny` or an `ask` matches without regard to case. An `allow`, a lease's scope and a writable root compare names exactly, against the resolved path in its stored case. **The project key is made from the stored case,** so one folder under any casing has one key and two folders that differ only in case have two. Nothing has been released, so no migration. | [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md), "Rules as built"; [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md), "On-disk layout" |
| **The list of likely secrets in the session store.** | **It becomes the read rules.** Base-state capture is given a gate, which the core fills from the engine, and `cortex-session` keeps no list. Files that look like secrets ask by default, as a default and not a rule, so that a user's `allow` can lift it; a project's `allow` lifts it only after trust; the built-in denies for the harness's own files stay denies. A `.env` that an edit tool changed stays in the checkpoint store, because the edit was approved and undo needs it. | [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md), "Files that look like secrets"; [security.md](security.md), section 4 |
| **What a command names.** `cat` is a read-only command, and `cat ~/.ssh/id_rsa` reads a private key. | **A read-only command's operands are checked as reads,** and the command is on the safe list only if each would be allowed unasked. A hit on a deny rule makes the command ask, not refuse. `cd dir && …` is followed. | [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md), "Command analysis as built" |
| **Level against rule.** | **A level that refuses is final:** at `read-only` no allow rule makes a write happen. Rules settle what a level leaves open. | The same page, "Levels" |
| **Marking a command read-only.** | **The loop takes it from the policy** (`Decision::AllowReadOnly`), and a read-only plan with an `exec` effect waits for every write before it. | [architecture/05-tools.md](architecture/05-tools.md), "The tool trait" |
| **Dependencies of `cortex-policy`.** | **`toml`, `serde` and `thiserror`,** all in the build already, and a glob matcher of our own, tested against the cases `globset` documents. | [dependencies.md](dependencies.md), [architecture/01-workspace.md](architecture/01-workspace.md) |

### Still outstanding

| Item | Who | When |
|---|---|---|
| **The Windows Terminal checklist** ([measurements/windows-terminal-checklist.md](measurements/windows-terminal-checklist.md)). Decisions 0001 and 0002 stay provisional until it is run. | The owner | Before step 20 |
| **One detail of speed tiers:** a `--speed` flag for headless mode, which has a recommendation in [decision 0027](decisions/0027-speed-tiers.md). The shortest cooldown was answered in step 14 (one minute); its configuration key, `model.speed_cooldown_min`, is step 13's to add, because step 14 could not touch `cortex-config`. | Step 13 | In that step |
| **The Secret Service backend of the credential store,** for the Linux `keychain` feature ([decision 0018](decisions/0018-credential-storage.md)). Step 9 built the store selection, the feature name and the error a build without the backend gives. The backend needs a non-blocking CI job that runs it against a real keyring daemon, because the blocking jobs cannot. | A step the owner places | Before the first release |
| **Settings a later step declares.** `cortex-config` reserves the sections whose keys belong to parts not built yet (the list is in [configuration/reference.md](configuration/reference.md)): a file may hold them, and a project's file cannot set them, or must wait for trust. The step that builds a part declares its keys in `crates/cortex-config/src/schema/sections.rs` and says, key by key, what a project may do. Two shapes to settle then: the user's catalog entries (`[models]`, which step 5's catalog reads as `[[model]]` tables with a `version`), and the HTTP client's settings (`[http]`: connect timeout, proxy, extra certificate authorities). | Steps 5, 13, 15, 16 and the others, each for its own section | In those steps |
| **Fuzz targets for the decoders still to come.** Step 14 added coverage-guided fuzzing: a `fuzz` job in the nightly workflow, with targets for the SSE parser and the Anthropic decoder ([testing-and-ci.md](testing-and-ci.md), "Not blocking"). Each later protocol adds a target for its decoder to `fuzz/` and a line to the list in `xtask/src/outside.rs`. | Steps 31, 35 and 36 | In those steps |
| **A role for a system message in the middle of a conversation.** Anthropic now documents it for all four models, and its documentation recommends it for instructions and tool changes that arrive after a conversation began, because editing the system prompt or the tools invalidates the reasoning already in the conversation. The neutral message model has the roles user, assistant and tool. Whether it gains one is a change to `cortex-protocol`. | Step 15, which builds context epochs and instruction-file notices | In that step |
| **Stopping at the edge of a task lease.** Step 12 built the way for the policy to end a turn (`Decision::Stop`, `Verdict::Stop`), and the loop ends the turn with the outcome `denied`. What is left is for task leases: at the edge of one the holder stops and reports, and the session falls back to its standing lease. | Step 37 | In that step |
| **The start overhead on Windows, in a release build.** Step 11 measured it on a hosted runner in the unoptimised test build, because nobody has a Windows machine at a desk. | Step 33 (the performance pass), or sooner on any Windows machine | Before the first release |
| **QuickJS with the Windows toolchain.** Step 2's dependency probe compiled and ran it with the MSVC toolchain on a hosted Windows runner ([testing-and-ci.md](testing-and-ci.md)). Step 28 still confirms it in the real crate, and switches to Lua if it gives trouble, as agreed. | Step 28, in its first hour | |

## Features: first release or later

| Feature | First usable release | Later | Reason for "later" |
|---|---|---|---|
| Project profile | Observed commands and flaky tests, user entries, review, in the prompt (step 17) | A session-end pass that proposes entries | It spends tokens every session. It should follow evidence that proposals are good. |
| Project memory | `remember` proposals with source and expiry; the `check` field stored and shown (step 17) | **Checked memory:** running the checks (step 43) | It needs the verification runner and handling of unreliable checks. |
| Per-model formats and prompt style | Variant points, catalog defaults, overrides, outcomes recorded (step 16) | Choice from measured success rates (step 48) | It needs statistics that only exist after use, a second edit format, and the evaluation engine to compare fairly. |
| Model routing within a budget | Roles for utility calls, budgets (step 16) | Delegated sub-tasks on the fast model (step 50) | Needs sub-agents. |
| Modes and profiles | Yes (steps 9, 12) | | |
| **Autonomy leases** | The lease type, the standing lease every session has, the levels `read-only`, `supervised`, `autonomous`, `unrestricted`, and `narrow` with `is_within` (step 12) | Task leases and the `guided` level (step 37) | They need the sandbox, budgets, the profile and verification to exist, and they are the base the Coordinator is built on. |
| Hot-reload | Yes (steps 9, 24, 29) | | |
| Self-written extensions | Yes (step 29) | | |
| Repository map | Yes, approximate (step 18) | Parser-accurate outlines | Conflict C2. |
| Changed-only re-reads | Yes (step 6) | | |
| Compressed tool output, full output a key away | Structural compression, the viewer (steps 19, 24) | Model-written summaries of long output (step 52) | Costs tokens. Wait for routing data. |
| Cache-ordered prompt | Yes (steps 14, 15) | | |
| Live tokens, cost, cache hit rate | Yes (steps 16, 24) | | |
| **Speed tiers (fast mode)** | The protocol (step 3), the catalog and the fallback with its cooldown (5, 7), the setting (9), Anthropic's fast mode (14), cost by speed (16), the `/effort` toggle and the status line (22 to 24), tiers of OpenAI-compatible servers (31) | The tiers of OpenAI Responses (35) and Google (36) | They arrive with their providers. |
| **`/usage`** | Our own usage and cost per provider, model and project, for the session, today, the week, the month and all time; limits left from response headers, and from the opt-in refresh; `cortex usage` (steps 16, 24) | Usage recorded by Claude Code and Codex, then Opencode, Pi and others (step 45) | Reading other tools' formats is separate work with its own fixtures, and nothing else depends on it. |
| **Context profiler** | The by-category `/context` view, and the per-request record it needs (steps 15, 16, 24) | The full view, cache-miss explanations and suggestions (step 44) | Presentation over data the first release already keeps. |
| Tools start when arguments finish; parallel tools | Yes (step 7) | Side-effecting calls started early | Conflict C6. |
| Prefetch of likely reads | Calls that only read start when their block ends, and what ran early is used again when a failed response is retried (step 7). Files named with `@` are warmed (step 24). No result cache across requests: a repeated read is answered "unchanged" or with a diff | Prediction from history (step 52) | Needs statistics, and a wrong guess costs tokens. |
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
12. **No real waiting in tests.** Timeouts, retries, backoff and cooldowns are tested with paused or simulated time (the async runtime's test clock, or the fake clock), never with real sleeps. The details are in [testing-and-ci.md](testing-and-ci.md).
13. **Budgets are enforced.** Each test has a time limit for its tier, set in the nextest configuration: the runner reports a test that passes its tier's slow mark and stops one that passes its tier's limit. CI reports when a tier's total test time goes over its budget: a warning on the pull request, and a failed job at one and a half times the budget. The numbers, and why the two differ, are in [testing-and-ci.md](testing-and-ci.md).

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

- **Goal and scope.** `cortex-protocol`: the `Op`, `Event`, item, message, part, usage, stop-reason and error-code types from [architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md), [03-agent-loop.md](architecture/03-agent-loop.md) and [04-providers.md](architecture/04-providers.md), including client roles, plan, usage (session and report) and project operations, and the wire form of levels and leases. JSON-RPC envelopes. `xtask schema` generating JSON Schema and TypeScript declarations into `docs/protocol/`. **The non-blocking workflows, in their first form** ([testing-and-ci.md](testing-and-ci.md), "Not blocking"): `nightly`, which runs `cargo deny check advisories`, the unused-dependency check and a release build for every target; and `main`, which after each merge builds the release profile and records the binary's size and the crate count. A failure opens or updates an issue. They are placed here, in the first step after the skeleton, so that the nightly tier is not empty for long; later steps add to them (fuzzing with the first decoder in step 5, the performance checks in step 20). **Added by the owner on 2026-11-12:** the protocol side of speed tiers ([decision 0027](decisions/0027-speed-tiers.md)): the setting for the session and per role, the speed a request ran at in its usage record, the fallback and cooldown notices, and the speeds a model offers. And a repo-root `CLAUDE.md` that includes `AGENTS.md`.
- **Depends on.** 2.
- **Done when.** The two workflows have each run once, and a forced failure has opened an issue. Round-trip tests for every type. Unknown fields and enum values tolerated where the versioning rules say so. The schema test fails when the checked-in schema is stale. One golden transcript exists. The crate depends only on serde and serde_json in a normal build, and its clean build time is recorded.
- **Must not change.** Other crates.
- **Size.** M. **Parallel.** No.

## Step 4: Base primitives

- **Goal and scope.** `cortex-base`: error kinds, `Secret`, ids, clock and RNG traits with fakes, the cancellation token tree, workspace paths with the Windows rules, text width, wrapping and truncation helpers, atomic file writes, the newline-delimited JSON-RPC reader and writer, the modification-time check used for hot-reload.
- **Depends on.** 3.
- **Done when.** Property tests for path normalisation and truncation. Windows-only tests for case, device names, UNC rejection and replace-while-locked. A test proves `Secret` cannot be printed or serialised. The framing reader passes every chunk split and both line endings.
- **Must not change.** `cortex-protocol` types, except agreed additions.
- **Size.** M. **Parallel.** No. Steps 5, 6, 8, 9 and 20 start after it.

## Step 5: Provider abstraction and test kit

- **Goal and scope.** `cortex-llm` without any real protocol: request and event types, the `Protocol`, `StreamDecoder`, `Transport`, `ModelClient` and `AuthMethod` traits, the SSE parser, the retry layer, usage normalisation, the catalog loader with a first `catalog/models.toml` including variant defaults, the HTTP transport. `cortex-testkit`: the scripted provider, the scripted SSE server, fake clock helpers. **Speed tiers** ([decision 0027](decisions/0027-speed-tiers.md), [architecture/04-providers.md](architecture/04-providers.md)): the request carries a speed and the usage carries the speed that ran; the catalog format has per-model speed offers, each with its own rate card; the classification tells a tier's own limit from the model's; and the retry layer has the wait, the fallback to standard and the cooldown.
- **Depends on.** 4.
- **Done when.** The SSE parser passes fixtures split at every byte. The retry layer is tested against the fake clock for every row of the classification table, including server waits and the maximum wait. The scripted provider can express text, reasoning, tool calls, usage, mid-stream errors, stalls and aborts. The real transport is tested against the scripted SSE server, including cancellation and the idle timeout. The HTTP client's creation cost is measured and it is created lazily. A request refused for a speed tier's own limit waits when the provider's wait is short, and otherwise falls back to standard and starts a cooldown that ends on the fake clock; with fallback off it is an ordinary rate limit; a response that reports another speed than was sent is recorded at the speed it reports.
- **Must not change.** `cortex-protocol` beyond agreed additions.
- **Size.** L. **Parallel.** With 6, 8, 9 and 20.

## Step 6: Tool contract and read-only tools

- **Goal and scope.** In `cortex-tools`: the `Tool` trait, `Plan`, effects, concurrency classes, results, the output policy with structural compression and artifacts (through a content-store trait), file snapshots. The tools `read` (with changed-only re-reads), `list`, `search`. The dynamic-tool registry with `tool_search` and `tool_call`.
- **Depends on.** 4.
- **Done when.** Contract tests for every row of each tool's section in [architecture/05-tools.md](architecture/05-tools.md). Re-read tests: unchanged, small change (a diff), large change, `fresh`. Property tests for the output policy: never cuts inside a line, always says what was cut, the artifact holds everything. Search over a large generated tree within a recorded time and cancellable. `tool_call` validates arguments and never changes the declared tool list (test). Windows path cases pass.
- **What later steps inherit,** all described in [architecture/05-tools.md](architecture/05-tools.md):
  - **Step 7** runs every call with `Tool::plan` and then `execute`, never `Tool::run`, puts every result it makes itself (a plan error, a denial) through `bound`, and schedules with `Plan::conflicts_with` and `Plan::has_side_effects`. `plan` may ask the file system where a path leads, so the loop calls it through `run_blocking`. A `RunCx` is made per call, with a child of the turn's cancellation token and the turn's number. `MemoryStore` is the content store of a session with no disk.
  - **Step 8** implements `ContentStore` (artifacts written in pieces, file versions under their `ContentHash`) for the session directory, or step 13 adapts its stores to the trait. It files checkpoints under `ContentHash::to_hex`.
  - **Steps 10 and 11** add their tools to the `Toolbox`. The editing tools read `FileTable::staleness` and call `FileTable::record_write`; they declare `Effect::Write` and `Concurrency::Keyed`. The shell tool shapes its own output with `output::shape` (`Keep::HeadAndTail`, or `Keep::Important` for builds and tests) and streams its artifact through `ContentStore::begin_artifact`.
  - **Step 12** decides each `Effect` of a plan, supplies the `ReadGate` that a directory walk asks about each entry, and matches `Effect::Ext` by `source/tool`.
  - **Step 15** calls `FileTable::begin_epoch` when a context epoch starts, and carries `FileTable::seen` across compaction.
  - **Step 18** implements `Outliner`, which `read` asks for the outline of a file too large to show whole.
  - **Steps 28 and 30** register their tools with `ToolRegistry::set_source`.
- **Must not change.** `cortex-base` and `cortex-protocol` beyond agreed additions.
- **Size.** L. **Parallel.** With 5, 8, 9 and 20.

## Step 7: Agent loop

- **Goal and scope.** `cortex-agent`: the turn state machine, streaming, early start of side-effect-free calls with a result cache, the tool scheduler, steering and queueing, plan-edit notices, retries, stop reasons, loop guards, limits as a ledger charged to the session's lease, interruption, statistics events through a trait, a verification stage behind a trait (stubbed), several sessions per core. Generic over the model client, tool set, policy (stub) and session store (in-memory). Context assembly is a stub until step 15. Each request is sent at the speed in effect for its role, and the loop emits the `speed_fallback` and `speed_cooldown` notices and keeps the session state's speed in effect current ([decision 0027](decisions/0027-speed-tiers.md)).
- **Depends on.** 5, 6.
- **Done when.** Every state transition has a test with the scripted provider and fake tools. Tests cover parallel read-only calls, same-path serialisation, exclusive calls, results in call order, exactly one result per started call. Early start: a read begins before the response ends (asserted by event order), and a failed response that is retried reuses or discards early results correctly with no side effect having run. Interruption in each state leaves a history a following request accepts. The whole suite runs in under 5 seconds.
- **What later steps inherit,** all described in [architecture/03-agent-loop.md](architecture/03-agent-loop.md):
  - **Step 8 and step 13.** The loop sees a session only as `SessionStore` (`open`, `append`, `end_turn`). Step 13 adapts `cortex-session` to it, running each blocking call on the blocking pool. `open` gives the active branch's items, already repaired, and the highest turn and item numbers in the log (`Log::last_ids` of `cortex-session`); from then on the loop makes the ids (`t_1`, `i_1`). After a rewind or a change of branch the core calls `SessionHandle::reload`, and the loop reads the session again before its next turn. A discarded request attempt is `Record::Attempt`, and the turn's outcome arrives with `end_turn`. After an append fails, the loop ends the turn and opens the store again before the next one, so a store that repairs on open brings memory back to what the log holds.
  - **Step 10.** A user's edit to the plan reaches the model through `SessionHandle::notice(NoticeSource::Plan, text)`: the loop delivers it at the next safe boundary, and step 10 writes the text. `plan` and `remember` must say that they change the session (settled in step 10: `Plan::changes_session`). A turn "changed files" when a call with a `Write` effect succeeded: that is what sends it to verification.
  - **Step 11.** A plan with an `Exec` effect never starts before its response has ended, and an exclusive call runs alone. What the tool sends to its `ToolOutput` reaches front ends as `tool_output` deltas. **A tool must stop when its token fires:** the loop waits for the result of every call it started, so a tool that ignores its token holds up the interrupt.
  - **Step 12.** `cortex-policy` is reached through the `Policy` trait: `decide` is called once per call, off the session's task, as soon as the call is planned, which can be while the model is still writing; `ask` puts the question and returns the answer, one question at a time in call order, and the loop drops the future to withdraw a question; `read_gate` supplies the gate of a walk. A denial that ends the turn is the outstanding item above.
  - **Step 13.** A session is `Session::open(SessionParts)`, then `Session::run` as one task; everything else goes through `SessionHandle` (`prompt`, `interrupt`, `notice`, `set_speed`, `set_role_speed`, `state`, `usage`, `reload`, `close`). The core supplies the `EventSink`, which must not block (the bounded channel with merging of deltas is its job); the parts of the session's state the loop does not own (`SessionInfo`); the timeout of an approval, inside its `Policy::ask`; and the things sessions share: the model clients, one `SpeedCooldowns` table for the clients and every session, and any `Account` of the ledger. Not built here, for step 13 to place: a change of a role's model or reasoning level in a running session, and the content of an item in progress for `session/read`.
  - **Step 15** replaces `StubContext` behind the `Context` trait: `load`, `push`, `assemble`, `needs_compaction` and `compact`. An assistant item can be in the log with nothing in it (a response that was interrupted or failed before it said anything): a context leaves it out, as `cortex_agent::context::append` does, because no provider accepts an empty message. The states `Compacting` and the "compact once, then give up" rule for a provider's context overflow are built and tested against a scripted context. `Record` is open to more kinds (an epoch, a summary). Requests of the utility roles (the summary on `fast`) are step 15's to send, at the speed `SpeedSettings::of` gives the role, and to charge to the session's ledger and usage.
  - **Step 16** records the `StatEvent`s the loop hands to `Stats`: one at the end of each request, tool call and turn.
  - **Step 19** implements `Verifier`. The loop decides when a round runs (the model ended its turn, and the turn wrote files), counts the rounds against `LoopConfig::verify_rounds`, appends a failure as a notice and reports the last result in `turn.completed`. Files changed by a shell command do not count as written yet: step 19 decides how step 11's detection feeds in.
  - **Step 37.** A lease's budget becomes an account with `Account::for_lease`.
- **Must not change.** The trait shapes from steps 5 and 6 without recording it.
- **Size.** L. **Parallel.** With 8 to 11, 14 and 20.

## Step 8: Session store

- **Goal and scope.** `cortex-session`: the JSONL log, entry kinds including `base`, `outcome` and the reserved `escalation` and `decision`, the tree and active branch, durability settings, streaming read, tail repair, resume, rewind, fork, artifacts, the snapshot (checkpoint) store, the index. Recording of the workspace's base state in the background. Leases are read from the log through the protocol's strict decoder: one that this build does not understand in full is refused, not opened in part ([decision 0028](decisions/0028-enforced-values-fail-closed.md)).
- **Depends on.** 4.
- **Done when.** A log cut at every byte of its last entry opens cleanly. Dangling tool calls are repaired on resume. Unknown kinds and fields are skipped. A failed write leaves memory and disk in agreement. A turn's snapshot restores exactly, including files that did not exist. The base state is recorded for a git and a non-git workspace without delaying the first prompt. Opening 10,000 entries and listing 1,000 sessions meet the budgets. Files are owner-only on both OSes.
- **What later steps inherit,** all described in [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md):
  - **Step 7** owns turn and item ids while a session is open, starting after `Log::last_ids`. `Session::append` fails without changing anything, in memory or on disk, so the loop treats its error as fatal for the turn. With durability `turn`, the loop calls `Session::end_turn` at the end of each turn.
  - **Step 10**'s editing tools are in `cortex-tools`, which cannot see `cortex-session`, so they reach the snapshot through a seam of their own crate, which the core fills with a `RoundCheckpoint` per tool round: `before` for every file a tool will change, `after` with what it left, and `finish` into the round's `checkpoint` entry. Files over `KEEP_LIMIT` (16 MiB) are recorded and cannot be restored. `before` keeps regular files only and never follows a link, so a tool that changes a file through a link passes the resolved path.
  - **Step 11** records what a shell command changed through the same checkpoint store and the same limit, and runs git with `base::GIT_HARDENING`. Step 13 turns its report into a round's checkpoint with `RoundCheckpoint::record_before`: a created file as `Before::Absent`, a file in `restorable` as `Before::Version` of its content at `base_commit` (stored with `DiskStore::put_stream`), and any other changed file as `Before::Unknown`, which an undo reports and never removes or overwrites.
  - **Step 12** reads leases from the log only through `Log::lease_in_force`, which refuses rather than reads in part, and replaces `base::is_likely_secret` with the read rules. Done: `BaseCapture` takes a `KeepGate`, and the list is gone.
  - **Step 13** adapts `DiskStore` to `cortex-tools`' `ContentStore` (`begin_artifact` and `ArtifactFile::finish`, `open_artifact`, `put_blob` for `put_version`, `get_blob` for `get_version`), computes the project key with `AbsPath::project_key`, maps `session.durability` to `Durability`, builds the redaction filter behind `Redactor`, starts `BaseCapture` with the project's trust after the session starts, and appends what it returns.
- **Must not change.** `cortex-protocol` beyond agreed additions.
- **Size.** M to L. **Parallel.** With 5 to 7, 9 and 20.

## Step 9: Configuration and credentials

- **Goal and scope.** `cortex-config`: layers, merging, profiles, provenance, validation, locks, the trust classification of keys, schema generation, reload with the "old configuration stays on error" rule, the credential store for both OSes behind the `AuthMethod` layer, the state-directory files, discovery of other tools' files (reported, not yet acted on). The speed tier for the session and per role, and whether to fall back, with key names settled here ([decision 0027](decisions/0027-speed-tiers.md)). The utility roles are at `standard` by default and do not inherit the session's speed (decided in the review of step 3, 2026-11-12).
- **Depends on.** 4.
- **Done when.** A table-driven test covers every merge rule, layer order and profile. Every key is classified for project trust (test). Provenance output is snapshot-tested. A reload with an invalid file keeps the old configuration. Credentials round-trip on both OSes, the file store is owner-only, and concurrent writers do not corrupt it. Loading configuration meets its start-up time.
- **Must not change.** Other crates.
- **Size.** M to L. **Parallel.** With 5 to 8 and 20.

## Step 10: Editing tools and the plan

- **Goal and scope.** `write`, `edit`, `plan`, `remember` (storing proposals through a trait until step 17), with the staleness guard, the two matching tiers, line-ending and byte-order-mark preservation, atomic writes and per-turn snapshots.
- **Depends on.** 6, 8.
- **Done when.** Property tests for `edit`: a failed call leaves the file byte-identical; applying then reversing restores the original; mixed and CRLF endings survive; overlapping and ambiguous edits are refused. The tolerant tier is always reported. Staleness is tested. Undo of a turn restores every file the turn's tools changed. A user's edit to the plan reaches the model as a notice and the next tool result returns the current plan. Windows: locked-file behaviour.
- **What later steps inherit,** all described in [architecture/05-tools.md](architecture/05-tools.md) ("`write`", "`edit`", "`plan`", "`remember`", "Checkpoints", "Text files"):
  - **Step 12** decides the one `write` effect of a `write` or `edit` call, whose path is the resolved file: for a path through a link, the file the link leads to, with whether that is inside the workspace. `plan` and `remember` declare no effect, so there is nothing to decide for them: their plans carry `changes_session`, which is for the scheduler. `Plan::conflicts_with` now also says that two session-changing calls conflict; step 12's own change to that function keeps both rules. **Left for steps 12 and 13:** the editing tools refuse to change a file over 16 MiB, because the change could not be undone. Asking the user through the policy, in place of refusing, would let such a change be made knowingly. The tools take their limit from `Editing::with_limit`, so the change is in how a refusal is turned into a question, not in the tools' reading of the file.
  - **Step 13** makes a session's tools: `WriteTool::new(editing)` and `EditTool::new(editing)`, where `Editing::new(checkpoints, clock)` holds the session's implementation of `cortex_tools::Checkpoints` and the clock that the retries of a replace wait on; `PlanTool::new(board)` with the session's `PlanBoard`; and `RememberTool::new(proposals)` with `MemoryProposals` until step 17. It implements `Checkpoints` over the round's `RoundCheckpoint`: `before(file, content)` stores the bytes with `DiskStore::put_blob` and records them with `record_before` (`Before::Absent` for no content, and `kept: false` with no blob for content over `RoundCheckpoint::limit`), `after(file, snapshot)` goes to `RoundCheckpoint::after` (no snapshot means no file, which is what a round records for a file that is gone), and the round is finished into its `checkpoint` entry when the round's results are recorded. `crates/cortex-core/tests/it/undo.rs` has that adapter as test support, with the tests that show undo restoring every file: lift it. It sets `Editing::with_limit` to the store's `KEEP_LIMIT`. For the plan it applies `plan/update` with `PlanBoard::apply`, hands the returned notice to `SessionHandle::notice(NoticeSource::Plan, text)`, and calls `PlanBoard::delivered(revision)` when it sees that notice recorded (its session-store adapter sees the notice item appended, and a queue of the revisions of the notices it handed over, in order, says which). **What `delivered` is for:** it saves a call, and nothing depends on it for correctness. The loop tells every tool call its round (`RunCx::round`, which moves on only when a round's results are recorded), and `plan` takes an edit as shown once a call of a later round arrives. So without `delivered` the model's first `plan` call after a user's edit is answered with the current plan and replaces nothing, even when the model had read the notice; with it, that call goes through. A caller that runs the tool outside the loop and gives no round number must call `delivered`, or every call after an edit is refused. It emits `plan.updated` after `apply`, and after a `plan` call whose payload says `replaced`. It answers `plan/get` with `PlanBoard::get`, and restores the plan of a resumed session with `PlanBoard::with_plan`: where the plan is kept in the log between sessions is step 13's to decide with step 15, which carries it across compaction.
  - **Step 15** carries the plan across compaction with `PlanBoard::get`, and the files read and changed with `FileTable::seen`, which the editing tools keep current (`record_write`).
  - **Step 17** implements `cortex_tools::Proposals` in `cortex-knowledge`: `propose(Proposal)` stores an entry as proposed, with its kind, text, evidence, check and turn, and answers `Stored` or `AlreadyKnown`. A check's path arrives relative to the workspace root and already inside it. A check's `command` is the model's own text, unverified: step 43, which runs checks, treats it as untrusted and puts it to the permission engine.
  - **Step 19** treats a turn as having changed files when a call with a `write` effect succeeded. A `write` or `edit` call that found nothing to change (the content was already there) succeeds too, with `kind: unchanged` in its payload: if that should not send a turn to verification, the payload is how to tell.
  - **Steps 23 and 24** show the payloads: `diff` for `write` and `edit` (absent, with `diff_omitted`, over 256 KiB), `ranges` and `tolerant` for `edit`, and `plan` for `plan`.
  - **Whoever adds a file-reading tool** opens with `cortex_base::open_regular`, never with a look and then an open. `read`, the editing tools and the checkpoint store do.
- **Must not change.** The `Tool` trait.
- **Size.** L. **Parallel.** With 7, 11, 12 and 14.

## Step 11: Shell tool and process runner

- **Goal and scope.** The process runner: process groups and Job Objects, pipes, merged output, the bounded buffer and artifact spill, timeouts, staged termination, clean-up. The `shell` tool. Shell selection per OS, with PowerShell 7, Windows PowerShell and `cmd.exe` each handled explicitly. Detection of files changed by a command. A launcher hook that step 26 fills.
- **Depends on.** 6.
- **Done when.** On both OSes a command with a grandchild that ignores the polite interrupt is gone after cancellation and after timeout, and children do not survive the harness being killed. Each Windows shell has quoting, merged-output and exit-status tests. Large-output tests cover streaming, head-and-tail and the artifact. Background children holding the pipes are detected. Changed-file detection is tested with and without git. Process start overhead is measured.
- **What later steps inherit,** all described in [architecture/05-tools.md](architecture/05-tools.md) ("`shell`") and [decision 0030](decisions/0030-process-tree-control-in-cortex-sandbox.md):
  - **Step 12** decides the `exec` effect, which carries the command exactly as the model wrote it, and the `write` effect on the workspace root beside it. A command it classifies as read-only is marked by setting the plan's concurrency to read-only: such a call runs beside others, and is not asked what it changed.
  - **Step 13** builds the tool for a session: `ShellSearch::from_env().find(…)` with `shell.program` and `shell.args`, `Shell::probe_version` off the start-up path (and kept in the state directory if PowerShell's answer proves slow), and a `ShellConfig` from `[shell]`, whose removal list is the key variable of every configured provider and `shell.env_remove` (the keys of the known providers are removed whatever it holds), and whose `repository_trusted` is the project's trust: without it the look for changed files does not ask git. It gives the runner the guard (`Guard::new` with the harness's own executable and a hidden subcommand whose body is `cortex_sandbox::tree::run_guard`), and wires that subcommand into the binary. It turns the `changes` of a result's payload into checkpoints through the session's checkpoint store, with that store's size limit: the content of each `restorable` file from `base_commit`, and every other changed or deleted file as changed, before unknown, and not restorable. `cortex-tools` cannot see `cortex-session`, so the payload is the seam, as step 8's notes foresee for the editing tools. The two crates each hold the list of flags their git commands start with (`cortex_tools::shell::changes::GIT_HARDENING` and `cortex_session::base::GIT_HARDENING`), for the same reason: one list in `cortex-base` would serve both, and is an addition to that crate for the owner to agree. It runs the tool's future on the core thread, never on a pool: the death signal is tied to the thread that started the command.
  - **Step 17** implements `CommandProfile`, which says whether a command is one of the project's builds or test runs, so that the lines of a long output that report a problem are kept first.
  - **Step 19** runs verification commands through the same `Runner`.
  - **Steps 13 and 23** decide how a second interrupt reaches a running command. The runner stops a tree in stages with a grace between them. A second token in `RunCx`, for "kill now", is the smallest way to skip the graces.
  - **Step 26** implements `Launcher` in `cortex-sandbox` and has `cortex-core` hand it to the tool: it is given the shell's command line and returns the launcher's, with the policy as the bytes of its standard input, and may add a line to the result that says what the sandbox blocked. It puts `landlock` and `seccompiler` behind a feature of `cortex-sandbox`, so that `cortex-tools`, which depends on that crate for process trees, does not link them ([architecture/01-workspace.md](architecture/01-workspace.md), rule 8). A session and the death signal are set by the runner before the launcher runs, so the launcher need not set them.
  - **Steps 28 and 30** start hook commands and MCP servers as trees with `cortex_sandbox::tree`, which both crates already depend on.
  - **Step 49** creates an AppContainer process itself, where the standard library cannot, and puts it in the same kind of Job Object.
- **Must not change.** The `Tool` trait and the output policy.
- **Size.** L. **Parallel.** With 7, 10, 12 and 14.

## Step 12: Permissions

- **Goal and scope.** `cortex-policy`: the one autonomy model from [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md). The ladder of levels, with `read-only`, `supervised`, `autonomous` and `unrestricted` usable now and `guided` defined for step 37. The `Lease` type with its grant, stop conditions, expiry and grantor, `narrow` and `is_within`, and the standing lease every session holds. Rules and their evaluation, built-in deny rules, the three safe lists, command analysis that fails closed (choosing the parser as that page describes), the read-only command table, approvals at three scopes, project trust with hash pinning. The engine reads leases, grants and rules only through the protocol's strict types, which refuse an unknown field or value ([decision 0028](decisions/0028-enforced-values-fail-closed.md)); it adds no lenient path.
- **Depends on.** 6, 9.
- **Done when.** A table of commands with expected outcomes is tested, including every bypass class in [security.md](security.md). Property tests: adding an `allow` never changes a `deny`; `narrow` is always within both inputs; no sequence of narrowing widens a lease; a narrowed lease never has a higher level, a later expiry or a larger budget than its parent. `autonomous` is refused where the sandbox does not enforce. Projects can tighten and cannot loosen without trust. A changed definition is disabled until re-approved. `cmd.exe` and PowerShell cases run on Windows.
- **What later steps inherit,** all described in [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md):
  - **Step 13** makes one `Engine` per session with `Engine::start(Parts)` and adapts it to the three seams, none of which `cortex-policy` can see.
    - **`Policy::decide`** is `Engine::decide` on a `Request`: the tool's name, its spec's default permission, and each effect of the plan as an `Access` (`Read` and `ReadTree` with the resolved path, `Write`, `Exec` with the command and the resolved working directory, `Network`, `Ext`). `Outcome::Allow { read_only: true }` is the loop's `AllowReadOnly`; `Deny` carries the reason for the model.
    - **`Policy::ask`** puts the `Question` to a front end as an `approval/request` with `Question::choices()`, shows what "for this session" and "in this project" would add (`Question::lifts`), applies the timeout, and passes the answer to `Engine::record`, which is blocking for a project approval and so runs on the blocking pool. With nobody to ask it answers a denial with `Question::refusal()`, or the loop's `Stop` when the run was started with `--fail-on-denial`, which is step 13's flag ([architecture/10-headless.md](architecture/10-headless.md)). A denial is listed in the result's `permission_denials`.
    - **`Policy::read_gate`** is `Engine::may_read`, and **the `KeepGate` of `BaseCapture`** is `Engine::may_store`.
    - **`Parts`** is filled from what the core already has: the level from `permissions.mode`; every rule of configuration with the layer it came from (`LayerKind` to `RuleSource`), and `--allow` rules as `CommandLine`; the stored approvals from `ApprovalStore::load` at `StateDir::approvals_file`; `default_sandbox` with the session's temporary directory and `sandbox.write` and `sandbox.protect`; `Protected` with the configuration directory and the state directory closed, the data directory unwritable and `other_sessions` for this project's key; `ShellSyntax::of_program` of the shell the tool found; the tools' own `PathResolver` as the `Locate`; and `SandboxState::NotEnforcing(NoSandbox::NotBuilt)` until step 26.
    - **Trust** comes from `TrustStore::project` at `StateDir::trust_file`. The project is trusted for its allow rules only while they are the ones the user saw: `ProjectTrust::allows_rules` with the rules the project's files hold now. That answer is what is passed to `cortex-config` as `ProjectTrust`, to the engine as `Parts::trusted`, to the shell tool as `repository_trusted` and to `BaseCapture`. An unreadable trust store or approvals file is shown as an error and treated as "nothing trusted" and "nothing approved".
    - **At start it shows `Engine::sandbox_notice()`** when there is one, and refuses the session with `LevelError::to_error_data()` when the level cannot be held. It records `Engine::lease()` as the session's lease. `cortex permissions` lists `Engine::approvals()` and the trust store, and removes with `Engine::revoke` and `TrustStore::revoke`.
  - **Step 17** hands the project's learned commands to the engine as rules with the source `Learned`: an allow among them waits for trust like a project's own.
  - **Step 26** reports the sandbox's state as `SandboxState`, which is what lets `autonomous` start, and owns the sandbox policy type from then on: `default_sandbox` and `PROTECTED_IN_WORKSPACE` of `cortex-policy` are the default it replaces or adopts. The engine's scope is the policy's writable roots and its protected paths, read from the lease's grant.
  - **Step 27** sends `approval/request` with the choices and what each would add, and `trust/request` with each definition's standing (`Standing::Changed` is the protocol's `changed`). Only a `user` client's answer reaches `Engine::record`.
  - **Steps 28 and 30** make a `Definition` for each hook, extension and MCP server they load (`Definition::of` with everything that decides what it does, part by part), ask `ProjectTrust::standing`, and use only what is `Approved`. They pin with `TrustStore::trust` when the user trusts a project and `TrustStore::approve` when the user accepts a change. A tool of theirs is an `Access::Ext` by its full name, decided by rules on `ext` and otherwise by the tool's own default.
  - **Step 37** builds task leases on `LeaseExt`: `narrow` for what a task lease may hold under the standing lease, `is_within` for the check, `Decision::Stop` for the edge, and the `guided` level, whose row in `level::decider` and whose refusal in `level::check_standing` are the two places to change. An engine holds one lease today; holding a task lease over a standing one is that step's.
  - **Step 40** narrows a Manager's lease for each Employee with the same `narrow`. Rule patterns are relative to each session's own workspace, which that step checks is what a team wants.
- **Must not change.** The `Plan` and effect types beyond agreed additions.
- **Size.** L. **Parallel.** With 7, 10, 11 and 14.

## Step 13: Core assembly and headless mode

- **Goal and scope.** `cortex-core`: wire the loop, tools, sessions, configuration and policy into a `Core` that takes `Op` and emits `Event`, with approvals as requests to the front end and client roles. `cortex-cli`: argument parsing, mode selection, headless mode with the three output formats and the exit-code table. Help and usage text print whichever name the program was started with, `cortex` or `cx`. Subcommands for sessions, configuration, permissions and credentials. `session/set` changes a role's speed and the session's, and the session state reports the speed set and in effect. Whether headless mode gets a `--speed` flag is open for the owner ([decision 0027](decisions/0027-speed-tiers.md)).
- **From step 12.** The core adapts `cortex-policy`'s `Engine` to the loop's `Policy`, to the tools' `ReadGate` and to the session store's `KeepGate`, as step 12's section lists. `--fail-on-denial` is built here: with it, a call that needs an answer nobody can give ends the turn through the loop's `Stop`, with the outcome `denied` and exit code 6; without it the call is denied and the run goes on ([architecture/10-headless.md](architecture/10-headless.md)).
- **Depends on.** 7 to 12.
- **From step 14.** Three things to wire. **Images:** `cortex-core` implements `cortex_llm::ImageSource` over the session's content store and passes it to the provider client (`ProviderClientBuilder::images`), so that an image `read` stored as an artifact reaches the request ([architecture/04-providers.md](architecture/04-providers.md), "Images"). Without it a request that refers to a stored image fails. **The provider:** `cortex_llm::anthropic::provider()` is the built-in entry, with the flag that makes it replay every reasoning block; `anthropic::api_key` places the key. A user-defined provider that speaks `anthropic-messages` gets the stricter replay rule unless its entry sets `replay_reasoning = "all"`. **A configuration key:** `model.speed_cooldown_min`, default `"1m"`, a preference, mapped to `SpeedPolicy::min_cooldown` ([decision 0027](decisions/0027-speed-tiers.md)). Also: a spending cap arrives as an error of kind `provider` with the code `enforced_spend_limit_reached`, which headless mode should print as it is.
- **From step 11.** The binary gains a hidden subcommand for the guard of process trees, whose body is `cortex_sandbox::tree::run_guard`, and the shell tool's runner is given a `Guard` that starts the harness's own executable with it. A process-level test kills the real binary while a command with a grandchild runs, and sees both gone, on both OSes. The rest of what step 11 hands on is in its section.
- **Done when.** Process-level tests run the real binary against the scripted SSE server for every exit code. `jsonl` output validates against the generated schema. Standard output carries only the result in every format. Signals behave as documented on both OSes. `cortex --version` and a trivial headless run meet the start-up budget, under both names.
- **Must not change.** Crate APIs from earlier steps without recording it.
- **Size.** L. **Parallel.** No.

## Step 14: Anthropic protocol

- **Goal and scope.** The `anthropic-messages` protocol: requests with cache hints, reasoning controls and tool definitions; the stream decoder; error classification; usage; raw-block replay. Begin by re-reading the provider's current documentation and updating [architecture/04-providers.md](architecture/04-providers.md) where it has moved. Catalog entries. `xtask record` for scrubbed fixtures, run by hand. Fast mode ([decision 0027](decisions/0027-speed-tiers.md)): `speed` in the request with the beta header where a model needs one, the speed that ran from the response's usage, the fast tier's own rate-limit headers, the classification of its 429 and 529, and the catalog offers with their rate cards.
- **Depends on.** 5.
- **Done when.** Request snapshots for plain text, tools, reasoning, cache markers, a multi-round tool conversation, and a model switch that drops reasoning. Decoder fixtures for text, reasoning with signature, one and several tool calls, a server-side tool block, an unknown event, an error after a 200, and every stop reason, replayed at every chunk boundary. The prefix-stability test passes over a recorded conversation. The classification table is tested row by row. A live smoke test exists and is not in CI. A request snapshot at fast speed, a decoder fixture whose usage reports the speed, and the fast tier's limit in the classification table.
- **Must not change.** The traits from step 5 without recording it. Nothing outside `cortex-llm`, the catalog and the test kit.
- **Size.** L. **Parallel.** With 6 to 12 and 20.

## Step 15: Context management

- **Goal and scope.** In `cortex-agent`: system prompt assembly, instruction files (`AGENTS.md` and `CLAUDE.md`) with discovery, nested files and change notices, context epochs, budgeting, compaction with carried state, background compaction on the fast role, the overflow fallback, and the per-request context record (token estimates by source, and where the cached prefix first differed on a miss) passed to the statistics trait. The core instructions text is written here, by us.
- **Depends on.** 13, 14.
- **From step 14.** The history must be append-only for more than the cache. On Anthropic's current models every reasoning block is tied to the system prompt, the tools and every message before it, and a block sent back after any of them changed fails the provider's check, on some models with a 400 ([architecture/04-providers.md](architecture/04-providers.md), "Prompt caching"). So: the system prompt and the tool list are built once for an epoch and never rebuilt; a notice, a changed instruction file or the date goes in a new message at the end; and a compaction starts a new epoch, in which the kept turns carry no reasoning (the provider's documentation says the same). The check of the prefix can use `AnthropicMessages::stable_prefix`, which is the request body without its closing brackets and without the one cache marker that can sit inside the messages: for two requests of an epoch the earlier one's is the beginning of the later one's. It is a method of that protocol, not of the `Protocol` trait; if the debug-build check is to work for every protocol it becomes a trait method, which is this step's to record. The cached-prefix figures the context record needs are in each response's usage: `cache_read` and `cache_write`, by lifetime class.
- **Done when.** The prefix check holds across a long scripted session with steering, notices and tool rounds, and fails in a test that edits history on purpose. Compaction never splits a tool round, carries the plan and file lists exactly, and can be undone. Background compaction: the session does not stop when the summary is ready in time, replayed turns carry no reasoning, and the fallback waits. Budget and instruction-file tests as listed in [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md). By hand with the real provider: a session that compacts in the background and continues, with cache-read tokens reported in each epoch.
- **Must not change.** The session log format beyond agreed additions.
- **Size.** L. **Parallel.** With 20 to 22.

*Milestone: headless alpha. Publish a preview build.*

## Step 16: Statistics, roles and variants

- **Goal and scope.** `cortex-stats`: events, monthly files, aggregates, retention, the `cortex stats` commands. Model roles (`main`, `fast`, `judge`) with utility calls routed to `fast`. Budgets per session and per day. Variant points with catalog defaults and overrides, fixed per epoch and recorded with every outcome. The usage figures (cache hit rate included) in `usage.updated`. **Usage history for `/usage`** ([architecture/18-usage.md](architecture/18-usage.md)): request events carry the project key, the billing kind and a cost fixed when recorded; the aggregates hold usage buckets by local day, provider, model, project and billing kind, folding into month buckets after 400 days; the range and grouping query; the latest limits each provider reported, with the time seen; `cortex usage` and `usage/report`. **The opt-in refresh of limits** (the same page, "The opt-in refresh"): the user-only setting `usage.refresh`, `cortex usage --refresh` and the protocol method behind it, for the providers that exist by this step. It begins by reading each provider's current documentation for a usage endpoint, and a provider without one has no refresh. The per-request context record is stored. The request event and the usage buckets carry the speed tier a request ran at, the cost is priced at that speed's rate, and the usage query can group by speed ([decision 0027](decisions/0027-speed-tiers.md)).
- **Depends on.** 15.
- **Done when.** Recording never blocks the loop (a test with a stalled writer). Nothing is read at start-up (the start-up file-access test). Aggregates rebuild to the same values from the raw files. Disabling statistics leaves every feature working on defaults. A variant override changes the request snapshot and is recorded. Usage ranges are right across day, week, month and time-zone boundaries (fake clock), all-time totals survive the pruning of raw files, and a cost already recorded does not change when catalog prices change. A limit past its reset time is reported as unknown, not as full. `cortex usage --range month` meets its budget and makes no network connection. With `usage.refresh` empty nothing connects; with a provider listed, `cortex usage --refresh` makes one request, to that provider's documented endpoint with that provider's credential, carrying no usage data (a test against the scripted server), and a project's configuration cannot turn it on.
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

- **Goal and scope.** In `cortex-tui`: view functions for every element in [architecture/09-tui.md](architecture/09-tui.md): streaming markdown with the commit rules, code blocks with optional highlighting, tool chips, output, diffs, notices, errors, reasoning, the status line, themes with the accent and its tint, glyph sets, density. The status line shows a speed other than standard, and a cooldown while one runs ([decision 0027](decisions/0027-speed-tiers.md)).
- **Depends on.** 20, 3.
- **Done when.** Snapshots for each element at 60, 80 and 120 columns, both glyph sets, with and without colour, on light and dark backgrounds. Feeding a document token by token commits exactly the lines that feeding it at once produces. Markdown stress cases pass. Highlighting never delays a frame. The accent tint meets its contrast target against sample backgrounds (computed in a test). Frame cost with a 30-line live region meets the budget.
- **Must not change.** `cortex-term`'s public API without recording it.
- **Size.** L. **Parallel.** With 21.

## Step 23: The interactive application

- **Goal and scope.** Wire `cortex-tui` to `Core`: state from events, operations from actions, the approval, question and trust dialogs, slash commands, pickers, the viewer, steering and queueing, interactive mode in `cortex-cli`. The `/effort` picker has the fast-mode toggle the owner asked for: on `Tab`, with the hint `tab - fast mode`, shown only when the current model offers a fast speed ([decision 0027](decisions/0027-speed-tiers.md)).
- **Depends on.** 13, 21, 22.
- **Done when.** End-to-end tests under a pseudo-terminal on both OSes with the scripted provider: a full turn with tools, an approval, an interruption mid-stream and mid-tool, a steer, a queue, a resume, an undo. Every slash command then available has a test. Start to first frame, idle memory and streaming budgets are measured with the real application. The fast-mode toggle appears for a model that offers a fast speed and not for one that does not, and toggling it sends `session/set`.
- **Must not change.** `cortex-core`'s operations and events, except through the protocol with a schema diff.
- **Size.** L. **Parallel.** No.

## Step 24: Panels and live views

- **Goal and scope.** The panel mechanism and the first-release panels: plan (with editing), review, usage, project. The usage panel has its session side and its history side: usage and cost per provider and model for today, the week, the month and all time, grouped by project on request, with limits left and each cost's basis ([architecture/18-usage.md](architecture/18-usage.md)), and the refresh key, which asks the providers listed in `usage.refresh` and nothing else. `/context` shows the by-category breakdown. Hot-reload wired end to end for themes, key bindings and settings. The `/usage`, `/plan`, `/project`, `/profile`, `/verify` and `/reload` commands. The usage panel shows cost by speed tier, and a tier's own rate limit beside the provider's.
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
- **From step 11.** `landlock` and `seccompiler` go behind a feature of `cortex-sandbox`, off by default, which `cortex-core`'s `sandbox` feature turns on: `cortex-tools` depends on this crate for process trees and must not link the backends ([decision 0030](decisions/0030-process-tree-control-in-cortex-sandbox.md)). The launcher is an implementation of `cortex_tools::process::Launcher`, handed to the shell tool by `cortex-core`. The process tree, the session, the death signal and the guard are already there, in `cortex_sandbox::tree`.
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

- **Goal and scope.** The `openai-chat` protocol: requests, the stream decoder with index-keyed tool calls, usage in the stream, reasoning fields under their various names, error classification, capability flags and provider entries for OpenAI, OpenRouter and local servers, discovery of a local server's loaded context size. The `/usage` refresh for each of these providers that documents a usage endpoint. Re-read current documentation first. The `service_tier` of servers that have one, as speed offers on their catalog entries ([decision 0027](decisions/0027-speed-tiers.md)).
- **Depends on.** 5, 14.
- **From step 14.** What exists to build on: the fixture format and the scrubber (`cortex_testkit::fixture`), the recorder and the smoke test as ignored tests started by `cargo xtask record` and `cargo xtask live` (add this protocol's scenarios beside Anthropic's, and a key-file variable for each provider), the helper that replays a stream cut at every byte, and the fuzz package, which wants a target for this decoder. The image seam and the replay rule are protocol-neutral: a message from another provider or protocol is sent as its neutral projection, which is the switch this step's done criteria ask for. Each protocol writes its request body in an order that keeps the stable parts first; `anthropic/json.rs` is the small writer that does it and can be shared.
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

- **Goal and scope.** The `openai-responses` protocol, used statelessly: requests, the stream decoder, encrypted reasoning items stored and replayed as raw blocks, tool calls, usage, rate-limit headers, error classification, catalog entries. Its service tiers as speed offers, with a silent downgrade recorded at the tier the response reports and shown as a `speed_fallback` notice ([decision 0027](decisions/0027-speed-tiers.md)).
- **Depends on.** 31.
- **Done when.** The same fixture categories as step 14. A session switches between all three protocols and back with reasoning dropped and everything else kept. The prefix-stability test passes over a recorded conversation. Nothing is stored on the provider's side (asserted from the request snapshots). Limits reported by the provider appear in `/usage`.
- **Must not change.** The other adapters' snapshots. The neutral types, except agreed additions listed in the report.
- **Size.** M to L. **Parallel.** With 36 to 45.

### Step 36: Google Gemini protocol

- **Goal and scope.** Google's models. Choose between Google's two APIs from the documentation at that time and record the choice. Function calls, thought signatures replayed as raw blocks, the ordering rules for function responses, safety-block stop reasons, usage, catalog entries. Record what, if anything, the API reports about limits; where it reports nothing, `/usage` shows "not available". The `/usage` refresh for Google, if it documents a usage endpoint. Its service tiers as speed offers, read from the response header that reports a downgrade ([decision 0027](decisions/0027-speed-tiers.md)).
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

- **Goal and scope.** Only if the owner makes macOS a target: a Seatbelt sandbox backend, Keychain credential storage, terminal checks, release binaries for both architectures under both command names, and CI. One known gap to close: `cortex-config` reads the effective user id from `/proc/self/status` to judge whose a `.git` or `.cortex` is when it looks for the workspace root ([architecture/08-configuration.md](architecture/08-configuration.md), "Layers"). macOS has no `/proc`, so there no candidate counts and the root is always the working directory, until the user id is read another way.
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
  - **Left by earlier steps for this review:**
    - **From step 10: whether a file is still the one that was read, on Windows.** Just before `write` or `edit` replaces a file, they check that it is still the file they opened (`crates/cortex-tools/src/change.rs`, `Stamp`). On Linux that compares the device and the inode as well as the length and the modification time. On Windows it compares the length and the modification time only, so another file of the same length, put under the name within one tick of the clock, would pass. Compare the volume serial number and the file index too, once the standard library exposes them stably, or read them through `cortex_base::private::windows`.
- **Depends on.** Everything built so far. It can also be run before the first release at the owner's request.
- **Done when.** A report lists findings ranked by severity, each with its location, evidence, and a suggested fix sized S, M or L. Performance numbers are given before and after. Nothing is changed.
- **Must not change.** Anything.
- **Size.** L. **Parallel.** No.
