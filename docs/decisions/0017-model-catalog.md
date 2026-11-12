# 0017: The model catalog is a file in the repo

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

The harness needs facts about each model that provider APIs mostly do not serve: prices, cache price multipliers and minimum sizes, reasoning controls, which models can read which models' reasoning, capability flags. Of the providers in scope, only an aggregator's model list includes prices. Two provider list endpoints give context and output limits; one gives almost nothing.

## Options considered

| Option | Assessment |
|---|---|
| Fetch a community catalog at start-up or on a timer | Always current. Adds a network request the user did not ask for, a dependency on someone else's uptime and data quality, and start-up work. Both reference harnesses do a version of this; one also runs its own overlay service. |
| Fetch at build time | Builds need the network and are no longer reproducible. |
| A checked-in file, compiled into the binary, updated by a maintainer script | Offline, reproducible, reviewable. Goes stale between releases. |
| No catalog: ask the user | Unusable for prices and capabilities. |

## Decision

- `catalog/models.toml` in the repo is the source of truth. A build step compiles it into the binary.
- `xtask catalog update` drafts changes from provider list endpoints and from models.dev (MIT-licensed data). A maintainer reviews the diff.
- Users add or override entries in configuration.
- `cortex models refresh` overlays live limits and capabilities from a provider's list endpoint, only when the user runs it.
- Local servers are queried at connect time for their loaded context size.

## Why

- No network at build time or at start-up, which the start-up budget and the no-telemetry position both require.
- Model facts change by provider announcement, a few times a month at most. A release or a one-line user override handles that.
- Corrections are reviewed in this repo instead of patched around an upstream feed.

## Revisit when

Releases cannot keep up with model changes, and users override entries so often that an opt-in refresh from a hosted source would serve them better.
