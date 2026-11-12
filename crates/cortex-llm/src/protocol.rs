//! Protocols and providers: a wire dialect is code, an endpoint is data
//! (`docs/architecture/04-providers.md`, "Three separate concepts").
//!
//! - A [`Protocol`] is a wire dialect: how to build a request, how to decode
//!   a response stream and how to classify a failure. It is pure: no I/O,
//!   no clock of its own, no state between requests. Each one is tested
//!   from files.
//! - A [`ProviderInfo`] is an endpoint that speaks a protocol: a base URL,
//!   headers, capability flags. It comes from the built-in table or from
//!   the user's configuration.
//! - A model is a catalog entry ([`ModelEntry`](crate::ModelEntry)).
//!
//! A quirk of a server is a flag on its provider or model entry, never a
//! test of a model's name.

use std::collections::BTreeMap;
use std::time::Duration;

use cortex_base::UtcTime;
use cortex_protocol::ProviderId;
use serde::Deserialize;

use crate::error::{BuildError, DecodeError, ProviderError};
use crate::event::EventSink;
use crate::http::{Headers, HttpRequest};
use crate::request::ModelRequest;
use crate::sse::SseEvent;

/// The value of a capability flag.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum FlagValue {
    /// On or off.
    Bool(bool),
    /// A count or a size.
    Number(i64),
    /// A name, such as the field a server puts its reasoning in.
    Text(String),
}

/// Capability flags of a provider or a model: what a protocol needs to know
/// about this server or this model that the neutral fields do not say.
///
/// The names belong to the protocols. Each protocol documents the flags it
/// reads, beside its fixtures, and reads no others.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Flags(BTreeMap<String, FlagValue>);

impl Flags {
    /// No flags.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a flag.
    pub fn set(&mut self, name: impl Into<String>, value: FlagValue) {
        self.0.insert(name.into(), value);
    }

    /// A flag, whatever its type.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&FlagValue> {
        self.0.get(name)
    }

    /// An on-or-off flag. `None` when it is not set or is not a boolean.
    #[must_use]
    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.0.get(name) {
            Some(FlagValue::Bool(value)) => Some(*value),
            _ => None,
        }
    }

    /// A text flag. `None` when it is not set or is not text.
    #[must_use]
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.0.get(name) {
            Some(FlagValue::Text(value)) => Some(value),
            _ => None,
        }
    }

    /// A number flag. `None` when it is not set or is not a number.
    #[must_use]
    pub fn number(&self, name: &str) -> Option<i64> {
        match self.0.get(name) {
            Some(FlagValue::Number(value)) => Some(*value),
            _ => None,
        }
    }

    /// Every flag, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &FlagValue)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// An endpoint that speaks a protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    /// The provider's configured name.
    pub id: ProviderId,
    /// The wire dialect it speaks, by the protocol's name.
    pub protocol: String,
    /// The base URL, without a trailing slash.
    pub base_url: String,
    /// Headers added to every request, from the provider's entry. A value
    /// that came from the environment is set as a sensitive header.
    pub headers: Headers,
    /// Capability flags of the server.
    pub flags: Flags,
    /// The longest silence allowed on a response stream. `None` uses the
    /// retry policy's value.
    pub idle_timeout: Option<Duration>,
    /// The longest wait for the first event of a response. `None` uses the
    /// retry policy's value. A local server that reads a long prompt before
    /// it sends anything needs more than a hosted one.
    pub first_event_timeout: Option<Duration>,
}

impl ProviderInfo {
    /// A provider with a name, a protocol and a base URL, and nothing else.
    #[must_use]
    pub fn new(
        id: impl Into<ProviderId>,
        protocol: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            id: id.into(),
            protocol: protocol.into(),
            base_url,
            headers: Headers::new(),
            flags: Flags::new(),
            idle_timeout: None,
            first_event_timeout: None,
        }
    }
}

/// A response whose status is not a success, with all of its body that was
/// read.
#[derive(Debug, Clone, Copy)]
pub struct HttpFailure<'a> {
    /// The HTTP status.
    pub status: u16,
    /// The response headers.
    pub headers: &'a Headers,
    /// The body, up to the limit the client reads of a failed response.
    pub body: &'a [u8],
    /// When the response arrived, for a header that gives a date.
    pub now: UtcTime,
}

/// A wire dialect. Stateless and pure: no I/O.
pub trait Protocol: Send + Sync + std::fmt::Debug {
    /// The dialect's name, as catalog and provider entries write it:
    /// `anthropic-messages`, `openai-chat`.
    fn name(&self) -> &str;

    /// Builds the HTTP request for one model call, without credentials.
    ///
    /// `request.speed` is the speed to ask for. A protocol always sends an
    /// explicit tier, also for `standard`, and never relies on a default
    /// set at the provider, so that what was asked for is known.
    fn build(
        &self,
        request: &ModelRequest,
        provider: &ProviderInfo,
    ) -> Result<HttpRequest, BuildError>;

    /// A fresh decoder for the response to `request`.
    fn decoder(&self, request: &ModelRequest, provider: &ProviderInfo) -> Box<dyn StreamDecoder>;

    /// Turns a failed response into a typed error: the kind, the provider's
    /// code, the wait it asked for, its request id.
    ///
    /// `request` is the request as it was sent. A protocol uses it to tell
    /// a speed tier's own limit from the model's: when the refusal belongs
    /// to the tier the request was sent at, the error names that tier
    /// ([`ProviderError::for_tier`]).
    fn classify(&self, failure: &HttpFailure<'_>, request: &ModelRequest) -> ProviderError;

    /// Reads the headers of a successful response into the sink: the
    /// provider's request id, the rate limits it reports, and the speed
    /// that served the request where a header says it. The default reads
    /// nothing.
    fn read_headers(&self, headers: &Headers, now: UtcTime, out: &mut EventSink) {
        let _ = (headers, now, out);
    }
}

/// An incremental decoder: the events of one response stream in, neutral
/// model events out.
///
/// The rules every decoder follows:
///
/// - Blocks are keyed by index. Indexes need not be contiguous or in order.
/// - An event type the decoder does not know is skipped, never an error.
/// - An error event after a successful status is written with
///   [`EventSink::error`], classified like any other failure.
/// - It reads until the provider's terminal event, because some providers
///   send required data (a signature, the usage) in the last chunk.
/// - It reports the speed that served the request only from what the
///   response says, and never fills it in from the request.
pub trait StreamDecoder: Send {
    /// Decodes one event of the stream.
    fn feed(&mut self, event: SseEvent<'_>, out: &mut EventSink) -> Result<(), DecodeError>;

    /// The stream has ended. Writes whatever the decoder was still holding.
    fn finish(&mut self, out: &mut EventSink) -> Result<(), DecodeError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_read_by_type() {
        let flags: Flags = serde_json::from_str(
            r#"{"usage_in_stream": false, "reasoning_field": "reasoning_content", "max_tools": 128}"#,
        )
        .unwrap();
        assert_eq!(flags.bool("usage_in_stream"), Some(false));
        assert_eq!(flags.text("reasoning_field"), Some("reasoning_content"));
        assert_eq!(flags.number("max_tools"), Some(128));
        // A flag of another type is not converted, and an absent one is absent.
        assert_eq!(flags.bool("reasoning_field"), None);
        assert_eq!(flags.text("usage_in_stream"), None);
        assert_eq!(flags.number("usage_in_stream"), None);
        assert_eq!(flags.bool("tool_choice"), None);
        assert_eq!(flags.iter().count(), 3);
        assert!(!flags.is_empty());

        let mut flags = Flags::new();
        assert!(flags.is_empty());
        flags.set("tool_choice", FlagValue::Bool(false));
        assert_eq!(flags.get("tool_choice"), Some(&FlagValue::Bool(false)));
    }

    #[test]
    fn a_base_url_loses_its_trailing_slashes() {
        let provider = ProviderInfo::new("local", "openai-chat", "http://localhost:8080/v1//");
        assert_eq!(provider.base_url, "http://localhost:8080/v1");
        assert_eq!(provider.id.as_str(), "local");
        assert_eq!(provider.idle_timeout, None);
    }
}
