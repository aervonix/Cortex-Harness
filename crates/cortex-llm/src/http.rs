//! The HTTP request and response as this crate sees them: plain data that a
//! protocol builds and a transport carries
//! (`docs/architecture/04-providers.md`, "Traits").
//!
//! Nothing here does I/O. A [`Protocol`](crate::Protocol) returns an
//! [`HttpRequest`], a [`Transport`](crate::Transport) turns it into an
//! [`HttpResponse`], and the tests of both work on these values alone.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use cortex_base::{Secret, UtcTime};
use futures_core::Stream;

use crate::error::TransportError;
use crate::stream::{Emitter, Generated};

/// What a header's value prints as when it is a credential.
const REDACTED: &str = "[redacted]";

/// The headers of a request or a response.
///
/// Names are compared without regard to case and kept in lower case, as
/// HTTP/2 writes them. A header can be marked **sensitive**: its value is
/// still sent, and never printed. Credentials are added that way
/// ([`Headers::insert_secret`]), so a request can be logged whole.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<Header>,
}

#[derive(Clone, PartialEq, Eq)]
struct Header {
    name: String,
    value: String,
    sensitive: bool,
}

impl Headers {
    /// No headers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a header, replacing any value it had.
    pub fn insert(&mut self, name: &str, value: impl Into<String>) {
        self.set(name, value.into(), false);
    }

    /// Sets a header whose value is a credential. It is sent like any other
    /// and printed as `[redacted]`.
    pub fn insert_secret(&mut self, name: &str, value: &Secret) {
        self.set(name, value.expose().to_owned(), true);
    }

    /// Sets a header whose value is a credential with something in front of
    /// it, as in `Bearer <token>`.
    pub fn insert_secret_with_prefix(&mut self, name: &str, prefix: &str, value: &Secret) {
        self.set(name, format!("{prefix}{}", value.expose()), true);
    }

    fn set(&mut self, name: &str, value: String, sensitive: bool) {
        let name = name.to_ascii_lowercase();
        self.entries.retain(|header| header.name != name);
        self.entries.push(Header {
            name,
            value,
            sensitive,
        });
    }

    /// The value of a header, by name in any case.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }

    /// Whether a header is marked as holding a credential.
    #[must_use]
    pub fn is_sensitive(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|header| header.sensitive && header.name.eq_ignore_ascii_case(name))
    }

    /// Sets every header of `other` here, replacing values of the same
    /// name. A sensitive header stays sensitive.
    pub fn merge(&mut self, other: &Self) {
        for header in &other.entries {
            self.set(&header.name, header.value.clone(), header.sensitive);
        }
    }

    /// Removes a header.
    pub fn remove(&mut self, name: &str) {
        self.entries
            .retain(|header| !header.name.eq_ignore_ascii_case(name));
    }

    /// Every header as `(name, value)`, in the order they were set, **with
    /// the real values**. This is what a transport sends. Print a `Headers`
    /// with `{:?}` instead when the point is to show it.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
    }

    /// How many headers there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl std::fmt::Debug for Headers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut map = f.debug_map();
        for header in &self.entries {
            let value = if header.sensitive {
                REDACTED
            } else {
                header.value.as_str()
            };
            map.entry(&header.name, &value);
        }
        map.finish()
    }
}

impl<'a> FromIterator<(&'a str, &'a str)> for Headers {
    fn from_iter<I: IntoIterator<Item = (&'a str, &'a str)>>(iter: I) -> Self {
        let mut headers = Self::new();
        for (name, value) in iter {
            headers.insert(name, value);
        }
        headers
    }
}

/// The HTTP method of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`: a model list, a usage endpoint.
    Get,
    /// `POST`: a model call.
    Post,
}

impl Method {
    /// The method as HTTP writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// One HTTP request, complete: what a protocol builds for one model call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// The method.
    pub method: Method,
    /// The full URL.
    pub url: String,
    /// The headers. Credentials are added by the
    /// [`AuthMethod`](crate::AuthMethod), as sensitive headers, after the
    /// protocol has built the rest.
    pub headers: Headers,
    /// The body, as the bytes to send.
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// A `POST` of a body to a URL, with no headers yet.
    #[must_use]
    pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            headers: Headers::new(),
            body,
        }
    }
}

/// The start of a response: its status and headers, and the body still to
/// be read. A transport returns one for **every** status; whether the status
/// is a failure is for the protocol to say.
#[derive(Debug)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// The response headers.
    pub headers: Headers,
    /// The body, arriving in chunks.
    pub body: BodyStream,
}

/// A response body: chunks of bytes as the network delivers them, split
/// wherever a read happened to end.
///
/// Dropping it closes the connection, which is how a request is cancelled
/// (`docs/architecture/02-runtime.md`, "Cancellation").
pub struct BodyStream {
    inner: Pin<Box<dyn Stream<Item = Result<Vec<u8>, TransportError>> + Send>>,
}

impl BodyStream {
    /// Wraps a stream of chunks.
    pub fn new(
        stream: impl Stream<Item = Result<Vec<u8>, TransportError>> + Send + 'static,
    ) -> Self {
        Self {
            inner: Box::pin(stream),
        }
    }

    /// A body made by a piece of async code that hands over each chunk as
    /// it has it. The code runs only as far as the reader has asked.
    pub fn generate<F, Fut>(body: F) -> Self
    where
        F: FnOnce(Emitter<Result<Vec<u8>, TransportError>>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::new(Generated::new(body))
    }

    /// A body that is already here, in the chunks given.
    #[must_use]
    pub fn from_chunks(chunks: Vec<Vec<u8>>) -> Self {
        Self::generate(move |out| async move {
            for chunk in chunks {
                out.emit(Ok(chunk)).await;
            }
        })
    }

    /// The next chunk. `None` when the body is complete.
    pub async fn next(&mut self) -> Option<Result<Vec<u8>, TransportError>> {
        std::future::poll_fn(|context| self.inner.as_mut().poll_next(context)).await
    }
}

impl Stream for BodyStream {
    type Item = Result<Vec<u8>, TransportError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

impl std::fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BodyStream")
    }
}

/// Reads a `retry-after` header: how long the server asks the client to
/// wait. `None` when the value is neither form the header may take.
///
/// The value is either a number of seconds (providers also send fractions,
/// which are accepted) or an HTTP date, which is measured from `now`. A date
/// in the past is a wait of zero.
#[must_use]
pub fn parse_retry_after(value: &str, now: UtcTime) -> Option<Duration> {
    let value = value.trim();
    if let Some(seconds) = parse_seconds(value) {
        return Some(seconds);
    }
    let at = parse_http_date(value)?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// The longest wait a header may ask for: a year. Anything longer is a
/// mistake at the other end, and capping it keeps the arithmetic safe.
const LONGEST_WAIT: Duration = Duration::from_secs(366 * 24 * 60 * 60);

/// A count of seconds, whole or with a fraction: `30`, `1.5`.
fn parse_seconds(value: &str) -> Option<Duration> {
    let plain = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && value.bytes().filter(|byte| *byte == b'.').count() <= 1
        && value.bytes().any(|byte| byte.is_ascii_digit());
    if !plain {
        return None;
    }
    let seconds: f64 = value.parse().ok()?;
    if !seconds.is_finite() {
        return None;
    }
    Some(Duration::try_from_secs_f64(seconds).map_or(LONGEST_WAIT, |wait| wait.min(LONGEST_WAIT)))
}

/// How long until `at`, seen from `now`. Zero when it has passed. For the
/// reset times providers put in their rate-limit headers.
#[must_use]
pub fn wait_until(at: UtcTime, now: UtcTime) -> Duration {
    at.duration_since(now)
        .unwrap_or(Duration::ZERO)
        .min(LONGEST_WAIT)
}

/// Reads the one date form HTTP still writes: `Sun, 06 Nov 1994 08:49:37 GMT`.
fn parse_http_date(value: &str) -> Option<UtcTime> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_weekday, rest) = value.split_once(", ")?;
    let mut parts = rest.split(' ');
    let day = parts.next()?;
    let month = parts.next()?;
    let year = parts.next()?;
    let time = parts.next()?;
    if parts.next()? != "GMT" || parts.next().is_some() {
        return None;
    }
    let month = MONTHS.iter().position(|name| *name == month)? + 1;
    let digits = |text: &str, len: usize| {
        (text.len() == len && text.bytes().all(|byte| byte.is_ascii_digit())).then_some(())
    };
    digits(day, 2)?;
    digits(year, 4)?;
    if time.len() != 8 {
        return None;
    }
    // The same fields, in the one form the time type reads. It checks the
    // ranges, so the 31st of February is refused there.
    UtcTime::parse(&format!("{year}-{month:02}-{day}T{time}Z")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> UtcTime {
        UtcTime::parse("2026-11-12T10:00:00Z").unwrap()
    }

    #[test]
    fn header_names_ignore_case_and_a_second_value_replaces_the_first() {
        let mut headers = Headers::new();
        headers.insert("Content-Type", "application/json");
        headers.insert("X-Trace", "one");
        headers.insert("x-trace", "two");
        assert_eq!(headers.get("content-type"), Some("application/json"));
        assert_eq!(headers.get("CONTENT-TYPE"), Some("application/json"));
        assert_eq!(headers.get("x-trace"), Some("two"));
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers.iter().collect::<Vec<_>>(),
            [("content-type", "application/json"), ("x-trace", "two")]
        );
        headers.remove("X-TRACE");
        assert_eq!(headers.get("x-trace"), None);
        assert!(!headers.is_empty());
    }

    #[test]
    fn a_credential_is_sent_and_never_printed() {
        let key = Secret::new("sk-live-12345");
        let mut headers = Headers::new();
        headers.insert("accept", "text/event-stream");
        headers.insert_secret("x-api-key", &key);
        headers.insert_secret_with_prefix("Authorization", "Bearer ", &key);

        assert_eq!(headers.get("x-api-key"), Some("sk-live-12345"));
        assert_eq!(headers.get("authorization"), Some("Bearer sk-live-12345"));
        assert!(headers.is_sensitive("X-Api-Key"));
        assert!(!headers.is_sensitive("accept"));

        let request = HttpRequest {
            headers,
            ..HttpRequest::post("https://example.invalid/v1", b"{}".to_vec())
        };
        let printed = format!("{request:?}");
        assert!(!printed.contains("sk-live"), "{printed}");
        assert!(printed.contains("[redacted]"), "{printed}");
        assert!(printed.contains("text/event-stream"), "{printed}");
    }

    #[test]
    fn retry_after_reads_seconds() {
        assert_eq!(
            parse_retry_after("30", now()),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after(" 0 ", now()), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after("1.5", now()),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            parse_retry_after("99999999999999999999", now()),
            Some(LONGEST_WAIT),
            "a wait no one means is capped, not an overflow"
        );
    }

    #[test]
    fn retry_after_reads_a_date_and_measures_it_from_now() {
        assert_eq!(
            parse_retry_after("Thu, 12 Nov 2026 10:02:00 GMT", now()),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            parse_retry_after("Thu, 12 Nov 2026 09:00:00 GMT", now()),
            Some(Duration::ZERO),
            "a date that has passed is no wait"
        );
    }

    #[test]
    fn retry_after_refuses_what_it_cannot_read() {
        for value in [
            "",
            "soon",
            "-5",
            "1e3",
            "1.2.3",
            ".",
            "inf",
            "NaN",
            "12 Nov 2026 10:02:00 GMT",
            "Thu, 12 Nov 2026 10:02:00",
            "Thu, 12 Nov 2026 10:02:00 UTC",
            "Thu, 31 Feb 2026 10:02:00 GMT",
            "Thu, 12 Nov 2026 10:02:00 GMT extra",
            "Thu, 1 Nov 2026 10:02:00 GMT",
        ] {
            assert_eq!(parse_retry_after(value, now()), None, "{value:?}");
        }
    }

    #[test]
    fn a_reset_time_is_a_wait_from_now() {
        let at = UtcTime::parse("2026-11-12T10:00:45Z").unwrap();
        assert_eq!(wait_until(at, now()), Duration::from_secs(45));
        assert_eq!(wait_until(now(), at), Duration::ZERO);
    }
}
