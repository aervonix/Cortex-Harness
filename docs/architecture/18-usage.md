# Usage and cost: the `/usage` view

One place to see what was used and what it cost: per provider and per model, over time, per project, and across the other coding agents installed on the same machine. The owner asked for this on 2026-11-12. The choices are recorded in [decision 0025](../decisions/0025-usage-view.md).

It rests on four rules.

1. **Nothing leaves the machine.** The view reads local files and the responses the harness already receives. It sends nothing and asks nobody.
2. **One store for our own numbers.** Cortex-Harness's figures come from the local statistics ([14-adaptation.md](14-adaptation.md)). There is no second record of them.
3. **Other tools' records are read, never written.** Each source is opt-in, and every number says where it came from.
4. **Nothing is guessed.** A limit is shown only when the provider officially reports it. A cost that is not a real bill is labelled an estimate.

## What it shows

**Measures,** for each provider and model:

| Measure | Meaning |
|---|---|
| Input tokens | Fresh input: tokens sent that were not read from a cache. |
| Cache reads | Input tokens served from the provider's prompt cache. |
| Cache writes | Input tokens written to the cache. |
| Output tokens | Everything the model produced, with reasoning tokens shown as a part of it where the provider reports them. |
| Requests | How many model requests. |
| Cost | In US dollars, with its basis (below). |

**Ranges.**

| Range | Definition |
|---|---|
| This session | The open session, live. This is the session view that the usage panel already had: tokens by class for the session and the last request, cache hit rate, cost by model and role, time to first token and tokens per second. |
| Today | The local calendar day. |
| Week | The calendar week, starting on Monday unless `usage.week_start` says otherwise. |
| Month | The calendar month. |
| All time | Everything on record. |

`usage.ranges = "rolling"` switches week and month to the last 7 and 30 days, for people whose allowance works that way.

**Groupings.** By provider, then model. By project. By source (Cortex-Harness, or another tool). Any two together, for example a project's cost per model this month.

**A sketch of the panel** (the look follows [09-tui.md](09-tui.md); this shows content, not final wording):

```
Usage · month                      [session] today  week  month  all        by: provider  project  source
anthropic                                   in      cache read  cache write      out      cost
  opus-5.5                               1.21M         38.4M        2.90M      612k    $41.87
  haiku-5.5                              0.44M          6.1M        0.52M      201k     $1.92
openrouter
  some-open-model                        0.80M            —            —       310k     $0.61
                                                                              total    $44.40
Limits   anthropic   tokens 71% left · resets 14:32      requests 96% left      as of 2 min ago
         openrouter  not available
Sources  Cortex-Harness
```

## Where our own numbers come from

The statistics store already records one event per model request. For this view each request event carries:

| Field | Note |
|---|---|
| Time | UTC, with the local day it fell on. |
| Provider, model, role | Role is `main`, `fast` or `judge`. |
| Project | The project key used elsewhere in the state directory. No path is stored in the event. |
| Session | The session id. |
| Tokens by class | Fresh input, cache read, cache write, output, reasoning. |
| Cost | Computed when the event is recorded, from the catalog's prices at that moment, so a later price change does not rewrite history. Stored with the catalog version it came from. Absent when the model has no known price. |
| Billing | `metered` (an API key billed per token), `subscription`, or `unknown`. |

**Aggregates.** The small aggregates file gains buckets keyed by local day, provider, model, project and billing. Today, week, month and all time are sums over day buckets, so opening the view reads kilobytes and scans nothing. Day buckets older than 400 days are folded into month buckets, which are kept until the user resets statistics, so "all time" survives the pruning of raw event files.

**This costs nothing new at start-up.** The aggregates are read the first time the view, or an adaptive feature, needs them.

**If statistics are disabled** (`stats.enabled = false`), the session range still works, from the session itself, and the other ranges say that recording is off.

## Cost, and what kind of number it is

Every cost on screen has one of four bases, shown beside it or in the column header.

| Basis | When | Shown as |
|---|---|---|
| **Metered** | The request was billed per token on an API key, and the catalog has the price. | A plain amount. It is our calculation from the provider's published prices, not the provider's invoice, and the view says so once. |
| **API-equivalent estimate** | The usage was on a subscription, or the billing kind is unknown. | An amount marked `est.`, with the legend "what this would have cost at API prices; a subscription is not billed this way". Never added into a metered total without the mark. |
| **Recorded by the tool** | Another tool's own files state a cost. | The amount, marked with the tool's name. |
| **Unknown price** | The model is not in the catalog. | Tokens only, cost `—`. |

A total that mixes bases shows them apart: `$44.40 metered + $212.10 est.`

## Limits left

For each provider: how much of its rate limit or allowance remains, and when it resets.

| Source of the figure | Used? |
|---|---|
| Rate-limit headers on responses the harness already receives (limit, remaining and reset time, for requests and tokens) | **Yes.** The agent loop already parses them ([03-agent-loop.md](03-agent-loop.md)). The latest values per provider and credential are kept, with the time they were seen. |
| Usage or allowance fields inside those responses | **Yes**, where a provider documents them. |
| A subscription's remaining allowance | **Only** where the provider officially exposes it to third-party tools in what the harness already receives. Otherwise "not available". |
| Scraping a web page, an undocumented endpoint, another tool's private files | **Never.** |
| A provider's documented usage endpoint | **Not called.** It is official, but calling it is a network request the user did not cause, which rule 1 excludes. See "One point left open". |

- A figure is shown with its age (`as of 2 min ago`). Once its reset time has passed it is shown as `reset since; unknown until the next request`, not as full.
- A provider that reports nothing shows `not available`. As read on 2026-11-12, Anthropic and OpenAI document rate-limit headers, and none was found documented for Google's API. The provider build steps re-check this.
- These are rate limits per minute or per day, as each provider defines them. They are not a spending forecast, and the view does not turn them into one.

## Usage recorded by other tools

Many people run more than one coding agent. The view can include what the others recorded on this machine.

**Order of work.** Claude Code and Codex first. Then Opencode and Pi. Others as people ask.

**Each tool has its own adapter**, behind one small trait:

```rust
pub trait UsageSource {
    fn id(&self) -> SourceId;                        // "claude-code", "codex", ...
    /// Is this tool's data present, and in a version we can read?
    fn detect(&self, home: &Path) -> Detection;      // Absent | Readable { version } | Unreadable { version, reason }
    /// Read records newer than the cursor. Never writes. Never fails as a whole.
    fn scan(&self, cursor: &Cursor, sink: &mut dyn RecordSink) -> ScanReport;
}
```

| Rule | What it means in practice |
|---|---|
| **Opt-in per source** | Nothing is read until the user turns a source on: `usage.sources = ["claude-code"]`, or the prompt the first time `/usage` is opened ("Usage records from Claude Code and Codex were found. Include them?"). Detection looks only for the tool's data directory. It opens no file. |
| **Read-only** | Files are opened for reading, with sharing that never blocks the other tool. Nothing is created, changed or locked in another tool's directory. |
| **Only the numbers** | Other tools keep usage inside files that also hold conversations. An adapter extracts the time, provider, model, project path, token counts, any cost and a record id. Prompt and response text is never copied, cached or logged. |
| **Tolerant** | Unknown fields are ignored. A record it cannot understand is skipped and counted. A file it cannot parse is skipped and named. |
| **Honest about versions** | Each adapter lists the format versions it was written against. Faced with a newer one it does not recognise, the source reports "cannot read this version of the tool's records (last supported: …)" and shows `not available`. It never crashes the view and never shows numbers it is unsure of. |
| **No double counting** | Records are keyed by the tool's own record id, so reading a file twice, or a file the tool rewrote, adds nothing. |
| **Provenance** | Every row and every total says which source it came from. |

**Separate or merged.** The `by: source` grouping shows each tool on its own. The merged view adds them into one table per provider and model, with the source split one key away, and keeps estimates marked as estimates.

**Cost for other tools' usage** follows the table above: the tool's own recorded cost if there is one, otherwise an API-equivalent estimate from our catalog. Usage from a tool that was signed in to a subscription is the main case for the estimate label.

**Projects.** Other tools record a working directory. It is matched to a Cortex-Harness project by canonical path. Paths with no match are listed under their own path, shortened with `~`.

**The cache.** Other tools' history can be large, so it is never read when the view opens.

- A background task on the blocking pool indexes each enabled source into `<state dir>/usage-cache/<source>/`: day buckets in the same shape as our own aggregates, and a cursor per file (size, modification time, offset).
- Indexing is incremental, runs at low priority on one thread, and starts when the view is first opened or when a session has been idle for a while. It never runs at start-up.
- The view opens at once from whatever is cached, says `updating…` while the index catches up, and refreshes when it has.
- The cache is derived data. `cortex usage forget <source>` deletes it, and it can always be rebuilt. It holds numbers and project keys, nothing else.

**How adapters are written.** From the files each tool writes on disk and from its public documentation. The formats belong to those tools; we only read them. No code from any of them is used, and the adapters for Pi and Opencode are subject to the same rule as everything else in this project: study, then write our own. These formats are not promises, which is why every adapter is version-aware and why a failure is reported and never fatal. The step that builds each adapter starts by examining what the current version of the tool writes, and records the versions it handles.

## Commands and protocol

| Surface | What |
|---|---|
| TUI | `/usage` opens the panel. Keys switch range and grouping. The status line shows a provider's limit when less than a fifth is left ([09-tui.md](09-tui.md)). |
| Command line | `cortex usage [--range session\|today\|week\|month\|all] [--by provider\|model\|project\|source] [--json]`. `cortex usage sources` lists detected sources and their state. `cortex usage forget <source>` deletes a source's cache. |
| RPC | `usage/get` (the session, live) and `usage/report` (a range and grouping). After the first release, `usage/sources` to list, enable and disable sources ([11-rpc-protocol.md](11-rpc-protocol.md)). |

Enabling a source is a user-only setting. A project's configuration cannot turn one on.

## Security and privacy

- **No network.** Stated above, and tested: opening the view and running a full index make no connection (a test with the network denied).
- **Other tools' files are untrusted input.** They are parsed with size and line-length limits. Symbolic links that lead out of the tool's data directory are not followed. A malformed file costs that file, not the view.
- **Least reading.** An adapter reads only the files that hold usage, and keeps only numbers.
- **The cache is owner-only**, like all state files.
- Threats and mitigations are listed in [../security.md](../security.md).

## Speed

| Budget | Limit |
|---|---|
| Opening `/usage` | A frame, from cached aggregates. No scan, no parse of raw history. |
| `cortex usage --range month` | Within the headless start-up budget plus reading the aggregates. |
| Background indexing | One low-priority thread, bounded memory, cancellable. Never on the start-up path, and paused while a turn is streaming. |

The figures are in [../performance-budgets.md](../performance-budgets.md).

## Testing

- Aggregation is tested with a fake clock across day, week, month and time-zone boundaries, and after raw files are pruned.
- Each adapter has fixtures: **small, made-up example files** written for the tests, one set per format version it supports, plus damaged and unknown-version examples. **Real session data is never used and never committed.** The repository check that rejects home-directory paths also covers fixtures.
- Adapters are fuzzed in the non-blocking tier.
- A test asserts that an adapter's output and the cache contain no text field from the input.

## First release, and later

| Part | When | Why |
|---|---|---|
| Our own usage and cost by provider and model, all five ranges, per-project breakdown | First release: recorded in the statistics step, shown in the panels step. | It is the statistics store plus a panel, and it makes the adaptive features' data visible from day one. |
| Limits left, from response headers | First release. | The loop parses them already. |
| `cortex usage` and `usage/report` | First release. | The same query as the panel. |
| The API-equivalent estimate label | First release for the mechanism. It first matters with other tools' records and with sign-in flows. | The first release uses API keys only, which are metered. |
| Adapters for Claude Code and Codex, the cache, separate and merged views | After the release, as its own step. | Reading other tools' formats is separate work with its own fixtures, and nothing else depends on it. |
| Adapters for Opencode and Pi, then others | The same step or follow-ups to it. | |

## One point left open

The brief allows "documented usage endpoints" as a source of limits, and also says the view sends nothing anywhere. A usage endpoint is a network call. This design takes the stricter reading and calls none. If the owner wants allowance figures that only an endpoint provides, the fit would be an explicit, per-provider, off-by-default "refresh limits" action that calls only that provider's documented endpoint with the user's own credential. It is noted here and in the build plan, and not built until asked for.
