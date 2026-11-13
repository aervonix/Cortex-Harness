//! Building the request body.
//!
//! [`render`] turns a [`ModelRequest`] into a [`Body`]: the JSON text of the
//! request, kept in two parts so that the part a provider caches can be
//! looked at by itself.
//!
//! # The order of the body
//!
//! ```text
//! {"model", "max_tokens", "stream", "thinking", "output_config", "speed",
//!  "cache_control", "tools", "system", "messages": [ … ]}
//! ```
//!
//! Everything before the messages is the same bytes for every request of a
//! context epoch, and the messages only grow at the end. So the body of one
//! request, without its closing brackets, is the beginning of the next one:
//! [`Body::prefix`]. The provider's cache follows the same order (tools,
//! then the system prompt, then the messages), and so does the check it
//! runs on reasoning blocks that are sent back, which fails a request whose
//! earlier content changed.
//!
//! # Cache markers
//!
//! | Hint | Marker |
//! |---|---|
//! | [`stable_prefix`](crate::CacheHints::stable_prefix) | `cache_control` on the system prompt, which covers the tools before it. On the last tool when there is no system prompt. |
//! | [`history`](crate::CacheHints::history) | `cache_control` at the top level: the provider marks the last block of the request itself, so nothing inside the messages moves from one request to the next. |
//! | the same, after a long round | One more marker, on the block the request before this one ended with. See [`Body::finish`]. |
//!
//! # Replay
//!
//! An assistant message is sent back in one of two ways.
//!
//! - **Raw.** Each part's stored block, as the text that was stored. This is
//!   what keeps a signature valid. It applies to a message this provider
//!   produced through this protocol.
//! - **The neutral projection.** Text and tool calls written from the
//!   neutral fields; reasoning and provider blocks left out. It applies to
//!   every other message.
//!
//! Whether a message of the same provider is replayed raw to **another
//! model** is the provider's flag `replay_reasoning` (see the
//! [module](super)).

use cortex_protocol::model::{Modality, ReasoningControlKind};
use cortex_protocol::message::ImagePart;
use cortex_protocol::{Message, Part, RawJson, Role};

use super::json::{self, Array, Object};
use super::{NAME, flags};
use crate::catalog::ToolSchemaStyle;
use crate::error::BuildError;
use crate::protocol::ProviderInfo;
use crate::request::{ModelRequest, ReasoningVisibility, is_valid_tool_name};

/// How many positions the provider looks back from a cache marker for an
/// entry an earlier request wrote, the marker's own block included.
pub(crate) const LOOKBACK: usize = 20;

/// What stands where an image was, for a model that takes none.
const IMAGE_PLACEHOLDER: &str = "[An image was here. This model does not take images.]";

/// The key under which a tool call's arguments are sent back when the model
/// wrote text that is not a JSON object. It is the key the provider's own
/// documentation uses for input that could not be read.
pub(crate) const INVALID_INPUT_KEY: &str = "INVALID_JSON";

/// A request body, in the two parts described in the [module](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Body {
    /// Everything up to and including the opening of the messages array.
    head: String,
    /// The messages, without any cache marker.
    messages: Vec<WireMessage>,
    /// The cache marker to put inside the messages after a long round, when
    /// the request asked for its history to be cached.
    marker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WireMessage {
    role: &'static str,
    blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Block {
    /// The block: one JSON object.
    json: String,
    kind: BlockKind,
    /// Whether the block was written here, from neutral fields, and so can
    /// be given a cache marker by adding a member to it.
    ours: bool,
}

/// What the provider's lookback counts as one position: a run of tool calls
/// is one, a run of tool results is one, and any other block is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    ToolCall,
    ToolResult,
    Other,
}

impl Body {
    /// The part of the body that later requests of the same context epoch
    /// repeat byte for byte: everything up to the end of the last message,
    /// without the closing brackets and without any cache marker inside the
    /// messages.
    ///
    /// For two requests of one epoch, the earlier one's prefix is the
    /// beginning of the later one's. A test in the crate's suite checks it
    /// over a recorded conversation.
    pub(crate) fn prefix(&self) -> String {
        self.write(None)
    }

    /// The whole body.
    ///
    /// **The marker after a long round.** The provider writes a cache entry
    /// only where a marker is, and when it reads, it looks back
    /// [`LOOKBACK`] positions from a marker for an entry an earlier request
    /// wrote. The top-level marker lands on the last block of each request,
    /// so the request before this one wrote its entry on the last block
    /// before the newest assistant message. When this request has added
    /// more positions than the lookback reaches, that entry would be
    /// missed and the whole conversation written again. One explicit marker
    /// on that block makes the provider look there too.
    ///
    /// It is the only marker that is ever inside the messages. It is not
    /// content: the provider's cache and its check of reasoning blocks both
    /// ignore markers, so it moves freely.
    pub(crate) fn finish(&self) -> String {
        let mut out = self.write(self.long_round());
        out.push_str("]}");
        out
    }

    fn write(&self, marked: Option<(usize, usize)>) -> String {
        let size: usize = self
            .messages
            .iter()
            .flat_map(|message| &message.blocks)
            .map(|block| block.json.len() + 1)
            .sum();
        let mut out = String::with_capacity(self.head.len() + size + 64 * self.messages.len());
        // The head ends with the opening bracket of the messages array.
        out.push_str(&self.head);
        for (at, message) in self.messages.iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
            let mut object = Object::open(&mut out);
            object.str("role", message.role);
            let mut content = Array::open(object.key("content"));
            for (block_at, block) in message.blocks.iter().enumerate() {
                let item = content.item();
                match (&self.marker, marked) {
                    (Some(marker), Some(place)) if place == (at, block_at) => {
                        // The block ends with its closing brace: the marker
                        // becomes its last member.
                        item.push_str(&block.json[..block.json.len() - 1]);
                        item.push_str(",\"cache_control\":");
                        item.push_str(marker);
                        item.push('}');
                    }
                    _ => item.push_str(&block.json),
                }
            }
            content.close();
            object.close();
        }
        out
    }

    /// Where the marker after a long round goes: the message and block the
    /// request before this one ended with, when this request's own end is
    /// further from it than the lookback reaches.
    fn long_round(&self) -> Option<(usize, usize)> {
        self.marker.as_ref()?;
        let newest_assistant = self
            .messages
            .iter()
            .rposition(|message| message.role == "assistant")?;
        // The last block before it that a marker can be put on.
        let (message_at, block_at) = self.messages[..newest_assistant]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(at, message)| {
                let block_at = message.blocks.iter().rposition(|block| block.ours)?;
                Some((at, block_at))
            })?;

        // Positions from the end of the request back to that block,
        // counting both ends.
        let mut positions = 0;
        let mut last_kind = None;
        let after = self.messages[message_at].blocks[block_at..]
            .iter()
            .chain(
                self.messages[message_at + 1..]
                    .iter()
                    .flat_map(|message| &message.blocks),
            );
        for block in after {
            let same_run = block.kind != BlockKind::Other && last_kind == Some(block.kind);
            if !same_run {
                positions += 1;
            }
            last_kind = Some(block.kind);
        }
        (positions > LOOKBACK).then_some((message_at, block_at))
    }
}

/// Renders a request. See the [module](self) for the body's order.
pub(crate) fn render(request: &ModelRequest, provider: &ProviderInfo) -> Result<Body, BuildError> {
    let info = &request.model.info;
    if info.protocol != NAME {
        return Err(BuildError::new(format!(
            "the model `{}` speaks `{}`, not `{NAME}`",
            info.id, info.protocol
        )));
    }

    let marker = cache_marker(request);
    let mut head = String::with_capacity(4096 + request.system.len());
    let mut body = Object::open(&mut head);
    body.str("model", info.id.as_str());
    let max_tokens = request
        .max_output
        .map_or(info.max_output, |asked| asked.min(info.max_output));
    body.number("max_tokens", max_tokens);
    body.yes("stream");
    thinking(&mut body, request, max_tokens)?;
    output_config(&mut body, request)?;
    speed(&mut body, request)?;
    if request.cache.history {
        body.raw("cache_control", &marker);
    }
    tools(&mut body, request, provider, &marker)?;
    if !request.system.is_empty() {
        let mut system = Array::open(body.key("system"));
        let mut block = Object::open(system.item());
        block.str("type", "text");
        block.str("text", &request.system);
        if request.cache.stable_prefix {
            block.raw("cache_control", &marker);
        }
        block.close();
        system.close();
    }
    // The messages follow the head. `Body::write` writes them and `finish`
    // closes the array and the body.
    body.key("messages").push('[');

    let mut messages = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        if let Some(wire) = wire_message(message, request, provider)? {
            messages.push(wire);
        }
    }
    Ok(Body {
        head,
        messages,
        marker: request.cache.history.then_some(marker),
    })
}

/// The cache marker of this request: `{"type":"ephemeral"}`, with the
/// lifetime class when the request names one.
fn cache_marker(request: &ModelRequest) -> String {
    let mut out = String::new();
    let mut marker = Object::open(&mut out);
    marker.str("type", "ephemeral");
    if let Some(lifetime) = &request.cache.lifetime {
        marker.str("ttl", lifetime);
    }
    marker.close();
    out
}

/// The reasoning control, as the model's own kind of control wants it.
///
/// The visibility is always sent, so that what a response shows does not
/// depend on a model's default: `summarized` or `omitted`.
fn thinking(
    body: &mut Object<'_>,
    request: &ModelRequest,
    max_tokens: u64,
) -> Result<(), BuildError> {
    let info = &request.model.info;
    let display = match request.reasoning.visibility {
        ReasoningVisibility::Summary => "summarized",
        ReasoningVisibility::None => "omitted",
    };
    let level = request.reasoning.level.as_deref();
    let budget = request.reasoning.budget_tokens;
    match &info.reasoning.kind {
        ReasoningControlKind::Levels => {
            if budget.is_some() {
                return Err(BuildError::new(format!(
                    "the model `{}` takes a reasoning level, not a token budget",
                    info.id
                )));
            }
            if let Some(level) = level
                && !info.reasoning.levels.iter().any(|known| known == level)
            {
                return Err(BuildError::new(format!(
                    "the model `{}` has no reasoning level `{level}`; its levels are {}",
                    info.id,
                    info.reasoning.levels.join(", ")
                )));
            }
            let mut thinking = Object::open(body.key("thinking"));
            thinking.str("type", "adaptive");
            thinking.str("display", display);
            thinking.close();
        }
        ReasoningControlKind::Budget => {
            if level.is_some() {
                return Err(BuildError::new(format!(
                    "the model `{}` takes a reasoning budget in tokens, not a level",
                    info.id
                )));
            }
            if let Some(budget) = budget {
                let min = info.reasoning.min_tokens.unwrap_or(0);
                let max = info.reasoning.max_tokens.unwrap_or(u64::MAX);
                if budget < min || budget > max || budget >= max_tokens {
                    return Err(BuildError::new(format!(
                        "a reasoning budget of {budget} tokens is outside what the model `{}` \
                         takes ({min} to {max}), or is not below the {max_tokens} output tokens \
                         of this request",
                        info.id
                    )));
                }
                let mut thinking = Object::open(body.key("thinking"));
                thinking.str("type", "enabled");
                thinking.number("budget_tokens", budget);
                thinking.str("display", display);
                thinking.close();
            }
        }
        _ => {
            if level.is_some() || budget.is_some() {
                return Err(BuildError::new(format!(
                    "the model `{}` has no reasoning control",
                    info.id
                )));
            }
        }
    }
    Ok(())
}

/// The reasoning level and the output schema, which share one object.
fn output_config(body: &mut Object<'_>, request: &ModelRequest) -> Result<(), BuildError> {
    let info = &request.model.info;
    let level = match info.reasoning.kind {
        ReasoningControlKind::Levels => request.reasoning.level.as_deref(),
        _ => None,
    };
    if request.output_schema.is_some() && !info.capabilities.structured_output {
        return Err(BuildError::new(format!(
            "the model `{}` cannot be held to an output schema",
            info.id
        )));
    }
    if level.is_none() && request.output_schema.is_none() {
        return Ok(());
    }
    let mut config = Object::open(body.key("output_config"));
    if let Some(level) = level {
        config.str("effort", level);
    }
    if let Some(schema) = &request.output_schema {
        let mut format = Object::open(config.key("format"));
        format.str("type", "json_schema");
        format.value("schema", schema);
        format.close();
    }
    config.close();
    Ok(())
}

/// The speed tier. A model that offers tiers is always told which one this
/// request wants, `standard` included, so that nothing depends on a default
/// at the provider. A model that offers none has no such choice, and the
/// field is left out.
fn speed(body: &mut Object<'_>, request: &ModelRequest) -> Result<(), BuildError> {
    let info = &request.model.info;
    if !info.offers(&request.speed) {
        return Err(BuildError::new(format!(
            "the model `{}` does not offer the speed `{}`",
            info.id,
            request.speed.as_str()
        )));
    }
    if sends_speed(request) {
        body.str("speed", request.speed.as_str());
    }
    Ok(())
}

/// Whether the request names a speed tier at all.
pub(crate) fn sends_speed(request: &ModelRequest) -> bool {
    !request.model.info.speeds.is_empty()
}

fn tools(
    body: &mut Object<'_>,
    request: &ModelRequest,
    provider: &ProviderInfo,
    marker: &str,
) -> Result<(), BuildError> {
    if request.tools.is_empty() {
        return Ok(());
    }
    let strict = request.variants.tool_schema == ToolSchemaStyle::Strict;
    let eager = flags::eager_tool_streaming(provider);
    // With no system prompt to carry it, the marker for the stable prefix
    // goes on the last tool.
    let marked = (request.cache.stable_prefix && request.system.is_empty())
        .then(|| request.tools.len() - 1);
    let mut list = Array::open(body.key("tools"));
    for (at, tool) in request.tools.iter().enumerate() {
        if !is_valid_tool_name(&tool.name) {
            return Err(BuildError::new(format!(
                "`{}` is not a tool name every provider accepts",
                tool.name
            )));
        }
        let mut object = Object::open(list.item());
        object.str("name", &tool.name);
        object.str("description", &tool.description);
        object.value("input_schema", &tool.schema);
        if strict {
            object.yes("strict");
        }
        if eager {
            object.yes("eager_input_streaming");
        }
        if marked == Some(at) {
            object.raw("cache_control", marker);
        }
        object.close();
    }
    list.close();
    Ok(())
}

/// How an assistant message is sent back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replay {
    /// Each part's stored block, unchanged.
    Raw,
    /// The neutral fields. Reasoning and provider blocks are left out.
    Projection,
}

fn replay(message: &Message, request: &ModelRequest, provider: &ProviderInfo) -> Replay {
    let Some(origin) = &message.origin else {
        return Replay::Projection;
    };
    if origin.protocol != NAME || origin.provider != provider.id {
        return Replay::Projection;
    }
    let info = &request.model.info;
    let readable = origin.model == info.id || info.reads_reasoning_from.contains(&origin.model);
    if readable || flags::replays_every_block(provider) {
        Replay::Raw
    } else {
        Replay::Projection
    }
}

fn wire_message(
    message: &Message,
    request: &ModelRequest,
    provider: &ProviderInfo,
) -> Result<Option<WireMessage>, BuildError> {
    let mut blocks = Vec::with_capacity(message.parts.len());
    let role = match &message.role {
        Role::User | Role::Tool => {
            for part in &message.parts {
                match part {
                    Part::Text(text) => blocks.extend(text_block(&text.text)),
                    Part::Image(image) => blocks.push(image_block(image, request)?),
                    Part::ToolResult(result) => {
                        let mut json = String::new();
                        let mut object = Object::open(&mut json);
                        object.str("type", "tool_result");
                        object.str("tool_use_id", result.call_id.as_str());
                        let mut content = Vec::with_capacity(result.content.len());
                        for part in &result.content {
                            match part {
                                Part::Text(text) => content.extend(text_block(&text.text)),
                                Part::Image(image) => content.push(image_block(image, request)?),
                                other => return Err(misplaced(other, "a tool result")),
                            }
                        }
                        if !content.is_empty() {
                            let mut list = Array::open(object.key("content"));
                            for block in &content {
                                list.item().push_str(&block.json);
                            }
                            list.close();
                        }
                        if result.is_error {
                            object.yes("is_error");
                        }
                        object.close();
                        blocks.push(Block {
                            json,
                            kind: BlockKind::ToolResult,
                            ours: true,
                        });
                    }
                    other => return Err(misplaced(other, "a user message")),
                }
            }
            "user"
        }
        Role::Assistant => {
            let replay = replay(message, request, provider);
            for part in &message.parts {
                assistant_block(part, replay, &mut blocks)?;
            }
            "assistant"
        }
        other => {
            return Err(BuildError::new(format!(
                "a message has the role `{}`, which this protocol cannot send",
                other.as_str()
            )));
        }
    };
    // The provider refuses a message with no content. One whose parts were
    // all left out (reasoning alone, from another provider) is not sent.
    Ok((!blocks.is_empty()).then_some(WireMessage { role, blocks }))
}

fn misplaced(part: &Part, place: &str) -> BuildError {
    BuildError::new(format!(
        "a part of kind `{}` cannot be in {place}",
        part.tag()
    ))
}

/// A text block. The provider refuses empty text, so none is written for it.
fn text_block(text: &str) -> Option<Block> {
    if text.is_empty() {
        return None;
    }
    let mut json = String::with_capacity(text.len() + 32);
    let mut object = Object::open(&mut json);
    object.str("type", "text");
    object.str("text", text);
    object.close();
    Some(Block {
        json,
        kind: BlockKind::Other,
        ours: true,
    })
}

/// An image, inline. One that is still a reference to a stored artifact
/// cannot be sent: the client loads it before the request is built
/// ([`ImageSource`](crate::ImageSource)).
fn image_block(image: &ImagePart, request: &ModelRequest) -> Result<Block, BuildError> {
    if !request.model.info.input.contains(&Modality::Image) {
        return text_block(IMAGE_PLACEHOLDER)
            .ok_or_else(|| BuildError::new("the image placeholder is empty"));
    }
    let Some(data) = &image.data else {
        return Err(BuildError::new(match &image.artifact {
            Some(artifact) => format!(
                "the image in artifact `{artifact}` was not loaded before the request was built"
            ),
            None => "an image has neither data nor an artifact".to_owned(),
        }));
    };
    let mut json = String::with_capacity(data.len() + 96);
    let mut object = Object::open(&mut json);
    object.str("type", "image");
    let mut source = Object::open(object.key("source"));
    source.str("type", "base64");
    source.str("media_type", &image.media_type);
    source.str("data", data);
    source.close();
    object.close();
    Ok(Block {
        json,
        kind: BlockKind::Other,
        ours: true,
    })
}

/// The stored block of a part, when the message is replayed raw and the
/// block is one JSON object. A stored block that is anything else (a
/// damaged log, or text made to close the surrounding array and open
/// another message) is not sent: the part falls back to its neutral fields.
fn stored_block(raw: Option<&RawJson>, replay: Replay) -> Option<&str> {
    raw.filter(|_| replay == Replay::Raw)
        .map(RawJson::as_str)
        .filter(|text| json::is_object(text))
        .map(str::trim)
}

fn assistant_block(part: &Part, replay: Replay, blocks: &mut Vec<Block>) -> Result<(), BuildError> {
    match part {
        Part::Text(text) => match stored_block(text.raw.as_ref(), replay) {
            Some(raw) => blocks.push(Block {
                json: raw.to_owned(),
                kind: BlockKind::Other,
                ours: false,
            }),
            None => blocks.extend(text_block(&text.text)),
        },
        Part::ToolCall(call) => match stored_block(call.raw.as_ref(), replay) {
            Some(raw) => blocks.push(Block {
                json: raw.to_owned(),
                kind: BlockKind::ToolCall,
                ours: false,
            }),
            None => {
                let mut json = String::new();
                let mut object = Object::open(&mut json);
                object.str("type", "tool_use");
                object.str("id", call.id.as_str());
                object.str("name", &call.name);
                if call.arguments.is_object() {
                    object.value("input", &call.arguments);
                } else {
                    // The model wrote something that is not an object. The
                    // provider takes only an object, so the model is shown
                    // what it wrote inside one.
                    let mut input = Object::open(object.key("input"));
                    match &call.arguments {
                        serde_json::Value::String(text) => input.str(INVALID_INPUT_KEY, text),
                        other => input.str(INVALID_INPUT_KEY, &other.to_string()),
                    }
                    input.close();
                }
                object.close();
                blocks.push(Block {
                    json,
                    kind: BlockKind::ToolCall,
                    ours: true,
                });
            }
        },
        // Without its stored block a reasoning part has no payload the
        // provider would accept, and a provider block has nothing at all.
        Part::Reasoning(reasoning) => {
            if let Some(raw) = stored_block(reasoning.raw.as_ref(), replay) {
                blocks.push(Block {
                    json: raw.to_owned(),
                    kind: BlockKind::Other,
                    ours: false,
                });
            }
        }
        Part::ProviderBlock(block) => {
            if let Some(raw) = stored_block(Some(&block.raw), replay) {
                blocks.push(Block {
                    json: raw.to_owned(),
                    kind: BlockKind::Other,
                    ours: false,
                });
            }
        }
        other => return Err(misplaced(other, "an assistant message")),
    }
    Ok(())
}
