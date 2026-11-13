# 0020: The edit tool matches exactly, with one tolerant tier

- **Status:** Accepted by the owner on 2026-11-12. To be looked at again with evaluation data after the first release.
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

## As built in step 10

Settled with the owner on 2026-11-13, and recorded in full in [architecture/05-tools.md](../architecture/05-tools.md), "`edit`":

- **Both tiers match against the file's text as `read` shows it,** with every CRLF read as one line break. `read` shows lines without their endings, so an `old` copied from a CRLF file has line feeds; matched against the file's bytes, every such edit of more than one line would be a tolerant match, reported with a "text replaced" identical to `old`. Line endings are therefore not something the exact tier can get wrong, and the tolerant tier is about trailing white space and indentation.
- **The tolerated differences, exactly:** spaces, tabs and carriage returns at the end of a line, and the same extra spaces or tabs in front of every line of one side. Tabs against spaces is not tolerated. The tier matches whole lines, is tried only when the exact tier finds nothing, and must find exactly `count` places.
- **New text takes the file's line ending.** In a file with both kinds, the ending of the first line it replaces.

## Revisit when

An evaluation over real sessions shows a material rate of failed edits that a specific extra tier would fix without wrong matches, or shows that a model family does much better with a patch format.
