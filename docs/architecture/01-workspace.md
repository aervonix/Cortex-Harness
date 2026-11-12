# Workspace and crate layout

One Cargo workspace. Crates are split so that an edit recompiles as little as possible, so that heavy dependencies sit in leaf crates, and so that each crate can be tested alone.

## Crates

All crates use the `cortex-` prefix, live under `crates/`, and set `publish = false`.

| Crate | Job | Depends on (workspace) | Heavy third-party dependencies |
|---|---|---|---|
| `cortex-protocol` | The public data model: `Op`, `Event`, items, messages, parts, usage, stop reasons, error codes, RPC envelopes. Serde types only, no logic. JSON Schema generation behind a feature. | none | serde, serde_json (schemars behind `schema`) |
| `cortex-base` | Small shared primitives: error kinds, `Secret`, ids, clock and RNG traits, cancellation, workspace paths, text utilities (width, wrapping, truncation), atomic file writes. | protocol | unicode-width |
| `cortex-llm` | Provider abstraction: model request and model event types, the `Protocol` and `Transport` traits, SSE parser, retry classification and backoff, usage normalisation, the model catalog. One module per protocol. | protocol, base | reqwest and rustls (behind `http`), tokio |
| `cortex-tools` | Built-in tools and the `Tool` trait, the output policy, file snapshots, the process runner for shell commands, and the registry of dynamic tools behind `tool_search` and `tool_call`. | protocol, base | ignore, grep-searcher, grep-regex, globset, similar, tokio |
| `cortex-sandbox` | Sandbox policy types and the per-OS launchers (Landlock and seccomp on Linux, Job Objects on Windows). | base | landlock, seccompiler (Linux), windows-sys (Windows) |
| `cortex-policy` | Levels and leases, permission rules, shell-command analysis, approval bookkeeping, project trust. | protocol, base | none |
| `cortex-session` | Session log (JSONL), tree and branches, resume and repair, artifacts, checkpoints, the session index. | protocol, base | none |
| `cortex-config` | Configuration layers, merging with provenance, validation, credentials storage. | protocol, base | toml |
| `cortex-stats` | Local statistics: event recording, monthly files, aggregates, retention. | protocol, base | none |
| `cortex-knowledge` | Project profile and memory: entries, provenance, expiry, the two stores, the command observer, rendering for the prompt. | protocol, base | toml |
| `cortex-index` | Repository map: per-language outline extractors, the on-disk cache, ranking, the map renderer. | base | ignore |
| `cortex-agent` | The agent loop: turn state machine, tool scheduler, steering queue, verification stage, context assembly, compaction, instruction files, system prompt. Owns no I/O of its own: it is generic over the provider, tool and store traits. | protocol, base, llm (traits only), tools (trait only), policy, session, knowledge, stats | tokio |
| `cortex-ext` | Extension host: custom commands, skills, hooks, the script runtime and its host functions, trust pinning, authoring of new extensions. | protocol, base, policy, sandbox, tools (trait) | the script engine (isolated here) |
| `cortex-runs` | The orchestration layer: isolated workspaces, session supervision, messaging, the budget ledger, judges. Parallel attempts and replay. After the first release. | core, session, stats, knowledge | none |
| `cortex-coordinator` | The Coordinator: team leases and sub-leases, the team rows of the level table, approval routing, the escalation queue and decision log, the Manager's tools, integration. After the first release. | runs, policy | none |
| `cortex-usage` | Read-only adapters for usage recorded locally by other coding agents, and their cache. After the first release. | base, stats | none |
| `cortex-mcp` | MCP client. Registers server tools with the dynamic-tool registry. Reads other clients' server definitions. | protocol, base, tools (trait and registry), sandbox | rmcp (isolated here) |
| `cortex-core` | Wires everything into a `Core` that accepts `Op` and emits `Event`. The only crate that knows all the parts. | all of the above except runs and coordinator, which depend on it | none new |
| `cortex-rpc` | JSON-RPC server over stdio and local sockets, on top of `Core`. | protocol, core | none new |
| `cortex-term` | Terminal layer: raw mode, input decoding, capability detection, the line renderer and its two presenters. No knowledge of agents. | base | crossterm |
| `cortex-tui` | The interactive UI: transcript, editor, dialogs, status line, slash commands, themes. | protocol, base, term | pulldown-cmark; a syntax highlighter behind a feature |
| `cortex-cli` | The `cortex` binary, also installed under the alias `cx`: argument parsing, mode selection, headless printer. Kept tiny because it relinks on every change. | core, rpc, tui, config | lexopt |
| `cortex-testkit` | Test support: scripted provider, scripted SSE server, virtual terminal, PTY driver, temp workspaces, fake clock. Dev-dependency only. | protocol, base, llm, term | vt100, portable-pty |
| `xtask` | Developer commands: schema generation, perf runs, fixture recording, release packaging. Not shipped. | as needed | none shipped |

## Dependency direction

```
protocol ◄─ base ◄─┬─ llm ◄──────────────┐
                   ├─ tools ◄────────────┤
                   ├─ sandbox ◄──────────┤
                   ├─ policy ◄───────────┼─ agent ◄─┐
                   ├─ session ◄──────────┤          │
                   ├─ stats, knowledge ◄─┘          ├─ core ◄─┬─ rpc ◄─┐
                   │    └─ usage (later) ◄──────────┤         │        │
                   ├─ config, index ◄───────────────┤         ├─ runs ◄─ coordinator
                   │                                │         │        ├─ cli
                   ├─ ext, mcp ◄────────────────────┘         │        │
                   └─ term ◄─ tui ◄───────────────────────────┴────────┘
```

Rules, enforced by a workspace test that reads `cargo metadata`:

1. `cortex-protocol` depends on no workspace crate. It is the contract, so it stays small and stable.
2. `cortex-agent` depends on traits, not implementations. It never names reqwest, a concrete provider, or a concrete tool. That is what lets the loop be tested with a scripted provider and fake tools in microseconds.
3. `cortex-tui` and `cortex-term` never depend on `cortex-core`, `cortex-agent`, `cortex-llm` or `cortex-tools`. The TUI sees only `cortex-protocol` types. A UI edit therefore never rebuilds the core, and the UI cannot reach around the protocol.
4. `cortex-core` and `cortex-rpc` never depend on `cortex-tui` or `cortex-term`.
5. Only `cortex-llm` (feature `http`) links the HTTP and TLS stack. Only `cortex-mcp` links the MCP SDK. Only `cortex-tools` links the search libraries. Only `cortex-ext` links the script engine. A change elsewhere does not rebuild them.
6. Test support lives in `cortex-testkit`, never behind `cfg(test)` exports of production crates.
7. `cortex-usage` depends only on `cortex-base` and `cortex-stats`, and links no network code. Reading other tools' files stays apart from everything that can send.

## Why this split

- **Incremental builds.** The measured cost of a no-change relink of a binary carrying the full dependency set is in [../measurements/dependency-costs.md](../measurements/dependency-costs.md). The binary crate is kept tiny, and the two crates that change most during UI work (`cortex-tui`, `cortex-term`) sit beside the core, not under it, so they rebuild alone.
- **Test isolation.** The loop, the session store, the policy engine and the renderer each have tests that need no network, no terminal and no child process. Slow process-level tests are confined to a few crates (see [../testing-and-ci.md](../testing-and-ci.md)).
- **Dependency containment.** Each heavy dependency has one owner crate. Replacing it later (a different HTTP client, a hand-written MCP client) touches one crate.
- **Not too many crates.** Twenty-four, including test and tooling crates and three that arrive after the first release (`cortex-runs`, `cortex-coordinator`, `cortex-usage`). Each extra crate adds link and metadata overhead, so a new crate needs a reason from the three points above.

## Cargo features

Features exist to keep build cost optional, not to create product variants.

| Feature | Crate | Default | Purpose |
|---|---|---|---|
| `http` | `cortex-llm` | on | The real HTTP transport. Off for crates that only need the types. |
| `schema` | `cortex-protocol` | off | JSON Schema derivation. On only for `xtask` and the schema test. |
| `mcp` | `cortex-core` | on | Links `cortex-mcp`. |
| `scripts` | `cortex-core` | on | Links the script engine in `cortex-ext`. Without it, extensions are limited to custom commands, skills and hook commands. |
| `sandbox` | `cortex-core` | on | Links the OS sandbox backends. |
| `highlight` | `cortex-tui` | on | Syntax highlighting for code blocks and diffs. |
| `keychain` | `cortex-config` | on for Windows, off for Linux | OS keychain storage (see [08-configuration.md](08-configuration.md)). |

## Workspace settings

- `resolver = "3"`, `edition = "2024"`, one `[workspace.dependencies]` table so each third-party crate has one version and one feature set.
- `rust-toolchain.toml` pins the stable toolchain. The project builds a binary and publishes no libraries, so it follows current stable and promises no minimum supported Rust version.
- `[workspace.lints]` sets the shared Clippy and rustc lints (see [../testing-and-ci.md](../testing-and-ci.md)). Async crates deny blocking calls (`std::fs`, `std::process`, `std::thread::sleep`) through Clippy's disallowed-methods list.
- Profiles:

| Profile | Use | Settings |
|---|---|---|
| `dev` | Local work | `debug = "line-tables-only"`, dependencies built with `opt-level = 1` and `debug = false` |
| `test` | CI tests | as `dev`, `debug = 0` in CI through the environment |
| `release` | Local optimised builds, perf runs | `opt-level = 3`, `lto = "thin"`, `codegen-units = 16`, `debug = 0` |
| `dist` | Published binaries | inherits `release`, `lto = "fat"`, `codegen-units = 1`, `strip = true` |

`panic = "unwind"` in every profile. A panic in a tool task or an extension call is caught at the task boundary and becomes an error result, and the terminal is always restored. The size cost and the reasons are in [decision 0012](../decisions/0012-release-profile.md).

## Third-party dependency policy

Each direct dependency must earn its place. The rules:

1. Prefer the standard library, then a small focused crate, then a framework.
2. Default features off. Opt in to what is used.
3. A dependency that compiles C or C++ needs a decision record that states its measured build cost on both OSes. A dependency that needs build tools beyond the platform's own compiler (CMake, NASM, Perl, Go) is not accepted. Today that rules out aws-lc-rs, and it has kept out bundled SQLite and parser grammars on cost. The script engine is the one accepted C dependency ([decision 0005](../decisions/0005-extension-mechanism.md)).
4. Licences must be compatible with the project's MIT licence. The allow-list is in [../testing-and-ci.md](../testing-and-ci.md).
5. No dependency that is a port or extract of Pi or Opencode code.
6. Every direct dependency is listed with its purpose and owner crate in `docs/dependencies.md`, which the repo skeleton step creates. CI fails if `Cargo.toml` and that list disagree.

The initial set and the measured cost of each candidate are in [../measurements/dependency-costs.md](../measurements/dependency-costs.md).
