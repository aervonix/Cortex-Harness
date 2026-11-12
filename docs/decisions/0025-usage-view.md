# 0025: The usage view: one store for our numbers, read-only adapters for other tools, limits only from official sources

- **Status:** Accepted. The owner specified the feature on 2026-11-12. Amended the same day: the owner settled the point this record had left open, usage endpoints, with an opt-in refresh. The amendment is marked below.
- **Date:** 2026-11-12

## Context

The owner asked for a `/usage` command: usage and cost per provider and model, for the session, today, the week, the month and all time, with a breakdown per project. It should also read, read-only, the usage that other coding agents record locally (Claude Code and Codex first, then Opencode, Pi and others), show how much of each provider's limit is left, label subscription usage with an "API-equivalent cost" that is clearly an estimate, send nothing anywhere, and open instantly.

Three design questions follow.

## Where the numbers live

| Option | Assessment |
|---|---|
| A new store for usage | A second record of what the statistics store already holds. Two things to keep in step. The owner ruled it out. |
| The statistics store, with day buckets in its aggregates | Nothing new to maintain. The view reads a few kilobytes. "All time" needs buckets that outlive the pruning of raw event files. |

**Chosen:** the statistics store. Request events gain a project key, a billing kind and a cost fixed at record time. The aggregates gain buckets by local day, provider, model, project and billing; day buckets fold into month buckets after 400 days and are kept.

## How other tools' records are read

| Option | Assessment |
|---|---|
| Import them into our statistics | Mixes data we recorded with data we inferred from someone else's files. A parsing mistake would corrupt our own history, and "forget this source" becomes surgery. |
| Read them live when the view opens | Slow on a large history. The owner asked for an instant view. |
| A derived cache per source, filled in the background by one adapter per tool | Our store stays ours. The cache can be deleted and rebuilt. Each source can be turned off cleanly. The view opens from the cache. |

**Chosen:** one adapter per tool behind a small trait, in a crate of its own (`cortex-usage`), writing a derived cache per source. Adapters are opt-in, read-only, keep only numbers, tolerate unknown fields, and report an unreadable version instead of failing. Every number carries its source.

Other tools' on-disk formats are not documented promises. That is why each adapter names the versions it was written against, and why the default on anything unrecognised is "not available".

## Where "limits left" comes from

| Source | Assessment |
|---|---|
| Rate-limit headers and documented fields on responses the harness already receives | Official, free, and no extra request. Per-minute and per-day limits, which is what providers report. |
| A provider's documented usage endpoint | Official, and often the only place a subscription allowance appears. It is a network request the user did not cause. |
| Scraping a dashboard, undocumented endpoints, another tool's private state | Not official. Ruled out. |

**Chosen:** what arrives in responses the harness already receives, by default. **Amended 2026-11-12:** plus a refresh the user opts in to per provider, which calls that provider's documented usage endpoint with the user's own key. Where neither gives a figure, the view shows "not available".

## Decision

As specified in [architecture/18-usage.md](../architecture/18-usage.md):

- Our own figures come from `cortex-stats`. No second store.
- Other tools: per-tool adapters, opt-in, read-only, numbers only, version-aware, cached in the background, shown separately or merged, always with their source.
- Cost has a stated basis: metered, API-equivalent estimate, recorded by the tool, or unknown price. Estimates are never silently added to metered totals.
- Limits come only from official figures: those the harness already receives, and, where the user has turned it on for a provider, that provider's documented usage endpoint.
- By default no network request is made by the view or by indexing. The opt-in refresh is the one exception. It is off by default, per provider, user-only, made only when the user asks, sent only to the provider the key belongs to, and never telemetry.
- Tests use small made-up files. Real session data is never used or committed.
- **First release:** our own figures in every range and grouping, limits from headers, the panel, `cortex usage` and `usage/report`. **After it:** the adapters, in one step, Claude Code and Codex first. The opt-in refresh is built per provider once that provider's endpoint has been read and confirmed; the build plan does not yet name the step.

## Why

- It keeps the privacy promise: local files in, nothing out, unless the user asks a provider, with their own key, for their own figures.
- It keeps the start-up and memory budgets: nothing is read until the view is opened, and then only aggregates.
- It fails soft. Another tool changing its file layout costs that tool's numbers until its adapter is updated, and nothing else.

## Settled on 2026-11-12: usage endpoints

The brief lists "documented usage endpoints" among the allowed sources and also says nothing is sent anywhere. These pull apart, so this record first took the stricter reading, called no endpoint, and left the point to the owner.

**The owner's answer, 2026-11-12:** besides response headers, allow an opt-in refresh per provider that calls that provider's documented usage endpoint with the user's own key. It is off by default and is never telemetry. Where neither source gives a figure, show "not available".

| Option | Assessment |
|---|---|
| Never call an endpoint | Keeps "nothing is sent" without exception. A subscription's allowance, and any limit before the first request of a session, stay "not available" for good. |
| Call the endpoint whenever the view opens | The fullest figures. A request the user did not cause, on by default, which is what the privacy rule exists to prevent. |
| An opt-in refresh per provider, off by default, made only when the user asks | The figures for those who want them. The default stays exactly as before: no request. The request goes only to the provider the user already sends prompts to, with the same key, and carries no data of ours. **Chosen by the owner.** |

How it works is in [architecture/18-usage.md](../architecture/18-usage.md), "The opt-in refresh".

## Revisit when

- A provider documents allowance figures in ordinary responses: show them.
- Another tool publishes a stable, documented export of its usage: prefer it to reading session files.
- People ask for sources beyond the first four.
