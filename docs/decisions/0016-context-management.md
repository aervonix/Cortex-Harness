# 0016: Append-only history, context epochs, summary-first compaction

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

A long session must fit the model's context window without wasting money or making the model worse. The common techniques edit what has already been sent: trimming old tool output in place, rewriting the system prompt, injecting and later removing reminders, summarising older turns while replaying recent ones unchanged.

Two provider behaviours make those techniques expensive or invalid (sources in the research notes, to be re-read in step 14):

- Prompt caches match on exact prefixes. Any early change turns the rest of the request into a cache miss.
- Anthropic's current models bind each reasoning block to the system prompt, tool list and messages that preceded it. A request that replays a block after any of those changed is rejected, or the block is dropped. For newer accounts and the newest models this is enforced by default. OpenAI's newer models drop reasoning in the same situation. The provider's own guidance names keep-recent-turns compaction and in-place trimming as patterns that break.

## Options considered

| Option | Assessment |
|---|---|
| Edit history freely and strip reasoning when the provider complains | Simple. Loses reasoning between tool calls (measurably worse and more expensive, since the model thinks again) and loses the cache on every edit. |
| Per-provider workarounds | Each adapter learns which edits its provider tolerates. The loop's behaviour then differs by provider, and every new provider rule is a new bug. |
| Make append-only a core rule; change context only at explicit epoch boundaries | One rule for every provider. Cache-friendly everywhere. Costs flexibility: tokens are reclaimed only at compaction. |
| Rely on provider-side compaction and context editing | They do not invalidate reasoning. Available on some providers only, in beta, with provider-specific behaviour. |

## Decision

- History is append-only within a **context epoch**. The system prompt and tool list are frozen for the epoch.
- Tool output is bounded once, when the result is created.
- Mid-session changes are appended as notices.
- **Compaction starts a new epoch** from a model-written summary plus mechanically carried state (task list, file lists, the last user message). By default nothing else is replayed. Optionally, recent turns are replayed with their reasoning removed.
- A debug-build check and tests verify that consecutive requests in an epoch share a byte-identical prefix.
- Provider-side context management is left as a per-adapter optimisation for later.

Specified in [architecture/07-sessions-and-context.md](../architecture/07-sessions-and-context.md).

## Why

- It is the only approach that is correct on every provider without knowing which provider it is.
- It makes cache behaviour predictable and testable.
- Bounding output at creation removes most of the need to trim later.
- Carrying state mechanically means the things that must survive do not depend on the quality of a summary.

## What is given up

- A session cannot shed a large old tool result without a full compaction.
- After a compaction the model has a summary, not the recent transcript, unless the keep-recent option is on, in which case it loses the reasoning for those turns.

## Revisit when

- Providers offer a portable way to drop or replace earlier content without invalidating reasoning.
- Evaluation shows that summary-only compaction loses too much for typical coding sessions. The first response is to turn on the keep-recent option by default; the second is provider-side compaction where available.
