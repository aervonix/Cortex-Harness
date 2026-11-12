# Further ideas

Five proposals beyond the agreed [vision](vision.md). They are options for the owner to pick from. None is a commitment, and none is in the build plan until chosen.

Each was picked because current harnesses do it poorly or not at all, and because it builds on something the design already has.

| # | Idea | In one line | Builds on | Size |
|---|---|---|---|---|
| 1 | Checked memory | Every learned fact carries a cheap check that proves it is still true. | Project memory | M |
| 2 | Corrections become evaluations | When the user corrects the agent, that moment is saved as a test case for the replay engine. | Run engine, snapshots | M |
| 3 | Autonomy leases | The user grants a scoped, time- and cost-limited contract instead of answering prompts one by one. | Permissions, limits | M |
| 4 | Context profiler | "Explain the bill": where the tokens went, why the cache missed, and what to change. | Local statistics, epochs | M |
| 5 | Speculative verification | Start the likely next check before the model asks for it. | Verification, sandbox, snapshots | L |

---

## 1. Checked memory

**What it is.** A fact in project memory can carry a check: a short, read-only command or assertion whose result shows whether the fact still holds. "Tests run with `cargo nextest run`" is checked by confirming that the command exists and lists tests. "The API prefix is `/v2`" is checked by a search that must find it in the router. The harness runs checks cheaply, in the sandbox, when a fact is about to be used after a long gap or after the files it came from changed. A fact that fails its check is retired and the user is told.

**What is new.** Agent memory today is text with, at best, a date. It goes stale silently, and stale memory is worse than none because the model trusts it. The vision already asks for provenance and expiry. A check replaces a guessed expiry date with evidence.

**Cost.**
- *Complexity:* moderate. A check is a small policy-checked command run by the existing process runner. The memory store gains two fields and a scheduler that decides when a check is due.
- *Performance:* checks run in the background on the blocking pool, are rate-limited, and are skipped when their inputs have not changed (by file hash). No effect on start-up.
- *Maintenance:* low. The risk is checks that are flaky themselves, so a check that flips repeatedly is dropped and the fact falls back to plain expiry.

**Depends on.** Project memory ([architecture/14-adaptation.md](architecture/14-adaptation.md)), the sandbox, file snapshots.

**Where it fits.** A follow-up to the project-memory step, after the first release.

## 2. Corrections become evaluations

**What it is.** The most valuable signal a harness ever gets is the user saying "no". When the user undoes a turn, rewinds, rejects an edit, or steers with a correction, the harness saves a case: the starting state of the workspace, the task, what the agent did, and what the user accepted in the end. The replay engine can then run any model, prompt or setting against the user's own corrections and report how often each would have got it right the first time.

**What is new.** Evaluation suites for coding agents are public benchmarks that say little about one person's repositories and habits. Replay, which the vision already has, needs cases, and asking users to curate them does not work. This mines them automatically from behaviour the harness already sees.

**Cost.**
- *Complexity:* moderate. Case capture is bookkeeping over data the session log and the snapshot store already hold. Judging "would this run have been accepted" needs a comparison with the accepted outcome: tests where they exist, otherwise a diff-similarity measure, otherwise a model judge.
- *Performance:* capture is a few log entries. Replaying costs model tokens, so it runs only when the user asks or on an idle budget they set.
- *Maintenance:* moderate. Judging quality is the hard part and will need tuning. A wrong judge is worse than none, so results show their basis (tests, similarity, or model opinion).
- *Privacy:* cases contain the user's code and stay on the machine, like all statistics.

**Depends on.** The run engine ([architecture/16-run-engine.md](architecture/16-run-engine.md)), per-turn snapshots, the recorded starting state of sessions.

**Where it fits.** Directly after the replay step. It is what makes per-model adaptation trustworthy, because variants are then compared on real corrections instead of live trial and error.

## 3. Autonomy leases

**What it is.** A lease is a contract the user grants in one sentence: "for the next 30 minutes or 2 dollars, you may edit under `src/` and `tests/`, run the project's build and test commands, and nothing else; stop and report if the tests fail three times in a row." Inside the lease the agent does not ask. At its edge it stops and summarises. The status line shows what remains of the lease.

**What is new.** Harnesses offer modes (ask for everything, or ask for nothing) and remembered approvals for single commands. Neither describes a bounded piece of delegated work. A lease has scope, a budget, a time limit and stop conditions in one object that the user can read back, and that the policy engine and the sandbox enforce.

**Cost.**
- *Complexity:* moderate. A lease compiles to things the design already has: temporary permission rules, a sandbox policy, cost and time limits, and loop guards. The new parts are the lease object, turning a sentence into one (the model proposes, the user confirms the exact terms), and the stop conditions.
- *Performance:* none.
- *Maintenance:* low, since enforcement is existing machinery. The care needed is in wording: the confirmed terms must be exactly what is enforced.

**Depends on.** Permissions and limits ([architecture/06-permissions-and-sandbox.md](architecture/06-permissions-and-sandbox.md)), the project profile for "the project's build and test commands", automatic verification for the stop conditions.

**Where it fits.** After the first release, once the profile and verification exist. It is also the natural permission model for parallel attempts, which run unattended.

## 4. Context profiler

**What it is.** A view that answers three questions about a session. Where did the tokens go: by file, tool, instruction file and extension, including what was read more than once. Why did the cache miss: which request broke the prefix and what changed. What would be cheaper: concrete suggestions with estimated savings, such as "this 9,000-token file was read four times; its outline would have done for three of them" or "this MCP server's definitions cost 6% of every request; switch it to search".

**What is new.** Tools show totals: tokens, cost, a context gauge. Nobody shows causes. The design already records per-request usage with cache fields, knows every item's size, and checks prefix stability, so the profiler is mostly presentation over data that exists.

**Cost.**
- *Complexity:* moderate. Attribution needs per-item token estimates and a ledger of what each request contained. Suggestions are rules over the ledger.
- *Performance:* the ledger is a few numbers per item, kept in the session log. The view is computed on demand.
- *Maintenance:* low to moderate. Suggestions must stay true as features change, so each is a small tested rule.

**Depends on.** Local statistics ([architecture/14-adaptation.md](architecture/14-adaptation.md)), context epochs and the prefix check ([architecture/07-sessions-and-context.md](architecture/07-sessions-and-context.md)).

**Where it fits.** A small step after the first release. A reduced form (the breakdown by category) is already in the first release as the `/context` command.

## 5. Speculative verification

**What it is.** After the agent edits files, the next thing it nearly always asks for is the project's check: build, type-check, or the affected tests. The harness starts that check as soon as the edit lands, in the sandbox, without waiting to be asked. When the model asks, the result is already there or well under way. If the model edits again first, the run is cancelled and restarted.

**What is new.** The vision has tools starting as soon as their arguments finish and has prefetching for reads. This extends prediction from reads to the one expensive, highly predictable action in a coding session. On projects where the check takes tens of seconds, it removes most of a round trip's waiting from each edit cycle.

**Cost.**
- *Complexity:* high. The run must not interfere with what the model does next. Checks that only read the tree can run in place. Checks that write (build outputs, caches) need the project's own build directory handling, or an isolated copy, which is where this meets the worktree machinery of parallel attempts. Results must be tied to the exact file state they ran against, so a stale result is never served.
- *Performance:* it spends CPU that is sometimes wasted. It needs a budget (at most one speculative run at a time, cancelled promptly) and a per-project switch, and it should learn from the statistics whether its predictions are used.
- *Maintenance:* moderate. Per-ecosystem knowledge of which checks are safe to run concurrently with edits.

**Depends on.** Automatic verification ([architecture/15-efficiency.md](architecture/15-efficiency.md)), the project profile for commands and their usual duration, the sandbox, file-state hashing, and for write-heavy checks the isolated workspaces of the run engine.

**Where it fits.** After automatic verification and the run engine. It conflicts with the footprint goals on small machines, so it would ship off by default until statistics show it pays.

---

## Considered and not proposed

- **A second model reviewing risky actions before they run.** Useful, and other tools already have it.
- **Keeping provider caches warm with keep-alive requests.** One reference harness does this. It can be added as a routing rule if cache statistics show misses caused by idle gaps.
- **Learned context prefetch from which files are read together.** Promising, and close to the vision's "likely reads are fetched ahead". It is noted in [architecture/15-efficiency.md](architecture/15-efficiency.md) as the later form of prefetch, not as a separate idea.
