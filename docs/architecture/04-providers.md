# Providers

`cortex-llm` gives the agent loop one way to talk to any model. The design goal is an abstraction that loses nothing a provider needs to get back, while keeping every mapping a pure, fixture-testable function.

The provider facts on this page were read from the providers' public documentation in November 2026. The research notes list the sources. They change often: each provider step in the build plan starts by re-reading the current docs.

## Three separate concepts

Keeping these apart is the central decision. The same model can be reached through different wire dialects on different endpoints, each with different fidelity.

| Concept | What it is | Examples |
|---|---|---|
| **Protocol** | A wire dialect: how to build a request, decode a stream and classify an error. Code. | `anthropic-messages`, `openai-chat`, `openai-responses`, `gemini-generate` |
| **Provider** | An endpoint that speaks a protocol: base URL, credential source, extra headers, capability flags. Data, from the built-in table or user configuration. | `anthropic`, `openai`, `openrouter`, `ollama`, `google`, or a user-defined one |
| **Model** | A catalog entry: limits, prices, reasoning controls, capabilities. Data. | one row per provider and model id |

A user adds an OpenAI-compatible server by writing a provider entry in configuration. No code changes. Quirks are capability flags on the provider or model entry (for example `reasoning_field = "reasoning_content"`, `usage_in_stream = false`, `tool_choice = false`), not branches on model-name substrings.

## Traits

Sketches, to fix the boundaries. Exact signatures are settled in the provider-abstraction build step.

```rust
/// A wire dialect. Stateless and pure: no I/O.
pub trait Protocol: Send + Sync {
    /// Build the HTTP request for one model call.
    fn build(&self, req: &ModelRequest, model: &ModelInfo, provider: &ProviderInfo)
        -> Result<HttpRequest, BuildError>;
    /// A fresh decoder for one response stream.
    fn decoder(&self, model: &ModelInfo, provider: &ProviderInfo) -> Box<dyn StreamDecoder>;
    /// Turn a failed response into a typed error.
    fn classify(&self, status: u16, headers: &Headers, body: &[u8]) -> ProviderError;
}

/// Incremental decoder: SSE events in, neutral model events out.
pub trait StreamDecoder: Send {
    fn feed(&mut self, event: SseEvent<'_>, out: &mut Vec<ModelEvent>) -> Result<(), DecodeError>;
    fn finish(&mut self, out: &mut Vec<ModelEvent>) -> Result<(), DecodeError>;
}

/// Sends a request and streams the body. The only part that does I/O.
pub trait Transport: Send + Sync {
    fn send(&self, req: HttpRequest, cancel: CancelToken) -> BoxFuture<'_, Result<HttpStream, TransportError>>;
}

/// What the agent loop sees.
pub trait ModelClient: Send + Sync {
    fn stream(&self, req: ModelRequest, cancel: CancelToken) -> BoxStream<'_, ModelEvent>;
}
```

- `build`, `feed` and `classify` are tested from files: a request snapshot per scenario, and recorded response streams replayed with every possible chunk boundary.
- `Transport` has two implementations: the real one (reqwest) and a scripted one in the test kit.
- `ModelClient` is implemented once, generically, by combining a protocol, a transport and the retry layer. The scripted provider in the test kit implements `ModelClient` directly, below nothing, for loop tests.

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

**Raw blocks are kept beside the neutral view.** Every assistant message stores, for each part, the provider's own JSON for that block, byte for byte, with the protocol, provider, model and account scope that produced it. Replay follows one rule:

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
    Usage(UsageUpdate),                                  // may arrive more than once
    End { stop: StopReason, raw_stop: String, usage: Usage },
    Error(ProviderError),                                // including errors after HTTP 200
}
```

Rules for decoders:

- Blocks are keyed by index. Indexes need not be contiguous or arrive in order.
- Unknown provider event types are skipped, never an error.
- An error event after a successful status is an `Error`, classified like any other.
- The decoder reads until the provider's terminal event. Some providers deliver required data (a signature, the usage) in the last chunk.

**SSE parsing** is a small parser of our own over the byte stream: event name, data lines, comments, blank-line terminator, all three line endings, no reconnection logic. The available crates are unmaintained and do more than is wanted here. It is tested with every chunk split.

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

Providers disagree on what "input" includes, so each adapter documents and tests its normalisation (for example, subtracting cached tokens where the provider's input count includes them). Streaming usage may be cumulative, final-only, or absent after an abort; the adapter reports what it has, and `End.usage` is authoritative.

**Cost** is computed when the request is recorded: tokens × the catalog prices in force, with context-size tiers and lifetime-class multipliers. The entry stores the cost and the catalog version, so later price changes do not rewrite history. A provider that reports cost itself overrides the calculation. A model with no price data shows tokens only.

**Out-of-band usage** (compaction, title generation) is recorded as separate usage entries and included in session totals.

**Token estimates** for budgeting use the last reported usage plus a bytes-based estimate of what was added since. Provider count endpoints are used only near the limit, where precision matters. There is no local tokenizer.

## Model catalog

- **Embedded.** The binary carries a compact catalog generated at build time from `catalog/models.toml` in the repo. No network at build time or at startup.
- **Maintained by script.** `xtask catalog update` drafts changes from the providers' model-list endpoints and from models.dev (an MIT-licensed community catalog). A maintainer reviews the diff. The checked-in file is ours and is the source of truth.
- **Per entry:** id, display name, protocol, context window, maximum output, prices (input, output, cache read, cache write by class, tiers), reasoning control, which models' reasoning it can read, modalities, capability flags (strict tools, mid-conversation system messages, structured output, minimum cacheable size).
- **User overlay.** Configuration can add or override entries. This is how local and private models are described.
- **Runtime discovery, on request.** `cortex models refresh` queries the provider's list endpoint and overlays limits and capabilities in the user's state directory. It never runs on its own.
- **Local servers** are asked at connect time for the loaded context size, because it differs from the model's trained size.

## Credentials

- **Sources, in order:** a command-line flag, the provider's environment variable (`ANTHROPIC_API_KEY` and so on), then stored credentials. The active source is shown by `cortex auth status` and never the value.
- **Storage:** the OS keychain where enabled, otherwise a file in the configuration directory with owner-only permissions. Details and the per-OS choice are in [08-configuration.md](08-configuration.md).
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

The provider's request id is attached to every error and written to the log.

## Transport

- **Client:** reqwest with rustls and the ring crypto provider, HTTP/2 enabled, default features off. Measured against the alternatives in [decision 0004](../decisions/0004-http-and-tls.md).
- **One client per process**, created lazily on first use, off the start-up path. Connections are pooled.
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
