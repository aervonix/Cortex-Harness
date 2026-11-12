# 0027: Speed tiers: a per-model set of named tiers, recorded at the speed that ran

- **Status:** Accepted by the owner as a requirement on 2026-11-12 ("provider fast modes, starting with Anthropic's"). The design below is this record's. How utility roles behave was settled the same day, in the review of step 3. How long a cooldown lasts and what counts as a short wait were settled in step 5. Two details are left for the steps that need them, listed at the end.
- **Date:** 2026-11-12

## Context

The owner wants provider fast modes available in Cortex-Harness, starting with Anthropic's: Opus 5.5 and Fable 5.2 produce output about 2.5 times faster for twice the price per token. The docs written in step 1 had no concept of speed. Step 3 builds the protocol that every later step depends on, so the protocol side is designed now: a setting for the session and per role, the speed each request ran at in its usage record, events for a fallback and a cooldown, and which speeds a model offers.

The providers' own documentation was read on 2026-11-12. It goes stale; each provider step re-reads it.

| Provider | How a tier is asked for | What it buys | Price against standard | When it cannot be served | How the response says what ran |
|---|---|---|---|---|---|
| **Anthropic**, fast mode | `speed: "fast"` in the request body. A beta header as well on two older models. A separate deployment (`…-fast`) on one cloud platform. | Up to 2.5 times the output tokens per second. Not time to first token. | 2 times, across the whole rate card. Cache multipliers stack on top. | **An error**: 429 with `retry-after`, or 529. The tier has its own rate limit, reported in `anthropic-fast-*` headers. | `usage.speed`: `fast` or `standard`. |
| **Anthropic**, `service_tier` | `auto` or `standard_only` | Use of a capacity commitment the account bought. No longer sold, and not offered on current models. | A commitment, not a per-request price. | Silently served at standard. | `usage.service_tier` |
| **OpenAI**, `service_tier` | `fast` (called `priority` until July 2026), `flex`, `ultrafast`, `default`, `auto`, `scale` | `fast`: priority processing. `flex`: half price, slow, may be refused. `ultrafast`: a much faster serving path on two models. `auto`: whatever the project is set to. | `fast` about 2 times (1.75 to 2.5 by model). `flex` at batch rates. `ultrafast` 6 times on input. | `fast`: **silently served at standard** past a ramp limit, and billed at standard. `flex`: an error (429, not charged). | `service_tier` on the response, "based on the processing mode actually used". Older models still answer `priority`. |
| **Google**, Gemini API | `service_tier: "priority"` or `"flex"` | Priority: served ahead of standard. Flex: half price, minutes of latency. | Priority 75 to 100% more. Flex 50% less. | Priority: silently served at standard. Flex: an error (503 or 429). | A response header. |
| Others (aggregators, inference hosts, cloud platforms) | A body field, a suffix on the model id, a separate model id, a routing preference, a deployment setting or an account commitment. | Speed, priority or price. | Varies. | Varies. | A response field, a header, or nothing documented. |

Two more facts shape the design:

- **A change of speed can cost a cache rewrite.** Anthropic states that requests at different speeds do not share cached prefixes. No other provider says either way.
- **No provider's model list says which tiers a model offers.** It is in their documentation tables only.

## Options considered

| Option | Assessment |
|---|---|
| A boolean, `fast: true` | Fits the first provider and nothing else. No room for a cheaper, slower tier, or for a second fast tier. |
| One ordered scale for every provider (`economy` < `standard` < `fast` < `fastest`) | Reads well and is wrong. A tier that buys output speed, a tier that buys queue position and a tier that sells patience are not points on one line, and a scale hides the two things a user most needs to know: what it costs, and what happens when it is refused. |
| Pass the provider's own parameter through (`service_tier = "flex"` in configuration) | Honest, and it pushes every difference onto the user and onto every front end. The usage record could not be priced without knowing the provider. |
| **A per-model set of named tiers, each described by what it is.** A setting names a tier. The usage record carries the tier that ran. | The same shape as reasoning depth, which is already a per-model set and not a number. The differences are data on the offer, where front ends and the cost calculation can read them. |

## Decision

**A speed is a named tier that a model offers.**

- **Names.** `standard` is what every model has. `fast` is the well-known name for a tier that produces output faster at a higher price, so that a setting, a key binding and a status line can mean one thing across providers. Any other tier keeps the provider's own name (`flex`, `priority`, `ultrafast`): inventing neutral names for them would be the ordered scale again. On the wire a speed is an open string.
- **Offers.** A model's catalog entry lists the tiers it offers besides `standard`. Each offer says what the tier **buys** (`faster_output`, `priority`, `deferred`), its **own full rate card**, what the provider does **when it cannot serve it** (`error` or `downgrade`), whether it has a **rate limit of its own**, whether it **shares the prompt cache** with standard (yes, no, or not stated), and whether it is a **preview**. A front end shows a speed control only for a model with offers.
- **The setting.** A session has a speed, which is what the `main` role runs at. Each model role may have a speed of its own, which wins. **The utility roles (`fast` and `judge`) do not inherit the session's speed: they run at `standard` unless the user sets theirs.** This was decided in the review of step 3, on 2026-11-12. The reason is cost: summaries and titles gain little from speed, and when the `fast` role is not configured it falls back to the main model, so an inherited fast speed would double the price of every compaction. The default is `standard` everywhere. A setting the role's model does not offer is kept and is not in effect: the session state reports both the speed that is set and the speed in effect, as it does for the reasoning level.
- **The record.** Every usage record carries the speed the request **ran at**: the provider's own report where it gives one, otherwise the speed that was sent. The cost is tokens multiplied by **that** speed's rate card. When it differs from what was asked for, the record carries both.
- **Fallback and cooldown.** When a provider refuses a tier with its own limit, the harness may wait (if the provider's wait is short) or send the request at `standard`. After a fallback the tier is in a **cooldown**: later requests go at `standard` until it ends, and only then ask for the tier again. The harness does not flip between speeds request by request, because each flip can cost a cache rewrite that is larger than the time saved. A fallback and the start and end of a cooldown are events. A provider that downgrades by itself produces the same fallback event, marked as the provider's doing, and no cooldown.
- **Limits.** A tier's own rate limit is reported beside the model's ordinary one, labelled with the tier.

**What is not a speed.** Capacity an account has bought or reserved (Anthropic's Priority Tier, provisioned throughput, reserved tiers): it is a property of the credential, not a choice per request. Batch processing: it is a different API with a different lifetime. A project-wide or deployment-wide default at the provider (`auto`): the adapter always sends an explicit tier, so that what was asked for is known.

**Where it is on the wire** ([architecture/11-rpc-protocol.md](../architecture/11-rpc-protocol.md)):

| Need | In the protocol |
|---|---|
| The setting, for the session and per role | `speed` in the settings of `session/new` and `session/set`, and under `roles.<role>` |
| What is set and what is in effect | `speed`, and per role `speed`, `speed_in_effect` and `speed_cooldown_until`, in the session state |
| What each request ran at, and its price | `speed` and `speed_requested` in the usage record; cost by speed in the session's and the report's breakdowns |
| A fallback | The `speed_fallback` notice: requested, served, who moved it, why, and the cooldown's end |
| A cooldown | The `speed_cooldown` notice: started with its end, or ended |
| What a model offers | `speeds` in the model info of `models/list` |
| A tier's own rate limit | `speed` on a limit in `usage.updated` and `usage/report` |

## Why

- **It does not flatten.** Every difference in the table above is a field: what is bought, the price, the failure mode, the separate limit, the cache.
- **Cost stays true.** At every provider read, billing follows the tier that served the request. A record that stored only the setting would overstate the cost of every request a provider downgraded.
- **One switch for the common case.** `fast` means the same thing wherever it exists, so the owner's requirement for the terminal UI (a fast-mode toggle that appears only when the model offers a fast speed) needs no per-provider code in the front end.
- **The contract does not need reopening.** A new tier is a new string and a new catalog entry. A new kind of tier is a new value of an open enumeration.

## What is given up

- **Portability of the other names.** `flex` on one provider and `flex` on another are the same word for similar things, and nothing guarantees it stays so. The offer's fields are what a program should read, not the name.
- **A static table.** Which model offers what comes from our catalog, because no provider publishes it by API. It goes stale like prices do, and is maintained the same way.

## Who builds what

| Step | What |
|---|---|
| 3 (this one) | The protocol types, their schema, and these pages. |
| 5 | **Built.** The request carries a speed, and the usage carries the speed that ran: what the response reports, and what was sent only when it reports nothing, decided in one place for every protocol. The catalog format has per-model speed offers with their rate cards. A classified error names the tier whose own limit refused the request. The retry layer has the wait in place, the fallback and the cooldown, tested against the fake clock. The stream says each of them: a fallback (by the harness, or by the provider), and a cooldown starting and ending. The first catalog has Anthropic's fast mode on the two models that offer it. |
| 7 | The loop sends each request at the speed in effect and emits the fallback and cooldown notices. A change of the setting applies from the next request. |
| 9 | Configuration: the session's speed, a role's own, the utility roles at `standard` by default, and whether to fall back. **Built.** The keys are `model.speed` (the session's), `model.roles.<role>.speed` (a role's own), `model.speed_fallback`, `model.speed_wait` and `model.speed_cooldown`. |
| 13 | `session/set` wired through the core. Headless mode gets the setting from configuration. |
| 14 | Anthropic: `speed` in the request, `usage.speed` in the decoder, the `anthropic-fast-*` headers, the beta header where a model needs it, catalog entries. It begins, as always, by re-reading the provider's documentation. |
| 16 | The statistics record and the usage aggregates carry the speed, priced at its rate. `/usage` can group by it. |
| 22, 23, 24 | The status line shows a speed other than standard and a cooldown. The `/effort` picker gets the fast-mode toggle. The usage panel shows cost by speed. |
| 31, 35, 36 | OpenAI-compatible servers, OpenAI Responses and Google: their tiers as offers, and their silent downgrades as fallback events. |

**The owner's requirement for the terminal UI**, recorded here for steps 22 to 24: the `/effort` picker shows a fast-mode toggle on `Tab`, with the hint `tab - fast mode`, only when the current model offers a fast speed. The rest of the UI is our own design, not a copy of another harness's command or icon ([architecture/09-tui.md](../architecture/09-tui.md)).

## Settled and open

**Settled in the review of step 3, on 2026-11-12:** utility roles default to `standard` and do not inherit a fast session speed. It can be overridden per role. It is part of "The setting", above, and step 9 builds it.

**Settled in step 5, on 2026-11-12, and accepted by the owner:** how long a cooldown lasts, and how long a wait is "short". The recommendation above was taken as it stood.

| Parameter | Default | Meaning |
|---|---|---|
| Fall back to standard | on | Off makes a tier's refusal an ordinary rate limit: backoff, the server's wait, the maximum wait, then the error. |
| Short wait | 20 seconds | A budget for one request, not for one attempt. While everything the request has waited for its tier stays within it, the request waits as long as the provider asked and is sent again at the same speed. The first wait that would go over it falls back instead. A refusal that names no wait falls back at once. Zero never waits in place. |
| Cooldown | 10 minutes | Used only when the provider gives no time. When it gives one, the cooldown ends at that time, with no cap and no floor of ours. |

- **Why 20 seconds.** It is about as long as a person will watch a request wait before they would rather it carried on at standard speed. Anthropic describes its `retry-after` for fast mode as "typically short", because the limit refills continuously, so most refusals should be waited out and cost nothing: no change of speed, no cache rewrite.
- **Why the provider's time.** It is the cheapest moment to ask again. Earlier, the tier is refused. Much later, the prefix that was cached at the tier's speed has expired, and coming back costs a full rewrite.
- **Why a budget and not a limit per attempt.** A provider that answers "8 seconds" three times in a row would otherwise hold a request for as long as it has attempts.
- **All three are settings.** Step 5 defines them in the retry policy of `cortex-llm`; step 9 names the configuration keys. The cooldown is kept per provider, model and tier, in memory, for the life of the process.
- **The length of a cooldown is decided in one function** (`RetryPolicy::cooldown`), so that the open item below can be settled without touching anything else.

**Settled in step 9, on 2026-11-12: the configuration keys.**

| Key | Default | Meaning |
|---|---|---|
| `model.speed` | `"standard"` | The session's speed, which the `main` role runs at. Any tier name is accepted and is checked against the model's offers when it is used. |
| `model.roles.<role>.speed` | not set | A role's own speed. It wins for `main`, and it is the only way a utility role gets a speed other than `standard`. |
| `model.speed_fallback` | `true` | Whether a request a tier refuses is sent again at `standard`. Off, such a refusal is an ordinary rate limit. |
| `model.speed_wait` | `"20s"` | The longest one request waits in place for a tier, in total, before it falls back. `"0s"` never waits. |
| `model.speed_cooldown` | `"10m"` | How long requests stay at `standard` after a fallback, when the provider gives no time. |

The names sit under `[model]`, beside the model and the reasoning level, because a speed is a property of how a model is run, and `model.roles.<role>` mirrors `roles.<role>` on the wire. The three fallback settings are flat keys, so that `-c model.speed_fallback=false` reads as it should. The defaults of the last three are the ones step 5 settled for the retry layer.

**What a project's file may set** (the owner's decision, 2026-11-13). `standard` is a preference, for the session and for a role. Any other tier needs the user to have trusted the project, and until then is held back and listed with the other settings that wait for trust: a fast tier costs about twice as much per token, and an untrusted repository must not be able to raise what its user spends. A project can always bring the speed down. The three fallback settings are preferences.

**Open, for the steps that need them:**

1. **A shortest cooldown.** With no floor, a provider that keeps answering "in 25 seconds" gives 25-second cooldowns: the wait is too long to sit out and the cooldown too short to settle, so a session could change speed every half minute, each change a possible cache rewrite. That is the flip-flopping this record sets out to avoid. A floor would stop it at the price of a fourth parameter, and the right value depends on what the provider really sends. **Step 14 decides**, from Anthropic's real `retry-after` values for fast mode. The recommendation until then is none: the provider's own time is honoured as given.
2. **A command-line flag.** `--model` and `--effort` have flags in headless mode. A `--speed` flag beside them is the obvious addition and is not in [architecture/10-headless.md](../architecture/10-headless.md) yet. Step 13 decides.

## Revisit when

A provider ties speed to something this model cannot express, such as a tier chosen per tool call or a price that depends on time of day. Or when providers publish tier support in their model lists, which would let the catalog be checked against them.
