//! The `openai-chat` protocol: OpenAI's Chat Completions API, streamed, and
//! the many servers that speak it (`docs/architecture/04-providers.md`).
//!
//! | Part | Where | What it is tested from |
//! |---|---|---|
//! | The request | [`request`] | A snapshot of the body for each kind of request. |
//! | The response stream | [`decode`] | Recorded and hand-written streams, replayed cut at every byte. |
//! | Failures and headers | [`classify`] | The classification table, row by row. |
//! | A provider's usage or balance endpoint | [`account`] | The documented answers. |
//! | A local server's loaded context size | [`context`] | The documented answers. |
//! | The built-in providers | [`providers`] | Each entry, flag by flag. |
//!
//! The facts here were read from each server's documentation on 2026-11-13.
//! The page that describes the protocol lists the sources, fact by fact.
//!
//! # One dialect, many servers
//!
//! The servers that claim this dialect differ in ways that break a client
//! which assumes they do not. Each difference is a flag on a provider's or
//! a model's entry, or a branch the tests cover, and never a test of a
//! model's name.
//!
//! # A response has no blocks
//!
//! The dialect streams one assistant message as additions to its members:
//! `content`, a reasoning member, `tool_calls`. The neutral model wants
//! blocks, each with the provider's own JSON for it. So the decoder cuts
//! the message into blocks, and **the stored block of a part is the piece
//! of the assistant message that part stands for**, as an object with the
//! message's own member names:
//!
//! | Part | Stored block |
//! |---|---|
//! | text | `{"content":"…"}` |
//! | reasoning | `{"reasoning_content":"…"}`, under the name the server used, with `reasoning_details` where the server sent them |
//! | a tool call | `{"tool_calls":[{"id":"…","type":"function","function":{"name":"…","arguments":"…"}}]}` |
//!
//! Laid over one another in order (text appended to text, a list extended
//! by a list), the blocks of a message are the message the server would
//! have sent without streaming. That is how a message is sent back
//! ([`request`]).
//!
//! # Flags
//!
//! The flags this protocol reads. A flag it does not know, or a value a
//! flag cannot have, is refused when a request is built: a misspelt flag
//! must not silently be the default.
//!
//! | On | Flag | Value | Meaning |
//! |---|---|---|---|
//! | a provider | `max_tokens_field` | text | The request member that limits the output: `max_tokens` (the default, which every server knows) or `max_completion_tokens` (OpenAI's current name). |
//! | a provider | `usage_in_stream` | on or off | Whether the request asks for the usage in the stream (`stream_options.include_usage`). On unless set off, for a server that refuses the member. |
//! | a provider | `system_role` | text | The role the system prompt is sent under: `system` (the default) or `developer`. |
//! | a provider | `reasoning_param` | text | How a reasoning level is asked for: `reasoning_effort` (the default, a top-level name) or `reasoning` (an object with `effort` or `max_tokens`). |
//! | a provider | `reasoning_field` | text | The member of a delta that carries reasoning text. Unset, the decoder reads `reasoning_content`, and `reasoning` where a delta has no `reasoning_content`. |
//! | a provider or a model | `reasoning_tags` | on or off | The server writes reasoning inside the text, between `<think>` and `</think>` at the start of the content. The decoder takes it out. |
//! | a provider | `replay_reasoning` | `"all"` or `"readable"` | Which reasoning of this provider's own earlier messages is sent back after a change of model. `"readable"`, the default: only to the model that wrote it, or one whose catalog entry lists that model in `reads_reasoning_from`. |
//! | a provider | `reasoning_passback` | text | The member every assistant message must carry when it is sent back to this server, which refuses a conversation with tools otherwise: DeepSeek's `reasoning_content`. A message that has no reasoning to send (it came from another provider or another model) is sent with the member empty. |
//! | a provider | `prompt_cache_key` | on or off | Whether the server takes a `prompt_cache_key`. Off unless set on: a strict server refuses a member it does not know. |
//! | a provider | `extra_body` | text | Members to add to every request's body, as the text of one JSON object: what a server wants that the dialect has no name for. Venice.ai's entry uses it to turn off the system prompt that server adds by itself. A member the builder writes itself cannot be named here. |
//! | a provider | `usage_endpoint` | text | The provider's documented usage or balance endpoint, by the name of its format ([`account`]). |
//! | a provider | `context_probe` | text | How a local server is asked for the context size it loaded a model with, by the name of the format ([`context`]). |
//! | a model | `cache_control` | on or off | The model caches only where a request marks it, with Anthropic's markers. |
//! | a model | `speed_model_<tier>` | text | The model id that serves the tier, for a server that sells a speed as a separate model. A model without it is asked for a tier with `service_tier`. |
//! | a model | `speed_max_output_<tier>` | a number | The most output tokens the model that serves the tier gives, where that is less than the model's own. |
//! | a model | `tools_reasoning` | text | The one reasoning level at which the model takes tools on this protocol. |

pub mod account;
mod classify;
pub mod context;
mod decode;
pub mod providers;
mod request;

use cortex_base::{Secret, UtcTime};

use crate::auth::{ApiKeyAuth, KeyPlacement};
use crate::error::{BuildError, ProviderError};
use crate::event::EventSink;
use crate::http::{Headers, HttpRequest};
use crate::protocol::{HttpFailure, Protocol, ProviderInfo, StreamDecoder};
use crate::request::ModelRequest;

/// The protocol's name, as catalog and provider entries write it.
pub const NAME: &str = "openai-chat";

/// The path of the endpoint, after a provider's base URL.
pub const PATH: &str = "/chat/completions";

/// The names of the flags this protocol reads.
pub mod flag {
    /// On a provider: the request member that limits the output.
    pub const MAX_TOKENS_FIELD: &str = "max_tokens_field";
    /// On a provider: whether the usage is asked for in the stream.
    pub const USAGE_IN_STREAM: &str = "usage_in_stream";
    /// On a provider: the role of the system prompt.
    pub const SYSTEM_ROLE: &str = "system_role";
    /// On a provider: how a reasoning level is asked for.
    pub const REASONING_PARAM: &str = "reasoning_param";
    /// On a provider: the member of a delta that carries reasoning text.
    pub const REASONING_FIELD: &str = "reasoning_field";
    /// On a provider or a model: reasoning is inside the text, in tags.
    pub const REASONING_TAGS: &str = "reasoning_tags";
    /// On a provider: which reasoning is sent back after a change of
    /// model, `"all"` or `"readable"`.
    pub const REPLAY_REASONING: &str = "replay_reasoning";
    /// On a provider: the member every assistant message that is sent
    /// back must carry, empty when there is no reasoning to send.
    pub const REASONING_PASSBACK: &str = "reasoning_passback";
    /// On a provider: whether the server takes a `prompt_cache_key`.
    pub const PROMPT_CACHE_KEY: &str = "prompt_cache_key";
    /// On a provider: members to add to every request's body, as the
    /// text of one JSON object.
    pub const EXTRA_BODY: &str = "extra_body";
    /// On a provider: the format of its usage or balance endpoint.
    pub const USAGE_ENDPOINT: &str = "usage_endpoint";
    /// On a provider: how its loaded context size is asked for.
    pub const CONTEXT_PROBE: &str = "context_probe";
    /// On a model: the model caches only where a request marks it.
    pub const CACHE_CONTROL: &str = "cache_control";
    /// On a model: the beginning of the name of the flag that gives the
    /// model id serving a speed tier. The tier's name follows.
    pub const SPEED_MODEL: &str = "speed_model_";
    /// On a model: the beginning of the name of the flag that gives the
    /// output limit of the model serving a speed tier, where it is smaller
    /// than the model's own. The tier's name follows.
    pub const SPEED_MAX_OUTPUT: &str = "speed_max_output_";
    /// On a model: the one reasoning level at which it takes tools.
    pub const TOOLS_REASONING: &str = "tools_reasoning";
}

/// The flags, read.
mod flags {
    use cortex_protocol::Speed;

    use super::flag;
    use crate::protocol::{FlagValue, Flags, ProviderInfo};
    use crate::request::ModelRequest;

    /// What a flag's value must be.
    enum Kind {
        /// On or off.
        Switch,
        /// Any text that is not empty.
        Text,
        /// A whole number above zero.
        Count,
        /// One of these words.
        OneOf(&'static [&'static str]),
    }

    /// The flags of a provider's entry, and what each must be.
    const PROVIDER: &[(&str, Kind)] = &[
        (
            flag::MAX_TOKENS_FIELD,
            Kind::OneOf(&["max_tokens", "max_completion_tokens"]),
        ),
        (flag::USAGE_IN_STREAM, Kind::Switch),
        (flag::SYSTEM_ROLE, Kind::OneOf(&["system", "developer"])),
        (
            flag::REASONING_PARAM,
            Kind::OneOf(&["reasoning_effort", "reasoning"]),
        ),
        (flag::REASONING_FIELD, Kind::Text),
        (flag::REASONING_TAGS, Kind::Switch),
        (flag::REASONING_PASSBACK, Kind::Text),
        (flag::REPLAY_REASONING, Kind::OneOf(&["all", "readable"])),
        (flag::PROMPT_CACHE_KEY, Kind::Switch),
        (flag::EXTRA_BODY, Kind::Text),
        (flag::USAGE_ENDPOINT, Kind::Text),
        (flag::CONTEXT_PROBE, Kind::Text),
    ];

    /// The flags of a model's entry, and what each must be. A flag whose
    /// name begins with [`flag::SPEED_MODEL`] is text.
    const MODEL: &[(&str, Kind)] = &[
        (flag::CACHE_CONTROL, Kind::Switch),
        (flag::REASONING_TAGS, Kind::Switch),
        (flag::TOOLS_REASONING, Kind::Text),
    ];

    /// The members of an assistant message that are not reasoning: a flag
    /// that names where reasoning is cannot name one of these.
    const NOT_REASONING: &[&str] = &["role", "content", "tool_calls", "refusal", "name"];

    fn check_all(flags: &Flags, known: &[(&str, Kind)], whose: &str) -> Result<(), String> {
        for (name, value) in flags.iter() {
            let kind = known
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, kind)| kind);
            let kind = match kind {
                Some(kind) => kind,
                None if whose.starts_with("the model")
                    && name.starts_with(flag::SPEED_MAX_OUTPUT) =>
                {
                    &Kind::Count
                }
                None if whose.starts_with("the model") && name.starts_with(flag::SPEED_MODEL) => {
                    &Kind::Text
                }
                None => {
                    return Err(format!(
                        "{whose} has the flag `{name}`, which `openai-chat` does not know"
                    ));
                }
            };
            let fits = match (kind, value) {
                (Kind::Switch, FlagValue::Bool(_)) => true,
                (Kind::Text, FlagValue::Text(text)) => !text.trim().is_empty(),
                (Kind::Count, FlagValue::Number(count)) => *count > 0,
                (Kind::OneOf(words), FlagValue::Text(text)) => words.contains(&text.as_str()),
                _ => false,
            };
            if !fits {
                let wanted = match kind {
                    Kind::Switch => "true or false".to_owned(),
                    Kind::Text => "text".to_owned(),
                    Kind::Count => "a whole number above zero".to_owned(),
                    Kind::OneOf(words) => format!("one of {}", words.join(", ")),
                };
                return Err(format!(
                    "{whose} has the flag `{name}` with a value it cannot have: it must be {wanted}"
                ));
            }
        }
        Ok(())
    }

    /// Checks the flags of a request's provider and model: a flag this
    /// protocol does not know, or a value a flag cannot have, is refused
    /// before anything is sent. A misspelt flag must not silently be the
    /// default.
    pub(super) fn check(request: &ModelRequest, provider: &ProviderInfo) -> Result<(), String> {
        check_all(
            &provider.flags,
            PROVIDER,
            &format!("the provider `{}`", provider.id),
        )?;
        check_all(
            &request.model.flags,
            MODEL,
            &format!("the model `{}`", request.model.info.id),
        )?;
        for name in [flag::REASONING_FIELD, flag::REASONING_PASSBACK] {
            if let Some(member) = provider.flags.text(name)
                && NOT_REASONING.contains(&member)
            {
                return Err(format!(
                    "the provider `{}` has the flag `{name}` naming `{member}`, which is not \
                     where reasoning is",
                    provider.id
                ));
            }
        }
        Ok(())
    }

    pub(super) fn max_tokens_field(provider: &ProviderInfo) -> &str {
        provider
            .flags
            .text(flag::MAX_TOKENS_FIELD)
            .unwrap_or("max_tokens")
    }

    pub(super) fn usage_in_stream(provider: &ProviderInfo) -> bool {
        provider.flags.bool(flag::USAGE_IN_STREAM).unwrap_or(true)
    }

    pub(super) fn system_role(provider: &ProviderInfo) -> &str {
        provider.flags.text(flag::SYSTEM_ROLE).unwrap_or("system")
    }

    /// Whether a reasoning level is asked for with the `reasoning` object.
    pub(super) fn reasoning_object(provider: &ProviderInfo) -> bool {
        provider.flags.text(flag::REASONING_PARAM) == Some("reasoning")
    }

    pub(super) fn reasoning_field(provider: &ProviderInfo) -> Option<&str> {
        provider.flags.text(flag::REASONING_FIELD)
    }

    pub(super) fn reasoning_tags(request: &ModelRequest, provider: &ProviderInfo) -> bool {
        request
            .model
            .flags
            .bool(flag::REASONING_TAGS)
            .or_else(|| provider.flags.bool(flag::REASONING_TAGS))
            .unwrap_or(false)
    }

    pub(super) fn replays_every_block(provider: &ProviderInfo) -> bool {
        provider.flags.text(flag::REPLAY_REASONING) == Some("all")
    }

    pub(super) fn reasoning_passback(provider: &ProviderInfo) -> Option<&str> {
        provider.flags.text(flag::REASONING_PASSBACK)
    }

    pub(super) fn extra_body(provider: &ProviderInfo) -> Option<&str> {
        provider.flags.text(flag::EXTRA_BODY)
    }

    pub(super) fn prompt_cache_key(provider: &ProviderInfo) -> bool {
        provider.flags.bool(flag::PROMPT_CACHE_KEY).unwrap_or(false)
    }

    pub(super) fn cache_control(request: &ModelRequest) -> bool {
        request
            .model
            .flags
            .bool(flag::CACHE_CONTROL)
            .unwrap_or(false)
    }

    /// The model id that serves `speed`, where the server sells the tier
    /// as a separate model.
    pub(super) fn speed_model<'a>(request: &'a ModelRequest, speed: &Speed) -> Option<&'a str> {
        request
            .model
            .flags
            .text(&format!("{}{}", flag::SPEED_MODEL, speed.as_str()))
    }

    /// Whether the model's tiers are separate models.
    pub(super) fn speeds_are_models(request: &ModelRequest) -> bool {
        request
            .model
            .flags
            .iter()
            .any(|(name, _)| name.starts_with(flag::SPEED_MODEL))
    }

    /// The most output the model that serves `speed` gives, where that is
    /// another model with a limit of its own.
    pub(super) fn speed_max_output(request: &ModelRequest, speed: &Speed) -> Option<u64> {
        request
            .model
            .flags
            .number(&format!("{}{}", flag::SPEED_MAX_OUTPUT, speed.as_str()))
            .and_then(|limit| u64::try_from(limit).ok())
    }

    pub(super) fn tools_reasoning(request: &ModelRequest) -> Option<&str> {
        request.model.flags.text(flag::TOOLS_REASONING)
    }
}

/// The `openai-chat` wire dialect.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAiChat;

/// An API key, sent the way this protocol wants it: as a bearer token.
/// `source` says in words where the key came from, for `cortex auth
/// status`.
#[must_use]
pub fn api_key(key: Secret, source: impl Into<String>) -> ApiKeyAuth {
    ApiKeyAuth::new(key, KeyPlacement::Bearer, source)
}

impl Protocol for OpenAiChat {
    fn name(&self) -> &str {
        NAME
    }

    /// Everything up to the end of the last message, without the closing
    /// brackets. The servers' caches reuse the longest beginning they have
    /// seen, so this is the property that keeps a cache.
    fn stable_prefix(
        &self,
        request: &ModelRequest,
        provider: &ProviderInfo,
    ) -> Result<Vec<u8>, BuildError> {
        Ok(request::render(request, provider)?.prefix().into_bytes())
    }

    fn build(
        &self,
        request: &ModelRequest,
        provider: &ProviderInfo,
    ) -> Result<HttpRequest, BuildError> {
        let body = request::render(request, provider)?.finish();
        let mut http = HttpRequest::post(format!("{}{PATH}", provider.base_url), body.into_bytes());
        http.headers.insert("content-type", "application/json");
        http.headers.insert("accept", "text/event-stream");
        Ok(http)
    }

    fn decoder(&self, request: &ModelRequest, provider: &ProviderInfo) -> Box<dyn StreamDecoder> {
        Box::new(decode::Decoder::new(request, provider))
    }

    fn classify(&self, failure: &HttpFailure<'_>, request: &ModelRequest) -> ProviderError {
        classify::failure(failure, request)
    }

    fn read_headers(&self, headers: &Headers, now: UtcTime, out: &mut EventSink) {
        classify::read_headers(headers, now, out);
    }
}
