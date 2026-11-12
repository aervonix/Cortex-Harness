# 0020: The edit tool matches exactly, with one tolerant tier

- **Status:** Provisional. To be confirmed with evaluation data after the first release.
- **Date:** 2026-11-12

## Context

Models often get whitespace slightly wrong when quoting the text they want to replace. A strict tool fails those edits, and the model spends a round trip correcting itself. A tolerant tool can replace the wrong text.

The reference harnesses sit at different points. One tries exact matching and then a normalising pass over whitespace and typographic characters. The other tries nine strategies in turn, including edit-distance matching, and needs a guard against matches far larger than the request. Neither checks that the file is unchanged since the model read it.

## Options considered

| Option | Assessment |
|---|---|
| Exact only | Never wrong. Fails often on indentation and trailing whitespace. |
| Exact, then one tier that ignores trailing whitespace, line endings and a uniform indentation difference, accepted only when the number of matches is exactly as expected | Fixes the most common failures. The tolerated differences cannot change which code is meant. |
| A long ladder ending in fuzzy matching | Fewest failed edits. Each rung adds a way to hit the wrong span, and the behaviour becomes hard to explain to the model or the user. |
| A patch format instead of search-and-replace | Some model families are better at it. A second tool to maintain, with its own failure modes. |

## Decision

Two tiers, as specified in [architecture/05-tools.md](../architecture/05-tools.md). The result always says when the tolerant tier was used and shows the text replaced. A failed edit returns the closest region and how it differs. A staleness guard requires the file to be unchanged since it was last read in the session.

## Why

- Wrong edits are worse than failed edits. A failed edit costs one round trip. A wrong edit costs trust.
- A precise failure message recovers most of what a tolerant matcher would, at the cost of one extra call.
- The staleness guard removes a class of wrong edits that matching strategy cannot.

## Revisit when

An evaluation over real sessions shows a material rate of failed edits that a specific extra tier would fix without wrong matches, or shows that a model family does much better with a patch format.
