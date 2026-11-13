# Providers

`cortex-llm` gives the agent loop one way to talk to any model. The design goal is an abstraction that loses nothing a provider needs to get back, while keeping every mapping a pure, fixture-testable function.

The provider facts on this page were read from the providers' public documentation in November 2026. The research notes list the sources. They change often: each provider step in the build plan starts by re-reading the current docs. **Anthropic's were read again on 2026-11-13, in step 14**, which built the first protocol; what had moved is listed in "The `anthropic-messages` protocol". **The servers that speak `openai-chat` were read the same day, in step 31**, which built the second, and three of them were asked: "The `openai-chat` protocol", at the end of the page, says what each fact came from.

The neutral types this page describes (messages, parts, stop reasons, usage, model info) are defined in `cortex-protocol`, because front ends and the session log use them too. `cortex-llm` adds the request and the stream events, which nothing outside the provider layer sees.

## Three separate concepts

Keeping these apart is the central decision. The same model can be reached through different wire dialects on different endpoints, each with different fidelity.

| Concept | What it is | Examples |
|---|---|---|
| **Protocol** | A wire dialect: how to build a request, decode a stream and classify an error. Code. | `anthropic-messages`, `openai-chat`, `openai-responses`, `gemini-generate` |
| **Provider** | An endpoint that speaks a protocol: base URL, credential source, extra headers, capability flags. Data, from the built-in table or user configuration. | `anthropic`, `openai`, `openrouter`, `ollama`, `google`, or a user-defined one |
| **Model** | A catalog entry: limits, prices, reasoning controls, capabilities. Data. | one row per provider and model id |

A user adds an OpenAI-compatible server by writing a provider entry in configuration. No code changes. Quirks are capability flags on the provider or model entry (for example `reasoning_passback = "reasoning_content"`, `usage_in_stream = false`, `max_tokens_field = "max_completion_tokens"`), not branches on model-name substrings.

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
- **The first protocol changed none of these traits.** Step 14 added one thing beside them: `ImageSource`, through which an image that is stored as an artifact reaches a request ("Images", below).
- **Neither did the second.** Step 31 added `openai-chat` as a module beside the first, and changed one rule of the client: bytes that arrive before the first event of a response restart the wait for it ("Transport", below).
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

- A message that the **same provider** produced through the **same protocol**, sent to a model that can read its blocks: send each raw block unchanged.
- Anything else: send the neutral projection. Text, tool calls and tool results carry over. Reasoning and provider blocks are dropped, not translated.

This is what keeps signatures, encrypted reasoning items and thought signatures valid, and it means a block type added by a provider next month is preserved without a code change.

**Which models can read a block is the provider's to say, in one of two ways** (settled in step 14, with the owner, on 2026-11-13).

| The provider's entry says | What is replayed raw after a change of model | For |
|---|---|---|
| The server leaves out what a model cannot read (`replay_reasoning = "all"`) | Every block of that provider's own messages, whatever model wrote it and whatever model is asked. | Anthropic's own API. Its built-in entry says so. |
| Nothing | Only to the model that wrote the message, or to one whose catalog entry lists that model in `reads_reasoning_from`. For any other model the message is sent as its neutral projection. | Every other server, including an Anthropic-shaped one that does not drop blocks itself. |

- **Why Anthropic gets every block.** Its documentation asks for it ("Preserved thinking", read 2026-11-13): keep sending the whole conversation after a change of model; the API leaves out, unbilled, the reasoning the new model cannot read; "reasoning is lost permanently only if your code removes it". And leaving blocks out ourselves would break something. On its current models each reasoning block is tied to everything that came before it, and a block sent back after a change to earlier content fails a check. Take a conversation that goes from model A to model B and back. If the harness strips A's reasoning for B, then B's own reasoning is tied to a history without A's blocks. Back on A, the harness sends A's blocks again, B's reasoning no longer matches what is before it, and the request is answered with a 400. Sending every block every time keeps both models' blocks valid: what the server leaves out does not count as a change.
- **The catalog still records which models read which models' reasoning** (`reads_reasoning_from`). For Anthropic it is information for a front end, which can say before a switch that the next model will work without the earlier reasoning. For a server without the flag it is the rule.
- **The account is not part of the rule yet.** An assistant message records the account its credential belonged to, where the provider ties reasoning to it, and a request does not carry the account it is sent with, so the two cannot be compared. Anthropic's own API needs no comparison: it leaves out, without an error, the reasoning another account produced. A server under the stricter rule would be sent it. Step 13, which hands the credential to the client, decides whether a request should carry the account.
- **A stored block that is not one JSON object is never written into a request.** A session log can be damaged, or doctored: a "block" that closes the message it is in and opens another would otherwise put words in a user's mouth. Such a part is sent from its neutral fields, or left out when it has none.

**Images.** A tool that reads an image does not put the image in the conversation. It stores it and returns a reference: an image part with an artifact id and no data. The conversation, the session log and every front end carry the reference. A provider needs the bytes, a protocol is pure and reads no files, and `cortex-llm` cannot see the crate that owns the store. So the store is reached through a trait:

```rust
/// Where the bytes of a stored image come from.
pub trait ImageSource: Send + Sync + Debug {
    fn load<'a>(&'a self, artifact: &'a ArtifactId) -> BoxFuture<'a, Result<Vec<u8>, ImageError>>;
}
```

- **Who does what.** `cortex-core` implements it over the session's content store and gives it to the client (`ProviderClientBuilder::images`); step 13 wires it. Before a request is built, the client replaces every reference in its own copy of the request with the image's bytes in base64, once for all attempts. The conversation the loop holds keeps the references.
- **The prefix does not move.** An artifact's content never changes, so the same reference gives the same bytes in every request.
- **A reference that cannot be loaded fails the request before anything is sent,** with the code `image_unavailable`. A protocol that meets a reference nobody loaded refuses the request: a reference is never sent as it is.
- **A model that takes no images** is sent a line of text in the image's place that says an image was there.

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
- **Where a provider never sends a block whole, the decoder puts it together.** A streamed block arrives as the block when it starts and then what is added to it. The decoder keeps each member of the starting block as the text the provider sent, and when the block stops it writes them back in their order, with the streamed member replaced by what was added. No member is parsed and written again, so a number, an escape or the order of keys cannot change on the way. The result is the block as the provider would have sent it without streaming.
- **A block of a kind the decoder does not know keeps what is added to it.** An addition of an unknown kind to such a block is laid over the block, member by member, text appended to text. That is how a block type added next month is preserved without a code change: Anthropic's compaction block, for one, starts empty and gets all of its content in a single addition of its own kind. An addition of an unknown kind to text, reasoning or a tool call is skipped, like an unknown event.
- **An index names one block for the whole response.** A block that starts under an index that was already used is an error, because whoever reads the events keys a block by its index.

**SSE parsing** is a small parser of our own over the byte stream: event name, data lines, comments, blank-line terminator, all three line endings, no reconnection logic. The available crates are unmaintained and do more than is wanted here. It is tested with every chunk split: each fixture cut at every byte, one byte at a time, and at every pair of cuts for the short ones; and with bytes that are not a stream at all, cut at random, which must give the same events however they are cut. It keeps one event's worth of memory and refuses an event over 32 MiB.

## Tool definitions

- A tool is a name, a description and a JSON Schema for its arguments. Names match `^[a-zA-Z0-9_-]{1,64}$`, the strictest of the provider rules, so one name works everywhere.
- Schemas for built-in tools are written to the strict subset the providers share: objects with `additionalProperties: false`, every property listed as required, optional values expressed as nullable, no recursion, no numeric or length constraints. Strict mode is requested where the provider has it.
- Tool order in a request is fixed for the context epoch and sorted deterministically.
- The harness never forces a specific tool. Several current models reject forced tool choice. Where a fixed output shape is needed (compaction summaries, titles), the request uses the provider's structured-output feature or plain instructions.
- Streaming of tool arguments is requested in fine-grained mode where the provider offers it, so large arguments (file contents) appear as they are generated. The harness then owns validation, as described in [03-agent-loop.md](03-agent-loop.md). A call whose arguments are not a JSON object when the response ends (cut off by the output limit, or nonsense) is stored with the text the model wrote inside an object, so that the block can be sent back; the loop is handed the model's own text.

## Roles and per-model variants

Which model serves a request, and in which style it is addressed, are decided above this crate and passed in with the request.

- **Roles.** Configuration names models as `main`, `fast` and `judge`. The agent loop asks for a role; utility calls (summaries, titles) use `fast`. See [14-adaptation.md](14-adaptation.md).
- **Variants.** The edit format, tool-schema style and prompt style in effect for a model are part of the request, fixed for the context epoch, and recorded with every outcome so their success rates can be compared.

## Speed tiers

Some providers will run the same model faster, or slower and cheaper, at a different price. The design is in [decision 0027](../decisions/0027-speed-tiers.md). The provider facts in this section were read on 2026-11-12, and Anthropic's again on 2026-11-13, when its fast mode was also tried against the real API.

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

**In a request.** The loop passes the speed in effect for the role. Each adapter maps it to its provider's mechanism: a body field (`speed` at Anthropic, `service_tier` at OpenAI and Google), an extra header, or, where a platform sells the tier as a separate deployment, another model id. A provider-side default such as `auto` is never relied on: the adapter sends an explicit tier, so that what was asked for is known. **That holds for a model that offers a tier.** A model that offers none is not told a speed at all: Anthropic answers a request that names a speed, even `standard`, with a 400 on a model that has no fast mode (recorded in step 14, in the fixture `error-speed-not-offered`).

**In a response.** The adapter reads the tier that served the request wherever the provider reports it (`usage.speed`, a top-level `service_tier`, a response header) and passes it on. It never fills it in from the request. And it reads it only where the request could have named a tier that way: one server passes on its upstream host's `service_tier` on requests it bills at its own standard price ("The `openai-chat` protocol"). The event sink then applies one rule for every protocol: **a request is recorded at the speed the response reports, and at the speed it was sent at only when the response reports none.** That, not the setting, is what the request is priced at. When the two differ the usage carries both, and the stream carries a `SpeedFallback` event marked as the provider's doing, once. "Asked for fast, served standard" is never silent.

**When a tier is refused.** The classification table gains one distinction: a rate limit or overload that belongs to the tier, not to the model. The retry layer then has two choices beyond an ordinary retry.

| Situation | What happens |
|---|---|
| The provider's wait is short | Wait and send again at the same speed. Nothing changes, and the cache prefix survives. |
| The wait is long or unknown, and falling back is allowed | Send the request at `standard`, and start a **cooldown** for the tier on that provider and model. Until it ends, requests go at `standard`. Then they ask for the tier again. |
| Falling back is turned off | The refusal is an ordinary rate limit: backoff, then the error. |
| The provider served another tier by itself | Nothing to do. The usage says what ran, and the user is told. |

The numbers, settled in step 5 ([decision 0027](../decisions/0027-speed-tiers.md)). All three are settings: `model.speed_fallback`, `model.speed_wait` and `model.speed_cooldown` ([08-configuration.md](08-configuration.md)).

| Parameter | Default | Meaning |
|---|---|---|
| Fall back | on | Whether a refused tier's request is sent at `standard`. Off makes the refusal an ordinary rate limit. |
| Short wait | 20 s | How long **one request** may wait in place for its tier, in total. While what it has waited stays within this, it waits as long as the provider asked and is sent again at the same speed. The first wait that would go over it is a fallback instead. A refusal that names no wait is a fallback at once. Zero never waits in place. |
| Cooldown | 10 minutes | How long a cooldown lasts when the provider did not say when the tier is available again. When it did, the cooldown ends at that time, or after the shortest cooldown, whichever is later. |
| Shortest cooldown | 1 minute | The least a cooldown lasts when the provider named a time. A provider's wait says when one more request would get through, not when the pressure on the tier is over; a minute is the window its limits are counted in. Settled in step 14 ([decision 0027](../decisions/0027-speed-tiers.md)). It is a setting of the retry policy; its configuration key is step 13's to add. |

- **A fallback is not a retry of a failure.** The refused attempt is not counted against the request's attempts, so `standard` gets a send of its own even when the tier was refused on the last attempt.
- **A cooldown is kept per provider, model and tier**, in memory, for the life of the process, on the monotonic clock. The loop reads the same table to show the speed in effect and the cooldown's end.
- **Its end is noticed, not timed.** An idle session has no timers. The first request that asks for the tier after the time has passed is sent at the tier, and the stream says the cooldown ended.
- **Every request sent at `standard` during a cooldown says so**, with a `SpeedFallback` by the harness whose reason is `cooldown`. A request for a tier the model does not offer is sent at `standard` with the reason `not_offered`.
- **A provider's own downgrade starts no cooldown.** There is nothing to wait for: the next request asks for the tier again.

The cooldown exists because of the cache: where speeds do not share a prefix, every change of speed re-writes the cache for the whole conversation, which can cost more time and money than the tier saves. So the harness changes speed rarely and stays there for a while. Front ends are told through the `speed_fallback` and `speed_cooldown` notices ([11-rpc-protocol.md](11-rpc-protocol.md)).

**What is not a speed.** Capacity an account has bought or reserved, batch processing, and a default set at the provider for a whole project or deployment. Decision 0027 says why.

**Built in:** the types in step 3; the request field, the catalog format, the classification and the cooldown in step 5; Anthropic in step 14; OpenAI, OpenRouter and Venice.ai through `openai-chat` in step 31; the other providers with their own steps.

## Reasoning

Three things are kept apart because providers keep them apart.

| Aspect | Design |
|---|---|
| **Depth** | Each model's catalog entry lists its control: an ordered set of effort levels with a default, a token budget range, or none. Configuration names a level from that model's own set. A portable alias (`minimal`, `low`, `medium`, `high`, `max`) is mapped to the nearest level the model has, and the UI shows the level in effect. There is no numeric slider. |
| **Visibility** | A separate setting: `summary` (default, where the provider can give one), `none`. Providers differ in what they can show: a summary, nothing, or raw text on local servers under several field names. |
| **Continuity** | Opaque reasoning payloads are stored and replayed as raw blocks under the rule above ("The conversation model"). After a switch to another provider or protocol they are dropped. After a switch between models of one provider, the provider's entry decides: Anthropic's own API is sent every block and leaves out what the new model cannot read. The loop never edits them. |

Changing depth in the middle of a context epoch uses the provider's append-only mechanism where one exists. Otherwise the change takes effect at the next epoch, and the UI says so.

## Prompt caching

Caching is treated as a property of request shape, not a switch.

**The core's part: keep the prefix stable.**

- The system prompt and tool list are frozen for the context epoch.
- History is append-only (invariant 1).
- Serialisation is deterministic: stable key order, stable tool order, no timestamps or random ids in the prefix.
- A debug-build check, and a test in every provider's suite, compares each request with the previous one in the epoch and fails if the shared prefix is not byte-identical.
- **On Anthropic's current models a changed prefix is more than a cache miss.** Each reasoning block is tied to the system prompt, the tools and every message before it, and a block that is sent back after any of them changed fails a check: on some models with a 400, on others silently. An append-only history is therefore a matter of correctness there, and the same discipline that keeps the cache keeps the reasoning valid.

**The adapter's part: place markers.** The request carries neutral cache hints: "end of the stable prefix" and "end of history so far". Each adapter turns them into its provider's feature.

| Protocol | Mapping |
|---|---|
| Anthropic Messages | An explicit breakpoint after the tools and system prompt, plus the automatic moving breakpoint for the conversation. A second explicit breakpoint when one round adds more blocks than the lookback window. The one-hour lifetime is a configuration choice. As built: "The `anthropic-messages` protocol", below. |
| OpenAI Chat and the servers that speak it | Implicit caching at most of them: nothing to place. The session's cache key as `prompt_cache_key`, where the provider's entry says the server takes one. Anthropic's markers, for a model whose entry says it caches only where a request marks it: one on the system prompt and one at the top level. As built: "The `openai-chat` protocol", below. |
| OpenAI Responses | Implicit caching, with a session-derived cache key where the model uses one, or explicit breakpoints where the model supports them. |
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

Providers disagree on what "input" includes, so each adapter documents and tests its normalisation (for example, subtracting cached tokens where the provider's input count includes them). Streaming usage may be cumulative, final-only, or absent after an abort; the adapter reports what it has, and `End.usage` is authoritative. Anthropic's input count already leaves the cache out, so nothing is subtracted; its usage arrives twice, at the start of a response and with the stop reason, and a later count replaces an earlier one while a missing count leaves it standing. The `openai-chat` servers count every input token in `prompt_tokens`, so the cached ones are subtracted; their usage is the whole request's each time it is sent, and a later object replaces the earlier one whole.

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
- **Flags.** What a model's or a provider's protocol needs to know that the neutral fields do not say is a named flag on the entry. Each protocol documents the flags it reads. The ones `anthropic-messages` reads are listed with it, below.
- **Runtime discovery, on request.** `cortex models refresh` queries the provider's list endpoint and overlays limits and capabilities in the user's state directory. It never runs on its own.
- **Local servers** are asked at connect time for the loaded context size, because it differs from the model's trained size. Each has an endpoint of its own for it: "The `openai-chat` protocol", below.

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

Each protocol implements `classify` as a table from (transport error, status, provider error code, headers) to an `ErrorKind` and a retry hint. The table for each protocol is at the top of its `classify.rs`, and is tested row by row. The shared rules:

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

**A refusal can belong to a speed tier.** A classified error says whether it was a rate limit or an overload, and names the tier whose own limit or capacity turned the request away, when it was a tier's and not the model's. Only the protocol can tell, from the provider's headers and codes and the speed the request was sent at. How Anthropic's adapter tells is in "The `anthropic-messages` protocol", below.

Failures that are not HTTP responses have codes of their own: `connection_failed`, `connection_lost`, `timeout`, `idle_timeout`, `first_event_timeout`, `incomplete_stream` (the stream ended before the provider's terminal event), `malformed_stream`. All are `Transient`.

The provider's request id is attached to every error and written to the log.

**A spending cap is a hard stop.** Anthropic reports a monthly spending cap as a 429 with the same error type as a rate limit and no wait. Its detail code says what it is, and that wins: the error is reported at once, the request is never sent again, and it is never taken for a speed tier's refusal, so it starts no fallback and no cooldown.

## Transport

- **Client:** reqwest with rustls and the ring crypto provider, HTTP/2 enabled, default features off. Measured against the alternatives in [decision 0004](../decisions/0004-http-and-tls.md).
- **One client per process**, created lazily on first use, off the start-up path. Connections are pooled. Making the transport costs under a microsecond. Creating the client costs 1.06 to 1.13 ms warm and 44.6 ms on the first run after a build, when the certificate store is not in the file cache (step 5, [../performance-budgets.md](../performance-budgets.md)). `warm()` creates it ahead of the first request, from a background task.
- **Redirects are not followed.** A model endpoint has no reason to redirect, and following one would carry the credential headers to wherever it points. A redirect is a response like any other and is classified as a failure.
- **Time limits are the client's, not the transport's.** One limit runs from sending a request to the first event of its response (default 60 s), and one on silence after that (default 90 s; keep-alive pings count as bytes). Both are in the retry settings and a provider's entry can set its own, which a local server that reads a long prompt before it answers needs: the built-in entries for local servers allow ten minutes and five. Either one ending is a `Transient` failure.
- **Bytes before the first event restart the wait for it** (step 31). A server that has queued a request sends keep-alive comments until it begins: DeepSeek documents holding a request for up to ten minutes that way. Such bytes are not an event, and they do show the connection is alive. So a queue is waited out while it keeps saying so, and a connection that goes dead is given up on one first-event limit after its last byte, never later. **The wait has a ceiling:** ten first-event limits from the send, which is ten minutes at the default. A server that only ever says it is alive is not going to answer, and the one that documents its queue closes the connection itself after ten minutes.
- **Pre-warm.** When a session starts with a configured provider, the core opens a connection to it in the background, so the first request does not pay for DNS, TCP and TLS.
- **Proxies:** standard environment variables are honoured. Custom CA bundles are configurable.
- **Statelessness.** The harness sends the full history on every request and does not depend on server-side conversation state. Stateful provider features can be added later as optimisations behind the same trait.

## Protocols in scope

| Protocol | Covers | When |
|---|---|---|
| `anthropic-messages` | Anthropic. Also Anthropic-shaped endpoints on other servers. | First. **Built in step 14.** |
| `openai-chat` | OpenAI Chat Completions, OpenRouter, DeepSeek, Venice.ai, Ollama, llama.cpp, vLLM, LM Studio and other compatible servers, with capability flags for their differences. | Second, before the first release, because a second protocol is what proves the abstraction. **Built in step 31.** |
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

## The `anthropic-messages` protocol

As built in step 14 (`crates/cortex-llm/src/anthropic`). The provider's documentation was read again on 2026-11-13: the Messages API reference, and the pages on streaming, prompt caching, thinking, preserved thinking, effort, tool use, fast mode, errors and rate limits. Its fixtures are in `crates/cortex-llm/fixtures/anthropic-messages/`, and its classification table is in the module that implements it, `anthropic/classify.rs`.

**What had moved since this page and the catalog were written.**

| What | Before | Now |
|---|---|---|
| The API version | Not stated here. | `2026-10-21`, which every request names. It removed the sampling parameters (`temperature`, `top_p`, `top_k`), which the harness never sent. |
| Reasoning after a change of model | Dropped unless the catalog said the model could read it. | Sent back unchanged to Anthropic's own API, which leaves out what the model cannot read ("The conversation model", above). |
| Which models read which models' reasoning | "Not documented." | Documented, as a table. It is in the catalog as `reads_reasoning_from`. |
| System messages in the middle of a conversation | "Not documented." | Documented for all four models. The catalog's capability says so. The neutral message model has no such role yet; step 15 decides whether it needs one. |
| A spending cap | One row of the shared table: "429 with a spending, credit or quota code". | It has a detail code of its own, `enforced_spend_limit_reached`, in `error.details.error_code`. A second detail code, `ramp_limit_exceeded`, marks a 429 for usage that grew too fast; it is a rate limit like any other, with a wait. |
| Stop reasons | Six. | A seventh, `model_context_window_exceeded`: the response filled what was left of the context window. |
| A speed on a model with no fast mode | Not stated. | A 400, for `standard` as well. Found by asking the real API. |
| The catalog's prices, limits, effort levels and fast-mode offers for the four models | | All still as written. No price or limit changed. |

**The request.** `POST <base URL>/v1/messages`, with `anthropic-version`, and `anthropic-beta` only where a model's or a provider's flag asks for one. The body is written by hand, in this order, and not through a JSON value:

```text
{"model", "max_tokens", "stream", "thinking", "output_config", "speed",
 "cache_control", "tools", "system", "messages": [ … ]}
```

- **Everything before the messages is the same bytes for every request of a context epoch, and the messages only grow at the end.** So one request's body, without its two closing brackets, is the beginning of the next one's. A test checks it over a conversation recorded from the real API, and fails when earlier content is edited on purpose.
- **Reasoning.** A model whose control is a set of levels is sent adaptive thinking, with the level as the effort; no level leaves the model's default. A model whose control is a token budget is sent the budget. The visibility is always sent (`summarized` or `omitted`), so what a response shows does not rest on a model's default. A level the model does not have is refused before anything is sent.
- **Tools** carry `strict` when the variant in effect is strict schemas, and ask for their arguments to be streamed as they are written. No tool is ever forced: the current models refuse it.
- **An output schema** is the provider's structured output.
- **`max_tokens`** is the request's limit, or the model's maximum when it has none.
- **Messages.** One wire message for each neutral message. A tool's results are a user message of result blocks, in call order, right after the calls. An assistant message with nothing to send is left out, because the provider refuses empty content.
- **A tool call's id** from another provider can hold characters this one refuses (a dot, a colon). In the neutral projection each such character is written as `_`, in the call and in its result alike.
- **Not built:** a way to leave out the top-level cache marker for a server that refuses it and wants a marker on a block instead. The legacy Bedrock endpoint is one; it is not a target of the first release.

**Cache markers.**

| Hint | Marker |
|---|---|
| The stable prefix | `cache_control` on the system prompt, which covers the tools before it. On the last tool when there is no system prompt. |
| The history | `cache_control` at the top level. The provider then marks the last block of each request itself, so nothing inside the messages moves from one request to the next. |
| The history, after a long round | One more marker, on the block the request before this one ended with. |

The provider writes a cache entry only where a marker is, and when it reads it looks back 20 positions from a marker for an entry an earlier request wrote. A run of tool calls counts as one position, and so does a run of results. When one round adds more positions than that, the entry the request before wrote would be missed and the whole conversation written again, so the block it was written at is marked as well. That marker is the only one that is ever inside the messages. It is not content: the provider's cache and its check of reasoning blocks both ignore markers. So "the prefix is byte-identical" is checked on the body without it (`AnthropicMessages::stable_prefix`), which for every ordinary round is the body itself.

**Stop reasons.**

| The provider's | Neutral | |
|---|---|---|
| `end_turn` | `end` | |
| `stop_sequence` | `end` | The sequence is the stop's detail. The harness sends none. |
| `tool_use` | `tool_calls` | |
| `max_tokens` | `max_output` | |
| `model_context_window_exceeded` | `context_overflow` | The loop compacts and asks again, as for a request that did not fit. |
| `pause_turn` | `paused` | |
| `refusal` | `refusal` | The category is the stop's detail. |
| anything else | passed on under its own name | Never guessed at. |

**Usage.** `input_tokens`, `cache_read_input_tokens` and `cache_creation_input_tokens` are the three neutral input classes as they are. Cache writes are split by lifetime from `cache_creation`. The reasoning count is `output_tokens_details.thinking_tokens`. The speed that served the request is `usage.speed`, read only where the response has it.

**Errors.** The table from `error.type`, `error.details.error_code` and the status to a kind is in `anthropic/classify.rs`, and is tested row by row. Two rows stand out:

- **A prompt that is too long has no code of its own.** The provider documents it as a 400 whose message says "prompt is too long". That phrase is the one place where a message is read to classify a failure, because nothing else tells this 400 from any other and the loop answers it differently: it compacts.
- **A request that is too large in bytes** (413) is answered the same way, so it is classified the same way.

**A refusal by the fast tier.** A request at fast speed is counted against the fast tier's own token limits and its own ramp limit; the number of requests a minute is counted for the model whatever the speed. A recorded fast response shows it: it carries the model's request limit and the tier's two token limits, and no token limit of the model's. So:

| A request sent at fast is refused with | And the headers say | The refusal is |
|---|---|---|
| A rate limit or an overload | The tier has no tokens left | The tier's. |
| A rate limit or an overload | The tier has tokens left or says nothing, and the model's own limit has nothing left | The model's. |
| A rate limit or an overload | Anything else | The tier's. |
| A spending cap, a server failure, a refused request, a refused key | | Never the tier's. |

An error after a successful status has no headers; at fast speed a rate limit or an overload there is the tier's.

**Flags.**

| On | Flag | Meaning |
|---|---|---|
| a provider | `replay_reasoning` | `"all"`: every block of the provider's own messages is sent back whatever the model. Absent: only what the model can read. |
| a provider | `eager_tool_streaming` | Off for a server that does not know the field. On when absent. |
| a provider | `api_version` | Another `anthropic-version` than the built-in one. |
| a provider | `beta` | Names for the `anthropic-beta` header, sent with every request. |
| a model | `speed_beta` | The beta name a request must carry to name a speed on this model: the models whose fast mode is a preview. None of the four in the catalog needs it. |

## The `openai-chat` protocol

As built in step 31 (`crates/cortex-llm/src/openai_chat`): OpenAI's Chat Completions API, streamed, and the servers that speak it. Its fixtures are in `crates/cortex-llm/fixtures/openai-chat/`, and its classification table is in the module that implements it, `openai_chat/classify.rs`.

**What was read, and how each server was checked.** Every page was read on 2026-11-13.

| Server | What was read | Checked |
|---|---|---|
| OpenAI | developers.openai.com/api: the reference for creating a chat completion and for its stream; the guides to error codes, rate limits, fast mode, flex processing and prompt caching; the pricing page; the pages of GPT-6 Luna and GPT-6.1 Terra. (Its documentation moved there from platform.openai.com.) | Documentation only: there was no key. |
| OpenRouter | openrouter.ai/docs: the API overview, parameters, streaming, errors and debugging, limits, the credits endpoint, and the guides to service tiers, reasoning tokens and prompt caching. Its public model list and the list of a model's endpoints (`/api/v1/models`). | **Against the real API**: 19 recorded fixtures. |
| DeepSeek | api-docs.deepseek.com: the first page, the chat completion reference, thinking mode, the context cache, error codes, rate limits, pricing and the balance endpoint. | **Against the real API**: 18 recorded fixtures. |
| Venice.ai | docs.venice.ai: the chat completion reference, error codes, rate limiting, the rate-limits endpoint of a key, reasoning models and pricing. Its public model list (`/api/v1/models`). | **Against the real API**: 16 recorded fixtures. |
| Ollama | docs.ollama.com: OpenAI compatibility, the list of running models (`/api/ps`), context length and thinking. The response types in its source (`openai/openai.go`), for the names of members its pages do not give. | Documentation only: no local server was installed. |
| llama.cpp | The server's README (`tools/server/README.md` in github.com/ggml-org/llama.cpp), and the error types in its source (`server-common.cpp`). | Documentation only. |
| vLLM | docs.vllm.ai: reasoning outputs and online serving. Its source for the model list (`entrypoints/openai/models/serving.py`). | Documentation only. |
| LM Studio | lmstudio.ai/docs/developer: the OpenAI-compatible chat endpoint, tool use, the list of models (`/api/v1/models`) and the API changelog. | Documentation only. |

A fixture from a server that was not there to record from is written by hand from that server's documented format, and its note says so in its first line, so that a later recording can replace it.

**What reading and asking showed that this page did not know.**

| What | Where it came from | What was built for it |
|---|---|---|
| **DeepSeek wants its reasoning back.** Once a conversation has tools, every assistant message sent to it must carry `reasoning_content`, or the answer is a 400. | Its thinking-mode guide, and the real API: a history that began on an Anthropic model was refused, and accepted with the member empty. | An assistant message this provider produced goes back raw, reasoning included. The provider's flag `reasoning_passback` names the member, and a message with no reasoning to send carries it empty. |
| **OpenAI's newest models take tools on Chat Completions only at the reasoning level `none`, or not at all.** GPT-6 Luna's page: "Chat Completions supports function calling only when reasoning_effort is none". GPT-6.1 Terra's: "tool calling requires the Responses API". | Their model pages. | The model flag `tools_reasoning`. OpenAI itself has one catalog entry here; its real path is the `openai-responses` protocol. |
| **OpenAI's priority tier is now "fast mode"** (renamed on 2026-07-30). The request takes `priority` or `fast`; the response answers `priority`; a request it cannot serve fast is served at standard, billed at standard, and answered `default`. | Its fast-mode guide. | `service_tier` is written `priority` for the speed `fast`, and both words are read as `fast`. |
| **OpenRouter has service tiers too**, with the tier that served the request at the top level of the response. | Its service-tiers guide, and the real API: a request for `priority` was served and answered `priority`, at twice the price. | The same code as for OpenAI. |
| **Venice.ai's responses say `service_tier: "priority"` on requests it bills at its standard price.** It is its upstream host's word, passed on. | The real API, and the cost each response states. | A response's `service_tier` is read only for a model that is asked for its tier with that member. |
| **Venice.ai puts a system prompt of its own before the caller's**, about a thousand tokens, unless told not to. | The real API: a 19-token request was billed 1,101. Its reference documents the switch. | The provider flag `extra_body`, which adds members to every request. Venice.ai's entry sets `venice_parameters.include_venice_system_prompt` to false. |
| **Venice.ai sells a speed as another model** (`…-fast` beside the model), at its own price and with its own limits. | Its pricing page and model list. Sent to the real API. | The model flag `speed_model_<tier>`: the request names that model's id. |
| **A response can fail after it began at any of these servers**, each in its own form: an `error` at the top of a chunk (OpenRouter documents it; Venice.ai did it, recorded), or a stop reason that is a failure (DeepSeek's `insufficient_system_resource`). | Their documentation, and one real recording. | All are errors, never an end. |
| **OpenRouter's key endpoint answers with the key's label, which is a shortened form of the key.** | The real API: the recorder's own guard refused to print the answer. | What a usage endpoint answers is read and dropped. Nothing but its figures is kept. |
| **DeepSeek's price depends on the time of day**: off-peak is half of peak. | Its pricing page. | Not solved. The catalog holds one rate card for a speed, so the peak price is entered and the entry says so. [Decision 0027](../decisions/0027-speed-tiers.md) lists this case under "Revisit when". |
| **vLLM renamed its reasoning member** from `reasoning_content` to `reasoning`, and warns that a client reading the old name gets nothing. | Its reasoning-outputs page. | The decoder reads either name. |
| **A local server reports the context size it loaded a model with**, each at an endpoint of its own. | Their documentation. | The context probes, below. |

**The request.** `POST <base URL>/chat/completions`. The body is written by hand, in this order:

```text
{"model", "stream", "stream_options", "max_tokens", "reasoning_effort",
 "service_tier", "response_format", "prompt_cache_key", "cache_control",
 the provider's own members, "tools", "messages": [ the system prompt, … ]}
```

- **Everything before the messages is the same bytes for every request of a context epoch**, the system prompt is the first message, and the messages only grow at the end. One request's body, without its two closing brackets, is the beginning of the next one's (`OpenAiChat::stable_prefix`). A test checks it over the conversations recorded from all three servers, and fails when earlier content is edited on purpose.
- **The output limit** goes under the name the provider's entry gives (`max_tokens`, which every server knows, or `max_completion_tokens`). A request with no limit sends none: a server that counts the limit against the context window refuses one it cannot honour, so the model's maximum is not sent as a default.
- **Reasoning.** A level the model has is sent as `reasoning_effort`, or as `reasoning.effort` where the provider's entry asks for the object form (OpenRouter). A token budget is `reasoning.max_tokens`, and a server without the object form has no way to take one. No level leaves the model's default. A level the model does not have is refused before anything is sent.
- **Tools** carry `strict` when the variant in effect is strict schemas. `tool_choice` and `parallel_tool_calls` are never sent.
- **Messages.** The system prompt under the role the provider's entry names (`system`, or `developer` for OpenAI). A user message as a string, or as a list of parts when it holds more than one piece of text or an image. A tool's results as one `tool` message each, in call order, right after the calls.
- **A failed tool result** begins with `Tool error: `: the dialect has no flag for it.
- **An image a tool returned** cannot be in a `tool` message, which holds text. It follows in a `user` message after the results, which says which call it is from.
- **Not sent:** sampling parameters, `n`, `store`, `user`, and anything that names us. OpenRouter's optional headers that identify an application are not sent.

**A response has no blocks.** The dialect streams one assistant message as additions to its members. The decoder cuts the message into blocks, and the stored block of a part is **the piece of the assistant message that part stands for**, under the message's own member names:

| Part | Stored block |
|---|---|
| text | `{"content":"…"}` |
| reasoning | `{"reasoning_content":"…"}`, under the name the server used, with `reasoning_details` where the server sent them |
| a tool call | `{"tool_calls":[{"id":"…","type":"function","function":{"name":"…","arguments":"…"}}]}`, with whatever else the server put on the call |

Laid over one another in order (text appended to text, a list extended by a list), a message's blocks are the message the server would have sent without streaming. That is how a message is sent back raw. No stored text is copied into a request as it is: a string is read and written again, and any other value is written only after it was read as one whole JSON value. **And a part's stored block gives the message only the members of its own kind:** a text part its `content`, a reasoning part the reasoning members, a tool call the one item of `tool_calls` that has its own id. Whatever else a block holds is left out, so a damaged or doctored session log cannot give a message a role, or a text part a tool call.

**Replay** follows the rule of "The conversation model". A message this provider produced through this protocol, for a model that can read it, goes back raw: reasoning, signed pieces and all. Every other message goes as its neutral projection. Recorded from the real servers: DeepSeek and Venice.ai took their own `reasoning_content` back, a model of Anthropic's through OpenRouter took its signed `reasoning_details` back, all three took a response that an Anthropic model had produced as its projection, and DeepSeek took a conversation that went from one of its models to the other and back, each sent the other's responses without their reasoning.

A reasoning part gives the message `content` only at a server that writes its reasoning inside the text (`reasoning_tags`). And text that one response of this protocol streamed is one text again when its reasoning is dropped, with nothing put between its pieces; another protocol's separate blocks of text stay apart.

**Tool calls are keyed by index.** The fragments of several calls may arrive interleaved, each naming its call by `index`, and the order of arrival says nothing about which call a fragment belongs to. So a call is a block for each index, and every call stays open until the stop reason arrives. The cost: a call that only reads cannot start early while a later call of the same response is still streaming.

- **A call is announced, and its block begins, once its name is known.** A server that gives a call no id by then gets one made from the call's place in the response and, where the response has an id, the end of that, so that a result can name it and another response's calls do not get the same. It is kept short: some servers refuse a call id over 40 characters.
- **A server that gives no index at all** sends each call whole, or one call at a time: an item with no index, no id and no name is more of the call before it, and any other is the next call.
- **Another id under an index already in use is another call.** Some servers number every call of a response `0` and tell them apart by id alone; joined by index only, the second call's arguments would be appended to the first's.
- **A call that never gets a name is left out,** and the response stops as `malformed_tool_call`. It cannot be run, and the servers refuse a call with no name when it is sent back, which would fail every later request of the session.

**Reasoning, under its names.**

| Where a server puts it | Servers | Read when |
|---|---|---|
| `delta.reasoning_content` | DeepSeek, Venice.ai, llama.cpp, LM Studio | Always, unless the provider's `reasoning_field` names another member. |
| `delta.reasoning` | OpenRouter, Ollama, vLLM, LM Studio for some models | When the delta has no `reasoning_content`, unless `reasoning_field` names another member. |
| `delta.reasoning_details` | OpenRouter, Venice.ai | Always: its pieces are joined and kept for sending back. Its text is shown only when the delta has no reasoning text of its own. |
| inside `delta.content`, between `<think>` and `</think>` | a local server that does not separate it | When the provider's or the model's `reasoning_tags` is on. |

One delta is read under one name, so reasoning a server sends twice is never shown twice. A request that asks for no visible reasoning still gets the reasoning block and its stored block, without text.

**Stop reasons.**

| The server's | Neutral | |
|---|---|---|
| `stop` | `end` | `tool_calls` when the response holds a tool call: some servers end such a response with `stop`. `refusal` when the text arrived in `delta.refusal`. |
| `tool_calls`, `function_call` | `tool_calls` | When the response holds no call after all: `max_output` if the server's own word for the stop (`native_finish_reason`) is `length`, and `end` otherwise. |
| `length` | `max_output` | |
| `content_filter` | `content_filter` | |
| `stop`, `tool_calls`, `function_call`, after a call that never got a name | `malformed_tool_call` | |
| `error`, `insufficient_system_resource` | none: an error | The response failed. |
| an empty text | none | Some servers write `""` where others write null, on every chunk but the last. |
| anything else | passed on under its own name | Never guessed at. |

The response is complete at `[DONE]`, not at the stop reason: the usage comes in between. A server that closes the stream without `[DONE]` after a stop reason has completed its response too.

**Usage.** The dialect's `prompt_tokens` counts every input token, so the cached ones are taken out: `cache_read` is `prompt_tokens_details.cached_tokens` (or DeepSeek's `prompt_cache_hit_tokens`), `cache_write` is `prompt_tokens_details.cache_write_tokens` (or Venice.ai's `cache_creation_input_tokens`), and `input` is what is left. The reasoning count is `completion_tokens_details.reasoning_tokens`. A usage object is the whole request's count so far. Some servers send one in every chunk and OpenRouter repeats the stop reason with it, so **a later object replaces the earlier one, and nothing is ever added up.** A cost the provider states is read from where it puts it: `usage.cost` (OpenRouter), or `cost.usd` beside the usage (Venice.ai). Venice.ai's responses also write the count of cache reads beside the details (`cache_read_input_tokens`, recorded), and a count of writes is read from either place. **What a failed response used is counted:** a usage that comes in the chunk that fails is passed on before the error.

**Speed tiers.**

| Server | How a tier is asked for | How the response says what ran | When the tier cannot be served |
|---|---|---|---|
| OpenAI | `service_tier`: `default`, `priority` for fast, `flex` | `service_tier` at the top level | Fast: served at standard, answered `default`. Flex: a 429. |
| OpenRouter | The same | The same | Fast: served at standard. Flex: a capacity error. |
| Venice.ai | Another model's id, named by the model flag `speed_model_<tier>`. The variant gives less output than its base model (32,768 tokens against 262,144), so `speed_max_output_<tier>` holds a request at the tier to the variant's limit. | Nothing: the request ran where it was sent | The fast variant refuses (429 or 503). It has limits of its own, so a refusal at fast is the tier's. |
| DeepSeek, the local servers | No tier | | |

A model that offers tiers through `service_tier` is always told which one, `default` included. A model that offers none is told nothing, and a speed asked of it is refused before anything is sent.

**Errors.** The servers bend the dialect's `{"error": {"message", "type", "code"}}` each their own way: the status as the code (OpenRouter, llama.cpp), typed metadata (OpenRouter), a sentence with a code beside it in capitals (Venice.ai). Every name a body gives is read, most exact first, and the table in `openai_chat/classify.rs` decides; it is tested row by row with each server's own bodies. These rows stand out:

- **A code wins over its status.** OpenAI sends exhausted credit and a spending cap as a 429. They are never retried.
- **A 403 is not always a credential.** OpenRouter answers a moderation refusal and a spent budget with one. The first is a refused request, known by a typed name or by the `reasons` and `flagged_input` it carries, and the second a quota.
- **No part of a key is kept.** Some servers repeat the key they were sent in the message of a failure, masked to its last characters (DeepSeek's recorded 401 does: `Your api key: ****0000`). A message goes to logs and to the screen, so a word is taken out of the message before it is kept when it has a run of asterisks or bullets in it, or begins as these providers' keys do (`sk-`). What a server masks some other way is not recognised.
- **A context that is too long has a code at some servers and only a sentence at others.** "maximum context length" is the dialect's long-standing wording, which the servers without a code reuse. It is the one place a message is read here.

The rate-limit headers are `x-ratelimit-{limit,remaining,reset}-{requests,tokens}`, with the reset written as a duration (OpenAI), a time in milliseconds (Venice.ai) or a count of seconds. A 429 with no `retry-after` waits until the reset of a limit that has nothing left.

**Flags.** A flag this protocol does not know, or a value a flag cannot have, is refused when a request is built: a misspelt `usage_in_stream`, or `replay_reasoning = "All"`, must not silently be the default.

| On | Flag | Meaning |
|---|---|---|
| a provider | `max_tokens_field` | The member that limits the output: `max_tokens` (the default) or `max_completion_tokens`. |
| a provider | `usage_in_stream` | Off for a server that refuses `stream_options`. On when absent. |
| a provider | `system_role` | `system` (the default) or `developer`. |
| a provider | `reasoning_param` | `reasoning_effort` (the default) or `reasoning`, the object form. |
| a provider | `reasoning_field` | The member of a delta that carries reasoning text, where it is neither of the usual two. |
| a provider or a model | `reasoning_tags` | Reasoning is inside the text, between `<think>` tags. |
| a provider | `reasoning_passback` | The member every assistant message must carry when it is sent back, empty when there is no reasoning to send. |
| a provider | `replay_reasoning` | `"all"`: reasoning goes back whatever the model. `"readable"`, or absent: only to a model that can read it. |
| a provider | `prompt_cache_key` | The server takes one. Off when absent: a strict server refuses a member it does not know. |
| a provider | `extra_body` | Members added to every request, as the text of one JSON object. It cannot name a member the builder writes. |
| a provider | `usage_endpoint` | The format of its usage or balance endpoint. |
| a provider | `context_probe` | How its loaded context size is asked for. |
| a model | `cache_control` | The model caches only where a request marks it, with Anthropic's markers. |
| a model | `speed_model_<tier>` | The model id that serves a tier. |
| a model | `speed_max_output_<tier>` | The most output tokens that model gives, where it is less than the model's own. |
| a model | `tools_reasoning` | The one reasoning level at which the model takes tools on this protocol. |

**The built-in providers** (`openai_chat::providers`). A user's own entry for any other compatible server has the same form.

| Provider | Base URL | Key looked for in | What its entry sets |
|---|---|---|---|
| `openai` | `https://api.openai.com/v1` | `OPENAI_API_KEY` | `max_completion_tokens`, the `developer` role, a prompt cache key |
| `openrouter` | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` | the `reasoning` object, a usage endpoint |
| `deepseek` | `https://api.deepseek.com` | `DEEPSEEK_API_KEY` | `reasoning_content` on every assistant message, a usage endpoint |
| `venice` | `https://api.venice.ai/api/v1` | `VENICE_API_KEY` | `max_completion_tokens`, a prompt cache key, its own system prompt turned off, a usage endpoint |
| `ollama` | `http://localhost:11434/v1` | none | a context probe, the local time limits |
| `llamacpp` | `http://localhost:9931/v1` | none | a context probe, the local time limits |
| `vllm` | `http://localhost:8000/v1` | none | `max_completion_tokens`, a context probe, the local time limits |
| `lmstudio` | `http://localhost:1234/v1` | none | a context probe, the local time limits |

The ports are the ones each server's documentation gives as its default. llama.cpp's is 9931 in its current README; its examples still show 8080.

**The time limits of a local server.** A hosted server answers within seconds, or queues a request and says so. A local server does neither: before it sends anything it may load the model from disk, and it reads the whole prompt, at a few hundred tokens a second on a graphics card and a few dozen on a processor. A request that is given up on is read again from the start when it is retried, so a limit that is too short does not fail fast: it never succeeds.

- **The first event may take ten minutes.** That is a model load of a minute or two and a prompt of 100,000 tokens read at 200 tokens a second. It is also the default time limit of the dialect's own client libraries, so it is what these servers are run against.
- **Silence after that may last five minutes.** A server that parses tool calls may hold back a call's arguments until they are complete, and a file written by a slow model is minutes of output.

**A hosted server that queues a request keeps its ordinary limits.** DeepSeek holds a request for up to ten minutes when it is busy, and sends a keep-alive comment while it does. The client lets such bytes restart the wait for the first event ("Transport", above), so the queue is waited out, a connection that goes silent is still given up on after 60 seconds, and a server that only ever says it is alive after ten minutes.

**A local server's loaded context size** (`openai_chat::context`). A local model's window is what the server was started with, not what the model was trained for: a model trained for 131,072 tokens is commonly loaded with 4,096. None of these servers reports it through the dialect, so each is asked at an endpoint of its own. The module builds the request and reads the answer; sending it when a session connects is the core's work.

| Probe | Endpoint | Where the size is |
|---|---|---|
| `ollama-ps` | `GET <origin>/api/ps` | `models[].context_length`, for the running model of that name (a name without a tag is the tag `latest`) |
| `llamacpp-props` | `GET <origin>/props` | `default_generation_settings.n_ctx`: the size of one slot. (`/v1/models` has `n_ctx_train`, the trained size, which is not it.) |
| `vllm-models` | `GET <base>/models` | `data[].max_model_len`, for the model of that id |
| `lmstudio-models` | `GET <origin>/api/v1/models` | `models[].loaded_instances[].config.context_length`. (`max_context_length` is the most the model supports, which is not it.) |

`<origin>` is the base URL without its final `/v1`. A model that is not loaded yet has no size, and the catalog's figure stands.

**The `/usage` refresh** (`openai_chat::account`; [18-usage.md](18-usage.md), "The opt-in refresh"). The module builds a `GET` with no body and reads the answer; whether it is made at all is the user's setting, and making it is step 16's.

| Provider | Endpoint | What it reports |
|---|---|---|
| `openrouter` | `GET <base>/key` | What the key has spent, the key's own credit limit and what is left of it, and today's free-model requests. |
| `deepseek` | `GET <base>/user/balance` | The balance in each currency, and whether it is enough to make calls. |
| `venice` | `GET <base>/api_keys/rate_limits` | The balance in each of its currencies, whether the key may make calls, and each model's limits by the minute and by the day. |
| `openai` | none | Its usage API takes an admin key, which is not the key a user gives for model calls. |
| the local servers | none | |

- **All three report money**: an amount in a currency (US dollars, yuan, Venice.ai's own unit). The protocol's `LimitWindow` counts requests and tokens and cannot say that. So what is read is returned as a type of this crate (`AccountUsage`), and how a balance reaches `/usage` is step 16's to decide ([outstanding/a-balance-in-the-usage-view.md](../outstanding/a-balance-in-the-usage-view.md)).
- **The answer is read and dropped.** OpenRouter's holds a shortened form of the key. `AccountUsage` has no member for the provider's own text, and must not gain one.

**What the second protocol changed in the shared parts.**

| Part | Change |
|---|---|
| The neutral types in `cortex-protocol` | None. |
| The traits of `cortex-llm` (`Protocol`, `StreamDecoder`, `Transport`, `ModelClient`, `AuthMethod`) | None. The abstraction held: the second protocol is one module beside the first. |
| The client | One rule: bytes that arrive before the first event restart the wait for it, up to ten first-event limits in all ("Transport"). It holds for every protocol: an Anthropic `ping` that arrives before the first event restarts the wait too. |
| The JSON writer | Moved from the Anthropic module to the crate (`cortex_llm::json`, private), with the reader of an object's members in order. Both protocols write their bodies with it. Anthropic's snapshots did not change. |
| The catalog | Entries for the models the fixtures were recorded from, and one for OpenAI. No change to its format. |

**For the steps that add a protocol** (35 and 36).

- **A protocol is a module with five files** and a table of providers: `request`, `decode`, `classify`, and for this one `account` and `context`. Start from whichever of the two existing ones is nearer in shape.
- **Where a provider's stream has no blocks,** store for each part the piece of the provider's own message it stands for, and send a message back by laying the pieces over one another. Do not invent a block format.
- **Record first.** Four of the facts above (DeepSeek's refusal of a projected message, Venice.ai's system prompt, its `service_tier`, the key in OpenRouter's answer) are in no documentation, or were only believed after the real server showed them.
- **OpenAI's own models belong to step 35.** Tool calling with reasoning is on the Responses API. The entry here for GPT-6 Luna, its `tools_reasoning` flag and its seven hand-written fixtures are what could be done without a key; step 35 records from the real API and should replace them.
- **What was not recorded,** and why: a request the server refuses as invalid, from OpenRouter and Venice.ai (both answered the two invalid requests that were tried with a 200; DeepSeek's refusal is recorded); a refusal of a speed tier (it cannot be provoked; Venice.ai's is written by hand from its documented error); a server-side tool block (the dialect has none).
- **A reasoning payload that must go back is the rule now, not the exception:** Anthropic, DeepSeek, OpenRouter's signed pieces. Google's thought signatures arrive on tool calls, and a tool call's unknown members already go back with it here.
