//! Decoding a response stream.
//!
//! # The stream
//!
//! Every event is a chunk of one assistant message, as JSON, and the last
//! is the literal `[DONE]`.
//!
//! | In a chunk | What the decoder does |
//! |---|---|
//! | `id`, `model` | The response began. Taken from the first chunk. |
//! | `choices[0].delta.content` | More text. |
//! | `choices[0].delta.<reasoning member>` | More visible reasoning. The member's name differs by server (below). |
//! | `choices[0].delta.reasoning_details` | Reasoning in pieces, some of them opaque. Kept for sending back. |
//! | `choices[0].delta.refusal` | The text of a refusal. |
//! | `choices[0].delta.tool_calls` | A tool call's name, or a fragment of its arguments, **keyed by the call's `index`**. |
//! | `choices[0].finish_reason` | Why the response stopped. Every open block is complete. |
//! | `usage` | The usage, which is the whole request's. A later one replaces an earlier one. |
//! | `service_tier` | The speed tier that served the request, **for a model that is asked for its tier with `service_tier`** and for no other. |
//! | `cost` | What the request cost, at a server that states it beside the usage. |
//! | `error` | The request failed after a successful status. Classified by the same table as a failed response. |
//! | anything else | Skipped. |
//!
//! The response is complete at `[DONE]`, and not at the stop reason: the
//! usage comes in between. A server that closes the stream without `[DONE]`
//! after it gave a stop reason has also completed its response.
//!
//! Only the first choice is read. The harness never asks for more.
//!
//! **`service_tier` is not read from every server.** Venice.ai passes on
//! the word its upstream host uses (`priority`, on a request it bills at
//! its standard price), and a server that never heard of tiers may send
//! anything. So the member is read only where the request could have named
//! a tier with it: a model whose catalog entry offers tiers, and does not
//! say they are separate models. Anywhere else the request ran at the
//! speed it was sent at.
//!
//! # Blocks
//!
//! The dialect has none: it streams additions to the members of one
//! message. The decoder cuts the message into blocks, numbered in the order
//! they begin.
//!
//! - **Text and reasoning** are one block for each unbroken run. Reasoning
//!   that arrives while text is open ends the text and begins a reasoning
//!   block, and the other way round.
//! - **A tool call** is one block for each `index` the server uses. The
//!   fragments of several calls may arrive interleaved, each naming its
//!   call by `index`, and **the order of arrival says nothing about which
//!   call a fragment belongs to**. So every call stays open until the stop
//!   reason arrives.
//!   - A call is announced, and its block begins, once its name is known:
//!     with its first fragment, or a later one. Its id is whatever had
//!     arrived by then. A server that gives a call no id by then gets one
//!     made from the response's id and the call's place in the response,
//!     so that a result can name it; an id that arrives after that is not
//!     taken, because the call has been announced under the other.
//!   - A fragment that brings **another id under an index already in use**
//!     begins another call: some servers number every call of a response
//!     `0` and tell them apart by id alone.
//!   - A server that gives **no index at all** sends each call whole, or
//!     one call at a time: an item with no index, no id and no name is
//!     more of the call before it, and any other is the next call.
//!   - A call that never gets a name is not a call anybody can run or send
//!     back. It is left out, and the response stops as
//!     `malformed_tool_call`.
//!
//! **The stored block** of each is the piece of the assistant message it
//! stands for ([the module](super)): what the server would have sent for
//! it without streaming.
//!
//! # Reasoning, under its names
//!
//! | Where a server puts it | Servers | Read when |
//! |---|---|---|
//! | `delta.reasoning_content` | DeepSeek, Venice.ai, llama.cpp, LM Studio | Always, unless the provider's `reasoning_field` names another member. |
//! | `delta.reasoning` | OpenRouter, Ollama, vLLM, LM Studio for some models | When the delta has no `reasoning_content`, unless `reasoning_field` names another member. |
//! | `delta.reasoning_details` | OpenRouter, Venice.ai | Always. Its text is shown only when the delta has no reasoning text of its own. |
//! | inside `delta.content`, between `<think>` and `</think>` | a local server that does not separate it | When the provider's or the model's `reasoning_tags` is on. |
//!
//! A server may send the same reasoning under two names at once. One delta
//! is read under one name, so it is never shown twice.
//!
//! When the request asks for no visible reasoning, a reasoning block still
//! begins and ends, with its stored block, and carries no text: a server
//! that wants its reasoning back still gets it.
//!
//! # Usage
//!
//! The dialect's `prompt_tokens` counts every input token, cached or not,
//! so the cached ones are taken out:
//!
//! | Neutral | From |
//! |---|---|
//! | `cache_read` | `prompt_tokens_details.cached_tokens`, or `prompt_cache_hit_tokens` (DeepSeek), or `cache_read_input_tokens` beside the details (Venice.ai, recorded) |
//! | `cache_write` | `prompt_tokens_details.cache_write_tokens` (OpenAI, OpenRouter), or `cache_creation_input_tokens`, inside the details as Venice.ai's reference writes it or beside them as its responses write the count of reads |
//! | `input` | `prompt_tokens` less the two above |
//! | `output` | `completion_tokens` |
//! | `reasoning` | `completion_tokens_details.reasoning_tokens` |
//! | the cost the provider states | `usage.cost`, in US dollars (OpenRouter), or the chunk's `cost.usd` (Venice.ai) |
//!
//! A usage object is the count for the whole request so far. Some servers
//! send one in every chunk and some repeat the last one, so a later object
//! **replaces** the earlier one, and nothing is ever added up.

use std::borrow::Cow;
use std::collections::BTreeMap;

use cortex_protocol::usage::TokenUsage;
use cortex_protocol::{RawJson, Speed, StopReason};
use serde::Deserialize;
use serde_json::value::RawValue;

use super::{classify, flags};
use crate::error::DecodeError;
use crate::event::{BlockKind, EventSink, ReportedUsage};
use crate::json::{Array, Object, ordered_members};
use crate::protocol::{ProviderInfo, StreamDecoder};
use crate::request::{ModelRequest, ReasoningVisibility};
use crate::sse::{DEFAULT_EVENT, SseEvent};

/// The data of the event that ends a stream.
const DONE: &str = "[DONE]";

/// The members a delta's reasoning text is read from when the provider's
/// entry names none, in the order they are tried.
const REASONING_FIELDS: &[&str] = &["reasoning_content", "reasoning"];

/// The tags a server that does not separate reasoning writes around it.
const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// The most text that is held back at the start of a response while it
/// could still be white space before the opening tag.
const LEADING_SPACE: usize = 256;

/// The members of a piece of `reasoning_details` that arrive in fragments
/// to be joined. Any other member is whole each time it is sent.
const DETAIL_TEXT: &[&str] = &["text", "summary", "data"];

/// One chunk: the members the decoder reads.
#[derive(Deserialize)]
struct Chunk<'a> {
    #[serde(borrow, default)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    choices: Option<Vec<Choice<'a>>>,
    #[serde(borrow, default)]
    usage: Option<&'a RawValue>,
    #[serde(borrow, default)]
    service_tier: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    cost: Option<&'a RawValue>,
    #[serde(borrow, default)]
    error: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Choice<'a> {
    #[serde(default)]
    index: Option<u32>,
    #[serde(borrow, default)]
    delta: Option<&'a RawValue>,
    #[serde(borrow, default)]
    finish_reason: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    native_finish_reason: Option<Cow<'a, str>>,
}

/// One item of `delta.tool_calls`.
#[derive(Deserialize)]
struct CallDelta<'a> {
    #[serde(default)]
    index: Option<u32>,
    #[serde(borrow, default)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    function: Option<FunctionDelta<'a>>,
}

#[derive(Deserialize)]
struct FunctionDelta<'a> {
    #[serde(borrow, default)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    arguments: Option<Cow<'a, str>>,
}

#[derive(Default, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
    #[serde(default)]
    completion_tokens_details: Option<CompletionDetails>,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u64>,
    /// Venice.ai's names for the cache counts, beside the details and not
    /// inside them.
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    /// A number at the servers that state a cost. Read as any value, so
    /// that a cost written another way does not lose the counts.
    #[serde(default)]
    cost: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Default, Deserialize)]
struct CompletionDetails {
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

/// Reads a usage object. See the [module](self) for the mapping.
pub(crate) fn usage(raw: &RawValue) -> Result<ReportedUsage, DecodeError> {
    let wire: WireUsage = serde_json::from_str(raw.get())
        .map_err(|error| DecodeError::new(format!("a usage object could not be read: {error}")))?;
    let details = wire.prompt_tokens_details.unwrap_or_default();
    let cache_read = details
        .cached_tokens
        .or(wire.prompt_cache_hit_tokens)
        .or(wire.cache_read_input_tokens)
        .unwrap_or(0);
    let cache_write = details
        .cache_write_tokens
        .or(details.cache_creation_input_tokens)
        .or(wire.cache_creation_input_tokens)
        .unwrap_or(0);
    let tokens = TokenUsage {
        input: crate::usage::uncached_input(
            wire.prompt_tokens.unwrap_or(0),
            cache_read,
            cache_write,
        ),
        cache_read,
        cache_write,
        cache_write_classes: Vec::new(),
        output: wire.completion_tokens.unwrap_or(0),
        reasoning: wire
            .completion_tokens_details
            .and_then(|details| details.reasoning_tokens),
    };
    Ok(ReportedUsage {
        tokens,
        // The tier that served the request is a member of the chunk, not
        // of the usage, and is passed on from there.
        speed: None,
        provider_cost: wire
            .cost
            .as_ref()
            .and_then(serde_json::Value::as_f64)
            .and_then(dollars_as_nanos),
        raw: Some(RawJson::new(raw.get().to_owned())),
    })
}

/// The cost a server states beside the usage, at the top level of a chunk:
/// an object with the amount in US dollars as `usd`.
fn stated_cost(raw: &RawValue) -> Option<u64> {
    #[derive(Deserialize)]
    struct Cost {
        #[serde(default)]
        usd: Option<f64>,
    }
    serde_json::from_str::<Cost>(raw.get())
        .ok()?
        .usd
        .and_then(dollars_as_nanos)
}

/// A cost in US dollars as whole nano-dollars. `None` for a figure that is
/// not an amount.
fn dollars_as_nanos(dollars: f64) -> Option<u64> {
    let nanos = (dollars * 1e9).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked on the same line: finite, not negative, and below 1.8e19"
    )]
    (nanos.is_finite() && (0.0..1.8e19).contains(&nanos)).then_some(nanos as u64)
}

/// The tier a response names in `service_tier`, as a speed.
///
/// | `service_tier` | Speed |
/// |---|---|
/// | `default`, `standard` | `standard` |
/// | `priority`, `fast` | `fast`: the servers answer `priority` to a request for either |
/// | `auto`, `scale`, nothing | none reported. `auto` is a setting and not a tier that ran, and `scale` is capacity an account bought, which is not a speed. |
/// | any other | the tier of that name |
pub(crate) fn served_tier(name: &str) -> Option<Speed> {
    match name {
        "" | "auto" | "scale" => None,
        "default" | "standard" => Some(Speed::Standard),
        "priority" | "fast" => Some(Speed::Fast),
        other => Some(Speed::from_wire(other)),
    }
}

/// The dialect's stop reasons, in the neutral set.
///
/// | The server's | Neutral | |
/// |---|---|---|
/// | `stop` | `end` | `tool_calls` when the response holds a tool call: some servers stop a response that calls tools with `stop`. `refusal` when the response was a refusal. |
/// | `tool_calls`, `function_call` | `tool_calls` | When the response holds no call after all: `max_output` if the server's own word for the stop is `length`, and `end` otherwise. |
/// | `length` | `max_output` | |
/// | `content_filter` | `content_filter` | |
/// | `stop`, `tool_calls`, `function_call`, after a call that never got a name | `malformed_tool_call` | |
/// | anything else | itself | An unknown value is passed on under its own name, never guessed at. |
///
/// Two values are not stop reasons at all and never reach this table:
/// `error` and `insufficient_system_resource` are failures, and the decoder
/// reports them as errors.
pub(crate) fn stop_reason(
    raw: &str,
    native: Option<&str>,
    has_calls: bool,
    refused: bool,
    malformed: bool,
) -> StopReason {
    match raw {
        "tool_calls" | "function_call" if !has_calls && !malformed && native == Some("length") => {
            StopReason::MaxOutput
        }
        "stop" | "tool_calls" | "function_call" if malformed => StopReason::MalformedToolCall,
        "stop" | "tool_calls" | "function_call" if has_calls => StopReason::ToolCalls,
        "stop" if refused => StopReason::Refusal,
        "stop" | "tool_calls" | "function_call" => StopReason::End,
        "length" => StopReason::MaxOutput,
        "content_filter" => StopReason::ContentFilter,
        other => StopReason::Unknown(format!("openai:{other}")),
    }
}

/// Whether `data` is JSON that is not an object: a string, a number, a
/// list. No chunk is any of these.
fn is_other_json(data: &str) -> bool {
    !data.trim_start().starts_with('{')
        && serde_json::from_str::<serde::de::IgnoredAny>(data).is_ok()
}

/// What kind of text a text-like block holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run {
    Text,
    Reasoning,
    Refusal,
}

/// The text or reasoning block that is open. There is at most one.
struct Open {
    index: u32,
    run: Run,
    /// The member the text arrived under: `content`, `refusal`, or the
    /// server's name for reasoning. `None` for reasoning that arrived only
    /// as `reasoning_details`.
    member: Option<String>,
    /// What the member has added up to. For reasoning written inside the
    /// text, this is the text with its tags, exactly as it arrived.
    text: String,
    /// The pieces of `reasoning_details`, joined.
    details: Vec<Vec<(String, Detail)>>,
    /// The type of each piece, as the server wrote it, and where the piece
    /// of a type and index is: so that a fragment finds the piece it
    /// continues without looking at every piece.
    kinds: Vec<Option<String>>,
    placed: BTreeMap<(Option<String>, String), usize>,
}

/// One member of a piece of `reasoning_details`.
enum Detail {
    /// Text that arrives in fragments.
    Text(String),
    /// Any other value, as the server sent it.
    Raw(Box<RawValue>),
}

impl Open {
    /// The stored block: the piece of the assistant message this block
    /// stands for.
    fn whole(&self) -> String {
        let mut out = String::with_capacity(self.text.len() + 32);
        let mut object = Object::open(&mut out);
        if let Some(member) = &self.member {
            object.str(member, &self.text);
        }
        if !self.details.is_empty() {
            let mut list = Array::open(object.key("reasoning_details"));
            for detail in &self.details {
                let mut piece = Object::open(list.item());
                for (name, value) in detail {
                    match value {
                        Detail::Text(text) => piece.str(name, text),
                        Detail::Raw(raw) => piece.raw(name, raw.get()),
                    }
                }
                piece.close();
            }
            list.close();
        }
        object.close();
        out
    }
}

/// A tool call of the response.
struct Call {
    /// The block's index in the response.
    index: u32,
    /// Whether the call's block has ended.
    ended: bool,
    /// The server's `index` for the call, or the place that stood in for
    /// one.
    wire: u32,
    id: String,
    /// Whether the id is the server's. One of our own making says nothing
    /// about which call a later id belongs to.
    id_given: bool,
    name: String,
    arguments: String,
    /// Whether the call has been announced, and its block begun. That
    /// happens once its name is known.
    announced: bool,
    /// The members of the call's fragments that the decoder does not read,
    /// each as the server last sent it. They go back with the call.
    extra: Vec<(String, Box<RawValue>)>,
}

impl Call {
    /// The stored block: the call as an item of `tool_calls`.
    fn whole(&self) -> String {
        let mut out = String::with_capacity(self.arguments.len() + 96);
        let mut object = Object::open(&mut out);
        let mut list = Array::open(object.key("tool_calls"));
        let mut call = Object::open(list.item());
        call.str("id", &self.id);
        call.str("type", "function");
        let mut function = Object::open(call.key("function"));
        function.str("name", &self.name);
        function.str("arguments", &self.arguments);
        function.close();
        for (name, value) in &self.extra {
            call.raw(name, value.get());
        }
        call.close();
        list.close();
        object.close();
        out
    }
}

/// Where the decoder is in text that carries its reasoning in tags.
enum Tags {
    /// At the start: what has arrived is not yet enough to say whether the
    /// text begins with the opening tag.
    Start(String),
    /// Inside the tags. What is held may be the beginning of the closing
    /// tag.
    Inside(String),
    /// Past the reasoning, or there was none: everything is text.
    Text,
}

/// A piece of text that carried reasoning in tags, split.
enum Piece {
    /// Reasoning: what to show, and what arrived for it, tags included.
    Reasoning {
        shown: String,
        raw: String,
    },
    Text(String),
}

impl Tags {
    /// Takes more of the text and gives back what can be told apart so far.
    fn push(&mut self, more: &str, out: &mut Vec<Piece>) {
        match self {
            Self::Text => out.push(Piece::Text(more.to_owned())),
            Self::Start(held) => {
                held.push_str(more);
                let said = held.trim_start();
                if let Some(after) = said.strip_prefix(THINK_OPEN) {
                    let after = after.to_owned();
                    let raw = held[..held.len() - after.len()].to_owned();
                    out.push(Piece::Reasoning {
                        shown: String::new(),
                        raw,
                    });
                    *self = Self::Inside(String::new());
                    self.push(&after, out);
                } else if !THINK_OPEN.starts_with(said) || held.len() > LEADING_SPACE {
                    // Not the tag, or more white space before it than any
                    // model writes: looking at all of it again with every
                    // delta would cost the square of its length.
                    let text = std::mem::take(held);
                    *self = Self::Text;
                    out.push(Piece::Text(text));
                }
            }
            Self::Inside(held) => {
                held.push_str(more);
                if let Some(at) = held.find(THINK_CLOSE) {
                    let rest = held[at + THINK_CLOSE.len()..].to_owned();
                    out.push(Piece::Reasoning {
                        shown: held[..at].to_owned(),
                        raw: held[..at + THINK_CLOSE.len()].to_owned(),
                    });
                    *self = Self::Text;
                    if !rest.is_empty() {
                        out.push(Piece::Text(rest));
                    }
                    return;
                }
                // Everything but an ending that could be the start of the
                // closing tag is reasoning for certain.
                let keep = (1..THINK_CLOSE.len())
                    .rev()
                    .find(|length| {
                        held.len() >= *length
                            && held.is_char_boundary(held.len() - length)
                            && THINK_CLOSE.starts_with(&held[held.len() - length..])
                    })
                    .unwrap_or(0);
                let sure = held[..held.len() - keep].to_owned();
                if !sure.is_empty() {
                    held.drain(..sure.len());
                    out.push(Piece::Reasoning {
                        shown: sure.clone(),
                        raw: sure,
                    });
                }
            }
        }
    }

    /// The text has ended: what was held is what it looked like.
    fn finish(&mut self, out: &mut Vec<Piece>) {
        match std::mem::replace(self, Self::Text) {
            Self::Start(held) if !held.is_empty() => out.push(Piece::Text(held)),
            Self::Inside(held) if !held.is_empty() => out.push(Piece::Reasoning {
                shown: held.clone(),
                raw: held,
            }),
            _ => {}
        }
    }
}

/// The decoder of one response.
pub(crate) struct Decoder {
    /// The request as it was sent: an error in the stream is classified
    /// against it, to tell a speed tier's refusal from the model's.
    request: ModelRequest,
    /// The member reasoning text is read from, when the provider names one.
    reasoning_field: Option<String>,
    /// Whether visible reasoning is handed on.
    show_reasoning: bool,
    /// The state of reasoning written inside the text, where the server
    /// does that.
    tags: Option<Tags>,
    started: bool,
    ended: bool,
    /// The index the next block gets.
    next_index: u32,
    open: Option<Open>,
    /// The response's id, for the id of a call the server gave none.
    response_id: Option<String>,
    /// The tool calls of the response, in the order they began.
    calls: Vec<Call>,
    /// The latest call under each `index` the server has used.
    by_wire: BTreeMap<u32, usize>,
    /// Whether the response held a tool call, whether it held one that
    /// never got a name, and whether it was a refusal.
    has_calls: bool,
    malformed_call: bool,
    refused: bool,
    /// The stop reason and the server's own word for it, once given.
    stop: Option<(String, Option<String>)>,
    usage: Option<ReportedUsage>,
    /// Whether the response's `service_tier` says which tier served the
    /// request: only for a model that is asked for its tier that way.
    reads_tier: bool,
    /// Whether the tier that served the request has been passed on.
    tier_told: bool,
}

impl Decoder {
    pub(crate) fn new(request: &ModelRequest, provider: &ProviderInfo) -> Self {
        // Only what classification reads is kept: not the conversation.
        let mut kept = ModelRequest::new(request.model.clone(), request.role.clone());
        kept.speed = request.speed.clone();
        Self {
            request: kept,
            reasoning_field: flags::reasoning_field(provider).map(str::to_owned),
            show_reasoning: request.reasoning.visibility != ReasoningVisibility::None,
            tags: flags::reasoning_tags(request, provider).then(|| Tags::Start(String::new())),
            started: false,
            ended: false,
            next_index: 0,
            open: None,
            response_id: None,
            calls: Vec::new(),
            by_wire: BTreeMap::new(),
            has_calls: false,
            malformed_call: false,
            refused: false,
            stop: None,
            usage: None,
            reads_tier: !request.model.info.speeds.is_empty() && !flags::speeds_are_models(request),
            tier_told: false,
        }
    }

    fn close_open(&mut self, out: &mut EventSink) {
        if let Some(open) = self.open.take() {
            out.block_end(open.index, RawJson::new(open.whole()));
        }
    }

    /// The open block for a run of `run`, begun now if another kind of
    /// block, or none, was open.
    fn run(&mut self, run: Run, member: Option<&str>, out: &mut EventSink) -> &mut Open {
        if self.open.as_ref().is_some_and(|open| open.run != run) {
            self.close_open(out);
        }
        let next_index = &mut self.next_index;
        let open = self.open.get_or_insert_with(|| {
            let index = *next_index;
            *next_index += 1;
            out.block_start(
                index,
                match run {
                    Run::Text | Run::Refusal => BlockKind::Text,
                    Run::Reasoning => BlockKind::Reasoning,
                },
            );
            Open {
                index,
                run,
                member: None,
                text: String::new(),
                details: Vec::new(),
                kinds: Vec::new(),
                placed: BTreeMap::new(),
            }
        });
        if open.member.is_none() {
            open.member = member.map(str::to_owned);
        }
        open
    }

    fn text(&mut self, run: Run, member: &str, more: &str, out: &mut EventSink) {
        if more.is_empty() {
            return;
        }
        let open = self.run(run, Some(member), out);
        open.text.push_str(more);
        let index = open.index;
        out.text_delta(index, more);
    }

    /// More reasoning: `raw` is what is stored for it under `member`, and
    /// `shown` what a person reads.
    fn reasoning(&mut self, member: Option<&str>, raw: &str, shown: &str, out: &mut EventSink) {
        let show = self.show_reasoning;
        let open = self.run(Run::Reasoning, member, out);
        open.text.push_str(raw);
        let index = open.index;
        if show && !shown.is_empty() {
            out.reasoning_delta(index, shown);
        }
    }

    /// Text that may carry its reasoning in tags.
    fn content(&mut self, more: &str, out: &mut EventSink) {
        let Some(tags) = &mut self.tags else {
            self.text(Run::Text, "content", more, out);
            return;
        };
        let mut pieces = Vec::new();
        tags.push(more, &mut pieces);
        self.pieces(pieces, out);
    }

    fn pieces(&mut self, pieces: Vec<Piece>, out: &mut EventSink) {
        for piece in pieces {
            match piece {
                Piece::Text(text) => self.text(Run::Text, "content", &text, out),
                Piece::Reasoning { shown, raw } => {
                    self.reasoning(Some("content"), &raw, &shown, out);
                }
            }
        }
    }

    /// The pieces of `reasoning_details` in one delta: joined to the ones
    /// before them, and their text returned for showing. `None` for a list
    /// with nothing in it, which is no reasoning at all.
    fn details(
        &mut self,
        raw: &RawValue,
        out: &mut EventSink,
    ) -> Result<Option<String>, DecodeError> {
        let bad = |error: serde_json::Error| {
            DecodeError::new(format!("`reasoning_details` could not be read: {error}"))
        };
        let pieces: Vec<&RawValue> = serde_json::from_str(raw.get()).map_err(bad)?;
        if pieces.is_empty() {
            return Ok(None);
        }
        let mut shown = String::new();
        for piece in pieces {
            let members = ordered_members(piece).map_err(bad)?;
            let key = |name: &str| {
                members
                    .iter()
                    .find(|(known, _)| known == name)
                    .map(|(_, value)| value.get().to_owned())
            };
            let (kind, at) = (key("type"), key("index"));
            let open = self.run(Run::Reasoning, None, out);
            // A piece continues the one of the same type and index. Without
            // an index, it continues the last piece when that has its type.
            let position = match at {
                Some(at) => {
                    let next = open.details.len();
                    Some(*open.placed.entry((kind.clone(), at)).or_insert(next))
                }
                None => open
                    .details
                    .len()
                    .checked_sub(1)
                    .filter(|last| open.kinds[*last] == kind),
            };
            let position = match position {
                Some(position) if position < open.details.len() => position,
                _ => {
                    open.details.push(Vec::new());
                    open.kinds.push(kind);
                    open.details.len() - 1
                }
            };
            let detail = &mut open.details[position];
            for (name, value) in members {
                let fragment = DETAIL_TEXT.contains(&name.as_str()) && value.get().starts_with('"');
                let slot = detail.iter_mut().find(|(known, _)| *known == name);
                if fragment {
                    let more: String = serde_json::from_str(value.get()).map_err(bad)?;
                    // An opaque payload is kept and never shown.
                    if name != "data" {
                        shown.push_str(&more);
                    }
                    match slot {
                        Some((_, Detail::Text(text))) => text.push_str(&more),
                        Some((_, other)) => *other = Detail::Text(more),
                        None => detail.push((name, Detail::Text(more))),
                    }
                } else {
                    match slot {
                        // A null does not erase what an earlier fragment
                        // brought.
                        Some(_) if value.get() == "null" => {}
                        Some((_, known)) => *known = Detail::Raw(value),
                        None => detail.push((name, Detail::Raw(value))),
                    }
                }
            }
        }
        Ok(Some(shown))
    }

    fn delta(&mut self, raw: &RawValue, out: &mut EventSink) -> Result<(), DecodeError> {
        let bad = |error: serde_json::Error| {
            DecodeError::new(format!("a delta could not be read: {error}"))
        };
        let members = ordered_members(raw).map_err(bad)?;
        let member = |name: &str| {
            members
                .iter()
                .find(|(known, _)| known == name)
                .map(|(_, value)| &**value)
                .filter(|value| value.get() != "null")
        };
        let string = |name: &str| -> Result<Option<String>, DecodeError> {
            match member(name) {
                Some(value) if value.get().starts_with('"') => {
                    serde_json::from_str(value.get()).map(Some).map_err(bad)
                }
                _ => Ok(None),
            }
        };

        // Reasoning first: a delta that carries both has the reasoning
        // that led to the text.
        // An empty member is no reasoning: the next name is tried, and a
        // delta with nothing in any of them begins no block.
        let mut named = None;
        let fields: &[&str] = match &self.reasoning_field {
            Some(field) => &[field.as_str()],
            None => REASONING_FIELDS,
        };
        for field in fields {
            if let Some(text) = string(field)?.filter(|text| !text.is_empty()) {
                named = Some(((*field).to_owned(), text));
                break;
            }
        }
        let detailed = match member("reasoning_details") {
            Some(details) => self.details(details, out)?,
            None => None,
        };
        match (named, detailed) {
            (Some((field, text)), _) => self.reasoning(Some(&field), &text, &text, out),
            // The details alone: their text is what there is to show.
            (None, Some(shown)) => self.reasoning(None, "", &shown, out),
            (None, None) => {}
        }

        match member("content") {
            None => {}
            Some(value) if value.get().starts_with('"') => {
                let text: String = serde_json::from_str(value.get()).map_err(bad)?;
                self.content(&text, out);
            }
            Some(_) => {
                return Err(DecodeError::new(
                    "a delta's `content` is neither text nor null",
                ));
            }
        }
        if let Some(text) = string("refusal")? {
            self.refused |= !text.is_empty();
            self.text(Run::Refusal, "refusal", &text, out);
        }
        if let Some(calls) = member("tool_calls") {
            let calls: Vec<&RawValue> = serde_json::from_str(calls.get()).map_err(bad)?;
            // A server that leaves the index out sends each call whole, in
            // one delta: a call's place among the calls of the response is
            // then its index.
            let before = self.calls.len();
            for (position, call) in calls.into_iter().enumerate() {
                self.call(before + position, call, out)?;
            }
        }
        Ok(())
    }

    /// One item of a delta's `tool_calls`: the beginning of a call, or more
    /// of one, **by its `index`**. `place` stands in for the index where
    /// the server gives none.
    fn call(
        &mut self,
        place: usize,
        raw: &RawValue,
        out: &mut EventSink,
    ) -> Result<(), DecodeError> {
        let bad = |error: serde_json::Error| {
            DecodeError::new(format!("a tool call could not be read: {error}"))
        };
        let delta: CallDelta<'_> = serde_json::from_str(raw.get()).map_err(bad)?;
        let id = delta.id.as_deref().filter(|id| !id.is_empty());
        let named = delta.function.as_ref().is_some_and(|function| {
            function
                .name
                .as_deref()
                .is_some_and(|name| !name.is_empty())
        });
        // An item with no index that begins nothing (no id, no name) is
        // more of the call before it: a server that numbers nothing can
        // only be streaming one call at a time.
        let last = self.calls.len().checked_sub(1).filter(|last| {
            delta.index.is_none() && id.is_none() && !named && !self.calls[*last].ended
        });
        let wire = match (delta.index, last) {
            (Some(index), _) => index,
            (None, Some(last)) => self.calls[last].wire,
            (None, None) => u32::try_from(place).unwrap_or(u32::MAX),
        };
        // The call the fragment belongs to: the latest one under its index,
        // unless the fragment brings another id, which is another call.
        let known = self.by_wire.get(&wire).copied().filter(|at| {
            let call = &self.calls[*at];
            !call.id_given || id.is_none_or(|id| id == call.id)
        });
        let at = match known {
            Some(at) if self.calls[at].ended => {
                return Err(DecodeError::new(format!(
                    "the stream added to the tool call {wire}, which has ended"
                )));
            }
            Some(at) => at,
            None => {
                // Text and reasoning end where the calls begin, with
                // whatever of the text was still being told apart.
                self.flush_tags(out);
                self.close_open(out);
                let index = self.next_index;
                self.next_index += 1;
                let extra = ordered_members(raw)
                    .map_err(bad)?
                    .into_iter()
                    .filter(|(name, _)| {
                        !matches!(name.as_str(), "index" | "id" | "type" | "function")
                    })
                    .collect();
                self.calls.push(Call {
                    index,
                    ended: false,
                    wire,
                    id: String::new(),
                    id_given: false,
                    name: String::new(),
                    arguments: String::new(),
                    announced: false,
                    extra,
                });
                self.by_wire.insert(wire, self.calls.len() - 1);
                self.calls.len() - 1
            }
        };
        let call = &mut self.calls[at];
        if known.is_some() {
            // What a later fragment brings that the decoder does not read
            // goes back with the call too: one server puts a signature
            // there.
            for (name, value) in ordered_members(raw).map_err(bad)? {
                if matches!(name.as_str(), "index" | "id" | "type" | "function")
                    || value.get() == "null"
                {
                    continue;
                }
                match call.extra.iter_mut().find(|(known, _)| *known == name) {
                    Some(member) => member.1 = value,
                    None => call.extra.push((name, value)),
                }
            }
        }
        if call.id.is_empty()
            && !call.announced
            && let Some(id) = id
        {
            call.id = id.to_owned();
            call.id_given = true;
        }
        let (name, arguments) = match delta.function {
            Some(function) => (function.name, function.arguments),
            None => (None, None),
        };
        if call.name.is_empty()
            && let Some(name) = name
        {
            call.name = name.into_owned();
        }
        let more = arguments.as_deref().unwrap_or_default();
        call.arguments.push_str(more);
        if call.announced {
            if !more.is_empty() {
                out.tool_args_delta(call.index, more);
            }
        } else if !call.name.is_empty() {
            self.has_calls = true;
            Self::announce(at, self.response_id.as_deref(), call, out);
        }
        Ok(())
    }

    /// Says that a call has begun, with its id and name, and hands on the
    /// arguments that arrived before its name did. `at` is the call's place
    /// among the calls of the response.
    fn announce(at: usize, response_id: Option<&str>, call: &mut Call, out: &mut EventSink) {
        if call.id.is_empty() {
            // An id of our own: the call's place in the response, and
            // where the response has an id, the end of that, so that
            // another response's calls do not get the same.
            // The end of the response's id, which is where ids differ:
            // letters and digits only, and short, because some servers
            // refuse a call id over 40 characters.
            let tail: String = response_id
                .unwrap_or_default()
                .chars()
                .rev()
                .filter(char::is_ascii_alphanumeric)
                .take(16)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            call.id = if tail.is_empty() {
                format!("call_{at}")
            } else {
                format!("call_{tail}_{at}")
            };
        }
        call.announced = true;
        out.block_start(call.index, BlockKind::ToolCall);
        out.tool_call_start(call.index, call.id.as_str(), call.name.as_str());
        if !call.arguments.is_empty() {
            out.tool_args_delta(call.index, call.arguments.as_str());
        }
    }

    /// Hands on what the text held back while it could not yet be told
    /// from the beginning of a tag.
    fn flush_tags(&mut self, out: &mut EventSink) {
        if let Some(tags) = &mut self.tags {
            let mut pieces = Vec::new();
            tags.finish(&mut pieces);
            self.pieces(pieces, out);
        }
    }

    /// Ends every block that is open: the stop reason has arrived, or the
    /// response is over.
    fn close_all(&mut self, out: &mut EventSink) {
        self.flush_tags(out);
        self.close_open(out);
        for call in &mut self.calls {
            if call.ended {
                continue;
            }
            call.ended = true;
            if call.announced {
                out.block_end(call.index, RawJson::new(call.whole()));
            } else {
                // A call that never got a name cannot be run, and cannot be
                // sent back: the servers refuse a call with no name. It is
                // left out, and the response says a call was malformed.
                self.malformed_call = true;
            }
        }
    }

    fn start(&mut self, chunk: &Chunk<'_>, out: &mut EventSink) {
        if !self.started {
            self.started = true;
            self.response_id = chunk.id.as_deref().map(str::to_owned);
            out.start(
                chunk.id.as_deref().map(str::to_owned),
                chunk.model.as_deref().unwrap_or_default(),
            );
        }
    }

    /// The response is complete.
    fn end(&mut self, out: &mut EventSink) -> Result<(), DecodeError> {
        let Some((raw_stop, native)) = self.stop.take() else {
            return Err(DecodeError::new(
                "the response ended without saying why it stopped",
            ));
        };
        self.close_all(out);
        self.ended = true;
        out.end(
            stop_reason(
                &raw_stop,
                native.as_deref(),
                self.has_calls,
                self.refused,
                self.malformed_call,
            ),
            raw_stop,
            native,
            self.usage.take().unwrap_or_default(),
        );
        Ok(())
    }

    /// Passes on the usage a chunk carries, with the cost the server
    /// states beside it.
    fn take_usage(&mut self, chunk: &Chunk<'_>, out: &mut EventSink) -> Result<(), DecodeError> {
        if let Some(raw) = chunk.usage {
            let mut reported = usage(raw)?;
            if reported.provider_cost.is_none() {
                reported.provider_cost = chunk.cost.and_then(stated_cost);
            }
            out.usage(reported.clone());
            self.usage = Some(reported);
        }
        Ok(())
    }

    fn fail(&mut self, error: crate::error::ProviderError, out: &mut EventSink) {
        out.error(error);
        self.ended = true;
    }
}

impl StreamDecoder for Decoder {
    fn feed(&mut self, event: SseEvent<'_>, out: &mut EventSink) -> Result<(), DecodeError> {
        if self.ended {
            return Ok(());
        }
        if event.data.trim() == DONE {
            // A stream that says it is done before it began was no
            // response: the client reports it as incomplete.
            if !self.started {
                return Ok(());
            }
            return self.end(out);
        }
        let mut chunk: Chunk<'_> = match serde_json::from_str(event.data) {
            Ok(chunk) => chunk,
            // An event named `error` whose data is not JSON still says the
            // request failed.
            Err(_) if event.event == "error" => {
                let error = classify::stream_error(event.data, &self.request);
                self.fail(error, out);
                return Ok(());
            }
            // An event with a name of its own is of a kind this build does
            // not know, and so is data that is JSON and not an object.
            Err(_) if event.event != DEFAULT_EVENT || is_other_json(event.data) => {
                return Ok(());
            }
            Err(error) => {
                return Err(DecodeError::new(format!(
                    "a chunk of the response could not be read: {error}"
                )));
            }
        };

        if chunk.error.is_some() || event.event == "error" {
            // What the failed response used is still counted.
            self.take_usage(&chunk, out)?;
            let error = classify::stream_error(event.data, &self.request);
            self.fail(error, out);
            return Ok(());
        }
        // Data that is JSON and nothing a chunk has: an event of a kind
        // this build does not know.
        if chunk.choices.is_none() && chunk.usage.is_none() && chunk.id.is_none() {
            return Ok(());
        }
        self.start(&chunk, out);

        if self.reads_tier
            && !self.tier_told
            && let Some(speed) = chunk.service_tier.as_deref().and_then(served_tier)
        {
            self.tier_told = true;
            out.served_at(speed);
        }

        let first = chunk
            .choices
            .take()
            .unwrap_or_default()
            .into_iter()
            .find(|choice| choice.index.unwrap_or(0) == 0);
        if let Some(choice) = first {
            if let Some(delta) = choice.delta {
                self.delta(delta, out)?;
            }
            // Some servers write an empty stop reason where others write
            // null, on every chunk but the last.
            if let Some(reason) = choice.finish_reason.filter(|reason| !reason.is_empty()) {
                match reason.as_ref() {
                    // Not stop reasons: the response failed.
                    "error" | "insufficient_system_resource" => {
                        self.take_usage(&chunk, out)?;
                        let error = classify::stopped_with_failure(&reason, &self.request);
                        self.fail(error, out);
                        return Ok(());
                    }
                    _ => {
                        // Every block is complete, and the first reason
                        // given stands: a server may repeat it with the
                        // usage.
                        self.close_all(out);
                        if self.stop.is_none() {
                            self.stop = Some((
                                reason.into_owned(),
                                choice.native_finish_reason.map(Cow::into_owned),
                            ));
                        }
                    }
                }
            }
        }

        self.take_usage(&chunk, out)
    }

    fn finish(&mut self, out: &mut EventSink) -> Result<(), DecodeError> {
        // A server that closes the stream without `[DONE]` after it gave a
        // stop reason has completed its response. Without a stop reason
        // the stream was cut, and the client reports it as incomplete.
        if !self.ended && self.stop.is_some() {
            return self.end(out);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(parts: &[&str]) -> (String, String, String) {
        let mut tags = Tags::Start(String::new());
        let mut pieces = Vec::new();
        for part in parts {
            tags.push(part, &mut pieces);
        }
        tags.finish(&mut pieces);
        let (mut shown, mut raw, mut text) = (String::new(), String::new(), String::new());
        for piece in pieces {
            match piece {
                Piece::Reasoning { shown: s, raw: r } => {
                    shown.push_str(&s);
                    raw.push_str(&r);
                }
                Piece::Text(more) => text.push_str(&more),
            }
        }
        (shown, raw, text)
    }

    #[test]
    fn reasoning_in_tags_is_taken_out_however_the_text_is_cut() {
        let whole = "\n<think>\nLet me think. 2 < 3 </thin> so…\n</think>\n\nThe answer is 5.";
        let expected = (
            "\nLet me think. 2 < 3 </thin> so…\n".to_owned(),
            "\n<think>\nLet me think. 2 < 3 </thin> so…\n</think>".to_owned(),
            "\n\nThe answer is 5.".to_owned(),
        );
        assert_eq!(split(&[whole]), expected);
        // Cut in two at every character, and one character at a time.
        for cut in 1..whole.len() {
            if whole.is_char_boundary(cut) {
                assert_eq!(split(&[&whole[..cut], &whole[cut..]]), expected, "{cut}");
            }
        }
        let characters: Vec<String> = whole.chars().map(String::from).collect();
        let characters: Vec<&str> = characters.iter().map(String::as_str).collect();
        assert_eq!(split(&characters), expected);
        // What was stored and the text are together what arrived.
        assert_eq!(format!("{}{}", expected.1, expected.2), whole);
    }

    #[test]
    fn text_without_the_opening_tag_is_text() {
        for text in [
            "Hello",
            "<thinking> is not the tag",
            "  < 3",
            "",
            "<",
            "a <think>b</think>",
        ] {
            let (shown, raw, said) = split(&[text]);
            assert_eq!((shown.as_str(), raw.as_str()), ("", ""), "{text}");
            assert_eq!(said, text);
        }
        // White space is held back only while it could still come before
        // the tag: a long run of it is text, and is not looked at again
        // and again.
        let blank = "\n".repeat(LEADING_SPACE + 1);
        let mut tags = Tags::Start(String::new());
        let mut pieces = Vec::new();
        for _ in 0..LEADING_SPACE {
            tags.push("\n", &mut pieces);
        }
        assert!(pieces.is_empty(), "still held");
        tags.push("\n", &mut pieces);
        assert!(matches!(tags, Tags::Text));
        assert!(matches!(&pieces[..], [Piece::Text(text)] if *text == blank));
        // Reasoning that never closes is all reasoning.
        let (shown, raw, said) = split(&["<think>still going </thi"]);
        assert_eq!(shown, "still going </thi");
        assert_eq!(raw, "<think>still going </thi");
        assert_eq!(said, "");
    }

    #[test]
    fn every_stop_reason_has_its_neutral_form() {
        let rows = [
            ("stop", false, false, StopReason::End),
            ("stop", true, false, StopReason::ToolCalls),
            ("stop", false, true, StopReason::Refusal),
            ("tool_calls", true, false, StopReason::ToolCalls),
            ("tool_calls", false, false, StopReason::End),
            ("function_call", true, false, StopReason::ToolCalls),
            ("length", false, false, StopReason::MaxOutput),
            ("length", true, false, StopReason::MaxOutput),
            ("content_filter", false, false, StopReason::ContentFilter),
        ];
        for (raw, calls, refused, neutral) in rows {
            assert_eq!(
                stop_reason(raw, None, calls, refused, false),
                neutral,
                "{raw} {calls}"
            );
            // The server's own word changes nothing where the response is
            // what its stop reason says.
            if calls || raw == "stop" {
                assert_eq!(
                    stop_reason(raw, Some("length"), calls, refused, false),
                    neutral,
                    "{raw} {calls}"
                );
            }
        }
        // A call that never got a name: the response did not stop for
        // tools, nor end. The output limit is still the output limit.
        for raw in ["stop", "tool_calls", "function_call"] {
            for calls in [false, true] {
                assert_eq!(
                    stop_reason(raw, None, calls, false, true),
                    StopReason::MalformedToolCall,
                    "{raw}"
                );
            }
        }
        assert_eq!(
            stop_reason("length", None, true, false, true),
            StopReason::MaxOutput
        );
        // "For tools", with no call, and the server's own word says the
        // output limit cut it off.
        for raw in ["tool_calls", "function_call"] {
            assert_eq!(
                stop_reason(raw, Some("length"), false, false, false),
                StopReason::MaxOutput
            );
            assert_eq!(
                stop_reason(raw, Some("stop"), false, false, false),
                StopReason::End
            );
        }
        for raw in ["aborted", "end_turn", "end", ""] {
            let neutral = stop_reason(raw, None, false, false, false);
            assert!(!neutral.is_known(), "{raw}: {neutral:?}");
            assert_eq!(neutral.as_str(), format!("openai:{raw}"));
        }
    }

    #[test]
    fn a_tier_is_read_as_the_speed_it_names() {
        assert_eq!(served_tier("default"), Some(Speed::Standard));
        assert_eq!(served_tier("standard"), Some(Speed::Standard));
        assert_eq!(served_tier("priority"), Some(Speed::Fast));
        assert_eq!(served_tier("fast"), Some(Speed::Fast));
        assert_eq!(served_tier("flex"), Some(Speed::Other("flex".to_owned())));
        assert_eq!(
            served_tier("ultrafast"),
            Some(Speed::Other("ultrafast".to_owned()))
        );
        // A setting, and capacity an account bought: neither is a tier
        // that ran.
        for none in ["auto", "scale", ""] {
            assert_eq!(served_tier(none), None, "{none}");
        }
    }

    #[test]
    fn cached_tokens_are_taken_out_of_the_prompt_count() {
        let raw = |json: &str| -> Box<RawValue> { serde_json::from_str(json).unwrap() };
        // OpenAI and OpenRouter.
        let read = usage(&raw(
            r#"{"prompt_tokens":1000,"completion_tokens":50,"total_tokens":1050,"prompt_tokens_details":{"cached_tokens":600,"cache_write_tokens":300},"completion_tokens_details":{"reasoning_tokens":20},"cost":0.00012345}"#,
        ))
        .unwrap();
        assert_eq!(
            read.tokens,
            TokenUsage {
                input: 100,
                cache_read: 600,
                cache_write: 300,
                cache_write_classes: Vec::new(),
                output: 50,
                reasoning: Some(20),
            }
        );
        assert_eq!(read.provider_cost, Some(123_450));
        assert_eq!(read.speed, None, "the tier is not in the usage");
        assert!(read.raw.unwrap().as_str().contains("\"total_tokens\":1050"));
        // DeepSeek's own names, and Venice.ai's name for a cache write.
        let read = usage(&raw(
            r#"{"prompt_tokens":90,"completion_tokens":7,"prompt_cache_hit_tokens":64,"prompt_cache_miss_tokens":26}"#,
        ))
        .unwrap();
        assert_eq!((read.tokens.input, read.tokens.cache_read), (26, 64));
        assert_eq!(read.tokens.reasoning, None);
        assert_eq!(read.provider_cost, None);
        let read = usage(&raw(
            r#"{"prompt_tokens":90,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":0,"cache_creation_input_tokens":80}}"#,
        ))
        .unwrap();
        assert_eq!((read.tokens.input, read.tokens.cache_write), (10, 80));
        // Venice.ai's responses write the count of reads beside the
        // details (recorded), and a count of writes there is read too.
        let read = usage(&raw(
            r#"{"prompt_tokens":1254,"completion_tokens":35,"prompt_tokens_details":{},"cache_read_input_tokens":1024,"cache_creation_input_tokens":200}"#,
        ))
        .unwrap();
        assert_eq!(
            (
                read.tokens.input,
                read.tokens.cache_read,
                read.tokens.cache_write
            ),
            (30, 1024, 200)
        );
        // Counts that do not add up never go below nothing, and a server
        // that reports only two counts is read.
        let read = usage(&raw(
            r#"{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":50}}"#,
        ))
        .unwrap();
        assert_eq!(read.tokens.input, 0);
        assert!(usage(&raw(r#"{"prompt_tokens":"many"}"#)).is_err());
        assert_eq!(dollars_as_nanos(-1.0), None);
        assert_eq!(dollars_as_nanos(f64::NAN), None);
        assert_eq!(dollars_as_nanos(0.0), Some(0));
    }
}
