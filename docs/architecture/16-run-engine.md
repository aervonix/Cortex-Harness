# The orchestration layer: parallel attempts, replay and evaluation

The third pillar of the [vision](../vision.md) has three features that run more than one agent session at a time: parallel attempts, replay and evaluation, and the Coordinator. They share one layer. This page designs that layer and the first two features. The Coordinator is in [17-coordinator.md](17-coordinator.md).

None of this is in the first usable release. It is the first milestone after it. The reasons are at the end of this page, with what the first release does to prepare.

## One layer, three features

| Part of the layer | What it does | Used by |
|---|---|---|
| **Workspace manager** | Creates, tracks and removes isolated workspaces: a git worktree per session, or a directory copy where there is no git. | All three. |
| **Session supervisor** | Starts sessions, in this process or over the RPC protocol; watches their events; detects a stuck or runaway session; stops them. | All three. |
| **Messaging** | Sends a prompt or follow-up to a supervised session, and receives its results, questions and approval requests. | The Coordinator. Parallel attempts use only the first prompt. |
| **Budget ledger** | One account of money, tokens and time for a group of sessions, with hard caps. | All three. |
| **Judges** | Score an outcome: the project's checks, cost and time, diff measures, and optionally a model. | All three. |
| **Results** | Stores outcomes and scores in the local statistics. | All three. |

**A supervised session is an ordinary session.** It is reached through a handle with the same operations and events as any client uses ([11-rpc-protocol.md](11-rpc-protocol.md)). The handle hides whether the session runs in this process or in another one. That is what leaves room for sessions on another machine later, for example a `cortex serve --stdio` process started over SSH.

**Parallel attempts are the simplest use of the layer:** N sessions given the same task, judged, with the user picking one. The Coordinator is the general use: sessions given different tasks by a Manager session that reviews and integrates their work.

## What it is for

| Feature | The runs | The judge | What the user gets |
|---|---|---|---|
| **Parallel attempts** | One task, now, with N variants (different models, reasoning levels, or instructions). | The project's tests and checks, then the user. | The best attempt applied to their workspace. |
| **Replay** | One recorded session's task, again, with a different model, prompt or setting. | The same, plus comparison with what was accepted the first time. | "Would this set-up have done better on that task?" |
| **Evaluation** | Many recorded tasks × several variants. | The same, aggregated. | Which set-up works best on this user's own work, with numbers. The results feed the per-model choices in [14-adaptation.md](14-adaptation.md). |

## Concepts

```
Task ──┐
Base ──┼─► Run (one variant, one isolated workspace, one headless session) ─► Outcome ─► Judge ─► Result
Variant┘                                        × N in parallel                                    │
                                                                                                   ▼
                                                                        comparison, the user's pick, statistics
```

| Concept | Definition |
|---|---|
| **Task** | The prompt (or the first prompt and its follow-ups) and the checks that define success. |
| **Base** | The exact starting state of the workspace: a commit plus a patch of uncommitted changes, or a directory snapshot where there is no git. |
| **Variant** | What differs between runs: provider and model, reasoning level, variant points from [14-adaptation.md](14-adaptation.md) (edit format, prompt style), extra instructions, limits. |
| **Run** | One headless session of the ordinary core, in its own workspace, under a non-interactive permission policy and a budget. |
| **Outcome** | The diff against the base, the session log, usage, cost, duration, and how the run ended. |
| **Judge** | Something that scores an outcome. |
| **Result** | An outcome with its scores. Stored in the local statistics. |

A run is an ordinary session. The engine adds isolation, scheduling, judging and comparison. Nothing in the agent loop is special-cased for it.

## Isolated workspaces

| Situation | Mechanism |
|---|---|
| The workspace is a git repository | A git worktree per run, created from the base commit, with the base patch applied. Worktrees share the repository's object store, so they cost little disk for source. |
| No git | A copy of the directory, honouring ignore rules. A size check runs first and refuses above a limit. |

- Workspaces live under the state directory, never inside the user's workspace.
- A `runs.setup` command from the project profile runs in each workspace before the agent starts (install dependencies, link caches).
- Each run's sandbox policy makes its own workspace writable and nothing else.
- Workspaces are removed when a comparison is closed, and at start-up if a crash left any behind. Worktrees are pruned from the repository's metadata.

**The cost that cannot be designed away.** A fresh worktree has no build output. For compiled projects, N attempts mean N cold builds, in time, CPU and disk. The engine therefore:

- shows an estimate before starting (N × the usual build and test durations from the statistics, and free disk);
- caps concurrency (default: 2 at once, configurable, never more than the cores allow);
- lets the profile name shared cache locations that are safe to share (a compiler cache, a package cache);
- never starts attempts without the user asking.

This is one of the conflicts with the speed budgets listed in [../build-plan.md](../build-plan.md).

## Judges

Applied in this order. Each adds a score with its basis, and the comparison always shows the basis.

| Judge | Measures | Notes |
|---|---|---|
| **Checks** | Do the project's tests and checks pass in the run's workspace? | The profile's commands, the same ones automatic verification uses. The primary judge. |
| **Cost and time** | Tokens, money, wall-clock time, requests, tool failures. | From the run itself. |
| **Diff measures** | Size of the change, files touched, whether tests were modified or deleted. | Cheap signals. Deleting a failing test to make checks pass is flagged. |
| **Agreement with what was accepted** (replay only) | How close the outcome is to what the user kept the first time. | A similarity measure over the diffs. Shown as a hint, not a verdict. |
| **Model judge** (optional) | A model compares outcomes against the task. | Uses the `judge` role. Off by default: it costs tokens and its verdicts need care. |

No judge decides for the user in parallel attempts. The checks rank; the user picks.

## Parallel attempts

1. The user asks for attempts on a task: how many, and which variants (a default set comes from configuration).
2. The engine records the base, shows the estimate, and starts the runs.
3. The attempts panel shows each run's state, plan, cost and check results as they arrive. Any run can be opened to watch, or stopped.
4. When runs finish, the panel ranks them and shows each diff.
5. The user picks one. Its diff is applied to the real workspace as one checkpointed change, exactly like an edit, so undo works. The others are discarded.
6. The chosen run's session becomes part of the user's session history. The results are recorded.

If the real workspace changed while attempts ran, the pick is applied as a patch and conflicts are shown, never forced.

## Replay and evaluation

**Recording, which every session does from the first release:**

- the base state at session start (the commit and a patch of uncommitted changes, taken in the background);
- the session log, including every prompt and the per-turn file checkpoints;
- how each turn ended: completed, undone, rewound, verified or not.

**Replaying one session.** The engine builds a task from the recorded prompts and a base from the recorded state, runs it with the chosen variant, and judges it. Follow-up prompts that depended on the first run's exact output are not blindly replayed: by default a replay covers the first prompt up to the end of its turn, which is the unit that can be compared fairly.

**Evaluating a set-up.** `cortex eval` takes a selection of recorded tasks (by project, date, or tag) and a set of variants, runs the matrix under a budget the user sets, and reports per variant: pass rate on checks, cost, time, and failure kinds. Results go to the statistics, where the measured choice of variants reads them.

**Budget.** Every evaluation has a hard cap on money and on wall-clock time, shown before it starts. It never runs on its own.

## Unattended runs and safety

A run has nobody to ask.

- **Permissions.** Each run uses a non-interactive policy: reads and edits inside its own workspace, and the commands the user has already accepted (the profile's commands and the approved list). Anything else is denied and recorded, as in headless mode.
- **Sandbox.** The same launcher as everywhere. Network is off unless the task's set-up needs it and the user allowed it for runs.
- **Budgets.** Per run and for the whole batch: money, tokens, time, requests.
- **Isolation between runs.** Separate workspaces, separate sessions, no shared mutable state apart from caches the profile explicitly shares.

## Where it lives

- A crate `cortex-runs` holds the orchestration layer, parallel attempts and replay. It depends on `cortex-core`, `cortex-session`, `cortex-stats` and `cortex-knowledge`, and drives sessions through the ordinary operations. The Coordinator is a second crate on top of it.
- Protocol additions under `run/*`: start a batch, list runs, watch a run's events (a run is a session, so the existing events apply), stop, pick, discard. The TUI's attempts panel and the `cortex attempts` and `cortex eval` commands are clients of these.
- Git is used through the `git` program, not a library, for worktrees and patches. Where `git` is absent, the directory-copy path is used.

## Limits of the layer

| Resource | Rule |
|---|---|
| Sessions at once | A cap per group (default 2 for attempts, 3 for a team) and a cap for the machine. A session waiting on the model costs almost nothing; a session running a build costs cores. So the cap that matters is on **concurrent commands**: by default no more than half the machine's cores in running builds and tests across all supervised sessions, with the rest queued. |
| Memory | Each supervised session in this process is budgeted at 10 MB when idle, mostly its current context ([../performance-budgets.md](../performance-budgets.md)). Its child processes are the project's own cost and are not ours to budget, so the estimate shown before starting uses the project's measured build memory when the statistics have it. |
| Provider limits | Rate-limit state is shared across the group, so sessions do not all hit the limit together. The supervisor spaces out requests when the provider reports that the limit is near. |
| Spend | The ledger is checked before every request. At a session's cap that session stops; at the group's cap all of them stop, at their next safe boundary. Neither can be raised by a model. |
| A stuck session | The supervisor, not a model, watches for: no completed tool call or edit for a set time (default 10 minutes) while a turn is running, the repetition guard firing, or the request limit reached. It pauses the session and reports it. |

## Why it is after the first release

1. **It multiplies whatever the core does.** Unattended parallel sessions are only as trustworthy as one session, its sandbox, its verification and its limits. Those must be solid and measured first.
2. **Replay needs recordings.** The first release starts recording base states and outcomes. By the time the engine exists, there are real sessions to replay.
3. **It is large.** Workspace management, the judges, the batch scheduler and the panel are several build steps.
4. **Its cost model needs the statistics** (typical build and test durations) to give honest estimates.

## What the first release does to prepare

| Preparation | Where |
|---|---|
| Sessions record their base state and how each turn ended. | [07-sessions-and-context.md](07-sessions-and-context.md) |
| Per-turn file checkpoints. | [07-sessions-and-context.md](07-sessions-and-context.md) |
| The core runs several sessions in one process, each with its own workspace root. | [03-agent-loop.md](03-agent-loop.md), [11-rpc-protocol.md](11-rpc-protocol.md) |
| Headless runs with a non-interactive policy and budgets. | [10-headless.md](10-headless.md) |
| Verification commands in the project profile. | [14-adaptation.md](14-adaptation.md), [15-efficiency.md](15-efficiency.md) |
| Statistics with a `run` event reserved. | [14-adaptation.md](14-adaptation.md) |
| Variant points that a run can set. | [14-adaptation.md](14-adaptation.md) |
