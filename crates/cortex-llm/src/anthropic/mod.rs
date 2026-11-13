//! The `anthropic-messages` protocol: Anthropic's Messages API, streamed
//! (`docs/architecture/04-providers.md`).
//!
//! | Part | Where | What it is tested from |
//! |---|---|---|
//! | The request | [`request`] | A snapshot of the body for each kind of request. |
//! | The response stream | [`decode`] | Recorded streams, replayed cut at every byte. |
//! | Failures and headers | [`classify`] | The classification table, row by row. |
//!
//! The provider's facts here were read from its documentation on
//! 2026-11-13: the Messages API reference, and the pages on streaming,
//! prompt caching, thinking and preserved thinking, effort, tool use, fast
//! mode, errors and rate limits. The API version sent is [`API_VERSION`].
//!
//! # What must stay the same between requests
//!
//! The provider's cache reads a request as tools, then the system prompt,
//! then the messages, and reuses the longest beginning it has seen. Its
//! newer models also tie each reasoning block they produce to everything
//! that came before it, and answer a request that sends such a block back
//! after a change to earlier content with an error. So the adapter never
//! rewrites what it has sent: the body is written in a fixed order, a block
//! the provider produced goes back as the text that was stored, and the one
//! thing that moves is a cache marker, which is not content.
//!
//! # Flags
//!
//! The flags this protocol reads. It reads no others.
//!
//! | On | Flag | Value | Meaning |
//! |---|---|---|---|
//! | a provider | `replay_reasoning` | `"all"` or `"readable"` | Which reasoning blocks of this provider's own earlier messages are sent back after a change of model. `"all"`: every one, unchanged; the server leaves out what the new model cannot read. That is what Anthropic's own API asks for, and its built-in entry says so. `"readable"`, the default for any other server: only to the model that wrote them or one whose catalog entry lists that model in `reads_reasoning_from`; for any other model the message is sent as its neutral projection. |
//! | a provider | `eager_tool_streaming` | on or off | Whether tools ask for their arguments to be streamed as they are written. On unless set off, for a server that does not know the field. |
//! | a provider | `api_version` | text | The `anthropic-version` to send, in place of [`API_VERSION`]. |
//! | a provider | `beta` | text | Names for the `anthropic-beta` header, separated by commas, sent with every request. |
//! | a model | `speed_beta` | text | The beta name a request must carry to name a speed tier on this model. For the models whose fast mode is a preview. |

mod classify;
mod decode;
mod request;

use cortex_base::{Secret, UtcTime};

use crate::auth::{ApiKeyAuth, KeyPlacement};
use crate::error::{BuildError, ProviderError};
use crate::event::EventSink;
use crate::http::{Headers, HttpRequest};
use crate::protocol::{FlagValue, HttpFailure, Protocol, ProviderInfo, StreamDecoder};
use crate::request::ModelRequest;

/// The protocol's name, as catalog and provider entries write it.
pub const NAME: &str = "anthropic-messages";

/// The API version every request names, unless the provider's entry names
/// another.
pub const API_VERSION: &str = "2026-10-21";

/// The built-in provider's name.
pub const PROVIDER: &str = "anthropic";

/// The built-in provider's base URL.
pub const BASE_URL: &str = "https://api.anthropic.com";

/// The header an API key is sent in.
pub const KEY_HEADER: &str = "x-api-key";

/// The names of the flags this protocol reads.
pub mod flag {
    /// On a provider: which reasoning blocks are sent back after a change
    /// of model, `"all"` or `"readable"`.
    pub const REPLAY_REASONING: &str = "replay_reasoning";
    /// On a provider: whether tools ask for streamed arguments.
    pub const EAGER_TOOL_STREAMING: &str = "eager_tool_streaming";
    /// On a provider: the API version to send.
    pub const API_VERSION: &str = "api_version";
    /// On a provider: names for the `anthropic-beta` header.
    pub const BETA: &str = "beta";
    /// On a model: the beta name a request that names a speed must carry.
    pub const SPEED_BETA: &str = "speed_beta";
}

/// The flags, read.
mod flags {
    use super::flag;
    use crate::protocol::ProviderInfo;

    /// Whether every reasoning block of the provider's own messages is sent
    /// back, whatever the model.
    pub(super) fn replays_every_block(provider: &ProviderInfo) -> bool {
        provider.flags.text(flag::REPLAY_REASONING) == Some("all")
    }

    pub(super) fn eager_tool_streaming(provider: &ProviderInfo) -> bool {
        provider
            .flags
            .bool(flag::EAGER_TOOL_STREAMING)
            .unwrap_or(true)
    }
}

/// The `anthropic-messages` wire dialect.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicMessages;

impl AnthropicMessages {
    /// The part of a request's body that later requests of the same context
    /// epoch repeat byte for byte: everything up to the end of the last
    /// message, without the closing brackets and without the one cache
    /// marker that can be inside the messages.
    ///
    /// For two requests of one epoch, the earlier one's is the beginning of
    /// the later one's. That is the property the provider's cache needs,
    /// and its check of reasoning blocks: a harness can compare the two to
    /// find where a prefix first differed.
    pub fn stable_prefix(
        &self,
        request: &ModelRequest,
        provider: &ProviderInfo,
    ) -> Result<Vec<u8>, BuildError> {
        let _ = self;
        Ok(request::render(request, provider)?.prefix().into_bytes())
    }
}

/// Anthropic's own API as a provider entry: the built-in one.
#[must_use]
pub fn provider() -> ProviderInfo {
    let mut provider = ProviderInfo::new(PROVIDER, NAME, BASE_URL);
    provider
        .flags
        .set(flag::REPLAY_REASONING, FlagValue::Text("all".to_owned()));
    provider
}

/// An API key, sent the way this protocol wants it. `source` says in words
/// where the key came from, for `cortex auth status`.
#[must_use]
pub fn api_key(key: Secret, source: impl Into<String>) -> ApiKeyAuth {
    ApiKeyAuth::new(key, KeyPlacement::Header(KEY_HEADER.to_owned()), source)
}

impl Protocol for AnthropicMessages {
    fn name(&self) -> &str {
        NAME
    }

    fn build(
        &self,
        request: &ModelRequest,
        provider: &ProviderInfo,
    ) -> Result<HttpRequest, BuildError> {
        let body = request::render(request, provider)?.finish();
        let mut http = HttpRequest::post(
            format!("{}/v1/messages", provider.base_url),
            body.into_bytes(),
        );
        http.headers.insert("content-type", "application/json");
        http.headers.insert("accept", "text/event-stream");
        http.headers.insert(
            "anthropic-version",
            provider
                .flags
                .text(flag::API_VERSION)
                .unwrap_or(API_VERSION),
        );
        // The beta names: the provider's own, and the one a model needs
        // before it takes a speed.
        let mut betas: Vec<&str> = provider
            .flags
            .text(flag::BETA)
            .into_iter()
            .flat_map(|names| names.split(','))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        if request::sends_speed(request)
            && let Some(beta) = request.model.flags.text(flag::SPEED_BETA)
            && !betas.contains(&beta)
        {
            betas.push(beta);
        }
        if !betas.is_empty() {
            http.headers.insert("anthropic-beta", betas.join(","));
        }
        Ok(http)
    }

    fn decoder(&self, request: &ModelRequest, _provider: &ProviderInfo) -> Box<dyn StreamDecoder> {
        Box::new(decode::Decoder::new(request))
    }

    fn classify(&self, failure: &HttpFailure<'_>, request: &ModelRequest) -> ProviderError {
        classify::failure(failure, request)
    }

    fn read_headers(&self, headers: &Headers, _now: UtcTime, out: &mut EventSink) {
        classify::read_headers(headers, out);
    }
}
