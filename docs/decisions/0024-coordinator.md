# 0024: The Coordinator is built on the orchestration layer, with limits enforced by the harness

- **Status:** Provisional. Scope and timing are the owner's call.
- **Date:** 2026-11-12

## Context

The vision adds the Coordinator: a Manager session that runs a team of Employee sessions, each in its own git worktree, at one of three autonomy levels, inside hard limits the user sets (a budget cap, no force-pushes, nothing outside the project). It is to come right after the first usable release, share its machinery with parallel attempts, and be designed for now so nothing is reworked.

It is the most security-sensitive feature in the product. A session with broad permissions directs other agents, and text an Employee read somewhere can travel into the Manager's decisions.

## Options considered

**How the Manager drives Employees.**

| Option | Assessment |
|---|---|
| A special sub-agent mechanism inside the Manager's loop | Quick. A second kind of session to build, test and secure, and nothing the user can attach to. |
| Employees are ordinary sessions, driven through the same protocol as any client | Nothing new in the loop. The user can attach to any Employee with the normal UI. In-process or over a pipe, including to another machine later, with no change. Needs client roles and approval routing in the protocol. |

**How limits are enforced.**

| Option | Assessment |
|---|---|
| Tell the Manager its limits in its prompt | Not enforcement. One injected instruction defeats it. |
| Rules that match the Manager's and Employees' commands (for example, deny `git push --force`) | Better, and still parsing what a command says. Documented bypasses exist for this class of control. |
| Remove the capability, and give the harness the operation | Commands run with the network off, so no command can push. Merging, pushing and opening pull requests are harness operations with arguments the harness builds. The budget is a ledger charged before each request. Writes are confined by the sandbox. |

**How Employees get permissions.**

| Option | Assessment |
|---|---|
| The Manager writes each Employee's permissions | A steered Manager can grant too much. |
| The harness computes them by narrowing the Manager's own grant | An Employee can never hold more than the Manager, whatever is asked. Needs permissions to be a value that can be compared and narrowed. |

**Where it sits.** One layer for parallel attempts and the Coordinator, or two. Parallel attempts are the case where every Employee gets the same task, so one layer.

## Decision

- **Employees are ordinary sessions** reached through session handles in the orchestration layer ([architecture/16-run-engine.md](../architecture/16-run-engine.md)). The Manager is an ordinary session with a few extra tools.
- **Hard limits are enforced by the harness** by removing capabilities, not by instruction or by matching commands.
- **A child's grant is computed** by narrowing its parent's. The permission model represents a grant as one value ([architecture/06-permissions-and-sandbox.md](../architecture/06-permissions-and-sandbox.md)).
- **Approvals route to the Manager first, bounded by its grant,** and otherwise to the user through an escalation queue. A user's answer always wins.
- **The escalation queue and the decision log are session records.**
- **Levels above supervised require an enforcing sandbox.**
- **The first release carries** the grant type, client roles and approval routing, the reserved record kinds, the shared limits ledger and several sessions per core, so the Coordinator adds a crate and panels and changes none of them.
- **It is built in two steps right after the orchestration layer and parallel attempts.**

Specified in [architecture/17-coordinator.md](../architecture/17-coordinator.md). Threats in [security.md](../security.md), section 8.

## Why

- A feature this sensitive must not rest on a model following instructions. Every hard limit has a mechanism that does not involve a model.
- Reusing the session, the protocol and the permission engine means the Coordinator inherits their tests and their fixes.
- Building parallel attempts first exercises the orchestration layer on the simpler case before a Manager depends on it.

## What is given up

- **Choices about design, security and scope cannot be detected mechanically.** At the guided and autonomous levels they depend on the Manager recognising them. The decision log and review of the integration branch are the backstops, and the main branch stays out of reach.
- **On Windows, only the supervised level** until its sandbox exists.
- **Employees cannot talk to each other,** which costs some efficiency and buys containment.

## Revisit when

- The Windows sandbox lands: enable the higher levels there.
- Real use shows the supervised level is too chatty: improve what the Manager can decide from evidence the harness produced, not what it can decide on an Employee's word.
- Employees on other machines are wanted: the session handle already allows it; what is missing is listed in the architecture page.
