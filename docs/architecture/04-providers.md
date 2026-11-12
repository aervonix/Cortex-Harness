# Providers

`cortex-llm` gives the agent loop one way to talk to any model. The design goal is an abstraction that loses nothing a provider needs to get back, while keeping every mapping a pure, fixture-testable function.

The provider facts on this page were read from the providers' public documentation in November 2026. The research notes list the sources. They change often: each provider step in the build plan starts by re-reading the current docs.

The neutral types this page describes (messages, parts, stop reasons, usage, model info) are defined in `cortex-protocol`, because front ends and the session log use them too. `cortex-llm` adds the request and the stream events, which nothing outside the provider layer sees.

## Three separate concepts

Keeping these apart is the central decision. The same model can be reached through different wire dialects on different endpoints, each with different fidelity.

| Concept | What it is | Examples |
|---|---|---|
| **Protocol** | A wire dialect: how to build a request, decode a stream and classify an error. Code. | `anthropic-messages`, `openai-chat`, `openai-responses`, `gemini-generate` |
| **Provider** | An endpoint that speaks a protocol: base URL, credential source, extra headers, capability flags. Data, from the built-in table or user configuration. | `anthropic`, `openai`, `openrouter`, `ollama`, `google`, or a user-defined one |
| **Model** | A catalog entry: limits, prices, reasoning controls, capabilities. Data. | one row per provider and model id |

A user adds an OpenAI-compatible server by writing a provider entry in configuration. No code changes. Quirks are capability flags on the provider or model entry (for example `reasoning_field = "reasoning_content"`, `usage_in_stream = false`, `tool_choice = false`), not branches on model-name substrings.

## Traits

As built in step 5 (`crates/cortex-llm`). The steps that add a protocol or use the client build on these shapes. Changing one is recorded here and in the step's report.

```rust
/// A wire dialect. Stateless and pure: no I/O.
pub trait Protocol: Send + Sync + Debug {
    /// The dialect's name, as catalog and provider entries write it.
    fn name(&self) -> &str;
    /// Build the HTTP request for one model call, without credentials.
    fn build(&self, request: &ModelRequest, provider: &ProviderInfo)
        -> Result<HttpRequest, BuildError>;
    /// A fresh decoder for the response to `request`.
    fn decoder(&self, request: &ModelRequest, provider: &ProviderInfo) -> Box<dyn StreamDecoder>;
    /// Turn a failed response into a typed error.
    fn classify(&self, failure: &HttpFailure<'_>, request: &ModelRequest) -> ProviderError;
    /// Read the headers of a successful response: request id, rate limits,
    /// the speed that served the request. The default reads nothing.
    fn read_headers(&self, headers: &Headers, now: UtcTime, out: &mut EventSink) {}
}

/// Incremental decoder: SSE events in, neutral model events out.
pub trait StreamDecoder: Send {
    fn feed(&mut self, event: SseEvent<'_>, out: &mut EventSink) -> Result<(), DecodeError>;
    fn finish(&mut self, out: &mut EventSink) -> Result<(), DecodeError>;
}

/// Sends a request and streams the body. The only part that does I/O.
pub trait Transport: Send + Sync + Debug {
    fn send(&self, request: HttpRequest, cancel: CancelToken)
        -> BoxFuture<'_, Result<HttpResponse, TransportError>>;
}

/// What the agent loop sees.
pub trait ModelClient: Send + Sync {
    fn stream(&self, request: ModelRequest, cancel: CancelToken) -> ModelStream;
}

/// Something that can wait. Every wait in the crate goes through it.
pub trait Timer: Send + Sync + Debug {
    fn sleep(&self, duration: Duration) -> Sleep;
}
```

- `build`, `feed` and `classify` are tested from files: a request snapshot per scenario, and recorded response streams replayed with every possible chunk boundary.
- **The request carries the model.** `ModelRequest` holds the catalog entry of the model it is for, so `build` and `decoder` take the request and the provider and no third argument. A client is for one provider; which client a role's requests go to is decided where the core is assembled.
- **`classify` and `decoder` are given the request as it was sent**, because one thing cannot be told without it: whether a refusal belongs to the speed tier the request was sent at ("Speed tiers", below).
- **A decoder writes to an `EventSink`, not to a list of events.** The sink has one method per thing a response can say (`start`, `text_delta`, `tool_call_start`, `block_end`, `usage`, `end`, `error`, `served_at`, `limits`, `request_id`). It is the one place that turns "the response reported this speed" into "the request ran at this speed", so no adapter can get that rule wrong.
- **A transport returns a response for every status.** Whether a status is a failure is the protocol's to say, so a 429 reaches `classify` with its headers and body. A transport has no time limit of its own except for making a connection.
- `Transport` has two implementations: the real one (reqwest), behind the `http` feature, and a scripted one in the test kit.
- `ModelClient` is implemented once, for every protocol, by `ProviderClient`: a protocol, a transport, a credential and the retry layer. The scripted provider in the test kit implements `ModelClient` directly, below nothing, for loop tests.
- **`ModelStream` is a stream of `ModelEvent`s that owns everything it needs**, so it can be moved to a task of its own. Nothing is sent until it is first polled. It produces one event at a time and only when asked, so a slow reader holds no queue. Dropping it abandons the request and closes the connection. Its last event is always `End` or `Error`, exactly one of them; a cancelled request ends with an `Error` of kind `Interrupted`.
- **Nothing in the crate reads a real clock or asks the runtime for a timer.** The time comes from the `Clock` of `cortex-base` and every wait from a `Timer`. The harness passes the runtime's timer; a test passes the fake timer of the test kit, which moves the fake clock. The retry layer, the two time limits on a response and a ten-minute cooldown are therefore tested against one clock, in microseconds. Only the real transport needs the async runtime.

## The conversation model

Neutral types in `cortex-protocol`. A message is a role and an ordered list of parts.

| Part | Fields | Notes |
|---|---|---|
| Text | text | |
| Image | media type, data or artifact reference | Input only for now. |
| Tool call | id, name, arguments (JSON) | The id is the provider's. Protocols with two ids (OpenAI Responses) keep both in the raw block. |
| Tool result | call id, content parts, `is_error` | Protocols with no error flag get the error encoded in the text. |
| Reasoning | visible text (may be empty), opaque payload | See below. |
| Provider block | raw JSON | Anything the provider produced that the neutral model does not name: server-side tool use and results, compaction blocks, fallback markers. |

**Raw blocks are kept beside the neutral view.** Every assistant message stores, for each part, the provider's own JSON for that block, byte for byte, with the protocol, provider, model and account scope that produced it. In the protocol types a raw block is **a string that holds the JSON text**, not a nested JSON value: a value would be re-ordered, re-escaped or re-rounded by the first parser it passed through, in our code or in a client's, and "byte for byte" would stop being true. Replay follows one rule:

- Same protocol and a model that can read the block: send the raw block unchanged.
- Anything else: send the neutral projection. Text, tool calls and tool results carry over. Reasoning and provider blocks are dropped, not translated.

This is what keeps signatures, encrypted reasoning items and thought signatures valid, and it means a block type added by a provider next month is preserved without a code change. The catalog records which models can read which models' reasoning.

**Ordering constraints** are the protocol adapter's job, not the loop's. The loop always produces the same neutral shape (assistant message with calls, then one results message in call order). Each adapter arranges it as its dialect requires, for example results first and adjacent for Anthropic, or all calls then all results for Gemini.

## Stream events

```rust
pub enum ModelEvent {
    Start { response_id: Option<String>, model: String },
    BlockStart { index: u32, kind: BlockKind },          // text, reasoning, tool call, provider block
    TextDelta { index: u32, text: String },
    ReasoningDelta { index: u32, text: String },         // visible reasoning or summary only
    ToolCallStart { index: u32, id: String, name: String },
    ToolArgsDelta { index: u32, json: String },          // fragment, not yet valid JSON
    BlockEnd { index: u32, raw: RawJson },               // the provider's complete block
    Usage(Usage),                                        // may arrive more than once
    Limits(Vec<LimitWindow>),                            // the rate limits the response reported
    Retry(RetryInfo),                                    // the attempt failed; another follows a wait
    SpeedFallback(SpeedMove),                            // runs, or ran, at another speed than asked
    SpeedCooldown(CooldownChange),                       // a tier's cooldown began or ended
    End { stop: StopReason, raw_stop: String, stop_detail: Option<String>, usage: Usage },
    Error(ProviderError),                                // the request failed for good
}
```

**One stream can hold more than one response.** The retry layer is inside the client, so the loop sees an attempt that failed as events: `Retry` when the request will be sent again after a wait, and `SpeedFallback` by the harness when it is sent again at standard speed. Either is followed by a new `Start`, and everything between the earlier `Start` and that point belongs to an attempt that no longer counts. The loop discards it and tells front ends to (`item.reset`). An `Error` is the last event: the request failed and will not be tried again here.

Rules for decoders:

- Blocks are keyed by index. Indexes need not be contiguous or arrive in order.
- Unknown provider event types are skipped, never an error.
- An error event after a successful status is an `Error`, classified like any other.
- The decoder reads until the provider's terminal event. Some providers deliver required data (a signature, the usage) in the last chunk.

**SSE parsing** is a small parser of our own over the byte stream: event name, data lines, comments, blank-line terminator, all three line endings, no reconnection logic. The available crates are unmaintained and do more than is wanted here. It is tested with every chunk split: each fixture cut at every byte, one byte at a time, and at every pair of cuts for the short ones; and with bytes that are not a stream at all, cut at random, which must give the same events however they are cut. It keeps one event's worth of memory and refuses an event over 32 MiB.

## Tool definitions

- A tool is a name, a description and a JSON Schema for its arguments. Names match `^[a-zA-Z0-9_-]{1,64}$`, the strictest of the provider rules, so one name works everywhere.
- Schemas for built-in tools are written to the strict subset the providers share: objects with `additionalProperties: false`, every property listed as required, optional values expressed as nullable, no recursion, no numeric or length constraints. Strict mode is requested where the provider has it.
- Tool order in a request is fixed for the context epoch and sorted deterministically.
- The harness never forces a specific tool. Several current models reject forced tool choice. Where a fixed output shape is needed (compaction summaries, titles), the request uses the provider's structured-output feature or plain instructions.
- Streaming of tool arguments is requested in fine-grained mode where the provider offers it, so large arguments (file contents) appear as they are generated. The harness then owns validation, as described in [03-agent-loop.md](03-agent-loop.md).

## Roles and per-model variants

Which model serves a request, and in which style it is addressed, are decided above this crate and passed in with the request.

- **Roles.** Configuration names models as `main`, `fast` and `judge`. The agent loop asks for a role; utility calls (summaries, titles) use `fast`. See [14-adaptation.md](14-adaptation.md).
- **Variants.** The edit format, tool-schema style and prompt style in effect for a model are part of the request, fixed for the context epoch, and recorded with every outcome so their success rates can be compared.

## Speed tiers

Some providers will run the same model faster, or slower and cheaper, at a different price. The design is in [decision 0027](../decisions/0027-speed-tiers.md). The provider facts in this section were read on 2026-11-12.

**A speed is a named tier that a model offers.** `standard` is what every model has. `fast` is the well-known name for a tier that produces output faster at a higher price. Any other tier keeps the provider's own name. There is no scale: what a tier is comes from its catalog entry, never from its name.

| What a catalog entry says about a tier | Why it is there |
|---|---|
| Its name, and the provider's label for it | The name is what a setting and a usage record carry. |
| What it buys: `faster_output`, `priority` or `deferred` | Anthropic's fast mode buys output tokens per second. A priority tier buys queue position. A flex tier sells patience. They are not points on one line. |
| Its own full rate card | Fast mode is twice the standard price on every token class. Other tiers are not a single multiplier. |
| What the provider does when it cannot serve it: `error` or `downgrade` | Anthropic's fast mode fails with a 429 or a 529. OpenAI's fast tier and Google's priority tier are silently served at standard and billed at standard. Flex tiers fail. |
| Whether it has a rate limit of its own | Anthropic's has, with its own response headers. |
| Whether it shares the prompt cache with standard | Anthropic's does not: a change of speed is a cache miss. Where a provider does not say, the entry says "not stated". |
| Whether it is a preview | Access may need to be granted, and the terms may change. |

**The setting.** A session has a speed, which is what the `main` role runs at, and a role may have its own. The utility roles (`fast`, `judge`) do not inherit the session's speed: they run at `standard` unless the user sets theirs, so that a fast session does not also pay double for its summaries.

**In a request.** The loop passes the speed in effect for the role. Each adapter maps it to its provider's mechanism: a body field (`speed` at Anthropic, `service_tier` at OpenAI and Google), an extra header, or, where a platform sells the tier as a separate deployment, another model id. A provider-side default such as `auto` is never relied on: the adapter sends an explicit tier, so that what was asked for is known.

**In a response.** The adapter reads the tier that served the request wherever the provider reports it (`usage.speed`, a top-level `service_tier`, a response header) and passes it on. It never fills it in from the request. The event sink then applies one rule for every protocol: **a request is recorded at the speed the response reports, and at the speed it was sent at only when the response reports none.** That, not the setting, is what the request is priced at. When the two differ the usage carries both, and the stream carries a `SpeedFallback` event marked as the provider's doing, once. "Asked for fast, served standard" is never silent.

**When a tier is refused.** The classification table gains one distinction: a rate limit or overload that belongs to the tier, not to the model. The retry layer then has two choices beyond an ordinary retry.

| Situation | What happens |
|---|---|
| The provider's wait is short | Wait and send again at the same speed. Nothing changes, and the cache prefix survives. |
| The wait is long or unknown, and falling back is allowed | Send the request at `standard`, and start a **cooldown** for the tier on that provider and model. Until it ends, requests go at `standard`. Then they ask for the tier again. |
| Falling back is turned off | The refusal is an ordinary rate limit: backoff, then the error. |
| The provider served another tier by itself | Nothing to do. The usage says what ran, and the user is told. |

The numbers, settled in step 5 ([decision 0027](../decisions/0027-speed-tiers.md)). All three are settings; the configuration step names their keys.

| Parameter | Default | Meaning |
|---|---|---|
| Fall back | on | Whether a refused tier's request is sent at `standard`. Off makes the refusal an ordinary rate limit. |
| Short wait | 20 s | How long **one request** may wait in place for its tier, in total. While what it has waited stays within this, it waits as long as the provider asked and is sent again at the same speed. The first wait that would go over it is a fallback instead. A refusal that names no wait is a fallback at once. Zero never waits in place. |
| Cooldown | 10 minutes | How long a cooldown lasts when the provider did not say when the tier is available again. When it did, the cooldown ends at that time. |

- **A fallback is not a retry of a failure.** The refused attempt is not counted against the request's attempts, so `standard` gets a send of its own even when the tier was refused on the last attempt.
- **A cooldown is kept per provider, model and tier**, in memory, for the life of the process, on the monotonic clock. The loop reads the same table to show the speed in effect and the cooldown's end.
- **Its end is noticed, not timed.** An idle session has no timers. The first request that asks for the tier after the time has passed is sent at the tier, and the stream says the cooldown ended.
- **Every request sent at `standard` during a cooldown says so**, with a `SpeedFallback` by the harness whose reason is `cooldown`. A request for a tier the model does not offer is sent at `standard` with the reason `not_offered`.
- **A provider's own downgrade starts no cooldown.** There is nothing to wait for: the next request asks for the tier again.

The cooldown exists because of the cache: where speeds do not share a prefix, every change of speed re-writes the cache for the whole conversation, which can cost more time and money than the tier saves. So the harness changes speed rarely and stays there for a while. Front ends are told through the `speed_fallback` and `speed_cooldown` notices ([11-rpc-protocol.md](11-rpc-protocol.md)).

**What is not a speed.** Capacity an account has bought or reserved, batch processing, and a default set at the provider for a whole project or deployment. Decision 0027 says why.

**Built in:** the types in step 3; the request field, the catalog format, the classification and the cooldown in step 5; Anthropic in step 14; the other providers with their own steps.

## Reasoning

Three things are kept apart because providers keep them apart.

| Aspect | Design |
|---|---|
| **Depth** | Each model's catalog entry lists its control: an ordered set of effort levels with a default, a token budget range, or none. Configuration names a level from that model's own set. A portable alias (`minimal`, `low`, `medium`, `high`, `max`) is mapped to the nearest level the model has, and the UI shows the level in effect. There is no numeric slider. |
| **Visibility** | A separate setting: `summary` (default, where the provider can give one), `none`. Providers differ in what they can show: a summary, nothing, or raw text on local servers under several field names. |
| **Continuity** | Opaque reasoning payloads are stored and replayed as raw blocks under the rule above. On a model switch they are dropped. The loop never edits them. |

Changing depth in the middle of a context epoch uses the provider's append-only mechanism where one exists. Otherwise the change takes effect at the next epoch, and the UI says so.

## Prompt caching

Caching is treated as a property of request shape, not a switch.

**The core's part: keep the prefix stable.**

- The system prompt and tool list are frozen for the context epoch.
- History is append-only (invariant 1).
- Serialisation is deterministic: stable key order, stable tool order, no timestamps or random ids in the prefix.
- A debug-build check, and a test in every provider's suite, compares each request with the previous one in the epoch and fails if the shared prefix is not byte-identical.

**The adapter's part: place markers.** The request carries neutral cache hints: "end of the stable prefix" and "end of history so far". Each adapter turns them into its provider's feature.

| Protocol | Mapping |
|---|---|
| Anthropic Messages | An explicit breakpoint after the tools and system prompt, plus the automatic moving breakpoint for the conversation. A second explicit breakpoint when one round adds more blocks than the lookback window. The one-hour lifetime is a configuration choice. |
| OpenAI Responses and Chat | Implicit caching, with a session-derived cache key where the model uses one, or explicit breakpoints where the model supports them. |
| Gemini | Implicit caching. Nothing to place. |
| Local servers | Nothing. Their prefix reuse is automatic. |

**Reporting.** Cache reads and writes are separate usage fields (below), shown in the status line and stored per request, so a broken prefix shows up as a number, not a guess.

## Usage and cost

One normalised record per request, with the provider's raw usage object kept beside it.

| Field | Meaning |
|---|---|
| `input` | Input tokens billed at the normal rate (not read from or written to the cache). |
| `cache_read` | Input tokens read from the cache. |
| `cache_write` | Input tokens written to the cache, split by lifetime class where the provider has classes. |
| `output` | Output tokens, including reasoning. |
| `reasoning` | The part of `output` that was reasoning, when reported. |

Beside the token counts the record carries the provider, the model, the role, and **the speed the request ran at**, with the speed that was asked for when the two differ ("Speed tiers", above).

Providers disagree on what "input" includes, so each adapter documents and tests its normalisation (for example, subtracting cached tokens where the provider's input count includes them). Streaming usage may be cumulative, final-only, or absent after an abort; the adapter reports what it has, and `End.usage` is authoritative.

**Cost** is computed when the request is recorded: tokens × the catalog prices in force **for the speed the request ran at**, with context-size tiers and lifetime-class multipliers. A size tier is chosen by every input token the request sent (fresh, cache reads and cache writes together) and prices the whole request. Cache writes the provider did not put in a lifetime class are priced at the first class the rate card lists. A token class that was used and has no price gives no cost at all, never a guess. Money is kept as whole nano-dollars, so that sums are exact. The entry stores the cost and the catalog version, so later price changes do not rewrite history. A provider that reports cost itself overrides the calculation. A model with no price data shows tokens only.

**Out-of-band usage** (compaction, title generation) is recorded as separate usage entries and included in session totals.

**Token estimates** for budgeting use the last reported usage plus a bytes-based estimate of what was added since. Provider count endpoints are used only near the limit, where precision matters. There is no local tokenizer.

## Model catalog

- **Embedded.** The binary carries a compact catalog generated at build time from `catalog/models.toml` in the repo: the build script of `cortex-llm` turns the TOML into JSON, the crate embeds it, and reads it on first use (18 to 24 µs for the first four models). No network at build time or at startup. The file documents its own format, and an unknown key in it is an error. A test loads the built-in catalog, so an entry that is not valid cannot be merged.
- **Every fact has a source and a date.** Entries are read from the providers' documentation by hand and say where and when. A fact the documentation does not state is left out, not filled in. The first catalog (step 5) has Anthropic's four current models.
- **Maintained by script.** `xtask catalog update` drafts changes from the providers' model-list endpoints and from models.dev (an MIT-licensed community catalog). A maintainer reviews the diff. The checked-in file is ours and is the source of truth.
- **Per entry:** id, display name, protocol, context window, maximum output, prices (input, output, cache read, cache write by class, tiers), reasoning control, which models' reasoning it can read, modalities, capability flags (strict tools, mid-conversation system messages, structured output, minimum cacheable size), and the speed tiers it offers besides standard, each with its own rate card. No provider's model list reports tier support, so this part of an entry is maintained by hand from the providers' documentation.
- **User overlay.** Configuration can add or override entries. This is how local and private models are described. An overlaid entry replaces the built-in one for the same provider and model whole: nothing is merged field by field, so an entry says everything about its model.
- **Variant defaults.** Each entry names the default at each variant point ([14-adaptation.md](14-adaptation.md)). Left out, they are the `replace` edit format, strict tool schemas where the model has them, and the `concise` prompt style.
- **Flags.** What a model's or a provider's protocol needs to know that the neutral fields do not say is a named flag on the entry. Each protocol documents the flags it reads.
- **Runtime discovery, on request.** `cortex models refresh` queries the provider's list endpoint and overlays limits and capabilities in the user's state directory. It never runs on its own.
- **Local servers** are asked at connect time for the loaded context size, because it differs from the model's trained size.

## Credentials

- **Sources, in order:** a command-line flag, the provider's environment variable (`ANTHROPIC_API_KEY` and so on), then stored credentials. The active source is shown by `cortex auth status` and never the value.
- **Storage:** the OS keychain where enabled, otherwise a file in the configuration directory with owner-only permissions. Details and the per-OS choice are in [08-configuration.md](08-configuration.md).
- **Who does what.** `cortex-config` owns the stores and the lookup order, and returns the key as a `Secret` with a description of its source. It does not depend on this crate. `cortex-core` hands the key to the API-key `AuthMethod` below.
- **Handling:** credentials are `Secret` values from the moment they are read. They are attached to requests inside the transport, after request logging. They are never written to session logs, artifacts or debug bundles.
- **Sign-in beyond API keys.** The credential layer is a trait with one implementation in the first release (an API key) and room for more:

```rust
pub trait AuthMethod: Send + Sync {
    /// Headers for one request. May refresh a token first.
    fn authorize<'a>(&'a self, req: &'a mut HttpRequest) -> BoxFuture<'a, Result<(), AuthError>>;
    /// What `cortex auth status` shows. Never the secret.
    fn describe(&self) -> AuthSummary;
}
```

  A sign-in flow adds an implementation and a login routine (browser hand-off with a loopback redirect, and a paste-the-code fallback for remote shells). The same routine serves OAuth for remote MCP servers later. Only flows that a provider officially offers to third-party tools are candidates: Anthropic's terms do not allow third-party use of consumer-plan logins; OpenAI offers plan sign-in to open-source local clients under a preview with restrictions; an aggregator offers a sign-in that mints an API key. None is in the first release, and each is added only after its terms are checked.
- **Headers.** Each protocol sets its own authentication and version headers. Provider entries can add static headers. Header values that come from the environment are also `Secret`.

## Error classification

Each protocol implements `classify` as a table from (transport error, status, provider error code, headers) to an `ErrorKind` and a retry hint. The table for each protocol lives beside its fixtures. The shared rules:

| Signal | Kind |
|---|---|
| Connection refused or reset, DNS failure, TLS failure, timeout | `Transient` |
| 408, 409, 5xx, provider overload codes | `Transient` |
| 429 with a rate-limit code | `Transient`, with the server's wait |
| 429 or 402 with a spending, credit or quota code | `Provider` |
| 401, 403 | `Auth` |
| 400 context-length error | `ContextOverflow` |
| Other 4xx | `Provider` |
| Error event after a 200 | Classified by its code as above |

**A code that names the failure wins over the status it came with.** A spending cap is not retried because it arrived as a 429. Each protocol reads its provider's codes into one of a few signals (rate limit, quota, overload, server failure, context length, credential), and one shared function applies the table.

**A refusal can belong to a speed tier.** A classified error says whether it was a rate limit or an overload, and names the tier whose own limit or capacity turned the request away, when it was a tier's and not the model's. Only the protocol can tell, from the provider's headers and codes and the speed the request was sent at.

Failures that are not HTTP responses have codes of their own: `connection_failed`, `connection_lost`, `timeout`, `idle_timeout`, `first_event_timeout`, `incomplete_stream` (the stream ended before the provider's terminal event), `malformed_stream`. All are `Transient`.

The provider's request id is attached to every error and written to the log.

## Transport

- **Client:** reqwest with rustls and the ring crypto provider, HTTP/2 enabled, default features off. Measured against the alternatives in [decision 0004](../decisions/0004-http-and-tls.md).
- **One client per process**, created lazily on first use, off the start-up path. Connections are pooled. Making the transport costs under a microsecond. Creating the client costs 1.06 to 1.13 ms warm and 44.6 ms on the first run after a build, when the certificate store is not in the file cache (step 5, [../performance-budgets.md](../performance-budgets.md)). `warm()` creates it ahead of the first request, from a background task.
- **Redirects are not followed.** A model endpoint has no reason to redirect, and following one would carry the credential headers to wherever it points. A redirect is a response like any other and is classified as a failure.
- **Time limits are the client's, not the transport's.** One limit runs from sending a request to the first event of its response (default 60 s), and one on silence after that (default 90 s; keep-alive pings count as bytes). Both are in the retry settings and a provider's entry can set its own, which a local server that reads a long prompt before it answers needs. Either one ending is a `Transient` failure.
- **Pre-warm.** When a session starts with a configured provider, the core opens a connection to it in the background, so the first request does not pay for DNS, TCP and TLS.
- **Proxies:** standard environment variables are honoured. Custom CA bundles are configurable.
- **Statelessness.** The harness sends the full history on every request and does not depend on server-side conversation state. Stateful provider features can be added later as optimisations behind the same trait.

## Protocols in scope

| Protocol | Covers | When |
|---|---|---|
| `anthropic-messages` | Anthropic. Also Anthropic-shaped endpoints on other servers. | First. |
| `openai-chat` | OpenAI Chat Completions, OpenRouter, Ollama, llama.cpp, vLLM, LM Studio and other compatible servers, with capability flags for their differences. | Second, before the first release, because a second protocol is what proves the abstraction. |
| `openai-responses` | OpenAI's current API, needed for its newest models' tool calling. | After the first release. |
| `gemini-generate` | Google's `generateContent` API. Google now also documents a newer stateful API; which one to target is decided when that step starts. | After the first release. |

## What the abstraction must not flatten

A checklist for reviewing any change to the neutral types.

1. Opaque reasoning payloads: ordered, byte-exact, tagged with their producer.
2. Both ids of a tool call where a protocol has two.
3. The error flag on tool results, and its absence on some protocols.
4. Server-side tool blocks as content.
5. The full stop-reason union, including refusal categories, pauses and malformed calls.
6. Usage fields whose meaning differs per provider.
7. Reasoning depth as a per-model set, and visibility as a separate setting.
8. The difference between protocol, provider and model.
9. Speed tiers as a per-model set with their own prices and failure modes, and the difference between the speed that was asked for and the speed that ran.
