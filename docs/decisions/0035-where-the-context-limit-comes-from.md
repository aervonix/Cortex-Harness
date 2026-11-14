# 0035: A request's context limit comes from the local server's probe, or from the catalog

- **Status:** Accepted in step 15, on 2026-11-14.
- **Date:** 2026-11-14

## Context

The budget holds each request to a limit: the model's context window, less room for the answer and a margin ([architecture/07-sessions-and-context.md](../architecture/07-sessions-and-context.md), "Budgeting"). The window can come from two places.

- **The catalog.** Every entry has a `context_window`, read from the provider's documentation. The catalog's reader refuses an entry without one, or with zero, and that holds for an entry in the user's configuration (`[models.<name>]`) too ([04-providers.md](../architecture/04-providers.md), "Model catalog").
- **A local server's probe.** A local model's window is what the server loaded it with, not what it was trained for: a model trained for 131,072 tokens is commonly loaded with 4,096. Four servers say it at an endpoint of their own (`cortex_llm::openai_chat::context`), named by the provider's `context_probe` flag. Step 31 built the request and its reading; making it was left for this step.

Step 15 was to decide what happens when neither knows the window: refuse, assume, or ask.

## Options considered

| Option | Assessment |
|---|---|
| **Refuse** a model whose window is not known | Already true, one step earlier: a model must be in the catalog to be used at all, and the catalog refuses an entry with no window. Nothing is left to refuse at the start of a session. |
| **Assume** a window, such as a small one, when neither says | Never needed, for the same reason, and a guessed limit is a wrong budget either way: too small compacts for nothing, too large sends what a server cuts off. |
| **Ask** the user | Nothing to ask: the user wrote the window when they described the model. |
| **The probe's answer over the catalog's**, when a local server gives one | The server is the one place that knows what it loaded. Its answer wins whether it is smaller or larger than the catalog's. |

## Decision

- **"Neither knows" cannot arise.** A model is reached only through its catalog entry, and an entry is refused without a context window. So the catalog's figure is the floor of what is known, and there is no case to refuse, assume or ask in a session.
- **A local server's answer stands in for the catalog's.** When the main role's provider names a `context_probe`, the core asks the server once, in the background, when the session opens, and hands the answer to the loop (`SessionHandle::set_context_window`). Nothing waits for it: until it answers, and when it cannot (the server is not up, the model is not loaded, the answer is not what the server documents), the catalog's figure stands, and the failure is logged.
- **A wrong figure is caught by the provider.** A request a server refuses as too long is answered by one compaction and one more try ("Budgeting", the overflow fallback), as for any model.

## What is given up

- A window the user wrote too large for a model the server has not loaded yet is believed until the server is asked again (the next session) or refuses a request.
- The probe is asked for the main role's model only. A `fast` model on another local server keeps its catalog figure.

## Revisit when

- A hosted provider reports a window per account or per deployment that differs from its documentation.
- The `fast` and `judge` roles (step 16) are commonly local models.
