# The Coordinator

A mode in which one session, the **Manager**, runs a team of sessions, the **Employees**. It is part of the [vision](../vision.md). It comes right after the first usable release, on the orchestration layer in [16-run-engine.md](16-run-engine.md). The first release already carries the pieces it needs in the protocol, the permission model and the session log, so nothing has to be reworked.

**One autonomy model.** The Manager's authority is a **team lease**: the same lease type, with the same levels, that a single session holds ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). The three levels offered here, `supervised`, `guided` and `autonomous`, are the middle of that one ladder. This page adds only the decisions that exist because there is a team. Where this page says "the grant", it means the grant inside the team lease: its scope, models, budget and hard limits.

This page treats the Coordinator as a security-sensitive feature throughout: a Manager with broad permissions drives other agents, and text produced by an Employee can try to steer the Manager.

## The shape

```
                         the user
                 grants ▼     ▲ escalations, summaries
               ┌──────────────────────────┐
               │  Manager session         │   plans, assigns, reviews, integrates
               └──┬──────────┬──────────┬─┘
        task,     │          │          │      results, questions,
        follow-ups▼          ▼          ▼      approval requests ▲
            Employee A   Employee B   Employee C
            worktree A   worktree B   worktree C     each an ordinary session, in its own
            sandbox A    sandbox B    sandbox C      workspace, under its own grant
```

- **The Manager is an ordinary session** with a few extra tools and a grant from the user.
- **An Employee is an ordinary session** that the Manager drives through the same operations and events as any client ([11-rpc-protocol.md](11-rpc-protocol.md)). It has its own git worktree, its own sandbox, its own model and its own budget.
- **Employees do not talk to each other.** Everything goes through the Manager. This limits how far a bad instruction can spread.
- **The harness sits between them.** The Manager does not start processes, create worktrees, merge or push. It asks the harness to, through tools, and the harness checks every request against the grant.

## Starting

The user gives the Manager a task and a **team lease**. Its terms are shown in full before anything starts, in plain language, and must be confirmed. It is granted, extended and revoked exactly as a task lease for a single session is, by the user and never by a model.

```
Coordinator grant
  Task          Split the parser crate and add streaming support
  Level         guided
  Budget        $15.00 total · $4.00 per Employee · 2 hours
  Team          up to 3 Employees
  Models        opus-5.5 (Manager) · sonnet-5.5, haiku-5.5 (Employees)
  May edit      this repository, in worktrees only
  May run       the project's build and test commands · git (local)
  May not       push --force · touch anything outside the project · use the network from commands
  Integration   merge into branch coordinator/2026-11-12-parser · open a pull request: ask first
  Main branch   never without asking
```

## What the Manager does

| Step | How |
|---|---|
| Plans the work | Writes a plan with one item per piece of work (the `plan` tool). In `supervised`, the plan needs the user's approval before any Employee starts. |
| Creates a workspace per piece | Asks the harness for a worktree on a new branch from the integration branch. |
| Starts an Employee | Asks the harness to start a session in that worktree, with a model from the allowed list, a budget, and a grant that is a subset of its own. |
| Briefs it | Writes the Employee's first prompt. Sends follow-ups as steering or queued prompts. |
| Watches | Receives each Employee's status: working, waiting for an answer, waiting for approval, finished, stuck. Reads summaries, not full transcripts, unless it asks. |
| Answers | Replies to an Employee's questions, or escalates them. |
| Decides approvals | Receives each Employee's approval requests and allows, denies or escalates them (below). |
| Reviews | When an Employee finishes: the harness runs the project's checks in its worktree and gives the Manager the diff, the check results and the Employee's report. |
| Integrates | Asks the harness to merge the branch into the integration branch, or to open a pull request, as the level and the grant allow. |
| Reports | Sends the user a summary as each piece lands, and a final one. |

**The Manager's extra tools.** Each is a request to the harness, checked against the grant.

| Tool | Does |
|---|---|
| `team_start` | Create a worktree and start an Employee with a task, a model, a budget and a grant. |
| `team_send` | Send a follow-up to an Employee. |
| `team_status` | Status and a short summary of each Employee. |
| `team_read` | Read more of an Employee's work: its report, a diff, a file in its worktree, part of its transcript. |
| `team_decide` | Answer an Employee's question or approval request. |
| `team_review` | Run the checks on an Employee's branch and return the review packet. |
| `team_integrate` | Merge an Employee's branch into the integration branch, or open a pull request. |
| `team_stop` | Stop an Employee and, optionally, discard its worktree. |
| `escalate` | Put a question to the user. |

## Autonomy levels

Three levels, the same ones a single session's lease can have. For a single session a level settles who decides about commands. For a Manager it also settles who decides the things below. **All three sit inside the hard limits**, which no level relaxes.

| Decision | `supervised` | `guided` | `autonomous` |
|---|---|---|---|
| The plan and changes to it | User | Manager | Manager |
| Starting an Employee, choosing its model from the allowed list | User | Manager | Manager |
| An Employee's approval request for something the Manager's grant covers | User | Manager | Manager |
| An Employee's approval request beyond the Manager's grant | User | User | User (it is outside the grant) |
| Merging into the integration branch | User | Manager | Manager |
| Opening a pull request | User | User, unless the grant allows | Manager, if the grant allows |
| Spending past a soft threshold (default 50% and 80% of the budget) | User | User | Manager, with a notice |
| A change that touches a sensitive path (below) | User | User | User, unless the grant lists the path |
| A design choice, a security-relevant choice, a change of scope | User | User | Manager |
| Merging into the main branch | User | User | User, unless the grant explicitly allows it |

**What the harness can enforce, and what it cannot.** The rows about approvals, merges, pull requests, spending and sensitive paths are mechanical: the harness sees the request and applies the table, whatever the Manager says. The rows about design, security and scope depend on the Manager recognising that it is making such a choice. The harness cannot detect that. So for those rows the Manager is instructed to escalate, every decision it takes alone is written to the decision log, and the user can review the log at any time. This limit is stated to the user when they choose a level.

**Sensitive paths** always escalate unless the grant names them: CI configuration, git hooks and git configuration, the project's `.cortex` directory, dependency manifests' registry and script settings, and files matching the secret patterns in [../security.md](../security.md). The list is configurable upward.

**Levels above `supervised` need an enforcing sandbox,** as they do for a single session. Where the OS sandbox is not enforcing (Windows until its sandbox step), only `supervised` is offered, because the hard limits below could not be guaranteed. The owner confirmed this on 2026-11-12.

## Hard limits

Set by the user in the grant. Enforced by the harness. Never only written in a prompt.

| Limit | How it is enforced |
|---|---|
| **Budget cap** | The budget ledger of the orchestration layer. Every request by the Manager and by every Employee is charged to it before it is sent. At the cap, everything stops at the next safe boundary and the user is told. No tool can raise it. |
| **No force-pushes** | Models never run network git commands. Commands run in the sandbox with the network off, so `git push` from a shell cannot reach a remote at all. Pushing and opening pull requests are done only by `team_integrate`, where the harness itself runs git with arguments it builds, and it has no code path that forces. |
| **Nothing outside the project** | Each Employee's sandbox allows writes only inside its own worktree. The file tools check canonical paths against the same roots. The Manager's own write access is to the integration worktree only. |
| **No merge into the main branch** (unless allowed) | `team_integrate` refuses the main branch as a target unless the grant says otherwise. The main branch's name comes from the repository, not from the Manager. |
| **Team size, time** | The session supervisor. |

## Leases for the team

The lease type in [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md) is built so that a set of permissions is a value that can be compared and narrowed. The Coordinator adds no permission type of its own.

- **The Manager holds the team lease** the user confirmed: level, scope, models, budget, time, stop conditions and hard limits.
- **Each Employee holds a sub-lease.** When the Manager starts an Employee it proposes one. The harness replaces it with `manager_lease.narrow(proposed)`, so an Employee can never hold more than the Manager, whatever was asked for. An Employee's level is never above the Manager's.
- **Narrowing is defined per field:** the lower level; the parent's deny and ask rules always carried over; allow rules kept only where the parent allows the same thing; writable roots intersected with the Employee's own worktree; network allowed only if both allow it; budgets capped by what is left in the parent's; an expiry no later than the parent's.
- **When the team lease ends** (done, out of time or budget, a stop condition, or revoked by the user), every sub-lease ends with it. Employees stop at their next safe boundary and the Manager reports.
- A property test checks, for random leases, that `narrow` is always within both inputs and that no sequence of narrowing widens anything.
- The same type is what a settings profile, a headless run's flags and a single session's task lease compile to. The task-lease step is built before the Coordinator, and the Coordinator is built on it.

## Approval routing

An Employee's approval prompt needs someone to answer it. The protocol gives each connected client a role ([11-rpc-protocol.md](11-rpc-protocol.md)).

1. An Employee's tool call needs approval. The Employee's core sends `approval/request` to its **supervisor**, which is the Manager's side of the orchestration layer.
2. The layer applies the level table. If the request is inside the Manager's grant and the level lets the Manager decide, the request is handed to the Manager, which answers with `team_decide`. The answer and its reason go to the decision log.
3. Otherwise the request becomes an entry in the **escalation queue** and waits for the user.
4. A user who is attached to the Employee directly can answer its requests themselves. A user's answer always wins over the Manager's.
5. No answer within the timeout is a denial.

The Manager cannot approve what its own grant does not cover: step 2 checks the grant before the Manager is even asked.

## Two records

Both are entries in the Manager's session log ([07-sessions-and-context.md](07-sessions-and-context.md)), so they are stored, replayable and visible to every client. In the log, the field this page calls `kind` is stored as `category`, because `kind` is the name of every entry's own kind, and an answer is a later `escalation` record that names the one it `answers`: the log is never edited.

**The escalation queue.** What is waiting for the user.

| Field | Content |
|---|---|
| `from` | The Manager, or an Employee by name. |
| `kind` | plan, approval, merge, pull request, spend, sensitive path, design, security, scope, question. |
| `question` | What is being asked, in one or two sentences. |
| `detail` | References to what the user needs to decide: the command, the diff, the plan, the check results. |
| `options` | The choices on offer. |
| `status`, `answer`, `answered_by`, `at` | Filled in when it is resolved. |

The user works through the queue in the escalations panel or with `cortex team queue`. Work that does not depend on an open escalation carries on.

**The decision log.** What the Manager decided without asking.

| Field | Content |
|---|---|
| `decision` | What was decided. |
| `kind` | The same kinds as above. |
| `reason` | The Manager's stated reason. |
| `basis` | What it relied on: check results, a diff, an Employee's report. |
| `allowed_by` | The level and the grant clause that let it decide alone. Filled in by the harness, not the Manager. |
| `at` | When. |

The harness writes a log entry for every mechanical decision (approvals, merges, spend) whether or not the Manager remembers to explain it. The Manager adds its reasons.

## Visibility and control

- **Every Employee is visible.** The Employees panel ([09-tui.md](09-tui.md)) lists each one with its task, state, model, spend and last activity.
- **The user can attach to any Employee.** Attaching switches the view to that session's transcript. The user can read, send it a message, answer its approvals, or stop it. The Manager is told that the user intervened and what was said.
- **Summaries arrive as work lands.** When a branch is integrated, the Manager posts a short summary in its own session.
- **Everything can be stopped.** One key stops the whole team at the next safe boundary. Stopping the Manager stops its Employees.
- **Nothing is lost.** Each Employee's session is an ordinary stored session, and its worktree stays until the user or the Manager discards it.

## Resources

How many Employees fit, and how trouble is caught. The mechanisms are the orchestration layer's ([16-run-engine.md](16-run-engine.md)).

| Question | Answer |
|---|---|
| **Memory in the harness** | About 10 MB per idle Employee session, which is its current context. A team of 5 adds about 50 MB to the harness process. |
| **Memory and CPU outside it** | The real cost is the builds and tests Employees run. Concurrent commands across the team are capped at half the machine's cores by default; the rest wait. Before starting, the harness shows an estimate from the project's measured build time and, where known, build memory. |
| **Default team size** | Up to 3 Employees, configurable. A larger team mostly waits on the command cap and on provider rate limits. |
| **Provider limits** | Rate-limit state is shared across the team, and the supervisor spaces requests out when a limit is near. |
| **Runaway spend** | The budget ledger is checked before every request. Soft thresholds notify or escalate. The cap stops everything. |
| **A stuck Employee** | Detected by the supervisor, not by a model: no completed tool call or edit for 10 minutes during a turn, the repetition guard, or its request limit. It is paused and reported to the Manager and in the panel. |
| **A stuck Manager** | The same watchdog applies to the Manager. If it stops making progress while Employees wait on it, the user is told. |
| **Disk** | A worktree per Employee, plus its build output. The estimate includes free disk, and starting is refused below a floor. |

## Other machines

Not in scope, and not ruled out. A supervised session is reached through a handle that hides where it runs. An Employee on another machine would be a `cortex serve --stdio` process started over SSH, with its worktree and sandbox there. What that needs beyond this design: getting the code to the other machine and back (git handles it), credentials on that machine, and an estimate that counts its resources.

## Security

The threats and mitigations are in [../security.md](../security.md), section 8. The design rules that carry them:

1. Hard limits are enforced by the harness.
2. A child's grant is computed by narrowing the parent's. It cannot be wider.
3. Employee output reaches the Manager as labelled, bounded data, never as instructions with authority.
4. Integration is decided on evidence the harness produced (the checks it ran, the diff it computed), not on an Employee's claim.
5. Sensitive paths always escalate.
6. Employees cannot reach each other.
7. Everything the Manager decides alone is logged with the clause that allowed it.

## Where it lives

A crate `cortex-coordinator` on top of `cortex-runs`: sub-leases for Employees, the team rows of the level table, approval routing, the escalation queue and decision log, the Manager's tools, and the integration operations. The TUI gains the Employees and escalations panels. The protocol gains methods under `team/*`.

## What the first release carries for it

| Piece | Where |
|---|---|
| Permissions as a `Lease` value with `narrow` and `is_within`, and one ladder of levels | [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md) |
| Client roles, approval routing to a supervisor, several clients per session | [11-rpc-protocol.md](11-rpc-protocol.md) |
| Entry kinds reserved for escalations and decisions | [07-sessions-and-context.md](07-sessions-and-context.md) |
| Limits as a ledger that several sessions can share | [03-agent-loop.md](03-agent-loop.md) |
| Several sessions in one core, each with its own workspace root | [03-agent-loop.md](03-agent-loop.md) |
| Panels that open when needed | [09-tui.md](09-tui.md) |

One more piece comes after the first release and before the Coordinator: **task leases and the `guided` level** for a single session. The Coordinator's team lease is that feature with a team attached, so it is built once.
