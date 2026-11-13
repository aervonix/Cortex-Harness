# Cortex-Harness design docs

Cortex-Harness is a terminal agent harness written from scratch in Rust: an LLM agent loop that reads and edits code, runs commands, and talks to several model providers.

These docs are the specification that every build step works from. They were written before any harness code. A step's session has only these docs and its own prompt, so they are meant to be complete enough to build from.

## Start here

| If you want | Read |
|---|---|
| What the product is for | [vision.md](vision.md) |
| The shape of the whole system in one page | [architecture/00-overview.md](architecture/00-overview.md) |
| What to build next, what the owner decided, and what is still open | [build-plan.md](build-plan.md) |
| Why a choice was made | [decisions/README.md](decisions/README.md) |

## Everything

### Product

| Page | Content |
|---|---|
| [vision.md](vision.md) | The pitch, the audience, the three pillars, the defaults, and where each goal is designed. |
| [ideas.md](ideas.md) | Five further ideas, all adopted by the owner, with where each is designed and built. |

### Architecture

| Page | Content |
|---|---|
| [00-overview.md](architecture/00-overview.md) | Goals, the shape, threads, the data flow of a turn, the rules that hold everywhere, glossary. |
| [01-workspace.md](architecture/01-workspace.md) | Crates, their jobs and dependency direction, cargo features, profiles, the dependency policy. |
| [02-runtime.md](architecture/02-runtime.md) | Concurrency, error handling, cancellation, logging. |
| [03-agent-loop.md](architecture/03-agent-loop.md) | The turn state machine, streaming, tool rounds, steering, retries, rate limits, stop reasons, guards. |
| [04-providers.md](architecture/04-providers.md) | Protocols, providers and models; stream events; tools; reasoning; caching; usage and cost; the catalog; credentials. |
| [05-tools.md](architecture/05-tools.md) | The tool contract and every built-in tool. |
| [06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md) | Levels and leases (one autonomy model), rules, command analysis, approvals, project trust, and what is sandboxed on each OS. |
| [07-sessions-and-context.md](architecture/07-sessions-and-context.md) | The session log, resume and branches, snapshots and undo, what goes into a request, instruction files, budgeting, compaction. |
| [08-configuration.md](architecture/08-configuration.md) | Layers, profiles, reloading, reading other tools' set-up, the first run, credentials. |
| [09-tui.md](architecture/09-tui.md) | Rendering, the look, panels, the editor, keys, slash commands, the terminal layer. |
| [10-headless.md](architecture/10-headless.md) | Output formats and exit codes. |
| [11-rpc-protocol.md](architecture/11-rpc-protocol.md) | Transports, messages, versioning, roles, methods, events, backpressure. |
| [12-extensions.md](architecture/12-extensions.md) | Commands, skills, hooks, script extensions, extensions the harness writes itself. |
| [13-mcp.md](architecture/13-mcp.md) | MCP client scope and how its tools reach the model. |
| [14-adaptation.md](architecture/14-adaptation.md) | Local statistics, the project profile and memory (with checks), per-model variants, model roles. |
| [15-efficiency.md](architecture/15-efficiency.md) | The repository map, changed-only re-reads, output compression, early tool start, prefetch, verification and speculative verification, the context profiler. |
| [16-run-engine.md](architecture/16-run-engine.md) | The orchestration layer, parallel attempts, replay and evaluation, corrections as evaluation cases. |
| [17-coordinator.md](architecture/17-coordinator.md) | The Coordinator: a Manager session running a team of Employee sessions. |
| [18-usage.md](architecture/18-usage.md) | The `/usage` view: usage and cost over time, limits left, and usage recorded by other tools. |

### Quality

| Page | Content |
|---|---|
| [performance-budgets.md](performance-budgets.md) | The numbers, how each is measured, and where each is checked. |
| [testing-and-ci.md](testing-and-ci.md) | Test tiers and their time limits, the scripted provider, TUI testing, Windows coverage, the CI layout with its measured times, lints and repo rules, dependency hygiene. |
| [security.md](security.md) | Threats and mitigations. |
| [dependencies.md](dependencies.md) | Every direct third-party dependency: which crate uses it, why, what else was considered and what it costs. Checked against the manifests. |

### Generated

| Page | Content |
|---|---|
| [protocol/](protocol/README.md) | The protocol's JSON Schema and TypeScript declarations, generated from the Rust types in `cortex-protocol` by `cargo xtask schema`. Not edited by hand, and checked against the types in CI. |
| [configuration/reference.md](configuration/reference.md) | Every configuration key: its type, its default, what a project's file may do with it, and when a change takes effect. Generated from the settings in `cortex-config` by `cargo xtask gen`, with a JSON Schema of the same keys beside it ([configuration/schema.json](configuration/schema.json)). Not edited by hand, and checked against the settings in CI. |

### Background

| Page | Content |
|---|---|
| [reference-notes.md](reference-notes.md) | How Pi and Opencode approach the same problems, what is worth matching, and where Cortex-Harness differs. |
| [decisions/](decisions/README.md) | One record per significant choice: options, the choice, why, and what would reopen it. |
| [measurements/](measurements/README.md) | The spikes: what was built, how it was measured, the numbers. |

### Plan

| Page | Content |
|---|---|
| [build-plan.md](build-plan.md) | The owner's decisions and what is still open, the feature table, the rules for every step, and the ordered steps. |
| [steps/](steps/README.md) | What each step recorded about itself, one folder per step: the owner's answers to its questions, and its CI times. |
| [outstanding/](outstanding/README.md) | What is still to be settled, one file per item. |

## How these docs are kept

- **The docs are the specification.** When code must differ from a page, the page changes in the same pull request.
- **Decisions are recorded.** A choice that changes gets a new or amended record, with the reason.
- **Provisional means the owner has not confirmed it yet.** Work proceeds on the recommendation. Each such item is a file in [outstanding/](outstanding/README.md), and each decision the owner has made is recorded with its date, in the build plan or in the [records](steps/README.md) of the step that asked.
- **Provider and protocol facts go stale.** Pages that depend on them say when they were read. The steps that implement them begin by reading the current documentation.
- **Generated files are checked.** Protocol schemas and the configuration reference are generated from code and verified in CI. The protocol's are in [protocol/](protocol/README.md) and the configuration's in [configuration/](configuration/reference.md); a pull request that touches either is never treated as documentation-only.

## Ground rules

1. **Original work.** Pi and Opencode were studied to understand problems. Nothing is copied from them: no code, structure, names, prompt text, UI or configuration format. No dependency that is a port or extract of their code.
2. **Speed and efficiency come first,** in the product and in the tooling. The blocking pull-request checks finish in under 5 minutes per OS.
3. **No personal details** in commits, docs, code, configuration or fixtures, and no absolute home-directory paths.
4. **Everything fresh.** Nothing is carried over from earlier attempts.
5. **The brand is Cortex-Harness.** Crates use the `cortex-` prefix. The licence is MIT.
