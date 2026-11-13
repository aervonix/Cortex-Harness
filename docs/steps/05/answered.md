# Answered in step 5, on 2026-11-12

Step 5 built `cortex-llm` without a protocol, and the first parts of the test kit. These are the choices it put to the owner.

| Question | The owner's answer, 2026-11-12 | Recorded in |
|---|---|---|
| **How long a speed tier's cooldown lasts, and what a "short" wait is.** [Decision 0027](../../decisions/0027-speed-tiers.md) left both to this step, with a recommendation. | **Accepted as recommended.** Falling back is on by default. A request waits in place for its tier for up to 20 seconds in all, and then falls back. A cooldown lasts until the time the provider gives, and 10 minutes when it gives none. All three are settings, defined in `cortex-llm`; step 9 names their keys. | [Decision 0027](../../decisions/0027-speed-tiers.md), "Settled and open"; [architecture/04-providers.md](../../architecture/04-providers.md), "Speed tiers" |
| **A shortest cooldown.** With none, a provider that keeps answering "in 25 seconds" could make a session change speed every half minute. | **Left out for now.** Step 14 decides, from Anthropic's real waits. The length of a cooldown is decided in one function, so a floor can be added without touching anything else. | [Decision 0027](../../decisions/0027-speed-tiers.md), "Settled and open" |
| **The limit on the wait for a response's first event.** The docs named the limit and gave it no number. | **60 seconds, and a provider's entry can set its own.** Right for a hosted provider. A local server can take longer to read a long prompt; step 31 revisits the default for those. | [architecture/03-agent-loop.md](../../architecture/03-agent-loop.md), "Retries"; [architecture/04-providers.md](../../architecture/04-providers.md), "Transport" |
