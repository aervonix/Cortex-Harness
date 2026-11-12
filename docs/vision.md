# Product vision

What Cortex-Harness is for. The architecture, the decision records and the build plan are designed to serve this page, and each of them points back to it.

## In one sentence

An agent harness that learns your project, your models and your habits, and stays fast while it does.

## Who it is for

Everyone who uses a terminal coding agent today, not only its authors. Someone coming from another agent should be productive in minutes. That sets four standing requirements:

- Onboarding that works on the first run.
- Defaults that are sensible without any configuration.
- Documentation good enough that nothing needs to be guessed.
- No feature that makes sense only to the person who built it.

## Three pillars

### 1. It adapts

| To | What that means |
|---|---|
| **The project** | The harness learns each repository's build and test commands, its conventions and its flaky tests. It keeps them in a project profile that the user can read and edit, and it updates the profile as it learns. |
| **The model** | Edit formats, tool schemas and prompt style are chosen per model, from success rates measured on the user's own machine. Routine work (searching, summarising, reading logs) goes to a cheap, fast model and reasoning goes to the strong one, inside a budget the user sets. |
| **The user** | Modes and profiles. Configuration, extensions, themes and key bindings reload without a restart. The user can ask for a new tool or command; the harness writes it as a sandboxed extension, the user reviews it, and it loads at once. |

Adapting to the terminal and the network is expected, not a headline. Windows shells (PowerShell and `cmd`) and Linux shells must all work properly.

### 2. It is lean without being bare

Full-featured, and efficient in every dimension that costs the user something.

| Dimension | What that means |
|---|---|
| **Tokens and money** | Context is lean: a structural map of the repository instead of whole files, and a re-read sends only what changed. Tool output is compressed, with the full output one key press away. The prompt is laid out to get the most from provider caches. Tokens, cost and cache hit rate are always visible. |
| **Wall-clock time** | A tool starts as soon as its arguments have finished streaming. Independent tools run in parallel. Likely reads are fetched ahead. Compaction runs in the background instead of stopping the session. |
| **Footprint** | Near-instant start, small memory, and smooth rendering even with very large outputs. |
| **The user's attention** | Smart permissions and automatic verification, so there is less to watch over. |

### 3. It is ahead

- **Parallel attempts.** Run several approaches or models at once in isolated worktrees, judge them with the project's tests, and let the user pick the winner.
- **Replay and evaluation.** Record real sessions and replay them against other models, prompts or settings, to measure which set-up works best on the user's own tasks.
- **One engine for both.** Running the agent several times and judging the results is the same machinery in both cases. This is the feature most likely to set Cortex-Harness apart: it measures how well a set-up works and improves it, where other tools only let the user configure it.
- **The Coordinator.** One session, the Manager, runs a team of sessions, the Employees. The user gives the Manager a task and an explicit grant of permissions. The Manager plans the work, creates a git worktree for each piece, starts an Employee in each on a model the user allowed, writes their prompts, answers their questions, and reviews and merges their work.
  - Three autonomy levels (supervised, guided, autonomous), all inside hard limits the user sets: a budget cap, no force-pushes, nothing outside the project.
  - Employees never get more permission than the Manager was given. Their approval prompts go to the Manager, which passes them to the user according to its level.
  - Merging into the main branch always needs the user unless they explicitly allowed it.
  - The user can see every Employee and talk to any of them. The Manager logs every decision it made without asking, with its reason.
  - It shares its machinery with parallel attempts, which is the case where every Employee is given the same task. Employees on other machines are not ruled out.
- **Core features.**
  - Automatic verification: after edits, work out which tests are affected, run them, and feed failures back.
  - A live, editable plan: a task list the user can reorder, skip or edit while the agent works.
  - Self-written extensions, as above.
  - Project memory across sessions, where every fact records where it came from and when it expires, and the user can view and edit all of it.
- **More to find.** Further proposals are in [ideas.md](ideas.md). They are options for the owner, not commitments.

## Defaults and policies

| Area | Policy |
|---|---|
| **Autonomy** | A smart default the user can change. Reads run automatically. Edits inside the project apply automatically, with a snapshot so they can be undone. Shell commands ask first, unless they are on a learned or approved safe list. |
| **Interface** | A clean chat-style flow by default. Panels for the plan, parallel attempts and diffs open when there is something to show and close again. The look is original. The accent colour is the logo's purple, `#7A3CFB`. |
| **Compatibility** | The harness reads `AGENTS.md`, `CLAUDE.md`, existing MCP server configurations and Agent Skills folders, so people can switch without rewriting their set-up. Everything else uses Cortex-Harness's own formats. MCP client support is included. |
| **Privacy** | Usage statistics stay on the user's machine and feed only the adaptive features. Nothing is sent anywhere. There is no telemetry. |
| **Sign-in** | API keys, and sign-in flows that providers officially offer to third-party tools. The sign-in layer is designed so subscription logins can be added later, after each provider's terms are checked. They are not in the first release. |

## Not headlines, and not ruled out

- **Branching time-travel:** forking the conversation and the files together, and comparing branches. Per-turn snapshots for undo are required regardless.
- **Persistent sessions** that can be re-attached from another terminal, an editor, or a web or phone client over the RPC protocol.

## Brand

- The name is Cortex-Harness.
- The logo is a near-white hexagonal "C" around a `>_` prompt with two purple sparkles, on a transparent background. It disappears on light backgrounds, so the README and docs need a dark-on-light version. Step 2 of the build plan adds both.
- The logo's purple is the accent colour of the TUI ([architecture/09-tui.md](architecture/09-tui.md)).

## Where each goal is designed

| Goal | Designed in | In the first usable release? |
|---|---|---|
| Project profile and project memory | [architecture/14-adaptation.md](architecture/14-adaptation.md) | Yes, in a first form. See the build plan. |
| Per-model formats and prompt style from local success rates | [architecture/14-adaptation.md](architecture/14-adaptation.md), [architecture/04-providers.md](architecture/04-providers.md) | Measurement yes, automatic selection later. |
| Model routing within a budget | [architecture/14-adaptation.md](architecture/14-adaptation.md) | Roles for utility calls yes, delegated sub-tasks later. |
| Modes and profiles, hot-reload | [architecture/08-configuration.md](architecture/08-configuration.md) | Yes. |
| Self-written extensions | [architecture/12-extensions.md](architecture/12-extensions.md) | Yes. |
| Repository map, changed-only re-reads | [architecture/15-efficiency.md](architecture/15-efficiency.md), [architecture/05-tools.md](architecture/05-tools.md) | Yes, in a first form. |
| Compressed tool output with the full output a key press away | [architecture/05-tools.md](architecture/05-tools.md), [architecture/09-tui.md](architecture/09-tui.md) | Yes. |
| Cache-ordered prompt | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md) | Yes. |
| Live tokens, cost and cache hit rate | [architecture/09-tui.md](architecture/09-tui.md) | Yes. |
| Tools start when their arguments finish; parallel tools; prefetch | [architecture/03-agent-loop.md](architecture/03-agent-loop.md), [architecture/15-efficiency.md](architecture/15-efficiency.md) | Yes for the first two, prefetch in a limited form. |
| Background compaction | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md) | Yes. |
| Start-up, memory, rendering | [performance-budgets.md](performance-budgets.md) | Yes. Budgets are checked in CI. |
| Smart permissions | [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md) | Yes. |
| Automatic verification | [architecture/15-efficiency.md](architecture/15-efficiency.md) | Yes with configured and learned commands, affected-test selection later. |
| Live, editable plan | [architecture/05-tools.md](architecture/05-tools.md), [architecture/09-tui.md](architecture/09-tui.md) | Yes. |
| Parallel attempts; replay and evaluation; one engine | [architecture/16-run-engine.md](architecture/16-run-engine.md) | No. First milestone after the release. |
| The Coordinator | [architecture/17-coordinator.md](architecture/17-coordinator.md) | No. Right after the release, on the same layer. The first release carries the permission, protocol and log pieces it needs. |
| Autonomy default with snapshots | [architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md), [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md) | Yes. |
| Adaptive panels, accent colour | [architecture/09-tui.md](architecture/09-tui.md) | Yes. |
| Reading `AGENTS.md`, `CLAUDE.md`, MCP configurations, Skills folders | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md), [architecture/13-mcp.md](architecture/13-mcp.md), [architecture/12-extensions.md](architecture/12-extensions.md) | Yes. |
| Local-only statistics | [architecture/14-adaptation.md](architecture/14-adaptation.md) | Yes. |
| Sign-in layer open to later logins | [architecture/04-providers.md](architecture/04-providers.md) | API keys only. The layer is ready for more. |
| Onboarding | [architecture/08-configuration.md](architecture/08-configuration.md), [build-plan.md](build-plan.md) | Yes. |
| Branching time-travel, re-attachable sessions | [architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md), [architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md) | Not as features. The design keeps them possible. |

The reasons for each "later" are in the feature table of [build-plan.md](build-plan.md). Where a goal pulls against a speed budget, the conflict and a recommended resolution are in that page's open questions.
