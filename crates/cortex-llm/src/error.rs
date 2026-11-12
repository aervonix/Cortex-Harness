//! Errors of the provider layer, and how a failure is classified
//! (`docs/architecture/04-providers.md`, "Error classification").
//!
//! Classification is structural: it uses the transport error's type, the
//! HTTP status and the provider's machine-readable error code, never the
//! text of a message. Each protocol reads its provider's codes into a
//! [`Signal`], and [`classify`] holds the rules every protocol shares.

use std::time::Duration;

use cortex_base::{Classified, ErrorKind, UtcTime};
use cortex_protocol::{ErrorData, Speed};

/// Stable codes for the failures this crate reports itself. A provider's
/// own code is used where it gave one.
pub mod code {
    /// The connection could not be made: refused, no route, a name that did
    /// not resolve, a TLS failure.
    pub const CONNECTION_FAILED: &str = "connection_failed";
    /// The connection was lost while the response was arriving.
    pub const CONNECTION_LOST: &str = "connection_lost";
    /// The transport's own time limit ran out.
    pub const TIMEOUT: &str = "timeout";
    /// No bytes arrived on the response stream for the idle limit.
    pub const IDLE_TIMEOUT: &str = "idle_timeout";
    /// The response did not begin within the limit for its first event.
    pub const FIRST_EVENT_TIMEOUT: &str = "first_event_timeout";
    /// The response stream ended before the provider's terminal event.
    pub const INCOMPLETE_STREAM: &str = "incomplete_stream";
    /// The response stream could not be decoded.
    pub const MALFORMED_STREAM: &str = "malformed_stream";
    /// The request could not be built or sent as it stands.
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// The request was cancelled.
    pub const INTERRUPTED: &str = "interrupted";
    /// A rate limit was reached, where the provider gave no code of its own.
    pub const RATE_LIMITED: &str = "rate_limited";
    /// The provider was overloaded, where it gave no code of its own.
    pub const OVERLOADED: &str = "overloaded";
    /// A failure the provider did not name.
    pub const UNKNOWN: &str = "unknown_error";
}

/// What a provider's error code says, in the terms the shared rules use.
/// Each protocol maps its provider's codes onto this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    /// The provider gave no code, or one that says nothing more than the
    /// status does.
    None,
    /// A rate limit: requests or tokens per minute. It lifts by itself.
    RateLimit,
    /// A spending cap, exhausted credit or a quota. It does not lift by
    /// waiting.
    Quota,
    /// The provider has no capacity right now.
    Overload,
    /// A failure inside the provider.
    Server,
    /// The request is larger than the model's context window.
    ContextLength,
    /// The credential is missing, wrong or not allowed.
    Auth,
}

/// Why a request was turned away for now, when the provider said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Throttle {
    /// A rate limit was reached.
    RateLimit,
    /// The provider had no capacity.
    Overload,
}

/// The shared classification rules: an HTTP status and what the provider's
/// code says, to an error kind. `status` is `None` for an error event that
/// arrived after a successful status.
///
/// A code that names the failure wins over the status it came with: a
/// spending cap is not retried because it arrived as a 429.
///
/// | Signal | Kind |
/// |---|---|
/// | a context-length code | `ContextOverflow` |
/// | a credential code; otherwise 401, 403 | `Auth` |
/// | a spending, credit or quota code; otherwise 402 | `Provider` |
/// | a rate-limit code; otherwise 429 | `Transient`, a rate limit |
/// | an overload code; otherwise 529 | `Transient`, an overload |
/// | a server-failure code; otherwise 408, 409, 5xx | `Transient` |
/// | any other 4xx; anything unnamed | `Provider` |
#[must_use]
pub fn classify(status: Option<u16>, signal: Signal) -> (ErrorKind, Option<Throttle>) {
    match (signal, status) {
        // What the provider's code names.
        (Signal::ContextLength, _) => (ErrorKind::ContextOverflow, None),
        (Signal::Auth, _) => (ErrorKind::Auth, None),
        (Signal::Quota, _) => (ErrorKind::Provider, None),
        (Signal::RateLimit, _) => (ErrorKind::Transient, Some(Throttle::RateLimit)),
        (Signal::Overload, _) => (ErrorKind::Transient, Some(Throttle::Overload)),
        // Otherwise, what the status says.
        (_, Some(401 | 403)) => (ErrorKind::Auth, None),
        (_, Some(402)) => (ErrorKind::Provider, None),
        (_, Some(429)) => (ErrorKind::Transient, Some(Throttle::RateLimit)),
        (_, Some(529)) => (ErrorKind::Transient, Some(Throttle::Overload)),
        (Signal::Server, _) | (Signal::None, Some(408 | 409 | 500..=599)) => {
            (ErrorKind::Transient, None)
        }
        (Signal::None, _) => (ErrorKind::Provider, None),
    }
}

/// A failed model request, classified.
///
/// This is what [`Protocol::classify`](crate::Protocol::classify) returns,
/// what a decoder reports for an error event after a successful status, and
/// what the retry layer decides on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProviderError {
    /// What sort of failure it is. Only `Transient` is retried here.
    pub kind: ErrorKind,
    /// A stable, machine-readable name: the provider's own code, or one of
    /// [`code`].
    pub code: String,
    /// One or two sentences for a person.
    pub message: String,
    /// The HTTP status, when the failure was an HTTP response.
    pub status: Option<u16>,
    /// The provider's id for the request, for its support channel.
    pub request_id: Option<String>,
    /// How long the provider asked the client to wait, from the moment the
    /// response arrived.
    pub retry_after: Option<Duration>,
    /// Whether it was a rate limit or an overload, when it was either.
    pub throttle: Option<Throttle>,
    /// The speed tier whose own limit or capacity turned the request away.
    /// `None` when the refusal belongs to the model, which is the ordinary
    /// case. The retry layer waits or falls back to standard speed only
    /// when this names the speed the request was sent at
    /// (`docs/decisions/0027-speed-tiers.md`).
    pub tier: Option<Speed>,
}

impl ProviderError {
    /// An error of a kind, with a code and a message and nothing else.
    #[must_use]
    pub fn new(kind: ErrorKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
            status: None,
            request_id: None,
            retry_after: None,
            throttle: None,
            tier: None,
        }
    }

    /// An error classified by the shared rules ([`classify`]).
    #[must_use]
    pub fn classified(
        status: Option<u16>,
        signal: Signal,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let (kind, throttle) = classify(status, signal);
        Self {
            status,
            throttle,
            ..Self::new(kind, code, message)
        }
    }

    /// The request was cancelled.
    #[must_use]
    pub fn interrupted() -> Self {
        Self::new(
            ErrorKind::Interrupted,
            code::INTERRUPTED,
            "The request was cancelled.",
        )
    }

    /// With the wait the provider asked for.
    #[must_use]
    pub fn with_retry_after(mut self, wait: Option<Duration>) -> Self {
        self.retry_after = wait;
        self
    }

    /// With the provider's id for the request.
    #[must_use]
    pub fn with_request_id(mut self, id: Option<String>) -> Self {
        self.request_id = id;
        self
    }

    /// Marked as the refusal of a speed tier's own limit or capacity.
    #[must_use]
    pub fn for_tier(mut self, speed: Speed) -> Self {
        self.tier = Some(speed);
        self
    }

    /// The error as every front end sees it. `now` is the moment the error
    /// arrived: the provider's wait becomes the time a retry may be made.
    #[must_use]
    pub fn to_error_data_at(&self, now: UtcTime) -> ErrorData {
        let mut data = self.to_error_data();
        data.request_id.clone_from(&self.request_id);
        data.retry_at = self
            .retry_after
            .and_then(|wait| now.checked_add(wait))
            .map(UtcTime::to_timestamp);
        data
    }
}

impl Classified for ProviderError {
    fn kind(&self) -> ErrorKind {
        self.kind
    }

    fn code(&self) -> &str {
        &self.code
    }
}

/// How a transport failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportErrorKind {
    /// The connection could not be made: refused, unreachable, a name that
    /// did not resolve, a TLS failure.
    Connect,
    /// The transport's own time limit ran out.
    Timeout,
    /// The connection was lost, or the body could not be read, after the
    /// response had begun.
    Body,
    /// The request could not be sent as it stands: a URL that is not one, a
    /// header that HTTP cannot carry. Sending it again would fail the same
    /// way.
    InvalidRequest,
    /// The request was cancelled.
    Cancelled,
}

/// A failure below HTTP: the request did not get a response, or the
/// response stopped arriving.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    /// How it failed.
    pub kind: TransportErrorKind,
    /// What happened, for a person.
    pub message: String,
}

impl TransportError {
    /// A transport failure.
    #[must_use]
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl Classified for TransportError {
    fn kind(&self) -> ErrorKind {
        match self.kind {
            TransportErrorKind::Connect | TransportErrorKind::Timeout | TransportErrorKind::Body => {
                ErrorKind::Transient
            }
            TransportErrorKind::InvalidRequest => ErrorKind::Config,
            TransportErrorKind::Cancelled => ErrorKind::Interrupted,
        }
    }

    fn code(&self) -> &str {
        match self.kind {
            TransportErrorKind::Connect => code::CONNECTION_FAILED,
            TransportErrorKind::Timeout => code::TIMEOUT,
            TransportErrorKind::Body => code::CONNECTION_LOST,
            TransportErrorKind::InvalidRequest => code::INVALID_REQUEST,
            TransportErrorKind::Cancelled => code::INTERRUPTED,
        }
    }
}

impl From<TransportError> for ProviderError {
    fn from(error: TransportError) -> Self {
        Self::new(error.kind(), error.code().to_owned(), error.message)
    }
}

/// A protocol could not build the request: the model cannot take what the
/// request holds, or the request is not one the dialect can express.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BuildError {
    /// What is wrong with the request, for a person.
    pub message: String,
}

impl BuildError {
    /// A build failure.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Classified for BuildError {
    /// A request the harness assembled and its own adapter refuses is a
    /// mistake in the harness.
    fn kind(&self) -> ErrorKind {
        ErrorKind::Internal
    }

    fn code(&self) -> &str {
        code::INVALID_REQUEST
    }
}

impl From<BuildError> for ProviderError {
    fn from(error: BuildError) -> Self {
        Self::new(error.kind(), error.code().to_owned(), error.message)
    }
}

/// A response stream could not be decoded: an event of a known type whose
/// content is not what the dialect says it is.
///
/// An event type the decoder does not know is not this. It is skipped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DecodeError {
    /// What could not be read, for a person.
    pub message: String,
}

impl DecodeError {
    /// A decode failure.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Classified for DecodeError {
    /// A stream that stops making sense part-way is most often a damaged
    /// connection, and the request is sent again.
    fn kind(&self) -> ErrorKind {
        ErrorKind::Transient
    }

    fn code(&self) -> &str {
        code::MALFORMED_STREAM
    }
}

impl From<DecodeError> for ProviderError {
    fn from(error: DecodeError) -> Self {
        Self::new(error.kind(), error.code().to_owned(), error.message)
    }
}

/// A credential could not be attached to a request: it is missing, or a
/// token could not be refreshed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AuthError {
    /// What is wrong, for a person. Never the credential.
    pub message: String,
}

impl AuthError {
    /// A credential failure.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Classified for AuthError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Auth
    }
}

impl From<AuthError> for ProviderError {
    fn from(error: AuthError) -> Self {
        Self::new(error.kind(), error.code().to_owned(), error.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_of_the_shared_table() {
        use ErrorKind::{Auth, ContextOverflow, Provider, Transient};
        let rows: &[(Option<u16>, Signal, ErrorKind, Option<Throttle>)] = &[
            // 408, 409, 5xx and provider overload codes.
            (Some(408), Signal::None, Transient, None),
            (Some(409), Signal::None, Transient, None),
            (Some(500), Signal::None, Transient, None),
            (Some(502), Signal::None, Transient, None),
            (Some(503), Signal::None, Transient, None),
            (Some(504), Signal::None, Transient, None),
            (Some(599), Signal::None, Transient, None),
            (Some(500), Signal::Server, Transient, None),
            (Some(529), Signal::None, Transient, Some(Throttle::Overload)),
            (Some(529), Signal::Overload, Transient, Some(Throttle::Overload)),
            (Some(503), Signal::Overload, Transient, Some(Throttle::Overload)),
            // 429 with a rate-limit code, and with no code at all.
            (Some(429), Signal::RateLimit, Transient, Some(Throttle::RateLimit)),
            (Some(429), Signal::None, Transient, Some(Throttle::RateLimit)),
            // 429 or 402 with a spending, credit or quota code.
            (Some(429), Signal::Quota, Provider, None),
            (Some(402), Signal::Quota, Provider, None),
            (Some(402), Signal::None, Provider, None),
            (Some(403), Signal::Quota, Provider, None),
            // 401, 403.
            (Some(401), Signal::None, Auth, None),
            (Some(403), Signal::None, Auth, None),
            (Some(400), Signal::Auth, Auth, None),
            // 400 context-length error, under whichever status it comes.
            (Some(400), Signal::ContextLength, ContextOverflow, None),
            (Some(413), Signal::ContextLength, ContextOverflow, None),
            // Other 4xx.
            (Some(400), Signal::None, Provider, None),
            (Some(404), Signal::None, Provider, None),
            (Some(413), Signal::None, Provider, None),
            (Some(422), Signal::None, Provider, None),
            // An error event after a 200: classified by its code alone.
            (None, Signal::RateLimit, Transient, Some(Throttle::RateLimit)),
            (None, Signal::Overload, Transient, Some(Throttle::Overload)),
            (None, Signal::Server, Transient, None),
            (None, Signal::Quota, Provider, None),
            (None, Signal::Auth, Auth, None),
            (None, Signal::ContextLength, ContextOverflow, None),
            (None, Signal::None, Provider, None),
        ];
        for (status, signal, kind, throttle) in rows {
            assert_eq!(
                classify(*status, *signal),
                (*kind, *throttle),
                "status {status:?}, signal {signal:?}"
            );
        }
    }

    #[test]
    fn a_quota_code_is_never_retried_whatever_the_status() {
        for status in [None, Some(400), Some(402), Some(429), Some(500), Some(529)] {
            let (kind, throttle) = classify(status, Signal::Quota);
            assert_eq!((kind, throttle), (ErrorKind::Provider, None), "{status:?}");
        }
    }

    #[test]
    fn transport_failures_are_transient_except_the_two_that_cannot_change() {
        let kinds = [
            (TransportErrorKind::Connect, ErrorKind::Transient, code::CONNECTION_FAILED),
            (TransportErrorKind::Timeout, ErrorKind::Transient, code::TIMEOUT),
            (TransportErrorKind::Body, ErrorKind::Transient, code::CONNECTION_LOST),
            (TransportErrorKind::InvalidRequest, ErrorKind::Config, code::INVALID_REQUEST),
            (TransportErrorKind::Cancelled, ErrorKind::Interrupted, code::INTERRUPTED),
        ];
        for (transport, kind, code) in kinds {
            let error: ProviderError = TransportError::new(transport, "it failed").into();
            assert_eq!(error.kind, kind, "{transport:?}");
            assert_eq!(error.code, code);
            assert_eq!(error.message, "it failed");
            assert_eq!(error.status, None);
        }
    }

    #[test]
    fn the_other_errors_keep_their_kind_as_provider_errors() {
        let build: ProviderError = BuildError::new("no images").into();
        assert_eq!((build.kind, build.code.as_str()), (ErrorKind::Internal, code::INVALID_REQUEST));
        let decode: ProviderError = DecodeError::new("not JSON").into();
        assert_eq!(
            (decode.kind, decode.code.as_str()),
            (ErrorKind::Transient, code::MALFORMED_STREAM)
        );
        let auth: ProviderError = AuthError::new("no key").into();
        assert_eq!((auth.kind, auth.code.as_str()), (ErrorKind::Auth, "auth"));
        assert_eq!(ProviderError::interrupted().kind, ErrorKind::Interrupted);
    }

    #[test]
    fn the_error_a_front_end_sees_has_the_request_id_and_the_retry_time() {
        let now = UtcTime::parse("2026-11-12T10:00:00Z").unwrap();
        let error = ProviderError::classified(
            Some(429),
            Signal::RateLimit,
            "rate_limit_error",
            "Too many requests.",
        )
        .with_retry_after(Some(Duration::from_secs(150)))
        .with_request_id(Some("req_1".to_owned()))
        .for_tier(Speed::Fast);

        assert_eq!(error.tier, Some(Speed::Fast));
        let data = error.to_error_data_at(now);
        assert_eq!(data.kind, cortex_protocol::ErrorKind::Transient);
        assert_eq!(data.code, "rate_limit_error");
        assert_eq!(data.message, "Too many requests.");
        assert_eq!(data.request_id.as_deref(), Some("req_1"));
        assert_eq!(
            data.retry_at.map(|at| at.to_string()).as_deref(),
            Some("2026-11-12T10:02:30Z")
        );
        assert_eq!(error.to_string(), "Too many requests.");
    }
}
