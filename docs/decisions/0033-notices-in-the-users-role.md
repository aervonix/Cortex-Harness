# 0033: A notice is a message in the user's role, saying it is the harness's

- **Status:** Accepted by the owner on 2026-11-14, in step 15: no new role in `cortex-protocol`.
- **Date:** 2026-11-14

## Context

What the harness tells the model after a conversation began (a changed instruction file, an instruction file under a directory a tool touched, a repetition warning, a failed check, the user's edit to the plan, a limit) is a `notice` item in the log ([architecture/07-sessions-and-context.md](../architecture/07-sessions-and-context.md)). Until step 15 it was sent as a message in the user's role. The neutral message model has three roles: user, assistant and tool ([architecture/04-providers.md](../architecture/04-providers.md), "The conversation model").

Anthropic now documents a system message inside the conversation for its current models, and recommends it for exactly this: instructions and changes that arrive after a conversation began, which must not edit the system prompt or the tools because that invalidates the reasoning already in the conversation ([outstanding item](../outstanding/README.md) left by step 14). Step 15, which builds context epochs and the notices of instruction files, was to decide whether the neutral model gains a role for it.

**What Anthropic's page says**, read on 2026-11-14 (platform.claude.com/docs/en/build-with-claude/mid-conversation-system-messages):

| What | The page |
|---|---|
| The form | `{"role": "system", "content": "…"}` appended to `messages`, at the point where the instruction starts to apply. The content is a string or text blocks. No beta header. |
| The models | Claude Fable 5.2, 5.1 and 5, Claude Mythos 5.2, 5.1 and 5, Claude Opus 5.5, 5 and 4.8, Claude Sonnet 5.5, Claude Haiku 5.5. **Not Claude Sonnet 5.** The four models in our catalog have it. |
| Where it may be | "Must follow a `user` turn (or an `assistant` turn ending in a server tool result)." "It must then end `messages` or be immediately followed by an `assistant` turn." Anywhere else, including between a tool call and its result, is a 400. It cannot be the first message. |
| What it is for | Mid-session policy or persona changes, per-turn context that must be authoritative, state changes the application observes. "A `user` message is treated as coming from the end user, whereas a `system` message is treated as coming from you, the application operator." |
| The cache | Appended at the end, it leaves everything before it unchanged, so the cache still matches: the same profile as a user message appended at the end. |
| Reasoning | Appending one keeps later thinking blocks valid. Deleting or editing an earlier one makes every thinking block after it fail the prefix check. |

**The other protocol.** None of the servers that speak `openai-chat` and that we have recorded from (OpenRouter, DeepSeek, Venice.ai) documents a system message after the first, and the chat templates of many local models refuse one.

## Options considered

| Option | Assessment |
|---|---|
| **A `system` role in the neutral model**, sent as the provider's system message where the model has it and the place allows, and as user text elsewhere | Notices get operator authority on Anthropic's current models. But whether a message may be a system message depends on the message **after** it, and an append-only log cannot promise that: a request that is interrupted, or fails before any content, leaves its notice unanswered, and the next prompt then follows it directly, which is a 400. The way out is to send that notice differently on the next request, which changes bytes already sent, or to put an answer in the model's mouth. A notice after an assistant message (a malformed tool call, a failed check) cannot be one at all. Every model without it (Sonnet 5, every `openai-chat` server we use) needs the user form anyway. And it is a change to `cortex-protocol`. |
| **No new role: a notice stays a user message, and says it is the harness's** | Works on every provider and every model, in every position, without knowing which. It keeps the cache and every reasoning block valid: it is appended and never edited, which Anthropic's page gives the same profile. What it gives up is operator authority: the model reads a notice as the user's channel, told in words that the harness wrote it. |

## Decision

**No new role in `cortex-protocol`.** A notice is sent as a message in the user's role, on every protocol: a user turn holding its text on `anthropic-messages`, a `user` message on `openai-chat`.

- **Every notice says plainly that it comes from the harness.** Its text begins with one fixed lead-in, `Notice from the harness (Cortex-Harness), not from the user.` and a blank line (`cortex_agent::NOTICE_LEAD`), and so does a compaction's summary. The core instructions tell the model what such a message is. The lead-in is part of the cached prefix and never changes; the prefix tests cover it.
- **The lead-in is added when the message is built, not stored.** The log keeps the notice's own text and its source (`notice` items), never a role: the role is chosen in one place, when the context folds an item into the conversation (`cortex_agent::context::append`). So a later change of mind needs no migration of the log.
- **What instruction-file text carries.** Text from an instruction file sent as a notice (a changed file, a nested file) has the authority of a user message. That is the same as the system prompt's project section, which holds the same text at the start of an epoch: both are text the model reads, and neither grants anything ([../security.md](../security.md), section 2).

## Revisit when

- **The placement rule no longer depends on the message that follows**, at Anthropic or at a provider we add, so that a notice can be a system message wherever a user message can be. The change is then a new open-enum value of `Role`, the one mapping in `context::append`, and in each adapter a rendering that sends it as the provider's system message where the model's `mid_conversation_system` capability is set and as user text elsewhere.
- **A notice whose operator authority matters**: one that grants or narrows something, such as a permission or a policy notice the model must not take for the user's words. No notice does today: what the model is allowed is enforced by the harness, not asked of the model.
