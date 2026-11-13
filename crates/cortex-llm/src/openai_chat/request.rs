//! Building the request body.
//!
//! [`render`] turns a [`ModelRequest`] into a [`Body`]: the JSON text of the
//! request, kept so that the part a server caches can be looked at by
//! itself.
//!
//! # The order of the body
//!
//! ```text
//! {"model", "stream", "stream_options", "max_tokens", "reasoning_effort",
//!  "service_tier", "response_format", "prompt_cache_key", "cache_control",
//!  the provider's own members, "tools", "messages": [ the system prompt, … ]}
//! ```
//!
//! Everything before the messages is the same bytes for every request of a
//! context epoch, the system prompt is the first message, and the messages
//! only grow at the end. So the body of one request, without its closing
//! brackets, is the beginning of the next one: [`Body::prefix`]. The
//! servers that cache do it by the longest beginning they have seen before.
//!
//! # What each neutral message becomes
//!
//! | Neutral | On the wire |
//! |---|---|
//! | the system prompt | The first message, under the role the provider's entry names. |
//! | a user message | One `user` message: a string for one piece of text, a list of parts otherwise. |
//! | a tool's results | One `tool` message for each result, in call order, right after the calls. The dialect has no error flag, so a failed result's text begins with [`ERROR_PREFIX`]. A `tool` message holds text only, so an image a tool returned follows in a `user` message after the results. |
//! | an assistant message | One `assistant` message. See "Replay". |
//!
//! # Replay
//!
//! An assistant message is sent back in one of two ways.
//!
//! - **Raw.** The stored block of each part, laid over one another in
//!   order: text appended to text, a list extended by a list. Each stored
//!   block is a piece of the assistant message under the message's own
//!   member names ([the module](super)), so the result is the message the
//!   server sent, reasoning included. This is what a server that wants its
//!   reasoning back gets. It applies to a message this provider produced
//!   through this protocol, for a model that can read it.
//! - **The neutral projection.** `content` and `tool_calls` written from
//!   the neutral fields; reasoning and provider blocks left out. It applies
//!   to every other message.
//!
//! **A server may demand its reasoning member on every assistant message.**
//! DeepSeek answers a conversation with tools with a 400 when an assistant
//! message has no `reasoning_content`, and a projected message has none.
//! The provider's `reasoning_passback` names the member, and a message
//! without it is sent with the member empty.
//!
//! No stored text is ever copied into the body as it is. A string is read
//! and written again, and any other value is written only after it was
//! read as one whole JSON value, so a damaged or doctored session log
//! cannot change the shape of a request. And a part's stored block gives
//! the message only the members of its own kind: a text part its
//! `content`, a reasoning part the reasoning members, a tool call the one
//! item of `tool_calls` with its own id. Whatever else a block holds is
//! left out.
//!
//! # What is never sent
//!
//! `tool_choice` (the harness never forces a tool), `parallel_tool_calls`
//! (the servers' default is what is wanted), sampling parameters, `n`,
//! `store` and `user`.

use std::borrow::Cow;

use cortex_protocol::message::ImagePart;
use cortex_protocol::model::{Modality, ReasoningControlKind};
use cortex_protocol::{Message, Part, RawJson, Role, Speed};
use serde_json::Value;
use serde_json::value::RawValue;

use super::{NAME, flags};
use crate::catalog::ToolSchemaStyle;
use crate::error::BuildError;
use crate::json::{self, Array, Object, ordered_members};
use crate::protocol::ProviderInfo;
use crate::request::{ModelRequest, is_valid_tool_name};

/// What a failed tool result's text begins with: the dialect has no flag
/// that says a result is an error.
pub(crate) const ERROR_PREFIX: &str = "Tool error: ";

/// What stands where an image was, for a model that takes none.
const IMAGE_PLACEHOLDER: &str = "[An image was here. This model does not take images.]";

/// What a tool result says in place of an image it returned, when the image
/// itself follows in the next message.
const IMAGE_FOLLOWS: &str = "[The image this tool returned is in the next message.]";

/// The name an output schema is sent under. The dialect wants one.
const SCHEMA_NAME: &str = "output";

/// A request body: everything up to the messages, and each message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Body {
    /// Everything up to and including the opening of the messages array.
    head: String,
    /// The messages, each one JSON object.
    messages: Vec<String>,
}

impl Body {
    /// The part of the body that later requests of the same context epoch
    /// repeat byte for byte: everything up to the end of the last message,
    /// without the closing brackets.
    pub(crate) fn prefix(&self) -> String {
        let size: usize = self.messages.iter().map(|message| message.len() + 1).sum();
        let mut out = String::with_capacity(self.head.len() + size + 2);
        out.push_str(&self.head);
        for (at, message) in self.messages.iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
            out.push_str(message);
        }
        out
    }

    /// The whole body.
    pub(crate) fn finish(&self) -> String {
        let mut out = self.prefix();
        out.push_str("]}");
        out
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
    flags::check(request, provider).map_err(BuildError::new)?;

    let marker = flags::cache_control(request).then(|| cache_marker(request));
    let mut head = String::with_capacity(2048);
    let mut body = Object::open(&mut head);
    body.str("model", model_id(request)?);
    body.yes("stream");
    if flags::usage_in_stream(provider) {
        let mut options = Object::open(body.key("stream_options"));
        options.yes("include_usage");
        options.close();
    }
    if let Some(asked) = request.max_output {
        // The model's limit, or the smaller one of the model that serves
        // the speed, where a speed is another model.
        let limit = flags::speed_max_output(request, &request.speed).unwrap_or(info.max_output);
        body.number(
            flags::max_tokens_field(provider),
            asked.min(limit).min(info.max_output),
        );
    }
    reasoning(&mut body, request, provider)?;
    if let Some(tier) = service_tier(request) {
        body.str("service_tier", tier);
    }
    output_schema(&mut body, request)?;
    if flags::prompt_cache_key(provider)
        && let Some(key) = &request.cache.key
    {
        body.str("prompt_cache_key", key);
    }
    if request.cache.history
        && let Some(marker) = &marker
    {
        body.raw("cache_control", marker);
    }
    extra_body(&mut body, provider)?;
    tools(&mut body, request)?;
    // The messages follow the head. `Body::prefix` writes them and `finish`
    // closes the array and the body.
    body.key("messages").push('[');

    let mut messages = Vec::with_capacity(request.messages.len() + 1);
    if !request.system.is_empty() {
        let marker = marker.as_deref().filter(|_| request.cache.stable_prefix);
        messages.push(system_message(request, provider, marker));
    }
    for message in &request.messages {
        wire_messages(message, request, provider, &mut messages)?;
    }
    Ok(Body { head, messages })
}

/// The model id the request names: the model's own, or, where the server
/// sells a speed tier as a separate model, the id that serves the tier the
/// request asks for.
fn model_id(request: &ModelRequest) -> Result<&str, BuildError> {
    let info = &request.model.info;
    if !info.offers(&request.speed) {
        return Err(BuildError::new(format!(
            "the model `{}` does not offer the speed `{}`",
            info.id,
            request.speed.as_str()
        )));
    }
    if request.speed.is_standard() || !flags::speeds_are_models(request) {
        return Ok(info.id.as_str());
    }
    flags::speed_model(request, &request.speed).ok_or_else(|| {
        BuildError::new(format!(
            "the model `{}` offers the speed `{}`, and its entry does not say which model id \
             serves it",
            info.id,
            request.speed.as_str()
        ))
    })
}

/// The `service_tier` a request names. A model that offers tiers through
/// this member is always told which one the request wants, `default`
/// included, so that nothing depends on a default set at the provider. A
/// model that offers none is told nothing, and neither is one whose tiers
/// are separate models.
fn service_tier(request: &ModelRequest) -> Option<&str> {
    if request.model.info.speeds.is_empty() || flags::speeds_are_models(request) {
        return None;
    }
    Some(tier_name(&request.speed))
}

/// A speed, as `service_tier` names it.
///
/// | Speed | `service_tier` |
/// |---|---|
/// | `standard` | `default` |
/// | `fast` | `priority`: the name OpenAI and OpenRouter both take, and the one both answer with |
/// | any other | its own name |
pub(crate) fn tier_name(speed: &Speed) -> &str {
    match speed {
        Speed::Standard => "default",
        Speed::Fast => "priority",
        other => other.as_str(),
    }
}

/// The cache marker of this request, for a model that caches only where it
/// is marked: `{"type":"ephemeral"}`, with the lifetime class when the
/// request names one.
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

/// The reasoning control, in the form the provider's entry names.
///
/// A level the request does not name is not sent, and the model's own
/// default stands. A level the model does not have is refused here, before
/// anything is sent.
fn reasoning(
    body: &mut Object<'_>,
    request: &ModelRequest,
    provider: &ProviderInfo,
) -> Result<(), BuildError> {
    let info = &request.model.info;
    let object = flags::reasoning_object(provider);
    let mut level = request.reasoning.level.as_deref();
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
            // A model that takes tools at one level only is told that
            // level whenever tools are sent, and a request that names
            // another is refused: the server would refuse it.
            if let Some(required) = flags::tools_reasoning(request)
                && !request.tools.is_empty()
            {
                if level.is_some_and(|level| level != required) {
                    return Err(BuildError::new(format!(
                        "the model `{}` takes tools on `{NAME}` only at the reasoning level \
                         `{required}`",
                        info.id
                    )));
                }
                level = Some(required);
            }
            let Some(level) = level else { return Ok(()) };
            if object {
                let mut reasoning = Object::open(body.key("reasoning"));
                reasoning.str("effort", level);
                reasoning.close();
            } else {
                body.str("reasoning_effort", level);
            }
        }
        ReasoningControlKind::Budget => {
            if level.is_some() {
                return Err(BuildError::new(format!(
                    "the model `{}` takes a reasoning budget in tokens, not a level",
                    info.id
                )));
            }
            let Some(budget) = budget else { return Ok(()) };
            if !object {
                return Err(BuildError::new(format!(
                    "the provider `{}` has no way to send a reasoning budget in tokens",
                    provider.id
                )));
            }
            let min = info.reasoning.min_tokens.unwrap_or(0);
            let max = info.reasoning.max_tokens.unwrap_or(u64::MAX);
            if budget < min || budget > max {
                return Err(BuildError::new(format!(
                    "a reasoning budget of {budget} tokens is outside what the model `{}` takes \
                     ({min} to {max})",
                    info.id
                )));
            }
            let mut reasoning = Object::open(body.key("reasoning"));
            reasoning.number("max_tokens", budget);
            reasoning.close();
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

/// An output schema, as the dialect's structured output.
fn output_schema(body: &mut Object<'_>, request: &ModelRequest) -> Result<(), BuildError> {
    let Some(schema) = &request.output_schema else {
        return Ok(());
    };
    let info = &request.model.info;
    if !info.capabilities.structured_output {
        return Err(BuildError::new(format!(
            "the model `{}` cannot be held to an output schema",
            info.id
        )));
    }
    let mut format = Object::open(body.key("response_format"));
    format.str("type", "json_schema");
    let mut named = Object::open(format.key("json_schema"));
    named.str("name", SCHEMA_NAME);
    named.yes("strict");
    named.value("schema", schema);
    named.close();
    format.close();
    Ok(())
}

/// The members this request's body may hold: the ones the builder writes.
/// A provider's entry cannot add one of these.
const OWN_MEMBERS: &[&str] = &[
    "model",
    "stream",
    "stream_options",
    "max_tokens",
    "max_completion_tokens",
    "reasoning",
    "reasoning_effort",
    "service_tier",
    "response_format",
    "prompt_cache_key",
    "cache_control",
    "tools",
    "tool_choice",
    "messages",
];

/// The members a provider's entry adds to every request: what the server
/// wants that the dialect has no name for. They are the same for every
/// request, so they are part of the beginning that does not change.
fn extra_body(body: &mut Object<'_>, provider: &ProviderInfo) -> Result<(), BuildError> {
    let Some(text) = flags::extra_body(provider) else {
        return Ok(());
    };
    let refused = |why: &str| {
        BuildError::new(format!(
            "the provider `{}` has an `extra_body` that cannot be used: {why}",
            provider.id
        ))
    };
    let members = serde_json::from_str::<&RawValue>(text)
        .ok()
        .and_then(|raw| ordered_members(raw).ok())
        .ok_or_else(|| refused("it is not one JSON object"))?;
    for (name, value) in members {
        if OWN_MEMBERS.contains(&name.as_str()) {
            return Err(refused(&format!(
                "`{name}` is a member the request already has"
            )));
        }
        body.raw(&name, value.get());
    }
    Ok(())
}

fn tools(body: &mut Object<'_>, request: &ModelRequest) -> Result<(), BuildError> {
    if request.tools.is_empty() {
        return Ok(());
    }
    let strict = request.variants.tool_schema == ToolSchemaStyle::Strict;
    let mut budget = crate::strict::Budget::new(crate::strict::Rules::OpenAi);
    let mut list = Array::open(body.key("tools"));
    for tool in &request.tools {
        if !is_valid_tool_name(&tool.name) {
            return Err(BuildError::new(format!(
                "`{}` is not a tool name every provider accepts",
                tool.name
            )));
        }
        let mut object = Object::open(list.item());
        object.str("type", "function");
        let mut function = Object::open(object.key("function"));
        function.str("name", &tool.name);
        function.str("description", &tool.description);
        // Strict functions are sent reduced to what OpenAI's strict mode
        // takes; one that cannot be is sent whole, and not strict.
        let (schema, strict) = if strict {
            budget.reduce(&tool.schema)
        } else {
            (std::borrow::Cow::Borrowed(&tool.schema), false)
        };
        function.value("parameters", &schema);
        if strict {
            function.yes("strict");
        }
        function.close();
        object.close();
    }
    list.close();
    Ok(())
}

/// The system prompt, as the first message. With a cache marker it is a
/// list of one text part, which is where the marker goes.
fn system_message(request: &ModelRequest, provider: &ProviderInfo, marker: Option<&str>) -> String {
    let mut out = String::with_capacity(request.system.len() + 64);
    let mut message = Object::open(&mut out);
    message.str("role", flags::system_role(provider));
    match marker {
        None => message.str("content", &request.system),
        Some(marker) => {
            let mut content = Array::open(message.key("content"));
            let mut part = Object::open(content.item());
            part.str("type", "text");
            part.str("text", &request.system);
            part.raw("cache_control", marker);
            part.close();
            content.close();
        }
    }
    message.close();
    out
}

/// One part of a user message's content.
enum UserPart<'a> {
    Text(Cow<'a, str>),
    Image(&'a ImagePart),
}

/// Writes a `user` message. One piece of text is a string, as every server
/// takes it; anything else is a list of parts.
fn user_message(parts: &[UserPart<'_>]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut message = Object::open(&mut out);
    message.str("role", "user");
    if let [UserPart::Text(text)] = parts {
        message.str("content", text);
    } else {
        let mut content = Array::open(message.key("content"));
        for part in parts {
            let mut object = Object::open(content.item());
            match part {
                UserPart::Text(text) => {
                    object.str("type", "text");
                    object.str("text", text);
                }
                UserPart::Image(image) => {
                    object.str("type", "image_url");
                    let mut url = String::with_capacity(
                        image.data.as_ref().map_or(0, String::len) + image.media_type.len() + 16,
                    );
                    url.push_str("data:");
                    url.push_str(&image.media_type);
                    url.push_str(";base64,");
                    url.push_str(image.data.as_deref().unwrap_or_default());
                    let mut source = Object::open(object.key("image_url"));
                    source.str("url", &url);
                    source.close();
                }
            }
            object.close();
        }
        content.close();
    }
    message.close();
    Some(out)
}

/// An image as a part of a user message: the image for a model that takes
/// images, a line of text for one that does not. An image that is still a
/// reference to a stored artifact cannot be sent: the client loads it
/// before the request is built ([`ImageSource`](crate::ImageSource)).
fn image_part<'a>(
    image: &'a ImagePart,
    request: &ModelRequest,
) -> Result<UserPart<'a>, BuildError> {
    if !request.model.info.input.contains(&Modality::Image) {
        return Ok(UserPart::Text(Cow::Borrowed(IMAGE_PLACEHOLDER)));
    }
    if image.data.is_none() {
        return Err(BuildError::new(match &image.artifact {
            Some(artifact) => format!(
                "the image in artifact `{artifact}` was not loaded before the request was built"
            ),
            None => "an image has neither data nor an artifact".to_owned(),
        }));
    }
    Ok(UserPart::Image(image))
}

fn misplaced(part: &Part, place: &str) -> BuildError {
    BuildError::new(format!(
        "a part of kind `{}` cannot be in {place}",
        part.tag()
    ))
}

/// Writes the wire messages one neutral message becomes.
fn wire_messages(
    message: &Message,
    request: &ModelRequest,
    provider: &ProviderInfo,
    out: &mut Vec<String>,
) -> Result<(), BuildError> {
    match &message.role {
        Role::User | Role::Tool => {
            // The results come first and stay together: the dialect wants
            // every result right after the calls. Whatever else the
            // message holds, and the images the tools returned, follow in
            // one user message.
            let mut said: Vec<UserPart<'_>> = Vec::new();
            let mut returned: Vec<UserPart<'_>> = Vec::new();
            for part in &message.parts {
                match part {
                    Part::Text(text) if text.text.is_empty() => {}
                    Part::Text(text) => said.push(UserPart::Text(Cow::Borrowed(&text.text))),
                    Part::Image(image) => said.push(image_part(image, request)?),
                    Part::ToolResult(result) => {
                        let mut text = String::new();
                        if result.is_error {
                            text.push_str(ERROR_PREFIX);
                        }
                        let line = |text: &mut String, more: &str| {
                            if !(text.is_empty() || text.ends_with('\n') || text == ERROR_PREFIX) {
                                text.push('\n');
                            }
                            text.push_str(more);
                        };
                        for part in &result.content {
                            match part {
                                Part::Text(part) => line(&mut text, &part.text),
                                Part::Image(image) => match image_part(image, request)? {
                                    UserPart::Text(placeholder) => line(&mut text, &placeholder),
                                    image @ UserPart::Image(_) => {
                                        line(&mut text, IMAGE_FOLLOWS);
                                        returned.push(UserPart::Text(Cow::Owned(format!(
                                            "The image returned by the tool call `{}`:",
                                            result.call_id
                                        ))));
                                        returned.push(image);
                                    }
                                },
                                other => return Err(misplaced(other, "a tool result")),
                            }
                        }
                        let mut json = String::with_capacity(text.len() + 64);
                        let mut object = Object::open(&mut json);
                        object.str("role", "tool");
                        object.str("tool_call_id", result.call_id.as_str());
                        object.str("content", &text);
                        object.close();
                        out.push(json);
                    }
                    other => return Err(misplaced(other, "a user message")),
                }
            }
            said.append(&mut returned);
            out.extend(user_message(&said));
        }
        Role::Assistant => {
            let replay = replay(message, request, provider);
            let tags = flags::reasoning_tags(request, provider);
            out.extend(assistant_message(message, replay, provider, tags)?);
        }
        other => {
            return Err(BuildError::new(format!(
                "a message has the role `{}`, which this protocol cannot send",
                other.as_str()
            )));
        }
    }
    Ok(())
}

/// How an assistant message is sent back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replay {
    /// The stored block of each part, laid over one another.
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

/// What the blocks of a message have added up to under one member name.
enum Laid {
    /// Text, which each block appends to.
    Text(String),
    /// A list, which each block extends. Every item is one JSON value.
    List(Vec<Box<RawValue>>),
    /// Any other value. A later one replaces it.
    Other(Box<RawValue>),
}

/// The members of an assistant message, in the order they first appeared.
#[derive(Default)]
struct Members(Vec<(String, Laid)>);

impl Members {
    fn slot(&mut self, name: &str) -> Option<&mut Laid> {
        self.0
            .iter_mut()
            .find(|(known, _)| known == name)
            .map(|(_, laid)| laid)
    }

    fn text(&mut self, name: &str, more: &str) {
        match self.slot(name) {
            Some(Laid::Text(text)) => text.push_str(more),
            Some(other) => *other = Laid::Text(more.to_owned()),
            None => self.0.push((name.to_owned(), Laid::Text(more.to_owned()))),
        }
    }

    fn items(&mut self, name: &str, mut more: Vec<Box<RawValue>>) {
        match self.slot(name) {
            Some(Laid::List(items)) => items.append(&mut more),
            Some(other) => *other = Laid::List(more),
            None => self.0.push((name.to_owned(), Laid::List(more))),
        }
    }

    /// Lays a stored block over the message: the members of it that
    /// `allowed` names, and no others. `false` when the text is not one
    /// JSON object, or holds none of those members: nothing was laid then.
    ///
    /// A stored block is a piece of a message, and a part can only stand
    /// for the piece of its own kind: text for `content`, reasoning for the
    /// reasoning members, a tool call for its own item of `tool_calls`. A
    /// block that holds anything else did not come from the decoder, and
    /// what it holds beside is left out: a damaged or doctored session log
    /// cannot give a message a role, or a text part a tool call.
    fn lay(&mut self, block: &str, allowed: &dyn Fn(&str) -> bool) -> bool {
        if !json::is_object(block) {
            return false;
        }
        let Ok(raw) = serde_json::from_str::<&RawValue>(block) else {
            return false;
        };
        let Ok(members) = ordered_members(raw) else {
            return false;
        };
        let mut laid = false;
        for (name, value) in members {
            if !allowed(&name) {
                continue;
            }
            laid = true;
            let text = value.get();
            if text == "null" {
                continue;
            }
            if text.starts_with('"') {
                if let Ok(more) = serde_json::from_str::<String>(text) {
                    self.text(&name, &more);
                }
            } else if text.starts_with('[') {
                if let Ok(more) = serde_json::from_str::<Vec<Box<RawValue>>>(text) {
                    self.items(&name, more);
                }
            } else {
                match self.slot(&name) {
                    Some(slot) => *slot = Laid::Other(value),
                    None => self.0.push((name, Laid::Other(value))),
                }
            }
        }
        laid
    }

    /// The message, or `None` when it would say nothing: no text and no
    /// tool call. The dialect refuses such a message, and reasoning alone
    /// is not something to send.
    ///
    /// `passback` names a member the server wants on every assistant
    /// message. A message without it gets it, empty.
    fn write(mut self, passback: Option<&str>) -> Option<String> {
        if let Some(member) = passback
            && self.slot(member).is_none()
        {
            self.0.push((member.to_owned(), Laid::Text(String::new())));
        }
        let said = |member: &str| {
            self.0.iter().find_map(|(name, laid)| match laid {
                Laid::Text(text) if name == member && !text.is_empty() => Some(text.as_str()),
                _ => None,
            })
        };
        let content = said("content");
        // A refusal is something the model said, under a name of its own.
        let refused = said("refusal").is_some();
        let calls = self.0.iter().any(
            |(name, laid)| matches!(laid, Laid::List(items) if name == "tool_calls" && !items.is_empty()),
        );
        if content.is_none() && !calls && !refused {
            return None;
        }
        let mut out = String::new();
        let mut message = Object::open(&mut out);
        message.str("role", "assistant");
        // A message with tool calls and no text says so with a null, which
        // is how the servers themselves write it.
        match content {
            Some(text) => message.str("content", text),
            None => message.raw("content", "null"),
        }
        for (name, laid) in &self.0 {
            if name == "content" {
                continue;
            }
            match laid {
                Laid::Text(text) => message.str(name, text),
                Laid::List(items) => {
                    let mut list = Array::open(message.key(name));
                    for item in items {
                        list.item().push_str(item.get());
                    }
                    list.close();
                }
                Laid::Other(value) => message.raw(name, value.get()),
            }
        }
        message.close();
        Some(out)
    }
}

/// A tool call written from its neutral fields, as an item of `tool_calls`.
fn call_item(id: &str, name: &str, arguments: &Value) -> Option<Box<RawValue>> {
    let mut out = String::new();
    let mut call = Object::open(&mut out);
    call.str("id", id);
    call.str("type", "function");
    let mut function = Object::open(call.key("function"));
    function.str("name", name);
    // The arguments travel as JSON text inside a string. What the model
    // wrote that was not JSON is sent back as it was written.
    match arguments {
        Value::String(text) => function.str("arguments", text),
        other => {
            let mut text = String::new();
            json::value(&mut text, other);
            function.str("arguments", &text);
        }
    }
    function.close();
    call.close();
    RawValue::from_string(out).ok()
}

/// The stored block of a part, when the message is replayed raw.
fn stored(raw: Option<&RawJson>, replay: Replay) -> Option<&str> {
    raw.filter(|_| replay == Replay::Raw).map(RawJson::as_str)
}

/// The members a text part's stored block may hold.
fn text_member(name: &str) -> bool {
    matches!(name, "content" | "refusal")
}

/// Whether a stored block is the one tool call of this id, and nothing
/// else: `{"tool_calls":[{"id":"<id>", …}]}`.
fn is_call_block(block: &str, id: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct Block<'a> {
        #[serde(borrow)]
        tool_calls: Vec<Item<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct Item<'a> {
        #[serde(borrow)]
        id: Cow<'a, str>,
    }
    serde_json::from_str::<Block<'_>>(block)
        .is_ok_and(|block| matches!(&block.tool_calls[..], [call] if call.id == id))
}

fn assistant_message(
    message: &Message,
    replay: Replay,
    provider: &ProviderInfo,
    tags: bool,
) -> Result<Option<String>, BuildError> {
    let passback = flags::reasoning_passback(provider);
    // The members a reasoning part's stored block may hold: the names the
    // servers put reasoning under, the provider's own, and `content`, for
    // reasoning that arrived inside the text.
    let named = flags::reasoning_field(provider);
    let reasoning_member = |name: &str| {
        matches!(
            name,
            "reasoning_content" | "reasoning" | "reasoning_details"
        ) || Some(name) == named
            || Some(name) == passback
            // Only where the server writes its reasoning inside the text.
            || (tags && name == "content")
    };
    // Text that was one stream at this protocol is one text again. Text
    // another protocol kept in pieces stays apart.
    let ours = message
        .origin
        .as_ref()
        .is_some_and(|origin| origin.protocol == NAME);
    let mut members = Members::default();
    let mut text_parts = 0;
    for part in &message.parts {
        match part {
            Part::Text(text) => {
                if !stored(text.raw.as_ref(), replay)
                    .is_some_and(|block| members.lay(block, &text_member))
                {
                    if text.text.is_empty() {
                        continue;
                    }
                    // Two pieces of text from a provider that keeps them
                    // apart stay apart.
                    if text_parts > 0 && replay == Replay::Projection && !ours {
                        members.text("content", "\n\n");
                    }
                    members.text("content", &text.text);
                }
                text_parts += 1;
            }
            Part::ToolCall(call) => {
                let own = |block: &&str| is_call_block(block, call.id.as_str());
                if !stored(call.raw.as_ref(), replay)
                    .filter(own)
                    .is_some_and(|block| members.lay(block, &|name| name == "tool_calls"))
                {
                    let item = call_item(call.id.as_str(), &call.name, &call.arguments)
                        .ok_or_else(|| BuildError::new("a tool call could not be written"))?;
                    members.items("tool_calls", vec![item]);
                }
            }
            // Without its stored block a reasoning part has nothing the
            // server would take back.
            Part::Reasoning(reasoning) => {
                if let Some(block) = stored(reasoning.raw.as_ref(), replay) {
                    members.lay(block, &reasoning_member);
                }
            }
            // This protocol's decoder makes no provider block, so there is
            // none of its own to send back, and another protocol's means
            // nothing here.
            Part::ProviderBlock(_) => {}
            other => return Err(misplaced(other, "an assistant message")),
        }
    }
    Ok(members.write(passback))
}
