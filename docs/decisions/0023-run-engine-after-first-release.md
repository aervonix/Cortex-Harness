# 0023: One run engine for parallel attempts and for replay; after the first release

- **Status:** Accepted by the owner on 2026-11-12 (conflicts C3 and C9).
- **Date:** 2026-11-12

## Context

The vision names parallel attempts (several approaches in isolated worktrees, judged by tests) and replay and evaluation (recorded sessions re-run against other set-ups) as its most distinctive features, and says they share an engine. It also sets hard speed budgets. Parallel worktrees are the feature that most directly spends disk, CPU and time.

## Options considered

| Option | Assessment |
|---|---|
| Build the engine before the first release | The headline feature ships first. It would be built on a core whose single-session behaviour, sandbox, verification and limits had not yet been used by anyone, and there would be no recorded sessions to replay. |
| Build two separate features | Faster to a first demo of each. Two schedulers, two judges, two result stores. |
| One engine, as the first milestone after the release, with the first release recording what it will need | The engine arrives on a tested core with real data waiting. The headline feature is later. |

## Decision

- One engine, `cortex-runs`, designed in [architecture/16-run-engine.md](../architecture/16-run-engine.md): a run is an ordinary headless session in an isolated workspace; the engine adds isolation, scheduling, judging and comparison.
- It is the first milestone after the first usable release.
- The first release prepares for it: sessions record their base state and how each turn ended, each turn has a snapshot, the core runs several sessions at once, verification commands live in the project profile, and the statistics reserve a run event.

## Why

- An unattended, parallel session is only as safe and as good as one session. That has to be solid first.
- Replay without recordings is an empty feature. Starting to record at the first release means there is something to replay when the engine lands.
- The costs are real and cannot be designed away: a fresh worktree has no build output, so N attempts on a compiled project are N cold builds. The engine needs the statistics (typical build and test times) to give an honest estimate before it starts.

## How the conflict with the budgets is resolved

- Never automatic. Attempts and evaluations start only when the user asks.
- An estimate of time, cost and disk is shown first.
- Concurrency is capped, and budgets are hard limits.
- The engine adds nothing to start-up, idle memory or the interactive path.

## Revisit when

The owner wants the headline feature in the first release. The smallest version that could move forward is parallel attempts on git repositories with the checks judge only, at the cost of delaying the release by its three or four steps.
