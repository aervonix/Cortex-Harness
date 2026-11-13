//! Classifying a failure, and reading the headers of a response.
//!
//! # What a server says about a failure
//!
//! The dialect's own form is `{"error": {"message", "type", "code"}}`, and
//! the servers that speak it have each bent it:
//!
//! | Server | The body of a failure |
//! |---|---|
//! | OpenAI, DeepSeek, Ollama | `error.message`, `error.type`, and `error.code` as a name or null |
//! | OpenRouter | `error.code` as the HTTP status, `error.message`, and `error.metadata` with `error_type`, and `limit_source` for a budget |
//! | Venice.ai | `error` as a sentence, and a top-level `code` in capitals for some failures |
//! | llama.cpp | `error.code` as the HTTP status, `error.message`, `error.type` |
//! | vLLM | `error` as above, or the members at the top level |
//!
//! Every name a body gives (a code that is a word, a typed `error_type`, a
//! `limit_source`, the `type`) is read, most exact first, and the first one
//! the table below knows decides. Names are compared without regard to
//! case.
//!
//! # The classification table
//!
//! | The body names | Signal | Kind |
//! |---|---|---|
//! | `invalid_api_key`, `authentication_error`, `authentication`, `authentication_failed`, `authentication_failed_inactive_key`, `unauthorized`, `permission_error`, `api_access_disabled` | credential | `Auth` |
//! | `insufficient_quota`, `credit_balance_exhausted`, `organization_spend_limit_exceeded`, `project_spend_limit_exceeded`, `organization_usage_limit_exceeded`, `insufficient_balance`, `api_key_usd_spend_limit_exceeded`, `api_key_diem_spend_limit_exceeded`, `payment_required`, `openrouter_guardrail_budget`, `openrouter_workspace_budget`, `openrouter_key_limit` | quota | `Provider`: never retried |
//! | `rate_limit_exceeded`, `rate_limit_error`, `slow_down`, `openrouter_in_flight_budget` | rate limit | `Transient`, a rate limit, with the server's wait |
//! | `server_is_overloaded`, `service_unavailable_error`, `provider_overloaded`, `model_overloaded`, `model_at_capacity`, `model_offline`, `unavailable_error`, `insufficient_system_resource` | overload | `Transient`, an overload |
//! | `server_error`, `server`, `api_error`, `inference_failed`, `unknown_error`, `timeout`, `request_timeout` | server failure | `Transient` |
//! | `context_length_exceeded`, `exceed_context_size_error`, `too_many_tokens`, `payload_too_large` | context length | `ContextOverflow` |
//! | `content_policy_violation`, `refusal` | none | `Provider`, whatever the status |
//! | nothing the table knows | | by the status, below |
//!
//! | The status, when no name decided | Kind |
//! |---|---|
//! | 413 | `ContextOverflow`: the request is too large in bytes, which the loop answers the same way |
//! | 400 or 422 whose message says "maximum context length" | `ContextOverflow` |
//! | 503 | `Transient`, an overload: the servers use it for "no capacity right now" |
//! | none: the failure arrived inside a successful response and carries no status | `Transient`: a response that began and then failed without saying how may be asked for again |
//! | anything else | the shared rules ([`crate::error::classify`]) |
//!
//! - **A code wins over the status it came with.** OpenAI sends a spending
//!   cap and exhausted credit as a 429. The code says what it is, and it is
//!   never retried.
//! - **A context that is too long has a code at some servers and only a
//!   sentence at others.** "maximum context length" is the dialect's
//!   long-standing wording, which the servers without a code reuse. It is
//!   the one place a message is read here, for the same reason as in the
//!   other protocol: the loop answers this failure by compacting, and must
//!   be able to tell it from every other 400.
//! - **A refusal by moderation is not a credential's failure.** OpenRouter
//!   answers one with a 403, and says what was flagged in `metadata`
//!   (`reasons`, `flagged_input`). With or without a typed name it is read
//!   as `content_policy_violation`.
//! - **No part of a key is kept.** A server may repeat the key in the
//!   message of a failure, masked to its last characters. The word is
//!   taken out of the message before it is kept.
//! - **An error inside a successful response** is classified by this table
//!   too. It has no status; where the body carries a status as its code
//!   (OpenRouter, llama.cpp), that stands in for it.
//!
//! # A refusal by a speed tier
//!
//! The dialect has no header that says whose limit a refusal was. So the
//! tier's catalog entry decides:
//!
//! | The request was sent at | The failure is | The tier's entry says | It belongs to |
//! |---|---|---|---|
//! | `standard` | anything | | the model |
//! | a tier | anything | the provider serves standard when it cannot serve the tier | the model: the provider does not refuse a tier, it downgrades |
//! | a tier | an overload | it refuses | the tier |
//! | a tier | a rate limit | it refuses, and has a limit of its own | the tier |
//! | a tier | a rate limit | it refuses, and shares the model's limit | the model |
//! | a tier | anything else | | the model |
//!
//! # The rate-limit headers
//!
//! `x-ratelimit-{limit,remaining,reset}-{requests,tokens}`. The reset is
//! written three ways: a duration (`1s`, `6m0s`: OpenAI), a time in
//! milliseconds since 1970 (Venice.ai), or a count of seconds. All three
//! are read. A 429 with no `retry-after` waits until the reset of a limit
//! that has nothing left.

use std::time::Duration;

use cortex_base::UtcTime;
use cortex_protocol::Speed;
use cortex_protocol::speed::SpeedUnavailable;
use cortex_protocol::usage::{LimitMeasure, LimitWindow};
use serde_json::Value;

use crate::error::{ProviderError, Signal, Throttle, code};
use crate::event::EventSink;
use crate::http::{Headers, parse_retry_after, wait_until};
use crate::protocol::HttpFailure;
use crate::request::ModelRequest;

/// The headers that carry a server's id for a request, in the order they
/// are tried.
const REQUEST_ID: &[&str] = &["x-request-id", "request-id"];

/// The most of a server's message that is kept. A message is a sentence
/// or two; the limit keeps a page of HTML from a proxy out of an error.
const MESSAGE_LIMIT: usize = 600;

/// How the dialect words a context that is too long, at the servers that
/// give no code for it (see the [module](self)).
const CONTEXT_TOO_LONG: &str = "maximum context length";

/// The name given to a refusal by moderation that carries none of its own.
const MODERATED: &str = "content_policy_violation";

/// What a name in a failure's body says. This is the first table of the
/// [module](self). `None` for a name the table does not know.
pub(crate) fn signal(name: &str) -> Option<Named> {
    let signal = match name.to_ascii_lowercase().as_str() {
        "invalid_api_key"
        | "authentication_error"
        | "authentication"
        | "authentication_failed"
        | "authentication_failed_inactive_key"
        | "unauthorized"
        | "permission_error"
        | "api_access_disabled" => Signal::Auth,
        "insufficient_quota"
        | "credit_balance_exhausted"
        | "organization_spend_limit_exceeded"
        | "project_spend_limit_exceeded"
        | "organization_usage_limit_exceeded"
        | "insufficient_balance"
        | "api_key_usd_spend_limit_exceeded"
        | "api_key_diem_spend_limit_exceeded"
        | "payment_required"
        | "openrouter_guardrail_budget"
        | "openrouter_workspace_budget"
        | "openrouter_key_limit" => Signal::Quota,
        "rate_limit_exceeded"
        | "rate_limit_error"
        | "slow_down"
        | "openrouter_in_flight_budget" => Signal::RateLimit,
        "server_is_overloaded"
        | "service_unavailable_error"
        | "provider_overloaded"
        | "model_overloaded"
        | "model_at_capacity"
        | "model_offline"
        | "unavailable_error"
        | "insufficient_system_resource" => Signal::Overload,
        "server_error" | "server" | "api_error" | "inference_failed" | "unknown_error"
        | "timeout" | "request_timeout" => Signal::Server,
        "context_length_exceeded"
        | "exceed_context_size_error"
        | "too_many_tokens"
        | "payload_too_large" => Signal::ContextLength,
        "content_policy_violation" | "refusal" => return Some(Named::Refused),
        _ => return None,
    };
    Some(Named::Signal(signal))
}

/// What a name the table knows says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Named {
    /// One of the shared signals.
    Signal(Signal),
    /// The request was refused for what it holds. Not a credential's
    /// failure, whatever status it came with, and not retried.
    Refused,
}

/// What a failure's body says, read from any of the forms in the
/// [module](self).
#[derive(Debug, Default, PartialEq, Eq)]
struct Said {
    /// The names the body gives, most exact first.
    names: Vec<String>,
    /// A status the body carries as its code.
    status: Option<u16>,
    message: String,
    request_id: Option<String>,
}

fn said(body: &[u8]) -> Said {
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else {
        return Said::default();
    };
    let mut said = Said::default();
    let mut codes: Vec<&Value> = Vec::new();
    let mut kinds: Vec<&Value> = Vec::new();
    match body.get("error") {
        Some(Value::Object(error)) => {
            if let Some(Value::Object(metadata)) = error.get("metadata") {
                kinds.extend(metadata.get("error_type"));
                kinds.extend(metadata.get("limit_source"));
                // OpenRouter's moderation refusal says what was flagged
                // and why, and may give no typed name beside it.
                if metadata.contains_key("flagged_input") || metadata.contains_key("reasons") {
                    said.names.push(MODERATED.to_owned());
                }
            }
            codes.extend(error.get("code"));
            kinds.extend(error.get("type"));
            if let Some(Value::String(message)) = error.get("message") {
                said.message.clone_from(message);
            }
        }
        Some(Value::String(message)) => said.message.clone_from(message),
        _ => {}
    }
    codes.extend(body.get("code"));
    kinds.extend(body.get("type"));
    if said.message.is_empty()
        && let Some(Value::String(message)) = body.get("message")
    {
        said.message.clone_from(message);
    }
    for value in codes.into_iter().chain(kinds) {
        match value {
            Value::String(name) if !name.is_empty() => said.names.push(name.clone()),
            Value::Number(number) => {
                let status = number.as_u64().and_then(|code| u16::try_from(code).ok());
                said.status = said
                    .status
                    .or(status.filter(|code| (400..600).contains(code)));
            }
            _ => {}
        }
    }
    said.request_id = match body.get("request_id") {
        Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
        _ => None,
    };
    said
}

/// Classifies a failure from the JSON a server sent about it: the body of
/// a failed response, or the data of a chunk that holds an `error`, for
/// which `status` and `headers` are `None`.
pub(crate) fn error(
    status: Option<u16>,
    headers: Option<&Headers>,
    body: &[u8],
    now: UtcTime,
    request: &ModelRequest,
) -> ProviderError {
    let error = answered(status, headers, body, now);
    match refused_by_tier(request, error.throttle) {
        Some(tier) => error.for_tier(tier),
        None => error,
    }
}

/// Classifies what a server answered, whatever was asked of it: a model
/// call, or one of the provider's other endpoints. No speed tier comes
/// into it.
pub(crate) fn answered(
    status: Option<u16>,
    headers: Option<&Headers>,
    body: &[u8],
    now: UtcTime,
) -> ProviderError {
    let said = said(body);
    // An error inside a successful response has no status of its own. A
    // status the body carries stands in for it.
    let status = status.or(said.status);
    let decided = said
        .names
        .iter()
        .find_map(|name| Some((name.as_str(), signal(name)?)));
    let named = decided.map(|(_, named)| named);
    let signal = match named {
        Some(Named::Signal(signal)) => signal,
        Some(Named::Refused) => Signal::None,
        None => match status {
            Some(413) => Signal::ContextLength,
            Some(400 | 422) | None
                if said.message.to_ascii_lowercase().contains(CONTEXT_TOO_LONG) =>
            {
                Signal::ContextLength
            }
            Some(503) => Signal::Overload,
            // A response that began and then failed without saying how:
            // the failure is on the server's side, and the request may be
            // sent again.
            None => Signal::Server,
            _ => Signal::None,
        },
    };
    // The name that decided, or without one the most exact name the
    // server gave. Without any, a name for what the status says.
    let code = match (
        decided
            .map(|(name, _)| name)
            .or(said.names.first().map(String::as_str)),
        status,
    ) {
        (Some(name), _) => name,
        (None, Some(429)) => code::RATE_LIMITED,
        (None, Some(503 | 529)) => code::OVERLOADED,
        _ => code::UNKNOWN,
    };
    let message = if said.message.trim().is_empty() {
        match status {
            Some(status) => format!("The provider answered with HTTP status {status}."),
            None => "The provider reported a failure in the response stream.".to_owned(),
        }
    } else {
        shortened(&said.message)
    };

    let request_id = headers
        .and_then(|headers| REQUEST_ID.iter().find_map(|name| headers.get(name)))
        .map(str::to_owned)
        .or(said.request_id);

    let mut error =
        ProviderError::classified(status, signal, code, message).with_request_id(request_id);
    if named == Some(Named::Refused) {
        // A request refused for what it holds is not a credential's
        // failure, though some servers answer it with a 403, and sending
        // it again would be refused again.
        error.kind = cortex_base::ErrorKind::Provider;
        error.throttle = None;
    }
    let wait = headers.and_then(|headers| wait(headers, error.throttle, now));
    error.with_retry_after(wait)
}

/// Classifies a failed response.
pub(crate) fn failure(failure: &HttpFailure<'_>, request: &ModelRequest) -> ProviderError {
    error(
        Some(failure.status),
        Some(failure.headers),
        failure.body,
        failure.now,
        request,
    )
}

/// Classifies an error that arrived inside a successful response: the data
/// of the chunk, or of the `error` event, that holds it.
pub(crate) fn stream_error(data: &str, request: &ModelRequest) -> ProviderError {
    let mut failed = error(None, None, data.as_bytes(), UtcTime::UNIX_EPOCH, request);
    // Data that is not JSON is all the server said: it is the message.
    let unreadable = serde_json::from_str::<serde::de::IgnoredAny>(data).is_err();
    if unreadable && !data.trim().is_empty() {
        failed.message = shortened(data);
    }
    failed
}

/// A response that stopped with a stop reason that is a failure: `error`,
/// or DeepSeek's `insufficient_system_resource`.
pub(crate) fn stopped_with_failure(reason: &str, request: &ModelRequest) -> ProviderError {
    let signal = match signal(reason) {
        Some(Named::Signal(signal)) => signal,
        _ => Signal::Server,
    };
    let error = ProviderError::classified(
        None,
        signal,
        reason,
        format!("The provider ended the response with the stop reason `{reason}`."),
    );
    match refused_by_tier(request, error.throttle) {
        Some(tier) => error.for_tier(tier),
        None => error,
    }
}

/// A server's message, as it is kept: without any echo of the key, and cut
/// to [`MESSAGE_LIMIT`].
///
/// **Some servers repeat the key they were sent in the message of a
/// failure**, masked down to its last few characters (`Your api key:
/// ****abcd`), or to its beginning and its end (`sk-proj-…abcd`). A
/// message goes to logs and to the screen, and no part of a key belongs in
/// either. So a word is replaced whole when it has a run of asterisks or
/// of bullets in it, or begins as these providers' keys do, with `sk-`.
/// What a server masks some other way is not recognised.
fn shortened(message: &str) -> String {
    // A masked key (`****abcd`, `sk-…abcd`), or anything in the shape the
    // providers of this dialect give their keys.
    let masked = |word: &str| {
        word.contains("***")
            || word.contains("•••")
            || word
                .trim_start_matches(|character: char| !character.is_ascii_alphanumeric())
                .get(..3)
                .is_some_and(|start| start.eq_ignore_ascii_case("sk-"))
    };
    let cleaned: String;
    let message = if message.split(' ').any(masked) {
        cleaned = message
            .split(' ')
            .map(|word| if masked(word) { "[key]" } else { word })
            .collect::<Vec<_>>()
            .join(" ");
        cleaned.trim()
    } else {
        message.trim()
    };
    if message.len() <= MESSAGE_LIMIT {
        return message.to_owned();
    }
    let mut end = MESSAGE_LIMIT;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &message[..end])
}

/// The speed tier whose own limit or capacity turned the request away, if
/// it was the tier's and not the model's. The table is in the
/// [module](self).
fn refused_by_tier(request: &ModelRequest, throttle: Option<Throttle>) -> Option<Speed> {
    let sent = &request.speed;
    if sent.is_standard() {
        return None;
    }
    let offer = request.model.info.speed_offer(sent)?;
    // A tier the provider serves at standard when it is busy turns nothing
    // away.
    if offer.when_unavailable != SpeedUnavailable::Error {
        return None;
    }
    match throttle? {
        Throttle::Overload => Some(sent.clone()),
        // A rate limit can only be the tier's when the tier has one.
        Throttle::RateLimit if offer.separate_limit => Some(sent.clone()),
        Throttle::RateLimit => None,
    }
}

/// When a rate-limit header says a limit resets. The three forms are in
/// the [module](self).
pub(crate) fn reset(value: &str, now: UtcTime) -> Option<UtcTime> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        let number: i64 = value.parse().ok()?;
        return match number {
            // Milliseconds since 1970: no count of seconds is this large.
            100_000_000_000.. => UtcTime::from_unix_millis(number),
            // Seconds since 1970.
            1_000_000_000.. => UtcTime::from_unix(number, 0),
            _ => now.checked_add(Duration::from_secs(u64::try_from(number).ok()?)),
        };
    }
    now.checked_add(duration(value)?)
}

/// A duration as OpenAI writes one: `1s`, `6m0s`, `120ms`, `1h2m3.5s`.
fn duration(value: &str) -> Option<Duration> {
    let mut total = 0.0_f64;
    let mut rest = value;
    while !rest.is_empty() {
        let digits = rest
            .bytes()
            .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
            .count();
        let number: f64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let (unit, length) = [("ms", 0.001), ("h", 3600.0), ("m", 60.0), ("s", 1.0)]
            .into_iter()
            .find_map(|(unit, seconds)| rest.starts_with(unit).then_some((seconds, unit.len())))?;
        total += number * unit;
        rest = &rest[length..];
    }
    Duration::try_from_secs_f64(total).ok()
}

/// The families of rate-limit headers.
const LIMITS: &[(&str, LimitMeasureName)] = &[
    ("requests", LimitMeasureName::Requests),
    ("tokens", LimitMeasureName::Tokens),
];

#[derive(Debug, Clone, Copy)]
enum LimitMeasureName {
    Requests,
    Tokens,
}

fn number(headers: &Headers, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// How long a refused request should wait: what `retry-after` says, and
/// without it, for a rate limit, until the reset of a limit that has
/// nothing left.
fn wait(headers: &Headers, throttle: Option<Throttle>, now: UtcTime) -> Option<Duration> {
    if let Some(wait) = headers
        .get("retry-after")
        .and_then(|value| parse_retry_after(value, now))
    {
        return Some(wait);
    }
    if throttle != Some(Throttle::RateLimit) {
        return None;
    }
    LIMITS
        .iter()
        .filter(|(family, _)| {
            number(headers, &format!("x-ratelimit-remaining-{family}")) == Some(0)
        })
        .filter_map(|(family, _)| {
            let at = reset(headers.get(&format!("x-ratelimit-reset-{family}"))?, now)?;
            Some(wait_until(at, now))
        })
        .max()
}

/// Reads the headers of a successful response: the request's id and the
/// rate limits.
///
/// The speed that served the request is not in a header. It is a member of
/// the response, where the decoder reads it.
pub(crate) fn read_headers(headers: &Headers, now: UtcTime, out: &mut EventSink) {
    if let Some(id) = REQUEST_ID.iter().find_map(|name| headers.get(name)) {
        out.request_id(id);
    }
    let mut limits = Vec::new();
    for (family, measure) in LIMITS {
        let limit = number(headers, &format!("x-ratelimit-limit-{family}"));
        let remaining = number(headers, &format!("x-ratelimit-remaining-{family}"));
        let resets_at = headers
            .get(&format!("x-ratelimit-reset-{family}"))
            .and_then(|value| reset(value, now))
            .map(UtcTime::to_timestamp);
        if limit.is_none() && remaining.is_none() && resets_at.is_none() {
            continue;
        }
        limits.push(LimitWindow {
            measure: match measure {
                LimitMeasureName::Requests => LimitMeasure::Requests,
                LimitMeasureName::Tokens => LimitMeasure::Tokens,
            },
            speed: None,
            limit,
            remaining,
            resets_at,
        });
    }
    out.limits(limits);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(time: &str) -> UtcTime {
        UtcTime::parse(time).unwrap()
    }

    #[test]
    fn every_name_in_the_table_has_its_signal() {
        let rows: &[(&[&str], Named)] = &[
            (
                &[
                    "invalid_api_key",
                    "authentication_error",
                    "authentication",
                    "AUTHENTICATION_FAILED",
                    "AUTHENTICATION_FAILED_INACTIVE_KEY",
                    "UNAUTHORIZED",
                    "permission_error",
                    "API_ACCESS_DISABLED",
                ],
                Named::Signal(Signal::Auth),
            ),
            (
                &[
                    "insufficient_quota",
                    "credit_balance_exhausted",
                    "organization_spend_limit_exceeded",
                    "project_spend_limit_exceeded",
                    "organization_usage_limit_exceeded",
                    "INSUFFICIENT_BALANCE",
                    "API_KEY_USD_SPEND_LIMIT_EXCEEDED",
                    "API_KEY_DIEM_SPEND_LIMIT_EXCEEDED",
                    "PAYMENT_REQUIRED",
                    "openrouter_guardrail_budget",
                    "openrouter_workspace_budget",
                    "openrouter_key_limit",
                ],
                Named::Signal(Signal::Quota),
            ),
            (
                &[
                    "rate_limit_exceeded",
                    "RATE_LIMIT_EXCEEDED",
                    "rate_limit_error",
                    "slow_down",
                    "openrouter_in_flight_budget",
                ],
                Named::Signal(Signal::RateLimit),
            ),
            (
                &[
                    "server_is_overloaded",
                    "service_unavailable_error",
                    "provider_overloaded",
                    "MODEL_OVERLOADED",
                    "MODEL_AT_CAPACITY",
                    "MODEL_OFFLINE",
                    "unavailable_error",
                    "insufficient_system_resource",
                ],
                Named::Signal(Signal::Overload),
            ),
            (
                &[
                    "server_error",
                    "server",
                    "api_error",
                    "INFERENCE_FAILED",
                    "UNKNOWN_ERROR",
                    "timeout",
                    "REQUEST_TIMEOUT",
                ],
                Named::Signal(Signal::Server),
            ),
            (
                &[
                    "context_length_exceeded",
                    "exceed_context_size_error",
                    "TOO_MANY_TOKENS",
                    "PAYLOAD_TOO_LARGE",
                ],
                Named::Signal(Signal::ContextLength),
            ),
            (&["content_policy_violation", "refusal"], Named::Refused),
        ];
        for (names, expected) in rows {
            for name in *names {
                assert_eq!(signal(name), Some(*expected), "{name}");
            }
        }
        for unknown in [
            "invalid_request_error",
            "model_not_found",
            "MODEL_NOT_FOUND",
            "not_found_error",
            "a_code_added_next_year",
            "",
        ] {
            assert_eq!(signal(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn a_body_is_read_in_every_form_the_servers_use() {
        // The dialect's own form.
        let read = said(
            br#"{"error":{"message":"Incorrect API key provided.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#,
        );
        assert_eq!(read.names, ["invalid_api_key", "invalid_request_error"]);
        assert_eq!(read.message, "Incorrect API key provided.");
        assert_eq!(read.status, None);
        // The status as the code, and typed metadata.
        let read = said(
            br#"{"error":{"code":429,"message":"Rate limited","metadata":{"error_type":"rate_limit_exceeded","provider_code":"x"}},"user_id":"u"}"#,
        );
        assert_eq!(read.names, ["rate_limit_exceeded"]);
        assert_eq!(read.status, Some(429));
        // A sentence, with a code beside it.
        let read =
            said(br#"{"error":"Insufficient USD or Diem balance","code":"INSUFFICIENT_BALANCE"}"#);
        assert_eq!(read.names, ["INSUFFICIENT_BALANCE"]);
        assert_eq!(read.message, "Insufficient USD or Diem balance");
        // The members at the top level.
        let read = said(
            br#"{"object":"error","message":"This model's maximum context length is 4096 tokens.","type":"BadRequestError","param":null,"code":400}"#,
        );
        assert_eq!(read.names, ["BadRequestError"]);
        assert_eq!(read.status, Some(400));
        assert!(read.message.contains("maximum context length"));
        // A number that is not a status is not taken for one.
        assert_eq!(said(br#"{"error":{"code":7,"message":"x"}}"#).status, None);
        // Nothing readable says nothing.
        for nothing in [&b""[..], b"<html>502</html>", b"[1]", b"\"text\"", b"{}"] {
            assert_eq!(said(nothing), Said::default());
        }
    }

    #[test]
    fn a_reset_is_read_in_all_three_forms() {
        let now = at("2026-11-13T10:00:00Z");
        // A duration.
        assert_eq!(reset("1s", now), Some(at("2026-11-13T10:00:01Z")));
        assert_eq!(reset("6m0s", now), Some(at("2026-11-13T10:06:00Z")));
        assert_eq!(reset("1h2m3s", now), Some(at("2026-11-13T11:02:03Z")));
        assert_eq!(reset("500ms", now), Some(at("2026-11-13T10:00:00.500Z")));
        assert_eq!(reset("0.5s", now), Some(at("2026-11-13T10:00:00.500Z")));
        // Milliseconds since 1970, and seconds since 1970.
        assert_eq!(
            reset("1794564030000", now),
            UtcTime::from_unix_millis(1_794_564_030_000)
        );
        assert_eq!(
            reset("1794564030", now),
            UtcTime::from_unix(1_794_564_030, 0)
        );
        // A count of seconds.
        assert_eq!(reset("30", now), Some(at("2026-11-13T10:00:30Z")));
        for unreadable in ["", "soon", "5 minutes", "1x", "s", "-3"] {
            assert_eq!(reset(unreadable, now), None, "{unreadable}");
        }
    }

    #[test]
    fn a_long_message_is_cut_at_a_character() {
        let long = "é".repeat(MESSAGE_LIMIT);
        let cut = shortened(&long);
        assert!(cut.ends_with('…'));
        assert!(cut.len() <= MESSAGE_LIMIT + '…'.len_utf8());
        assert_eq!(shortened("  short  "), "short");
    }
}
