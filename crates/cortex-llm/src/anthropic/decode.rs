//! Decoding a response stream.
//!
//! # The stream
//!
//! | Event | What the decoder does |
//! |---|---|
//! | `message_start` | The response began: its id, its model, and the usage known so far. |
//! | `content_block_start` | A block began. Its kind comes from the block's `type`. |
//! | `content_block_delta` | More of a block: text, visible reasoning, a signature, a fragment of a tool call's arguments, a citation. |
//! | `content_block_stop` | The block is complete. Its whole JSON is put together and handed on. |
//! | `message_delta` | The stop reason, and usage, which is cumulative. |
//! | `message_stop` | The response is complete. |
//! | `ping` | Nothing. |
//! | `error` | The request failed after a successful status. Classified by the same table as a failed response. |
//! | anything else | Skipped. |
//!
//! An event is known by the `type` in its data, and by its SSE name only
//! when the data has none.
//!
//! # Blocks
//!
//! | `type` | Kind |
//! |---|---|
//! | `text` | text |
//! | `thinking`, `redacted_thinking` | reasoning |
//! | `tool_use` | a tool call |
//! | anything else | a provider block: a server-side tool's use or result, a compaction block, and whatever is added next month |
//!
//! **The whole block is put together here.** The provider never sends a
//! block whole when it streams: it sends the block as it starts, and then
//! what is added to it. The decoder keeps each member of the starting block
//! as the text the provider sent (no member is parsed and written again),
//! and when the block stops it writes them back in their order, with the
//! streamed member replaced by what the deltas added up to. The result is
//! the block as the provider would have sent it without streaming, which is
//! what a later request sends back.
//!
//! **A block of a kind this build does not know keeps what is added to
//! it.** An addition of an unknown kind to such a block is laid over the
//! block: each of its members becomes the block's member of the same name,
//! text appended to text. A compaction block arrives that way, with all of
//! its content in one addition. An addition of an unknown kind to text,
//! reasoning or a tool call is skipped: where it would go in the block is
//! not something to guess at.
//!
//! **An index names one block for the whole response.** A block that starts
//! under an index that was already used, open or closed, is an error.
//!
//! A tool call's arguments are kept as the text the model wrote. When that
//! text is not a JSON object (a response cut off by the output limit, or a
//! model that wrote nonsense), the provider would refuse it on replay, so
//! the stored block carries the text inside an object, under the key
//! `INVALID_JSON`. The fragments handed to the loop are the model's own
//! text either way.
//!
//! # Usage
//!
//! The provider's `input_tokens` does not include the tokens read from or
//! written to the cache, so nothing is subtracted:
//!
//! | Neutral | From |
//! |---|---|
//! | `input` | `input_tokens` |
//! | `cache_read` | `cache_read_input_tokens` |
//! | `cache_write` | `cache_creation_input_tokens` |
//! | `cache_write_classes` | `cache_creation.ephemeral_<class>_input_tokens`, for each class with tokens |
//! | `output` | `output_tokens` |
//! | `reasoning` | `output_tokens_details.thinking_tokens` |
//! | the speed that served the request | `speed`, only where the response has it |
//!
//! The usage arrives in two parts: at the start, and again with the stop
//! reason, where the counts are cumulative and a count may be missing. A
//! later count replaces an earlier one, and a missing count leaves the
//! earlier one standing. The raw usage kept beside the record is the first
//! object with the members of the later ones laid over it.

use std::borrow::Cow;
use std::collections::BTreeMap;

use cortex_base::UtcTime;
use cortex_protocol::usage::{CacheWriteTokens, TokenUsage};
use cortex_protocol::{RawJson, Speed, StopReason};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::classify;
use super::request::INVALID_INPUT_KEY;
use crate::error::DecodeError;
use crate::event::{BlockKind, EventSink, ReportedUsage};
use crate::json::{self, Array, Object, ordered_members};
use crate::protocol::StreamDecoder;
use crate::request::ModelRequest;
use crate::sse::SseEvent;

/// The events this decoder reads. Any other is skipped.
const KNOWN_EVENTS: &[&str] = &[
    "message_start",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "message_delta",
    "message_stop",
    "error",
];

/// The data of any event: the members the decoder reads, each kept as the
/// text the provider sent until it is needed.
#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(borrow, default, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(default)]
    index: Option<u32>,
    #[serde(borrow, default)]
    message: Option<&'a RawValue>,
    #[serde(borrow, default)]
    content_block: Option<&'a RawValue>,
    #[serde(borrow, default)]
    delta: Option<&'a RawValue>,
    #[serde(borrow, default)]
    usage: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct MessageStart<'a> {
    #[serde(borrow, default)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    usage: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BlockDelta<'a> {
    #[serde(borrow, default, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    text: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    thinking: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    signature: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    partial_json: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    citation: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct MessageDelta<'a> {
    #[serde(borrow, default)]
    stop_reason: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    stop_sequence: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    stop_details: Option<StopDetails<'a>>,
}

#[derive(Deserialize)]
struct StopDetails<'a> {
    #[serde(borrow, default)]
    category: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    output_tokens_details: Option<OutputDetails>,
    #[serde(default)]
    speed: Option<String>,
}

#[derive(Deserialize)]
struct OutputDetails {
    #[serde(default)]
    thinking_tokens: Option<u64>,
}

/// A block that has started and not yet stopped.
struct Block {
    /// The members of the block as it started, in the provider's order,
    /// each as the text the provider sent.
    members: Vec<(String, Box<RawValue>)>,
    shape: Shape,
}

enum Shape {
    Text {
        text: String,
        /// Citations the stream added, each as the provider sent it.
        citations: Vec<Box<RawValue>>,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    /// A call to one of the harness's tools: the arguments arrive as
    /// fragments of JSON text.
    Call {
        input: String,
    },
    /// A block of a kind this build does not name.
    Provider {
        /// The arguments of a server-side tool's call, which stream like
        /// any other call's.
        input: String,
        /// What additions of other kinds laid over the block's members.
        laid: Vec<(String, Laid)>,
    },
    /// A block that arrives whole.
    Whole,
}

/// What the stream laid over one member of a block of an unknown kind.
enum Laid {
    /// Text, which each addition appends to.
    Text(String),
    /// Any other value, as the provider sent it. A later one replaces it.
    Raw(Box<RawValue>),
}

/// Lays an addition of an unknown kind over a block of an unknown kind:
/// each member of the addition, apart from its `type`, becomes the block's
/// member of the same name. Text is appended to the text before it, as
/// every kind of addition the provider has so far does with text; any
/// other value replaces what was there.
///
/// It is the reading that loses nothing. A compaction block, for one,
/// starts with no content and gets all of it in a single addition; skipped,
/// the block would be stored empty and the summary would be gone.
fn lay(
    laid: &mut Vec<(String, Laid)>,
    started: &[(String, Box<RawValue>)],
    addition: &RawValue,
) -> Result<(), serde_json::Error> {
    for (key, value) in ordered_members(addition)? {
        if key == "type" {
            continue;
        }
        let text = if value.get().starts_with('"') {
            Some(serde_json::from_str::<String>(value.get())?)
        } else {
            None
        };
        let at = laid.iter().position(|(known, _)| *known == key);
        match (text, at) {
            (Some(more), Some(at)) => match &mut laid[at].1 {
                Laid::Text(text) => text.push_str(&more),
                other => *other = Laid::Text(more),
            },
            (Some(more), None) => {
                // The text the block started with, when it started with
                // text, comes first.
                let mut text: String = started
                    .iter()
                    .find(|(known, _)| *known == key)
                    .filter(|(_, value)| value.get().starts_with('"'))
                    .and_then(|(_, value)| serde_json::from_str(value.get()).ok())
                    .unwrap_or_default();
                text.push_str(&more);
                laid.push((key, Laid::Text(text)));
            }
            (None, Some(at)) => laid[at].1 = Laid::Raw(value),
            (None, None) => laid.push((key, Laid::Raw(value))),
        }
    }
    Ok(())
}

/// The usage reported so far.
#[derive(Default)]
struct UsageSoFar {
    tokens: TokenUsage,
    speed: Option<Speed>,
    raw: Option<Map<String, Value>>,
}

impl UsageSoFar {
    /// Lays a usage object over what is known: a count it has replaces the
    /// earlier one, a count it lacks leaves the earlier one standing.
    fn merge(&mut self, raw: &RawValue) -> Result<(), DecodeError> {
        if raw.get() == "null" {
            return Ok(());
        }
        let bad = |error: serde_json::Error| {
            DecodeError::new(format!("a usage object could not be read: {error}"))
        };
        let usage: WireUsage = serde_json::from_str(raw.get()).map_err(bad)?;
        if let Some(count) = usage.input_tokens {
            self.tokens.input = count;
        }
        if let Some(count) = usage.output_tokens {
            self.tokens.output = count;
        }
        if let Some(count) = usage.cache_read_input_tokens {
            self.tokens.cache_read = count;
        }
        if let Some(count) = usage.cache_creation_input_tokens {
            self.tokens.cache_write = count;
        }
        if let Some(classes) = usage.cache_creation {
            self.tokens.cache_write_classes = classes
                .iter()
                .filter_map(|(name, tokens)| {
                    let class = name
                        .strip_prefix("ephemeral_")?
                        .strip_suffix("_input_tokens")?;
                    let tokens = tokens.as_u64().filter(|tokens| *tokens > 0)?;
                    Some(CacheWriteTokens {
                        class: class.to_owned(),
                        tokens,
                    })
                })
                .collect();
        }
        if let Some(count) = usage
            .output_tokens_details
            .and_then(|details| details.thinking_tokens)
        {
            self.tokens.reasoning = Some(count);
        }
        if let Some(speed) = usage.speed {
            self.speed = Some(Speed::from_wire(&speed));
        }

        let members: Map<String, Value> = serde_json::from_str(raw.get()).map_err(bad)?;
        match &mut self.raw {
            None => self.raw = Some(members),
            Some(known) => {
                for (name, member) in members {
                    if !member.is_null() {
                        known.insert(name, member);
                    }
                }
            }
        }
        Ok(())
    }

    /// What the response has reported. The speed is what the response said
    /// and nothing else: it is never filled in from the request.
    fn reported(&self) -> ReportedUsage {
        ReportedUsage {
            tokens: self.tokens.clone(),
            speed: self.speed.clone(),
            provider_cost: None,
            raw: self
                .raw
                .as_ref()
                .map(|raw| RawJson::new(Value::Object(raw.clone()).to_string())),
        }
    }
}

/// The decoder of one response.
pub(crate) struct Decoder {
    /// The request as it was sent: an error in the stream is classified
    /// against it, to tell a speed tier's refusal from the model's.
    request: ModelRequest,
    started: bool,
    ended: bool,
    blocks: BTreeMap<u32, Block>,
    /// The index of every block that has started, open or not. An index is
    /// a block's name for the whole response: it is never used twice.
    seen: std::collections::BTreeSet<u32>,
    usage: UsageSoFar,
    /// The stop reason, its detail, once `message_delta` has given them.
    stop: Option<(String, Option<String>)>,
}

impl Decoder {
    pub(crate) fn new(request: &ModelRequest) -> Self {
        // Only what classification reads is kept: not the conversation.
        let mut kept = ModelRequest::new(request.model.clone(), request.role.clone());
        kept.speed = request.speed.clone();
        Self {
            request: kept,
            started: false,
            ended: false,
            blocks: BTreeMap::new(),
            seen: std::collections::BTreeSet::new(),
            usage: UsageSoFar::default(),
            stop: None,
        }
    }

    fn message_start(&mut self, raw: &RawValue, out: &mut EventSink) -> Result<(), DecodeError> {
        if self.started {
            return Err(DecodeError::new(
                "the response stream started a second message",
            ));
        }
        let message: MessageStart<'_> = serde_json::from_str(raw.get()).map_err(|error| {
            DecodeError::new(format!(
                "the `message_start` event could not be read: {error}"
            ))
        })?;
        self.started = true;
        out.start(
            message.id.map(Cow::into_owned),
            message.model.as_deref().unwrap_or_default(),
        );
        if let Some(usage) = message.usage {
            self.usage.merge(usage)?;
            out.usage(self.usage.reported());
        }
        Ok(())
    }

    fn block_start(
        &mut self,
        index: u32,
        raw: &RawValue,
        out: &mut EventSink,
    ) -> Result<(), DecodeError> {
        let bad = |error: serde_json::Error| {
            DecodeError::new(format!("the block {index} could not be read: {error}"))
        };
        // Whoever reads the events keys a block by its index. A second
        // block under an index that was used would overwrite the first.
        if !self.seen.insert(index) {
            return Err(DecodeError::new(format!("the block {index} started twice")));
        }
        let members = ordered_members(raw).map_err(bad)?;
        let text = |name: &str| -> Result<String, DecodeError> {
            match members.iter().find(|(key, _)| key == name) {
                Some((_, value)) if value.get() != "null" => {
                    serde_json::from_str(value.get()).map_err(bad)
                }
                _ => Ok(String::new()),
            }
        };
        let kind = text("type")?;
        let shape = match kind.as_str() {
            "text" => {
                out.block_start(index, BlockKind::Text);
                let text = text("text")?;
                if !text.is_empty() {
                    out.text_delta(index, text.as_str());
                }
                Shape::Text {
                    text,
                    citations: Vec::new(),
                }
            }
            "thinking" => {
                out.block_start(index, BlockKind::Reasoning);
                let thinking = text("thinking")?;
                if !thinking.is_empty() {
                    out.reasoning_delta(index, thinking.as_str());
                }
                Shape::Thinking {
                    thinking,
                    signature: text("signature")?,
                }
            }
            "redacted_thinking" => {
                out.block_start(index, BlockKind::Reasoning);
                Shape::Whole
            }
            "tool_use" => {
                out.block_start(index, BlockKind::ToolCall);
                out.tool_call_start(index, text("id")?, text("name")?);
                Shape::Call {
                    input: String::new(),
                }
            }
            _ => {
                out.block_start(index, BlockKind::ProviderBlock);
                // A server-side tool's call streams its arguments like any
                // other call, and may start without an `input` member. A
                // block that arrives whole has nothing added to it, and
                // then this shape leaves it exactly as it started.
                Shape::Provider {
                    input: String::new(),
                    laid: Vec::new(),
                }
            }
        };
        self.blocks.insert(index, Block { members, shape });
        Ok(())
    }

    fn block_delta(
        &mut self,
        index: u32,
        raw: &RawValue,
        out: &mut EventSink,
    ) -> Result<(), DecodeError> {
        let Some(block) = self.blocks.get_mut(&index) else {
            return Err(DecodeError::new(format!(
                "the stream added to the block {index}, which has not started"
            )));
        };
        let delta: BlockDelta<'_> = serde_json::from_str(raw.get()).map_err(|error| {
            DecodeError::new(format!(
                "an addition to the block {index} could not be read: {error}"
            ))
        })?;
        let Block { members, shape } = block;
        match (delta.kind.as_deref().unwrap_or_default(), shape) {
            ("text_delta", Shape::Text { text, .. }) => {
                let more = delta.text.as_deref().unwrap_or_default();
                if !more.is_empty() {
                    text.push_str(more);
                    out.text_delta(index, more);
                }
            }
            ("citations_delta", Shape::Text { citations, .. }) => {
                if let Some(citation) = delta.citation {
                    citations.push(citation.to_owned());
                }
            }
            ("thinking_delta", Shape::Thinking { thinking, .. }) => {
                let more = delta.thinking.as_deref().unwrap_or_default();
                if !more.is_empty() {
                    thinking.push_str(more);
                    out.reasoning_delta(index, more);
                }
            }
            ("signature_delta", Shape::Thinking { signature, .. }) => {
                signature.push_str(delta.signature.as_deref().unwrap_or_default());
            }
            ("input_json_delta", Shape::Call { input }) => {
                let more = delta.partial_json.as_deref().unwrap_or_default();
                if !more.is_empty() {
                    input.push_str(more);
                    out.tool_args_delta(index, more);
                }
            }
            // A server-side tool's arguments are the provider's business:
            // they are kept for the stored block and not handed to the loop.
            ("input_json_delta", Shape::Provider { input, .. }) => {
                input.push_str(delta.partial_json.as_deref().unwrap_or_default());
            }
            // An addition of a kind this build does not know, to a block of
            // a kind it does not know either: laid over the block, so that
            // the stored block has what the provider sent.
            (_, Shape::Provider { laid, .. }) => {
                lay(laid, members, raw).map_err(|error| {
                    DecodeError::new(format!(
                        "an addition to the block {index} could not be read: {error}"
                    ))
                })?;
            }
            // An addition of an unknown kind to text, reasoning or a tool
            // call, or one that does not belong to the block's kind:
            // skipped, like an event of an unknown type. Where it would go
            // in such a block is not something to guess at.
            _ => {}
        }
        Ok(())
    }

    fn block_stop(&mut self, index: u32, out: &mut EventSink) -> Result<(), DecodeError> {
        let Some(block) = self.blocks.remove(&index) else {
            return Err(DecodeError::new(format!(
                "the stream ended the block {index}, which has not started"
            )));
        };
        out.block_end(index, RawJson::new(block.whole()));
        Ok(())
    }

    fn message_delta(
        &mut self,
        delta: Option<&RawValue>,
        usage: Option<&RawValue>,
        out: &mut EventSink,
    ) -> Result<(), DecodeError> {
        if let Some(delta) = delta.filter(|delta| delta.get() != "null") {
            let delta: MessageDelta<'_> = serde_json::from_str(delta.get()).map_err(|error| {
                DecodeError::new(format!(
                    "the `message_delta` event could not be read: {error}"
                ))
            })?;
            if let Some(reason) = delta.stop_reason {
                let detail = delta
                    .stop_details
                    .and_then(|details| details.category)
                    .or(delta.stop_sequence)
                    .map(Cow::into_owned);
                self.stop = Some((reason.into_owned(), detail));
            }
        }
        if let Some(usage) = usage {
            self.usage.merge(usage)?;
            out.usage(self.usage.reported());
        }
        Ok(())
    }

    fn message_stop(&mut self, out: &mut EventSink) -> Result<(), DecodeError> {
        let Some((raw_stop, detail)) = self.stop.take() else {
            return Err(DecodeError::new(
                "the response ended without saying why it stopped",
            ));
        };
        // A block the provider never closed is closed here, with what
        // arrived of it, so that nothing that was sent is lost.
        while let Some((index, block)) = self.blocks.pop_first() {
            out.block_end(index, RawJson::new(block.whole()));
        }
        self.ended = true;
        out.end(
            stop_reason(&raw_stop),
            raw_stop,
            detail,
            self.usage.reported(),
        );
        Ok(())
    }
}

/// The provider's stop reasons, in the neutral set.
///
/// | The provider's | Neutral | |
/// |---|---|---|
/// | `end_turn` | `end` | |
/// | `stop_sequence` | `end` | The sequence is the stop's detail. The harness sends none. |
/// | `tool_use` | `tool_calls` | |
/// | `max_tokens` | `max_output` | |
/// | `model_context_window_exceeded` | `context_overflow` | The response filled the context window. The loop compacts and asks again, as it does for a request that did not fit. |
/// | `pause_turn` | `paused` | |
/// | `refusal` | `refusal` | The category is the stop's detail. |
/// | anything else | itself | An unknown value is passed on under its own name, never guessed at. |
pub(crate) fn stop_reason(raw: &str) -> StopReason {
    match raw {
        "end_turn" | "stop_sequence" => StopReason::End,
        "tool_use" => StopReason::ToolCalls,
        "max_tokens" => StopReason::MaxOutput,
        "model_context_window_exceeded" => StopReason::ContextOverflow,
        "pause_turn" => StopReason::Paused,
        "refusal" => StopReason::Refusal,
        other => StopReason::Unknown(format!("anthropic:{other}")),
    }
}

impl Block {
    /// The whole block as JSON text: its members as they started, in their
    /// order, with the streamed ones replaced by what the stream added up
    /// to.
    fn whole(self) -> String {
        let mut out = String::new();
        let mut object = Object::open(&mut out);
        // What the stream added, by the member it belongs to.
        let mut text_member = None;
        let mut extra: Vec<(&str, String)> = Vec::new();
        match &self.shape {
            Shape::Text { text, citations } => {
                text_member = Some(("text", text.as_str()));
                if !citations.is_empty() {
                    let mut list = String::new();
                    let mut array = Array::open(&mut list);
                    // The citations the block started with come first.
                    let started: Vec<Box<RawValue>> = self
                        .members
                        .iter()
                        .find(|(key, _)| key == "citations")
                        .and_then(|(_, value)| serde_json::from_str(value.get()).ok())
                        .unwrap_or_default();
                    for citation in started.iter().chain(citations) {
                        array.item().push_str(citation.get());
                    }
                    array.close();
                    extra.push(("citations", list));
                }
            }
            Shape::Thinking {
                thinking,
                signature,
            } => {
                text_member = Some(("thinking", thinking.as_str()));
                let mut written = String::new();
                json::string(&mut written, signature);
                extra.push(("signature", written));
            }
            Shape::Call { input } | Shape::Provider { input, .. } => {
                if json::is_object(input) {
                    extra.push(("input", input.trim().to_owned()));
                } else if !input.trim().is_empty() {
                    let mut wrapped = String::new();
                    let mut wrapper = Object::open(&mut wrapped);
                    wrapper.str(INVALID_INPUT_KEY, input);
                    wrapper.close();
                    extra.push(("input", wrapped));
                }
                // With nothing streamed, the block keeps the arguments it
                // started with.
                if let Shape::Provider { laid, .. } = &self.shape {
                    for (key, value) in laid {
                        let written = match value {
                            Laid::Text(text) => {
                                let mut written = String::new();
                                json::string(&mut written, text);
                                written
                            }
                            Laid::Raw(raw) => raw.get().to_owned(),
                        };
                        extra.push((key, written));
                    }
                }
            }
            Shape::Whole => {}
        }

        let mut written: Vec<&str> = Vec::new();
        for (key, value) in &self.members {
            written.push(key);
            if let Some((name, text)) = text_member
                && key == name
            {
                object.str(key, text);
            } else if let Some((_, replaced)) = extra.iter().find(|(name, _)| name == key) {
                object.raw(key, replaced);
            } else {
                object.raw(key, value.get());
            }
        }
        // A streamed member the starting block did not have comes last.
        if let Some((name, text)) = text_member
            && !written.contains(&name)
        {
            object.str(name, text);
        }
        for (name, replaced) in &extra {
            if !written.contains(name) {
                object.raw(name, replaced);
            }
        }
        object.close();
        out
    }
}

impl StreamDecoder for Decoder {
    fn feed(&mut self, event: SseEvent<'_>, out: &mut EventSink) -> Result<(), DecodeError> {
        if self.ended {
            return Ok(());
        }
        let envelope: Envelope<'_> = match serde_json::from_str(event.data) {
            Ok(envelope) => envelope,
            // Data that is not what an event's data is. For an event this
            // decoder reads, the stream is damaged. For any other, it is
            // an event of a kind this build does not know.
            Err(error) if KNOWN_EVENTS.contains(&event.event) => {
                return Err(DecodeError::new(format!(
                    "the `{}` event could not be read: {error}",
                    event.event
                )));
            }
            Err(_) => return Ok(()),
        };
        let kind = envelope.kind.as_deref().unwrap_or(event.event);

        if kind == "error" {
            // The `error` member is read by the classification, from the
            // whole of the event's data.
            out.error(classify::error(
                None,
                None,
                event.data.as_bytes(),
                UtcTime::UNIX_EPOCH,
                &self.request,
            ));
            self.ended = true;
            return Ok(());
        }
        if !KNOWN_EVENTS.contains(&kind) {
            return Ok(());
        }
        if kind == "message_start" {
            let Some(message) = envelope.message else {
                return Err(DecodeError::new("a `message_start` event has no message"));
            };
            return self.message_start(message, out);
        }
        if !self.started {
            return Err(DecodeError::new(format!(
                "the response stream sent `{kind}` before it started a message"
            )));
        }
        let index = || {
            envelope
                .index
                .ok_or_else(|| DecodeError::new(format!("a `{kind}` event has no index")))
        };
        match kind {
            "content_block_start" => {
                let Some(block) = envelope.content_block else {
                    return Err(DecodeError::new(
                        "a `content_block_start` event has no block",
                    ));
                };
                self.block_start(index()?, block, out)
            }
            "content_block_delta" => {
                let Some(delta) = envelope.delta else {
                    return Err(DecodeError::new(
                        "a `content_block_delta` event has no delta",
                    ));
                };
                self.block_delta(index()?, delta, out)
            }
            "content_block_stop" => self.block_stop(index()?, out),
            "message_delta" => self.message_delta(envelope.delta, envelope.usage, out),
            _ => self.message_stop(out),
        }
    }

    fn finish(&mut self, _out: &mut EventSink) -> Result<(), DecodeError> {
        // Nothing is held back: every event was handed on as it was read.
        // A stream that ends before `message_stop` is the client's to
        // report, as an incomplete stream.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(json: &str) -> Vec<(String, Box<RawValue>)> {
        let raw: Box<RawValue> = serde_json::from_str(json).unwrap();
        ordered_members(&raw).unwrap()
    }

    fn whole(start: &str, shape: Shape) -> String {
        Block {
            members: members(start),
            shape,
        }
        .whole()
    }

    #[test]
    fn members_keep_their_order_and_their_text() {
        let read = members(r#"{"type":"x", "b" : 1.50,"a":{"z":1,"y":[2, 3]},"n":null}"#);
        let keys: Vec<&str> = read.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["type", "b", "a", "n"]);
        assert_eq!(read[1].1.get(), "1.50", "a number is not written again");
        assert_eq!(read[2].1.get(), r#"{"z":1,"y":[2, 3]}"#);
        // A key written twice keeps its place and its last value.
        let twice = members(r#"{"a":1,"b":2,"a":3}"#);
        assert_eq!(twice.len(), 2);
        assert_eq!(twice[0].1.get(), "3");
        // Only an object has members.
        let array: Box<RawValue> = serde_json::from_str("[1]").unwrap();
        assert!(ordered_members(&array).is_err());
    }

    #[test]
    fn a_text_block_is_the_start_with_its_text_filled_in() {
        let block = whole(
            r#"{"type":"text","text":"","citations":null}"#,
            Shape::Text {
                text: "Hello \"you\"\n".to_owned(),
                citations: Vec::new(),
            },
        );
        assert_eq!(
            block,
            r#"{"type":"text","text":"Hello \"you\"\n","citations":null}"#
        );
    }

    #[test]
    fn citations_are_added_after_the_ones_the_block_started_with() {
        let one: Box<RawValue> = serde_json::from_str(r#"{"type":"char_location","n":1}"#).unwrap();
        let two: Box<RawValue> = serde_json::from_str(r#"{"type":"char_location","n":2}"#).unwrap();
        let block = whole(
            r#"{"type":"text","text":"","citations":[{"type":"char_location","n":0}]}"#,
            Shape::Text {
                text: "cited".to_owned(),
                citations: vec![one, two],
            },
        );
        assert_eq!(
            block,
            r#"{"type":"text","text":"cited","citations":[{"type":"char_location","n":0},{"type":"char_location","n":1},{"type":"char_location","n":2}]}"#
        );
    }

    #[test]
    fn a_thinking_block_gets_its_text_and_its_signature() {
        let block = whole(
            r#"{"type":"thinking","thinking":"","signature":""}"#,
            Shape::Thinking {
                thinking: String::new(),
                signature: "EqQBCgIYAhIM+/=".to_owned(),
            },
        );
        assert_eq!(
            block,
            r#"{"type":"thinking","thinking":"","signature":"EqQBCgIYAhIM+/="}"#
        );
        // A member the start did not have is added at the end.
        let block = whole(
            r#"{"type":"thinking"}"#,
            Shape::Thinking {
                thinking: "hm".to_owned(),
                signature: "sig".to_owned(),
            },
        );
        assert_eq!(
            block,
            r#"{"type":"thinking","thinking":"hm","signature":"sig"}"#
        );
    }

    #[test]
    fn a_tool_call_keeps_the_arguments_as_the_model_wrote_them() {
        let start = r#"{"type":"tool_use","id":"toolu_1","name":"read","input":{}}"#;
        let call = |input: &str| {
            whole(
                start,
                Shape::Call {
                    input: input.to_owned(),
                },
            )
        };
        // The model's own spacing, escapes and number forms survive.
        assert_eq!(
            call(r#"{"path": "a\/b",  "n": 1.0}"#),
            r#"{"type":"tool_use","id":"toolu_1","name":"read","input":{"path": "a\/b",  "n": 1.0}}"#
        );
        // Nothing streamed: the arguments the block started with.
        assert_eq!(call(""), start);
        // Text that is not an object is carried inside one.
        assert_eq!(
            call(r#"{"path": "a"#),
            r#"{"type":"tool_use","id":"toolu_1","name":"read","input":{"INVALID_JSON":"{\"path\": \"a"}}"#
        );
        assert_eq!(
            call("[1]"),
            r#"{"type":"tool_use","id":"toolu_1","name":"read","input":{"INVALID_JSON":"[1]"}}"#
        );
        for block in [call(r#"{"path": "a"#), call("[1]"), call("{}")] {
            assert!(json::is_object(&block), "{block}");
        }
    }

    #[test]
    fn additions_of_an_unknown_kind_are_laid_over_a_block_of_an_unknown_kind() {
        let raw = |json: &str| -> Box<RawValue> { serde_json::from_str(json).unwrap() };
        let start = r#"{"type":"compaction","content":null,"note":"so far: ","n":1}"#;
        let mut laid = Vec::new();
        let started = members(start);
        // Text replaces a null, is appended to text the block started
        // with, and to text an earlier addition brought.
        lay(
            &mut laid,
            &started,
            &raw(r#"{"type":"compaction_delta","content":"The user asked "}"#),
        )
        .unwrap();
        lay(
            &mut laid,
            &started,
            &raw(r#"{"type":"compaction_delta","content":"about \"Oslo\".","note":"one"}"#),
        )
        .unwrap();
        // Any other value replaces what was there, and a new member is added.
        lay(
            &mut laid,
            &started,
            &raw(r#"{"type":"other_delta","n":2,"extra":{"b":1,"a":[1.50]}}"#),
        )
        .unwrap();
        lay(&mut laid, &started, &raw(r#"{"type":"other_delta","n":3}"#)).unwrap();
        let block = whole(
            start,
            Shape::Provider {
                input: String::new(),
                laid,
            },
        );
        assert_eq!(
            block,
            r#"{"type":"compaction","content":"The user asked about \"Oslo\".","note":"so far: one","n":3,"extra":{"b":1,"a":[1.50]}}"#
        );
        // The addition's own `type` never becomes the block's.
        assert!(block.starts_with(r#"{"type":"compaction""#));
        // Nothing added: exactly what started.
        assert_eq!(
            whole(
                start,
                Shape::Provider {
                    input: String::new(),
                    laid: Vec::new()
                }
            ),
            start
        );
        assert!(lay(&mut Vec::new(), &started, &raw("[1]")).is_err());
    }

    #[test]
    fn a_block_that_arrives_whole_is_what_was_sent() {
        let start = r#"{"type":"web_search_tool_result","tool_use_id":"srvtoolu_1","content":[{"type":"web_search_result","url":"https://example.com","page_age":null}]}"#;
        assert_eq!(whole(start, Shape::Whole), start);
    }

    #[test]
    fn every_stop_reason_has_its_neutral_form() {
        let rows = [
            ("end_turn", StopReason::End),
            ("stop_sequence", StopReason::End),
            ("tool_use", StopReason::ToolCalls),
            ("max_tokens", StopReason::MaxOutput),
            ("model_context_window_exceeded", StopReason::ContextOverflow),
            ("pause_turn", StopReason::Paused),
            ("refusal", StopReason::Refusal),
        ];
        for (raw, neutral) in rows {
            assert_eq!(stop_reason(raw), neutral, "{raw}");
        }
        // A value added later is neither an error nor mistaken for one of
        // the neutral values, whatever it is called.
        for raw in ["compaction", "error", "interrupted", "end"] {
            let neutral = stop_reason(raw);
            assert!(!neutral.is_known(), "{raw}: {neutral:?}");
            assert_eq!(neutral.as_str(), format!("anthropic:{raw}"));
        }
    }

    #[test]
    fn usage_counts_are_replaced_and_missing_ones_stand() {
        let raw = |json: &str| -> Box<RawValue> { serde_json::from_str(json).unwrap() };
        let mut usage = UsageSoFar::default();
        usage
            .merge(&raw(
                r#"{"input_tokens":25,"cache_creation_input_tokens":1200,"cache_read_input_tokens":5000,"cache_creation":{"ephemeral_5m_input_tokens":200,"ephemeral_1h_input_tokens":1000},"output_tokens":1,"service_tier":"standard","speed":"fast","inference_geo":"global"}"#,
            ))
            .unwrap();
        usage
            .merge(&raw(
                r#"{"input_tokens":null,"output_tokens":340,"output_tokens_details":{"thinking_tokens":120},"server_tool_use":null}"#,
            ))
            .unwrap();
        usage.merge(&raw("null")).unwrap();
        let reported = usage.reported();
        assert_eq!(
            reported.tokens,
            TokenUsage {
                input: 25,
                cache_read: 5000,
                cache_write: 1200,
                cache_write_classes: vec![
                    CacheWriteTokens {
                        class: "1h".to_owned(),
                        tokens: 1000
                    },
                    CacheWriteTokens {
                        class: "5m".to_owned(),
                        tokens: 200
                    },
                ],
                output: 340,
                reasoning: Some(120),
            }
        );
        assert_eq!(reported.speed, Some(Speed::Fast));
        let kept: Value = serde_json::from_str(reported.raw.unwrap().as_str()).unwrap();
        assert_eq!(kept["input_tokens"], 25, "a null does not erase a count");
        assert_eq!(kept["output_tokens"], 340);
        assert_eq!(kept["output_tokens_details"]["thinking_tokens"], 120);
        assert_eq!(kept["service_tier"], "standard");

        // A class with no tokens is not a class that was written, and a
        // response that says nothing of the speed reports none.
        let mut usage = UsageSoFar::default();
        usage
            .merge(&raw(
                r#"{"input_tokens":3,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0},"output_tokens":2}"#,
            ))
            .unwrap();
        assert!(usage.reported().tokens.cache_write_classes.is_empty());
        assert_eq!(usage.reported().speed, None);
        assert!(usage.merge(&raw(r#"{"input_tokens":"many"}"#)).is_err());
    }
}
