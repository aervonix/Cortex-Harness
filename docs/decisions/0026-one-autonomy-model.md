# 0026: One autonomy model: levels and leases, for single sessions and for teams

- **Status:** Accepted. The owner asked for one model on 2026-11-12, when adopting autonomy leases.
- **Date:** 2026-11-12

## Context

Three parts of the design each controlled how much the agent may do without asking:

- **Permission modes** for a session: `read-only`, `standard`, `autonomous`, `unrestricted`.
- **The Coordinator's levels** for a Manager: `supervised`, `guided`, `autonomous`.
- **Autonomy leases**, then an idea: a scoped, time- and cost-limited contract for one piece of work.

The owner adopted leases and said that leases and the Coordinator's levels must be one model, not two. Modes are the same subject again, so the answer has to cover all three.

## Options considered

| Option | Assessment |
|---|---|
| Keep modes, levels and leases as three mechanisms with a mapping between them | Three vocabularies for one idea. "Autonomous" would mean one thing for a session and another for a Manager. Each mechanism needs its own enforcement and its own tests. |
| Make leases a feature on top of modes, and leave the Coordinator's levels as they are | Two models, which is what the owner ruled out. |
| One ladder of levels and one lease type. A mode is the level of a session's standing lease. A task lease is a temporary lease. A team lease is a task lease held by a Manager, and Employees hold narrowed sub-leases. | One vocabulary, one type, one `narrow`, one set of property tests. The Coordinator adds decisions to the ladder, not a ladder of its own. |

## Decision

- **One ladder:** `read-only`, `supervised`, `guided`, `autonomous`, `unrestricted`. A level says who decides when no rule settles a question: the lease's holder or its supervisor.
- **`supervised` is the default.** It is what earlier drafts called `standard`, with the same behaviour: reads and in-project edits do not ask, commands ask unless safe-listed. The name changed so that one word means one thing for a session and for a team.
- **One lease type:** a grant (level, rules, sandbox policy, models, budget, hard limits) plus stop conditions, an expiry and who granted it.
- **Three kinds of lease, same type:** the standing lease every session has; a task lease the user confirms for one piece of work; a team lease held by a Manager, with sub-leases for Employees produced by `narrow`.
- **Only a person grants, extends or revokes a lease.** A model's draft of the terms is text. The terms the user confirms are exactly the terms enforced.
- **At the edge of a task lease the agent stops and reports.** It does not fall into asking one question after another.
- **`guided` and `autonomous` need an enforcing sandbox,** for a session and for a team alike. `unrestricted` cannot be given to or through a team.
- **Order of building:** the type, the standing lease and four levels in the first release (the permissions step). Task leases and `guided` as the first autonomy step after the release. The Coordinator after that, on the same code.

Specified in [architecture/06-permissions-and-sandbox.md](../architecture/06-permissions-and-sandbox.md) and [architecture/17-coordinator.md](../architecture/17-coordinator.md).

## Why

- A user learns one ladder and can predict what any session, attempt or Employee may do.
- The security argument is made once: every limit is a field of the lease, enforced by the harness, and a child can only hold a subset.
- The Coordinator, the most sensitive feature, reuses code that single sessions will have exercised first.

## What is given up

- **The name `standard`.** It was in the drafts the owner accepted. If the owner prefers it for the default level, it can be kept as the display name of `supervised`; the model does not depend on the word.
- **A little generality.** A team cannot use a level that a session cannot, and the other way round.

## Revisit when

Real use shows a decision that fits no rung of the ladder. The answer then is a new row in the level table, not a second mechanism.
