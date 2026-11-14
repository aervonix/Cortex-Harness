# Answered in step 15, on 2026-11-14

Step 15 built context management in `cortex-agent` and wired it into the core: the system prompt, instruction files, context epochs, the budget, compaction with carried state, summaries in the background on the `fast` role, the overflow fallback, and the per-request context record. These are the points its prompt asked it to settle, and the choices it made on the way.

## The seven points

| Point | Settled | Recorded in |
|---|---|---|
| **1. A system message in the middle of a conversation.** Anthropic documents `{"role": "system", …}` inside `messages` for its current models (not Sonnet 5), and recommends it for instructions that arrive after a conversation began. | **No new role in `cortex-protocol`** (the owner, 2026-11-14, as recommended). Its placement rule depends on the message after it ("must end `messages` or be followed by an `assistant` turn"), which an append-only log cannot promise; models and `openai-chat` servers without it need the user form anyway; and a user-role notice keeps the cache and the reasoning just as well. A notice is a user message on every protocol, and its text says plainly it is the harness's (`NOTICE_LEAD`), byte-stable and covered by the prefix tests. Instruction-file text sent as a notice has a user message's authority, as the prompt's project section has. | [Decision 0034](../../decisions/0034-notices-in-the-users-role.md); [security.md](../../security.md), section 2 |
| **2. The prefix check across protocols.** | **`stable_prefix` is a required method of `Protocol`** now, which both protocols and the test protocol write, and `ModelClient::stable_prefix` (default `None`) is how the loop reaches it. A debug build holds every request to the one before it in its epoch, with the same settings, on whichever protocol it speaks; the tests run it on Anthropic's and on `openai-chat`'s bytes over a long session, and show it failing on an edited history. | [architecture/04-providers.md](../../architecture/04-providers.md), "Traits" and "Prompt caching" |
| **3. The fast role before step 16.** `cortex-config` resolves `model.fast` (and `model.roles.fast.speed`, `effort`, `provider`); the core already built `Models` with the `fast` role when it is set. | **That is what the loop uses:** a summary, in the background or not, goes to the `fast` role's model when there is one and its window holds the epoch, at the role's own speed (standard unless set), and to the main model otherwise. Left for step 16: other utility calls, the `judge`, budgets by role, variants. | [build-plan.md](../../build-plan.md), step 15, "What later steps inherit" |
| **4. The core instructions and the summary instructions.** | **Written here, short and plain**, without reading another harness's prompts: `cortex_agent::CORE_INSTRUCTIONS` and `cortex_agent::SUMMARY_INSTRUCTIONS`. Both are in full in the step's report. | `crates/cortex-agent/src/context/prompt.rs`, `context/compact.rs` |
| **5. A change of model as an epoch.** | **Built:** before a request whose main model, provider, protocol or reasoning level is not the epoch's, an `epoch` entry with the reason `model` begins one. It keeps the system prompt and the tools exactly, so the reasoning in the history stays valid and the provider's replay rule decides what the new model reads, and it starts a new epoch of the table of seen files. | [architecture/07-sessions-and-context.md](../../architecture/07-sessions-and-context.md), "Context epochs" |
| **6. Where the limit comes from.** | **A local server's probe, or the catalog; "neither knows" cannot arise**, because a model is reached only through a catalog entry, and an entry without a context window is refused. The core asks a local server once, in the background, when a session opens; its answer stands in for the catalog's. | [Decision 0035](../../decisions/0035-where-the-context-limit-comes-from.md) |
| **7. The headless spend in the ledger.** | **Settled:** each headless run of the real binary in `cortex-cli`'s `tests/system/live.rs` adds a line to the spend ledger from its `--output json` record's tokens and cost, and `cargo xtask live` gives that test the ledger's path. | [testing-and-ci.md](../../testing-and-ci.md), "Fixtures" |

## Choices made on the way

| Choice | Why | Recorded in |
|---|---|---|
| **The date rides in the epoch's first user message**, as a text part before what the user wrote, taken from that message's own `created_at`. Not a notice of its own. | It is what 07 says, it is the same bytes when the history is read again, and it adds no item to every session. A session read again on a later day is told the date in a notice. | 07, "What goes into a request" |
| **A resumed session goes on in its epoch** when the harness would build the same prompt but for the instruction files (the `base` hash) and the same tools, and the log does not hold the prompt masked. It is not restarted because the cache may have expired. | A new epoch drops every reasoning block of the history; going on keeps them, and changed instruction files are told as notices. | 07, "Resume, branches and forks" |
| **Reasoning is replayed only under the prefix it was produced under**, one rule for resume, compaction and a changed prompt, applied by the epoch each item was recorded under. | One rule, the same live and read again from the log. | 07, "Context epochs" |
| **The ratio of bytes to tokens is measured** from the session's own requests, not looked up in the catalog; the counting endpoint is not called yet. | The catalog has no such field, and a measured ratio follows the model and the content. | 07, "Budgeting"; [outstanding](../../outstanding/the-token-counting-endpoint-near-the-limit.md) |
| **Instruction files are read as the agent would read them:** through the path resolver, a link followed only inside the workspace, past the permission engine's gate; in the configuration directory only `AGENTS.md` and `CLAUDE.md`; a configured name only a plain `.md` file. | A file a project names, or links to, goes to the provider with every request. Found by the early review: a trusted project could have named the user's credentials file. | 07, "Project instruction files"; [security.md](../../security.md) |
| **`[context]` is declared:** `auto_compact`, `background`, `compact_at`, `prepare_at`, `keep_recent_tokens`, `instruction_names`, `instructions_cap`. The thresholds and the names raise what is spent (a project's value waits for trust); turning compaction or the background summary off is a preference. | The section was reserved for this step. | [architecture/08-configuration.md](../../architecture/08-configuration.md); [configuration/reference.md](../../configuration/reference.md) |
| **`session/compact`** compacts an idle session in a turn of its own, and a running one at its next safe boundary. | The protocol has it, and a headless client waits on `turn.completed`. | 07, "Compaction" |
| **The redaction mark** counts item entries only, is set again at a summary from what the new history holds, and is ended only by an epoch of a new prefix. | The step's prompt asked for items only; the early review found a masked summary losing its mark. | 07, the seams map |

## The live run

By hand, on 2026-11-14 (`cortex-cli`'s `live::live_headless_compaction`), one headless session of the real binary with the whole built-in toolbox on `claude-haiku-5-5` at effort `low`, the cheapest catalog model that caches. The thresholds were lowered through `context.prepare_at` and `context.compact_at` from a first request that measured the prompt and the tools (4,908 tokens), so that six reads of files of about 2,500 tokens each crossed them.

| Epoch | Requests | Read from the cache | Written to it |
|---|---|---|---|
| 1 (`start`) | 2 | 4,981 | 11,346 |
| 2 (`compaction`) | 2 | 10,742 | 7,459 |
| 3 (`compaction`) | 1 | 4,824 | 7,451 |
| 4 (`compaction`) | 1 | 4,824 | 7,449 |
| The four summaries, prepared in the background | 4 | 27,069 | 34,268 |

The session compacted three times, each from a summary prepared in the background, and carried on to the answer the task asked for. Each summary request read the epoch's own cache. The run cost $0.0110 by the catalog's prices, and the request that measured the prompt $0.0006: about 1.2 US cents in all, of the £3 allowed. Both are lines in the spend ledger.
