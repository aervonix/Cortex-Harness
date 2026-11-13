//! Classifying a failure, and reading the headers of a response.
//!
//! # The classification table
//!
//! The provider names a failure in `error.type`, and for two kinds of 429
//! also in `error.details.error_code`. Each name is read into one
//! [`Signal`], and the shared rules ([`crate::error::classify`]) turn the
//! signal and the status into a kind. The same table serves a failed
//! response and an `error` event that arrives after a successful status,
//! which has no status of its own.
//!
//! | `error.type` | Usual status | Signal | Kind |
//! |---|---|---|---|
//! | `invalid_request_error` | 400 | none | `Provider` |
//! | `invalid_request_error`, "prompt is too long" | 400 | context length | `ContextOverflow` |
//! | `authentication_error` | 401 | credential | `Auth` |
//! | `billing_error` | 402 | quota | `Provider` |
//! | `permission_error` | 403 | credential | `Auth` |
//! | `not_found_error` | 404 | none | `Provider` |
//! | `conflict_error` | 409 | none | `Transient` (by its status) |
//! | `request_too_large`, or any 413 | 413 | context length | `ContextOverflow` |
//! | `rate_limit_error` | 429 | rate limit | `Transient`, a rate limit, with the server's wait |
//! | `rate_limit_error`, detail `ramp_limit_exceeded` | 429 | rate limit | the same |
//! | `rate_limit_error`, detail `enforced_spend_limit_reached` | 429 | quota | `Provider`: never retried |
//! | `api_error` | 500 | server failure | `Transient` |
//! | `timeout_error` | 504, 500 | server failure | `Transient` |
//! | `overloaded_error` | 529 | overload | `Transient`, an overload |
//! | anything else, or no readable body | any | none | by the status |
//!
//! - **A spending cap arrives as a 429** with the same `error.type` as a
//!   rate limit and no `retry-after`. Waiting does not lift it, so its
//!   detail code wins: it is reported at once, it is never sent again, and
//!   it never counts as a refusal by a speed tier.
//! - **A prompt that is too long has no code of its own.** The provider
//!   documents it as a 400 `invalid_request_error` whose message says
//!   "prompt is too long". That phrase is the one place in this crate where
//!   a message is read to classify a failure, because the provider gives
//!   nothing else to read and the agent loop must be able to tell this
//!   failure from every other 400: it answers it by compacting.
//! - **A request that is too large in bytes** (413) is answered the same
//!   way, so it is classified the same way.
//!
//! # A refusal by a speed tier
//!
//! A request sent at a speed tier that has a limit of its own can be turned
//! away by that limit and not by the model's. The provider says "429, or
//! 529" for both, so the tier is told from the rest:
//!
//! | The request was sent at | The failure is | The tier's headers say | The standard headers say | It belongs to |
//! |---|---|---|---|---|
//! | `standard` | anything | | | the model |
//! | a tier | not a rate limit or an overload | | | the model |
//! | a tier | a rate limit or an overload | nothing left | | the tier |
//! | a tier | a rate limit or an overload | something left, or nothing said | nothing left | the model |
//! | a tier | a rate limit or an overload | something left, or nothing said | something left, or nothing said | the tier |
//!
//! The last row is the documented case: the tier has "a dedicated rate
//! limit that is separate from standard rate limits", its own ramp limit,
//! and its own capacity, so a request sent at the tier is counted against
//! the tier. Getting it wrong costs little in either direction. A model's
//! limit taken for the tier's sends the request once more at standard
//! speed, where it is refused and backs off like any other. A tier's limit
//! taken for the model's waits as the server asked and tries the tier
//! again.

use std::borrow::Cow;

use cortex_base::UtcTime;
use cortex_protocol::Speed;
use cortex_protocol::speed::SpeedUnavailable;
use cortex_protocol::usage::{LimitMeasure, LimitWindow};
use serde::Deserialize;

use crate::error::{ProviderError, Signal, Throttle, code};
use crate::event::EventSink;
use crate::http::{Headers, parse_retry_after};
use crate::protocol::HttpFailure;
use crate::request::ModelRequest;

/// The header that carries the provider's id for a request.
const REQUEST_ID: &str = "request-id";

/// The most of a provider's message that is kept. A message is a sentence
/// or two; the limit keeps a page of HTML from a proxy out of an error.
const MESSAGE_LIMIT: usize = 600;

/// How the provider's documentation words the one failure it gives no code
/// for (see the [module](self)).
const PROMPT_TOO_LONG: &str = "prompt is too long";

/// The detail code of a 429 that is a spending cap.
const SPEND_LIMIT: &str = "enforced_spend_limit_reached";

/// The body of a failed response, and the data of an `error` event.
#[derive(Debug, Default, Deserialize)]
struct Envelope<'a> {
    #[serde(borrow, default)]
    error: Option<ErrorObject<'a>>,
    #[serde(borrow, default)]
    request_id: Option<Cow<'a, str>>,
}

#[derive(Debug, Default, Deserialize)]
struct ErrorObject<'a> {
    #[serde(borrow, default, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    message: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    details: Option<Details<'a>>,
}

#[derive(Debug, Default, Deserialize)]
struct Details<'a> {
    #[serde(borrow, default)]
    error_code: Option<Cow<'a, str>>,
}

/// What `error.type`, its detail code and its message say, in the terms of
/// the shared rules. This is the table at the top of the [module](self).
pub(crate) fn signal(kind: &str, detail: Option<&str>, message: &str) -> Signal {
    if detail == Some(SPEND_LIMIT) {
        return Signal::Quota;
    }
    match kind {
        "authentication_error" | "permission_error" => Signal::Auth,
        "billing_error" => Signal::Quota,
        "rate_limit_error" => Signal::RateLimit,
        "overloaded_error" => Signal::Overload,
        "api_error" | "timeout_error" => Signal::Server,
        "request_too_large" => Signal::ContextLength,
        "invalid_request_error" if message.contains(PROMPT_TOO_LONG) => Signal::ContextLength,
        _ => Signal::None,
    }
}

/// Classifies a failure from the JSON the provider sent about it: the body
/// of a failed response, or the data of an `error` event, for which
/// `status` and `headers` are `None`.
pub(crate) fn error(
    status: Option<u16>,
    headers: Option<&Headers>,
    body: &[u8],
    now: UtcTime,
    request: &ModelRequest,
) -> ProviderError {
    let envelope: Envelope<'_> = serde_json::from_slice(body).unwrap_or_default();
    let error = envelope.error.unwrap_or_default();
    let kind = error.kind.as_deref().unwrap_or_default();
    let detail = error
        .details
        .as_ref()
        .and_then(|details| details.error_code.as_deref());
    let message = error.message.as_deref().unwrap_or_default();

    let signal = match signal(kind, detail, message) {
        Signal::None if status == Some(413) => Signal::ContextLength,
        signal => signal,
    };
    // The most exact name the provider gave. Without one, a name for what
    // the status says.
    let code = match (detail, kind, status) {
        (Some(detail), _, _) if !detail.is_empty() => detail,
        (_, kind, _) if !kind.is_empty() => kind,
        (_, _, Some(429)) => code::RATE_LIMITED,
        (_, _, Some(529)) => code::OVERLOADED,
        _ => code::UNKNOWN,
    };
    let message = if message.trim().is_empty() {
        match status {
            Some(status) => format!("The provider answered with HTTP status {status}."),
            None => "The provider reported a failure in the response stream.".to_owned(),
        }
    } else {
        shortened(message)
    };

    let wait = headers
        .and_then(|headers| headers.get("retry-after"))
        .and_then(|value| parse_retry_after(value, now));
    let request_id = headers
        .and_then(|headers| headers.get(REQUEST_ID))
        .map(str::to_owned)
        .or_else(|| envelope.request_id.map(Cow::into_owned));

    let error = ProviderError::classified(status, signal, code, message)
        .with_retry_after(wait)
        .with_request_id(request_id);
    match refused_by_tier(request, error.throttle, headers) {
        Some(tier) => error.for_tier(tier),
        None => error,
    }
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

fn shortened(message: &str) -> String {
    let message = message.trim();
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
fn refused_by_tier(
    request: &ModelRequest,
    throttle: Option<Throttle>,
    headers: Option<&Headers>,
) -> Option<Speed> {
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
        Throttle::Overload => {}
        // A rate limit can only be the tier's when the tier has one.
        Throttle::RateLimit if offer.separate_limit => {}
        Throttle::RateLimit => return None,
    }
    let Some(headers) = headers else {
        return Some(sent.clone());
    };
    let used_up = |family: &str, measures: &[&str]| {
        measures.iter().any(|measure| {
            headers
                .get(&format!("anthropic-{family}-{measure}-remaining"))
                .and_then(|value| value.trim().parse::<u64>().ok())
                == Some(0)
        })
    };
    let tier_used_up = used_up(sent.as_str(), &["input-tokens", "output-tokens"]);
    let standard_used_up = used_up(
        "ratelimit",
        &["requests", "tokens", "input-tokens", "output-tokens"],
    );
    (tier_used_up || !standard_used_up).then(|| sent.clone())
}

/// The families of rate-limit headers: the family's name in the header, what
/// it counts, and whether it is a speed tier's own.
const LIMITS: &[(&str, LimitMeasureName, bool)] = &[
    ("ratelimit-requests", LimitMeasureName::Requests, false),
    ("ratelimit-tokens", LimitMeasureName::Tokens, false),
    ("ratelimit-input-tokens", LimitMeasureName::Input, false),
    ("ratelimit-output-tokens", LimitMeasureName::Output, false),
    ("fast-input-tokens", LimitMeasureName::Input, true),
    ("fast-output-tokens", LimitMeasureName::Output, true),
];

#[derive(Debug, Clone, Copy)]
enum LimitMeasureName {
    Requests,
    Tokens,
    Input,
    Output,
}

/// Reads the headers of a successful response: the request's id, and the
/// rate limits, the fast tier's own among them, labelled with the tier.
///
/// The limits bought as a capacity commitment (`anthropic-priority-…`) are
/// not read: such capacity is a property of the account and not a speed
/// (`docs/decisions/0027-speed-tiers.md`, "What is not a speed").
///
/// The speed that served the request is not in a header. It is in the
/// response's usage, where the decoder reads it.
pub(crate) fn read_headers(headers: &Headers, out: &mut EventSink) {
    if let Some(id) = headers.get(REQUEST_ID) {
        out.request_id(id);
    }
    let number = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.trim().parse::<u64>().ok())
    };
    let mut limits = Vec::new();
    for (family, measure, fast) in LIMITS {
        let limit = number(&format!("anthropic-{family}-limit"));
        let remaining = number(&format!("anthropic-{family}-remaining"));
        let resets_at = headers
            .get(&format!("anthropic-{family}-reset"))
            .and_then(|value| UtcTime::parse(value.trim()).ok())
            .map(UtcTime::to_timestamp);
        if limit.is_none() && remaining.is_none() && resets_at.is_none() {
            continue;
        }
        limits.push(LimitWindow {
            measure: match measure {
                LimitMeasureName::Requests => LimitMeasure::Requests,
                LimitMeasureName::Tokens => LimitMeasure::Tokens,
                LimitMeasureName::Input => LimitMeasure::InputTokens,
                LimitMeasureName::Output => LimitMeasure::OutputTokens,
            },
            speed: fast.then_some(Speed::Fast),
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

    #[test]
    fn every_documented_error_type_has_its_signal() {
        let rows: &[(&str, Option<&str>, &str, Signal)] = &[
            ("invalid_request_error", None, "max_tokens: too large", Signal::None),
            (
                "invalid_request_error",
                None,
                "prompt is too long: 1200000 tokens > 1000000 maximum",
                Signal::ContextLength,
            ),
            ("authentication_error", None, "invalid x-api-key", Signal::Auth),
            ("billing_error", None, "", Signal::Quota),
            ("permission_error", None, "", Signal::Auth),
            ("not_found_error", None, "", Signal::None),
            ("conflict_error", None, "", Signal::None),
            ("request_too_large", None, "", Signal::ContextLength),
            ("rate_limit_error", None, "", Signal::RateLimit),
            (
                "rate_limit_error",
                Some("ramp_limit_exceeded"),
                "",
                Signal::RateLimit,
            ),
            (
                "rate_limit_error",
                Some("enforced_spend_limit_reached"),
                "",
                Signal::Quota,
            ),
            ("api_error", None, "", Signal::Server),
            ("timeout_error", None, "", Signal::Server),
            ("overloaded_error", None, "Overloaded", Signal::Overload),
            ("a_type_added_next_year", None, "", Signal::None),
            ("", None, "", Signal::None),
            // The phrase counts only on the error type the provider
            // documents it for.
            ("api_error", None, "prompt is too long", Signal::Server),
            ("not_found_error", None, "prompt is too long", Signal::None),
        ];
        for (kind, detail, message, expected) in rows {
            assert_eq!(
                signal(kind, *detail, message),
                *expected,
                "{kind} {detail:?} {message:?}"
            );
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
