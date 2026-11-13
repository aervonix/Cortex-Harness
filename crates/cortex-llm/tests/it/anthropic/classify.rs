//! The classification table, row by row, and what the client then does.
//!
//! The first half checks the table itself: a failed response in, a typed
//! error out, and the retry layer's decision about it. The second half runs
//! the same failures through the whole client over a scripted transport,
//! with the fake clock: fast mode waited out, fallen back from, cooled
//! down, and never silently not fast.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cortex_base::{CancelToken, ErrorKind, SeededRng, UtcTime};
use cortex_llm::anthropic::AnthropicMessages;
use cortex_llm::error::Throttle;
use cortex_llm::image::IMAGE_UNAVAILABLE;
use cortex_llm::retry::{Decision, RetryState};
use cortex_llm::{
    BoxFuture, EventSink, Headers, HttpFailure, ImageError, ImageSource, ModelClient, ModelEvent,
    ModelRequest, Protocol, ProviderClient, ProviderError, RetryPolicy,
};
use cortex_protocol::message::{ImagePart, ToolResultPart};
use cortex_protocol::speed::{
    CooldownState, SpeedFallbackBy, SpeedFallbackReason, SpeedUnavailable,
};
use cortex_protocol::usage::{LimitMeasure, LimitWindow};
use cortex_protocol::{ArtifactId, CallId, Message, Part, Role, Speed};
use cortex_testkit::fixture::Fixture;
use cortex_testkit::{FakeClock, FakeTimer, Reply, ScriptedTransport, block_on};
use serde_json::json;

use super::scenario;
use super::{load, provider, user};

const NOW: &str = "2026-11-13T10:00:00Z";

fn now() -> UtcTime {
    UtcTime::parse(NOW).unwrap()
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// The body of a failed response, as the provider writes one.
fn error_body(kind: &str, message: &str, detail: Option<&str>) -> String {
    let mut error = json!({"type": kind, "message": message});
    if let Some(detail) = detail {
        error["details"] = json!({ "error_code": detail });
    }
    json!({"type": "error", "error": error, "request_id": "req_body"}).to_string()
}

fn classify(
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
    request: &ModelRequest,
) -> ProviderError {
    let headers: Headers = headers.iter().copied().collect();
    AnthropicMessages.classify(
        &HttpFailure {
            status,
            headers: &headers,
            body: body.as_bytes(),
            now: now(),
        },
        request,
    )
}

fn classify_fixture(name: &str, request: &ModelRequest) -> ProviderError {
    let fixture = load(name);
    AnthropicMessages.classify(
        &HttpFailure {
            status: fixture.status,
            headers: &fixture.headers,
            body: &fixture.response,
            now: now(),
        },
        request,
    )
}

/// The request at fast speed, to a model whose fast tier is described
/// differently.
fn altered(change: impl FnOnce(&mut cortex_protocol::model::SpeedOffer)) -> ModelRequest {
    let mut request = scenario::fast();
    let mut entry = cortex_llm::ModelEntry::clone(&request.model);
    change(&mut entry.info.speeds[0]);
    request.model = Arc::new(entry);
    request
}

/// What the retry layer does about an error on a request's first attempt.
fn decision(error: &ProviderError, sent: &Speed) -> Decision {
    let mut state = RetryState::new();
    state.begin_attempt();
    RetryPolicy::default().decide(&mut state, error, sent, &mut SeededRng::new(1))
}

/// One row of the table: what came back, and what it must be read as.
struct Row {
    what: &'static str,
    status: u16,
    headers: &'static [(&'static str, &'static str)],
    kind_sent: &'static str,
    message: &'static str,
    detail: Option<&'static str>,
    expect_kind: ErrorKind,
    expect_code: &'static str,
    expect_throttle: Option<Throttle>,
    expect_wait: Option<u64>,
    /// Whether a request that failed this way is sent again.
    retried: bool,
}

const NO_HEADERS: &[(&str, &str)] = &[];

/// A case of the fast tier's table: what it is, the headers that came with
/// the refusal, and whether the refusal is the tier's.
type TierCase<'a> = (&'a str, &'a [(&'a str, &'a str)], bool);

#[test]
fn every_row_of_the_table_at_standard_speed() {
    use ErrorKind::{Auth, ContextOverflow, Provider, Transient};
    let row = |what, status, kind_sent, expect_kind, expect_throttle, retried| Row {
        what,
        status,
        headers: NO_HEADERS,
        kind_sent,
        message: "Something the provider said.",
        detail: None,
        expect_kind,
        expect_code: kind_sent,
        expect_throttle,
        expect_wait: None,
        retried,
    };
    let rows = vec![
        row(
            "400 invalid request",
            400,
            "invalid_request_error",
            Provider,
            None,
            false,
        ),
        Row {
            message: "prompt is too long: 1200042 tokens > 1000000 maximum",
            expect_kind: ContextOverflow,
            ..row(
                "400 prompt too long",
                400,
                "invalid_request_error",
                ContextOverflow,
                None,
                false,
            )
        },
        Row {
            // A limit the user set themselves arrives as a 400 and is not
            // a context problem, whatever its message mentions.
            message: "You have reached your specified API usage limits. You will regain access on 2026-12-01 at 00:00 UTC.",
            ..row(
                "400 own spend limit",
                400,
                "invalid_request_error",
                Provider,
                None,
                false,
            )
        },
        row(
            "401 authentication",
            401,
            "authentication_error",
            Auth,
            None,
            false,
        ),
        row("402 billing", 402, "billing_error", Provider, None, false),
        row("403 permission", 403, "permission_error", Auth, None, false),
        row(
            "404 not found",
            404,
            "not_found_error",
            Provider,
            None,
            false,
        ),
        row("409 conflict", 409, "conflict_error", Transient, None, true),
        row(
            "413 too large",
            413,
            "request_too_large",
            ContextOverflow,
            None,
            false,
        ),
        Row {
            headers: &[("retry-after", "12")],
            expect_wait: Some(12),
            ..row(
                "429 rate limit",
                429,
                "rate_limit_error",
                Transient,
                Some(Throttle::RateLimit),
                true,
            )
        },
        row(
            "429 rate limit, no wait named",
            429,
            "rate_limit_error",
            Transient,
            Some(Throttle::RateLimit),
            true,
        ),
        Row {
            headers: &[("retry-after", "30")],
            detail: Some("ramp_limit_exceeded"),
            expect_code: "ramp_limit_exceeded",
            expect_wait: Some(30),
            ..row(
                "429 ramp limit",
                429,
                "rate_limit_error",
                Transient,
                Some(Throttle::RateLimit),
                true,
            )
        },
        Row {
            // A spending cap: the same status and type as a rate limit, no
            // wait, and no point in waiting.
            message: "You have reached your API usage limits: your organization has crossed its monthly API usage threshold.",
            detail: Some("enforced_spend_limit_reached"),
            expect_code: "enforced_spend_limit_reached",
            ..row(
                "429 spend cap",
                429,
                "rate_limit_error",
                Provider,
                None,
                false,
            )
        },
        Row {
            // Even if a server were to name a wait with it.
            headers: &[("retry-after", "5")],
            detail: Some("enforced_spend_limit_reached"),
            expect_code: "enforced_spend_limit_reached",
            expect_wait: Some(5),
            ..row(
                "429 spend cap with a wait",
                429,
                "rate_limit_error",
                Provider,
                None,
                false,
            )
        },
        Row {
            headers: &[("retry-after", "121")],
            expect_wait: Some(121),
            // Longer than the longest wait that is obeyed: reported, with
            // the time, and not sent again.
            ..row(
                "429 with a long wait",
                429,
                "rate_limit_error",
                Transient,
                Some(Throttle::RateLimit),
                false,
            )
        },
        row("500 api error", 500, "api_error", Transient, None, true),
        row("500 timeout", 500, "timeout_error", Transient, None, true),
        row("504 timeout", 504, "timeout_error", Transient, None, true),
        row(
            "529 overloaded",
            529,
            "overloaded_error",
            Transient,
            Some(Throttle::Overload),
            true,
        ),
        row(
            "503 overloaded",
            503,
            "overloaded_error",
            Transient,
            Some(Throttle::Overload),
            true,
        ),
        // A type added later is read by its status.
        row(
            "400 unknown type",
            400,
            "some_new_error",
            Provider,
            None,
            false,
        ),
        row(
            "503 unknown type",
            503,
            "some_new_error",
            Transient,
            None,
            true,
        ),
        row(
            "429 unknown type",
            429,
            "some_new_error",
            Transient,
            Some(Throttle::RateLimit),
            true,
        ),
    ];

    let request = scenario::text();
    for row in rows {
        let body = error_body(row.kind_sent, row.message, row.detail);
        let error = classify(row.status, row.headers, &body, &request);
        let what = row.what;
        assert_eq!(error.kind, row.expect_kind, "{what}: kind");
        assert_eq!(error.code, row.expect_code, "{what}: code");
        assert_eq!(error.status, Some(row.status), "{what}: status");
        assert_eq!(error.throttle, row.expect_throttle, "{what}: throttle");
        assert_eq!(error.retry_after, row.expect_wait.map(secs), "{what}: wait");
        assert_eq!(error.message, row.message, "{what}: message");
        assert_eq!(error.request_id.as_deref(), Some("req_body"), "{what}: id");
        assert_eq!(
            error.tier, None,
            "{what}: sent at standard, so never a tier's"
        );
        let decided = decision(&error, &Speed::Standard);
        if row.retried {
            match (&decided, row.expect_wait) {
                (Decision::Wait(wait), Some(asked)) => assert_eq!(*wait, secs(asked), "{what}"),
                (Decision::Wait(_), None) => {}
                other => panic!("{what}: {other:?}"),
            }
        } else {
            assert_eq!(decided, Decision::GiveUp, "{what}: must not be sent again");
        }
    }
}

#[test]
fn a_failure_with_no_readable_body_is_read_by_its_status() {
    let request = scenario::text();
    let rows: &[(u16, &str, ErrorKind, &str, Option<Throttle>)] = &[
        (
            429,
            "",
            ErrorKind::Transient,
            "rate_limited",
            Some(Throttle::RateLimit),
        ),
        (
            529,
            "",
            ErrorKind::Transient,
            "overloaded",
            Some(Throttle::Overload),
        ),
        (
            502,
            "<html><body>Bad Gateway</body></html>",
            ErrorKind::Transient,
            "unknown_error",
            None,
        ),
        (500, "not json", ErrorKind::Transient, "unknown_error", None),
        (401, "", ErrorKind::Auth, "unknown_error", None),
        (403, "{}", ErrorKind::Auth, "unknown_error", None),
        (400, "[1,2,3]", ErrorKind::Provider, "unknown_error", None),
        (
            404,
            "{\"error\":\"a string, not an object\"}",
            ErrorKind::Provider,
            "unknown_error",
            None,
        ),
        // A request too large in bytes is turned away by a proxy, in HTML.
        (
            413,
            "<html>Request Entity Too Large</html>",
            ErrorKind::ContextOverflow,
            "unknown_error",
            None,
        ),
        (302, "", ErrorKind::Provider, "unknown_error", None),
    ];
    for (status, body, kind, code, throttle) in rows {
        let error = classify(*status, &[("request-id", "req_header")], body, &request);
        assert_eq!(error.kind, *kind, "{status} {body:?}");
        assert_eq!(error.code, *code, "{status} {body:?}");
        assert_eq!(error.throttle, *throttle, "{status}");
        assert_eq!(error.request_id.as_deref(), Some("req_header"));
        assert_eq!(
            error.message,
            format!("The provider answered with HTTP status {status}.")
        );
    }
    // The header's id wins over the body's, and a long message is cut.
    let long = error_body("api_error", &"x".repeat(5000), None);
    let error = classify(500, &[("Request-Id", "req_header")], &long, &request);
    assert_eq!(error.request_id.as_deref(), Some("req_header"));
    assert!(error.message.len() < 700);
    // The wait can be a date.
    let error = classify(
        429,
        &[("retry-after", "Fri, 13 Nov 2026 10:00:42 GMT")],
        &error_body("rate_limit_error", "Slow down.", None),
        &request,
    );
    assert_eq!(error.retry_after, Some(secs(42)));
}

#[test]
fn the_failures_recorded_from_the_real_api() {
    let request = scenario::text();
    let rows: &[(&str, ErrorKind, &str, u16)] = &[
        (
            "error-authentication",
            ErrorKind::Auth,
            "authentication_error",
            401,
        ),
        (
            "error-not-found",
            ErrorKind::Provider,
            "not_found_error",
            404,
        ),
        (
            "error-invalid-request",
            ErrorKind::Provider,
            "invalid_request_error",
            400,
        ),
        (
            "error-speed-not-offered",
            ErrorKind::Provider,
            "invalid_request_error",
            400,
        ),
    ];
    for (name, kind, code, status) in rows {
        let error = classify_fixture(name, &request);
        assert_eq!(error.kind, *kind, "{name}");
        assert_eq!(error.code, *code, "{name}");
        assert_eq!(error.status, Some(*status), "{name}");
        assert!(
            error
                .request_id
                .as_deref()
                .is_some_and(|id| id.starts_with("req_")),
            "{name}"
        );
        assert_eq!(
            (error.throttle, error.tier.clone(), error.retry_after),
            (None, None, None)
        );
        assert_eq!(
            decision(&error, &Speed::Standard),
            Decision::GiveUp,
            "{name}"
        );
        assert!(!error.message.is_empty());
    }
    // The refusal of a speed is a refusal of the request: it is never the
    // tier's own limit, and never answered by falling back.
    let error = classify_fixture("error-speed-not-offered", &scenario::fast());
    assert_eq!(error.tier, None);
    assert_eq!(decision(&error, &Speed::Fast), Decision::GiveUp);
}

#[test]
fn the_fast_tiers_own_limit_and_capacity() {
    let fast = scenario::fast();
    let standard = scenario::standard_named();
    let rate_limit = error_body("rate_limit_error", "Fast mode rate limit.", None);
    let overloaded = error_body("overloaded_error", "Overloaded", None);

    // The fixture: a 429 with a short wait, the tier's output tokens used up.
    let error = classify_fixture("error-fast-rate-limit", &fast);
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(error.throttle, Some(Throttle::RateLimit));
    assert_eq!(error.tier, Some(Speed::Fast));
    assert_eq!(error.retry_after, Some(secs(7)));
    // A short wait is waited out in place: the speed does not change.
    assert_eq!(decision(&error, &Speed::Fast), Decision::Wait(secs(7)));
    // The same response to a request sent at standard is the model's.
    assert_eq!(
        classify_fixture("error-fast-rate-limit", &standard).tier,
        None
    );

    // The fixture: a 529 with no wait. Falls back at once, for the
    // cooldown used when the provider names no time.
    let error = classify_fixture("error-fast-overloaded", &fast);
    assert_eq!(error.throttle, Some(Throttle::Overload));
    assert_eq!(error.tier, Some(Speed::Fast));
    assert_eq!(
        decision(&error, &Speed::Fast),
        Decision::FallBack {
            reason: SpeedFallbackReason::Overloaded,
            cooldown: secs(600)
        }
    );

    // Each row: the headers that came with a 429 for a request sent at
    // fast, and whether the refusal is the tier's.
    let rows: &[TierCase<'_>] = &[
        ("nothing said by any header", &[], true),
        (
            "the tier's input tokens used up",
            &[("anthropic-fast-input-tokens-remaining", "0")],
            true,
        ),
        (
            "the tier's output tokens used up, and the model's requests too",
            &[
                ("anthropic-fast-output-tokens-remaining", "0"),
                ("anthropic-ratelimit-requests-remaining", "0"),
            ],
            true,
        ),
        (
            "the tier has tokens left and so has the model",
            &[
                ("anthropic-fast-input-tokens-remaining", "612000"),
                ("anthropic-fast-output-tokens-remaining", "9000"),
                ("anthropic-ratelimit-requests-remaining", "998"),
            ],
            true,
        ),
        (
            // Requests are counted for the model whatever the speed.
            "the tier has tokens left and the model's requests are used up",
            &[
                ("anthropic-fast-input-tokens-remaining", "612000"),
                ("anthropic-ratelimit-requests-remaining", "0"),
            ],
            false,
        ),
        (
            "the model's tokens are used up and the tier says nothing",
            &[("anthropic-ratelimit-input-tokens-remaining", "0")],
            false,
        ),
        (
            "a header that is not a number says nothing",
            &[("anthropic-ratelimit-requests-remaining", "none")],
            true,
        ),
    ];
    for (what, headers, tiers) in rows {
        let error = classify(429, headers, &rate_limit, &fast);
        assert_eq!(error.tier.is_some(), *tiers, "{what}");
        assert_eq!(error.kind, ErrorKind::Transient, "{what}");
        // The same headers never make a standard request a tier's.
        assert_eq!(
            classify(429, headers, &rate_limit, &standard).tier,
            None,
            "{what}"
        );
    }

    // A ramp limit is kept for the tier separately from the model's, so at
    // fast speed it is the tier's.
    let ramp = error_body("rate_limit_error", "Ramp.", Some("ramp_limit_exceeded"));
    let error = classify(429, &[("retry-after", "45")], &ramp, &fast);
    assert_eq!(error.tier, Some(Speed::Fast));
    assert_eq!(error.code, "ramp_limit_exceeded");
    // 45 s is too long to wait in place: standard speed now, and the tier
    // rests for at least the shortest cooldown.
    assert_eq!(
        decision(&error, &Speed::Fast),
        Decision::FallBack {
            reason: SpeedFallbackReason::RateLimited,
            cooldown: secs(60)
        }
    );
    let error = classify(429, &[("retry-after", "150")], &ramp, &fast);
    assert_eq!(
        decision(&error, &Speed::Fast),
        Decision::FallBack {
            reason: SpeedFallbackReason::RateLimited,
            cooldown: secs(150)
        }
    );

    // What is never a tier's refusal, at any speed: a spending cap, a
    // server failure, a bad request, a bad key.
    let spend = error_body(
        "rate_limit_error",
        "Cap.",
        Some("enforced_spend_limit_reached"),
    );
    let never: &[(u16, String)] = &[
        (429, spend),
        (500, error_body("api_error", "Oops.", None)),
        (504, error_body("timeout_error", "Slow.", None)),
        (400, error_body("invalid_request_error", "Bad.", None)),
        (401, error_body("authentication_error", "Who?", None)),
    ];
    for (status, body) in never {
        let error = classify(
            *status,
            &[("anthropic-fast-output-tokens-remaining", "0")],
            body,
            &fast,
        );
        assert_eq!(error.tier, None, "{status} {body}");
        assert!(
            !matches!(decision(&error, &Speed::Fast), Decision::FallBack { .. }),
            "{status}: no fallback, no cooldown"
        );
    }
    let error = classify(429, &[], &never[0].1, &fast);
    assert_eq!(error.kind, ErrorKind::Provider);
    assert_eq!(decision(&error, &Speed::Fast), Decision::GiveUp);

    // A tier with no rate limit of its own cannot be what a rate limit
    // belongs to; its capacity still can. And a tier the provider serves at
    // standard when it is busy turns nothing away at all.
    let shared_limit = altered(|offer| offer.separate_limit = false);
    assert_eq!(classify(429, &[], &rate_limit, &shared_limit).tier, None);
    let used_up = [("anthropic-fast-output-tokens-remaining", "0")];
    assert_eq!(
        classify(429, &used_up, &rate_limit, &shared_limit).tier,
        None
    );
    assert_eq!(
        classify(529, &[], &overloaded, &shared_limit).tier,
        Some(Speed::Fast)
    );
    let downgrades = altered(|offer| offer.when_unavailable = SpeedUnavailable::Downgrade);
    assert_eq!(classify(429, &[], &rate_limit, &downgrades).tier, None);
    assert_eq!(classify(529, &[], &overloaded, &downgrades).tier, None);

    // An overload at fast speed is the tier's, and at standard speed it is
    // nobody's tier. The headers are read for it as for a rate limit.
    assert_eq!(
        classify(529, &[], &overloaded, &fast).tier,
        Some(Speed::Fast)
    );
    assert_eq!(classify(529, &[], &overloaded, &standard).tier, None);
    let error = classify(
        529,
        &[("anthropic-ratelimit-requests-remaining", "0")],
        &overloaded,
        &fast,
    );
    assert_eq!(
        error.tier, None,
        "the model's own limit is used up: not the tier's doing"
    );
}

#[test]
fn the_headers_of_a_good_response_give_the_id_and_the_limits() {
    let read = |fixture: &Fixture| {
        let mut sink = EventSink::sent_at(Speed::Standard);
        AnthropicMessages.read_headers(&fixture.headers, now(), &mut sink);
        let id = sink.known_request_id().map(str::to_owned);
        let limits = match sink.take().pop() {
            Some(ModelEvent::Limits(limits)) => limits,
            other => panic!("{other:?}"),
        };
        (id, limits)
    };

    // A response at standard speed: the model's four limits.
    let (id, limits) = read(&load("text"));
    assert_eq!(id.as_deref(), Some("req_fixture0001"));
    let measures: Vec<_> = limits
        .iter()
        .map(|limit| (limit.measure.clone(), limit.speed.clone()))
        .collect();
    assert_eq!(
        measures,
        [
            (LimitMeasure::Requests, None),
            (LimitMeasure::Tokens, None),
            (LimitMeasure::InputTokens, None),
            (LimitMeasure::OutputTokens, None),
        ]
    );
    assert_eq!(
        limits[0],
        LimitWindow {
            measure: LimitMeasure::Requests,
            speed: None,
            limit: Some(1000),
            remaining: Some(999),
            resets_at: Some(
                UtcTime::parse("2026-11-13T05:39:18Z")
                    .unwrap()
                    .to_timestamp()
            ),
        }
    );

    // A response at fast speed, as recorded: the requests are still the
    // model's, and the tokens are the fast tier's own, labelled with it.
    let (_, limits) = read(&load("fast"));
    let measures: Vec<_> = limits
        .iter()
        .map(|limit| (limit.measure.clone(), limit.speed.clone()))
        .collect();
    assert_eq!(
        measures,
        [
            (LimitMeasure::Requests, None),
            (LimitMeasure::InputTokens, Some(Speed::Fast)),
            (LimitMeasure::OutputTokens, Some(Speed::Fast)),
        ]
    );
    assert_eq!(
        (limits[1].limit, limits[1].remaining),
        (Some(1_000_000), Some(1_000_000))
    );
    assert_eq!(
        (limits[2].limit, limits[2].remaining),
        (Some(100_000), Some(100_000))
    );
    assert!(limits[1].resets_at.is_some());

    // Capacity an account bought is not a speed and is not reported as
    // one; a value that cannot be read is left out; no headers, no event.
    let headers: Headers = [
        ("anthropic-priority-input-tokens-limit", "5000"),
        ("anthropic-ratelimit-tokens-limit", "lots"),
        ("anthropic-ratelimit-tokens-remaining", "17"),
        ("anthropic-ratelimit-tokens-reset", "soon"),
    ]
    .into_iter()
    .collect();
    let mut sink = EventSink::sent_at(Speed::Standard);
    AnthropicMessages.read_headers(&headers, now(), &mut sink);
    assert_eq!(
        sink.take(),
        [ModelEvent::Limits(vec![LimitWindow {
            measure: LimitMeasure::Tokens,
            speed: None,
            limit: None,
            remaining: Some(17),
            resets_at: None,
        }])]
    );
    AnthropicMessages.read_headers(&Headers::new(), now(), &mut sink);
    assert!(sink.take().is_empty());
    assert_eq!(sink.known_request_id(), None);
}

// The whole client, over a scripted transport and the fake clock.

struct Client {
    clock: FakeClock,
    timer: FakeTimer,
    transport: ScriptedTransport,
    client: ProviderClient,
}

impl Client {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self::with(replies, None)
    }

    fn with(
        replies: impl IntoIterator<Item = Reply>,
        images: Option<Arc<dyn ImageSource>>,
    ) -> Self {
        let clock = FakeClock::starting_at(NOW).unwrap();
        let timer = FakeTimer::auto(&clock);
        let transport = ScriptedTransport::new(replies);
        let mut builder = ProviderClient::builder(
            Arc::new(AnthropicMessages),
            provider(),
            Arc::new(transport.clone()),
            Arc::new(clock.clone()),
            Arc::new(timer.clone()),
        )
        .rng(Box::new(SeededRng::new(42)));
        if let Some(images) = images {
            builder = builder.images(images);
        }
        Self {
            clock,
            timer,
            transport,
            client: builder.build(),
        }
    }

    fn run(&self, request: ModelRequest) -> Vec<ModelEvent> {
        block_on(self.client.stream(request, CancelToken::new()).collect())
    }

    /// The `speed` of each request sent so far.
    fn speeds(&self) -> Vec<String> {
        self.transport
            .bodies()
            .iter()
            .map(|body| body["speed"].as_str().unwrap_or("-").to_owned())
            .collect()
    }
}

/// A fixture as a scripted reply, delivered in small pieces.
fn reply(name: &str) -> Reply {
    let fixture = load(name);
    let mut reply = Reply::status(fixture.status);
    for (header, value) in fixture.headers.iter() {
        reply = reply.header(header, value);
    }
    reply.chunks_of(97, &fixture.response)
}

fn failure(status: u16, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut reply = Reply::status(status);
    for (header, value) in headers {
        reply = reply.header(header, value);
    }
    reply.chunk(body)
}

fn kinds(events: &[ModelEvent]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::Start { .. } => Some("start"),
            ModelEvent::Retry(_) => Some("retry"),
            ModelEvent::SpeedFallback(_) => Some("speed_fallback"),
            ModelEvent::SpeedCooldown(_) => Some("speed_cooldown"),
            ModelEvent::End { .. } => Some("end"),
            ModelEvent::Error(_) => Some("error"),
            _ => None,
        })
        .collect()
}

fn final_usage(events: &[ModelEvent]) -> &cortex_llm::Usage {
    match events.last() {
        Some(ModelEvent::End { usage, .. }) => usage,
        other => panic!("{other:?}"),
    }
}

fn final_error(events: &[ModelEvent]) -> &ProviderError {
    match events.last() {
        Some(ModelEvent::Error(error)) => error,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_recorded_response_goes_through_the_whole_client() {
    let client = Client::new([reply("tool-calls")]);
    let events = client.run(scenario::tool_calls());
    // The headers' limits come first, then the response as the decoder
    // alone reads it.
    assert!(matches!(events[0], ModelEvent::Limits(_)));
    let direct = super::decode_whole(&scenario::tool_calls(), &load("tool-calls").response);
    assert_eq!(events[1..], direct[..]);
    // What was sent is the recorded request, with the protocol's headers.
    let sent = &client.transport.requests()[0];
    assert_eq!(sent.body, load("tool-calls").request);
    assert_eq!(sent.url, "https://api.anthropic.com/v1/messages");
    assert_eq!(sent.headers.get("anthropic-version"), Some("2026-10-21"));
}

#[test]
fn fast_refused_for_a_moment_is_waited_out_and_stays_fast() {
    let client = Client::new([reply("error-fast-rate-limit"), reply("fast")]);
    let events = client.run(scenario::fast());
    assert_eq!(kinds(&events), ["retry", "start", "end"]);
    assert_eq!(
        client.timer.sleeps(),
        [secs(7)],
        "the wait the provider asked for"
    );
    assert_eq!(client.speeds(), ["fast", "fast"]);
    let usage = final_usage(&events);
    assert_eq!(
        (&usage.speed, &usage.speed_requested),
        (&Speed::Fast, &None)
    );
    // The two requests are the same bytes: nothing in the prefix moved.
    let sent = client.transport.requests();
    assert_eq!(sent[0].body, sent[1].body);
}

#[test]
fn fast_with_no_capacity_falls_back_says_so_and_cools_down() {
    let client = Client::new([
        reply("error-fast-overloaded"),
        reply("standard-named"),
        reply("standard-named"),
        reply("fast"),
    ]);
    let events = client.run(scenario::fast());
    assert_eq!(
        kinds(&events),
        ["speed_fallback", "speed_cooldown", "start", "end"]
    );
    assert!(client.timer.sleeps().is_empty(), "nothing is waited for");
    assert_eq!(client.speeds(), ["fast", "standard"]);

    let until = now().checked_add(secs(600)).unwrap();
    let ModelEvent::SpeedFallback(moved) = &events[0] else {
        panic!()
    };
    assert_eq!(
        (
            moved.requested.clone(),
            moved.served.clone(),
            moved.by.clone()
        ),
        (Speed::Fast, Speed::Standard, SpeedFallbackBy::Harness)
    );
    assert_eq!(moved.reason, SpeedFallbackReason::Overloaded);
    assert_eq!(moved.cooldown_until, Some(until));
    let ModelEvent::SpeedCooldown(change) = &events[1] else {
        panic!()
    };
    assert_eq!(
        (change.state.clone(), change.until),
        (CooldownState::Started, Some(until))
    );
    // The record says what ran and what was asked for: never silently not
    // fast.
    let usage = final_usage(&events);
    assert_eq!(usage.speed, Speed::Standard);
    assert_eq!(usage.speed_requested, Some(Speed::Fast));

    // During the cooldown a fast request goes at standard, and says why.
    client.clock.advance(secs(599));
    let events = client.run(scenario::fast());
    assert_eq!(kinds(&events), ["speed_fallback", "start", "end"]);
    let ModelEvent::SpeedFallback(moved) = &events[0] else {
        panic!()
    };
    assert_eq!(moved.reason, SpeedFallbackReason::Cooldown);
    assert_eq!(final_usage(&events).speed_requested, Some(Speed::Fast));
    // When it is over, fast is asked for again, and the stream says so.
    client.clock.advance(secs(1));
    let events = client.run(scenario::fast());
    assert_eq!(kinds(&events), ["speed_cooldown", "start", "end"]);
    assert_eq!(final_usage(&events).speed, Speed::Fast);
    assert_eq!(client.speeds(), ["fast", "standard", "standard", "fast"]);
}

#[test]
fn a_long_wait_for_fast_cools_down_for_the_providers_time_or_the_floor() {
    let rate_limit = error_body("rate_limit_error", "Fast mode rate limit.", None);
    for (asked, cooldown) in [("25", 60), ("59", 60), ("90", 90)] {
        let client = Client::new([
            failure(429, &[("retry-after", asked)], &rate_limit),
            reply("standard-named"),
        ]);
        let events = client.run(scenario::fast());
        assert_eq!(
            kinds(&events),
            ["speed_fallback", "speed_cooldown", "start", "end"]
        );
        let ModelEvent::SpeedFallback(moved) = &events[0] else {
            panic!()
        };
        assert_eq!(moved.reason, SpeedFallbackReason::RateLimited);
        assert_eq!(
            moved.cooldown_until,
            Some(now().checked_add(secs(cooldown)).unwrap()),
            "the provider said {asked} s"
        );
    }
}

#[test]
fn a_spending_cap_at_fast_speed_is_a_hard_stop() {
    let cap = error_body(
        "rate_limit_error",
        "You have reached your API usage limits.",
        Some("enforced_spend_limit_reached"),
    );
    let client = Client::new([failure(
        429,
        &[("anthropic-fast-output-tokens-remaining", "0")],
        &cap,
    )]);
    let events = client.run(scenario::fast());
    // One send, one error: no retry, no fallback, no cooldown.
    assert_eq!(kinds(&events), ["error"]);
    assert_eq!(client.transport.requests().len(), 1);
    assert!(client.timer.sleeps().is_empty());
    let error = final_error(&events);
    assert_eq!(error.kind, ErrorKind::Provider);
    assert_eq!(error.code, "enforced_spend_limit_reached");
    assert_eq!(error.message, "You have reached your API usage limits.");
    // The next fast request is not held at standard by a cooldown.
    client.transport.push(reply("fast"));
    let events = client.run(scenario::fast());
    assert_eq!(kinds(&events), ["start", "end"]);
    assert_eq!(client.speeds(), ["fast", "fast"]);
}

#[test]
fn fast_that_runs_out_of_capacity_mid_response_falls_back_too() {
    let client = Client::new([reply("error-after-200-fast"), reply("standard-named")]);
    let events = client.run(scenario::fast());
    // The first response had started. The fallback tells the loop that
    // what it saw of it no longer counts, and a new response starts.
    assert_eq!(
        kinds(&events),
        ["start", "speed_fallback", "speed_cooldown", "start", "end"]
    );
    assert_eq!(client.speeds(), ["fast", "standard"]);
    assert_eq!(final_usage(&events).speed_requested, Some(Speed::Fast));
}

#[test]
fn a_response_that_reports_another_speed_than_was_asked_is_never_silent() {
    // The provider answers a fast request and its usage says `standard`.
    let client = Client::new([reply("standard-named")]);
    let events = client.run(scenario::fast());
    assert_eq!(kinds(&events), ["start", "speed_fallback", "end"]);
    let moved = events
        .iter()
        .find_map(|event| match event {
            ModelEvent::SpeedFallback(moved) => Some(moved),
            _ => None,
        })
        .unwrap();
    assert_eq!(moved.by, SpeedFallbackBy::Provider);
    assert_eq!(
        moved.cooldown_until, None,
        "the provider's own move starts no cooldown"
    );
    let usage = final_usage(&events);
    assert_eq!(usage.speed, Speed::Standard);
    assert_eq!(usage.speed_requested, Some(Speed::Fast));
    // Priced at what ran: the standard card, not the fast one.
    let record = cortex_llm::usage::request_usage(
        usage.clone(),
        &scenario::fast().model.info,
        cortex_protocol::model::ModelRole::Main,
        cortex_protocol::usage::Billing::Metered,
        "t",
    );
    let fast_record = cortex_llm::usage::request_usage(
        final_usage(&Client::new([reply("fast")]).run(scenario::fast())).clone(),
        &scenario::fast().model.info,
        cortex_protocol::model::ModelRole::Main,
        cortex_protocol::usage::Billing::Metered,
        "t",
    );
    // The two responses used the same tokens; fast costs twice as much.
    assert_eq!(record.tokens.input, fast_record.tokens.input);
    assert_eq!(
        fast_record.cost.unwrap().usd_nanos,
        2 * record.cost.unwrap().usd_nanos
    );
}

#[test]
fn a_model_with_no_fast_mode_is_sent_at_standard_and_says_so() {
    let client = Client::new([reply("text")]);
    let mut request = scenario::text();
    request.speed = Speed::Fast;
    let events = client.run(request);
    assert_eq!(kinds(&events), ["speed_fallback", "start", "end"]);
    let ModelEvent::SpeedFallback(moved) = &events[0] else {
        panic!()
    };
    assert_eq!(moved.reason, SpeedFallbackReason::NotOffered);
    // No speed is named to a model that offers none.
    assert_eq!(client.speeds(), ["-"]);
    assert_eq!(client.transport.requests()[0].body, load("text").request);
}

#[test]
fn ordinary_failures_are_retried_or_reported_as_the_table_says() {
    // An overload, then a server failure, then the answer.
    let overloaded = error_body("overloaded_error", "Overloaded", None);
    let api = error_body("api_error", "Internal server error", None);
    let client = Client::new([
        failure(529, &[], &overloaded),
        failure(500, &[], &api),
        reply("text"),
    ]);
    let events = client.run(scenario::text());
    assert_eq!(kinds(&events), ["retry", "retry", "start", "end"]);
    assert_eq!(client.timer.sleeps().len(), 2);

    // A rate limit waits as long as the server asked.
    let limited = error_body("rate_limit_error", "Slow down.", None);
    let client = Client::new([
        failure(429, &[("retry-after", "12")], &limited),
        reply("text"),
    ]);
    assert_eq!(
        kinds(&client.run(scenario::text())),
        ["retry", "start", "end"]
    );
    assert_eq!(client.timer.sleeps(), [secs(12)]);

    // An error after a good status is retried like the failure it is.
    let client = Client::new([reply("error-after-200"), reply("text")]);
    let events = client.run(scenario::text());
    assert_eq!(kinds(&events), ["start", "retry", "start", "end"]);

    // A stream that ends before the response is complete is sent again.
    let cut = load("text").response;
    let client = Client::new([Reply::ok(&cut[..cut.len() / 2]), reply("text")]);
    let events = client.run(scenario::text());
    assert_eq!(kinds(&events), ["start", "retry", "start", "end"]);
    let ModelEvent::Retry(retry) = &events[events
        .iter()
        .position(|e| matches!(e, ModelEvent::Retry(_)))
        .unwrap()]
    else {
        panic!()
    };
    assert_eq!(retry.error.code, "incomplete_stream");

    // What is not retried is reported at once, with the provider's id.
    for name in [
        "error-authentication",
        "error-not-found",
        "error-invalid-request",
    ] {
        let client = Client::new([reply(name)]);
        let events = client.run(scenario::text());
        assert_eq!(kinds(&events), ["error"], "{name}");
        assert_eq!(client.transport.requests().len(), 1, "{name}");
        assert!(final_error(&events).request_id.is_some());
    }
    // A prompt that is too long is the loop's to compact away.
    let long = error_body(
        "invalid_request_error",
        "prompt is too long: 1000001 tokens > 1000000 maximum",
        None,
    );
    let client = Client::new([failure(400, &[], &long)]);
    assert_eq!(
        final_error(&client.run(scenario::text())).kind,
        ErrorKind::ContextOverflow
    );
}

/// A store with one image in it.
#[derive(Debug, Default)]
struct Images {
    asked: Mutex<Vec<String>>,
}

impl ImageSource for Images {
    fn load<'a>(&'a self, artifact: &'a ArtifactId) -> BoxFuture<'a, Result<Vec<u8>, ImageError>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(artifact.to_string());
            if artifact.as_str() == "a_png" {
                Ok(b"\x89PNG\r\n\x1a\nfoobar".to_vec())
            } else {
                Err(ImageError::new("no such artifact"))
            }
        })
    }
}

fn stored_image(artifact: &str) -> Part {
    Part::Image(ImagePart {
        media_type: "image/png".to_owned(),
        data: None,
        artifact: Some(ArtifactId::new(artifact)),
    })
}

/// A request whose history holds a stored image in a tool result and one
/// in a user message.
fn with_images(first: &str, second: &str) -> ModelRequest {
    let mut request = scenario::tool_call();
    request.messages = vec![
        user("Read the picture."),
        Message {
            role: Role::Assistant,
            parts: vec![Part::ToolCall(cortex_protocol::message::ToolCallPart {
                id: CallId::new("toolu_1"),
                name: "get_weather".to_owned(),
                arguments: json!({"city": "Oslo"}),
                raw: None,
            })],
            origin: None,
        },
        Message {
            role: Role::Tool,
            parts: vec![Part::ToolResult(ToolResultPart {
                call_id: CallId::new("toolu_1"),
                content: vec![Part::text("An image."), stored_image(first)],
                is_error: false,
            })],
            origin: None,
        },
        Message {
            role: Role::User,
            parts: vec![stored_image(second), Part::text("And this one.")],
            origin: None,
        },
    ];
    request
}

#[test]
fn a_stored_image_is_loaded_into_the_request_that_is_sent() {
    let images = Arc::new(Images::default());
    let limited = error_body("rate_limit_error", "Slow down.", None);
    let client = Client::with(
        [
            failure(429, &[("retry-after", "3")], &limited),
            reply("text"),
        ],
        Some(images.clone()),
    );
    let request = with_images("a_png", "a_png");
    let events = client.run(request.clone());
    assert_eq!(kinds(&events), ["retry", "start", "end"]);

    // Both references were loaded, once each, for both sends.
    assert_eq!(*images.asked.lock().unwrap(), ["a_png", "a_png"]);
    let bodies = client.transport.bodies();
    assert_eq!(bodies.len(), 2);
    let expected = json!({"type": "image", "source": {
        "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgpmb29iYXI="}});
    for body in &bodies {
        assert_eq!(body["messages"][2]["content"][0]["content"][1], expected);
        assert_eq!(body["messages"][3]["content"][0], expected);
    }
    // The same image gives the same bytes, so the prefix does not move.
    let sent = client.transport.requests();
    assert_eq!(sent[0].body, sent[1].body);
    // The caller's request still holds the references, not the bytes.
    let Part::Image(image) = &request.messages[3].parts[0] else {
        panic!()
    };
    assert!(image.data.is_none() && image.artifact.is_some());
}

#[test]
fn a_stored_image_that_cannot_be_loaded_fails_the_request_before_it_is_sent() {
    // The store does not have it.
    let images = Arc::new(Images::default());
    let client = Client::with([reply("text")], Some(images));
    let events = client.run(with_images("a_png", "a_gone"));
    assert_eq!(kinds(&events), ["error"]);
    let error = final_error(&events);
    assert_eq!(
        (error.kind, error.code.as_str()),
        (ErrorKind::Internal, IMAGE_UNAVAILABLE)
    );
    assert!(error.message.contains("a_gone"), "{}", error.message);
    assert!(client.transport.requests().is_empty(), "nothing was sent");

    // There is no store at all: the builder refuses the reference.
    let client = Client::new([reply("text")]);
    let events = client.run(with_images("a_png", "a_png"));
    assert_eq!(kinds(&events), ["error"]);
    assert_eq!(final_error(&events).kind, ErrorKind::Internal);
    assert!(final_error(&events).message.contains("was not loaded"));
    assert!(client.transport.requests().is_empty());

    // A model that takes no images is not sent one, so none is loaded for
    // it, and one that is gone does not fail its request.
    let images = Arc::new(Images::default());
    let client = Client::with([reply("text")], Some(images.clone()));
    let mut request = with_images("a_gone", "a_png");
    let mut entry = cortex_llm::ModelEntry::clone(&request.model);
    entry
        .info
        .input
        .retain(|modality| *modality != cortex_protocol::model::Modality::Image);
    request.model = Arc::new(entry);
    assert_eq!(kinds(&client.run(request)), ["start", "end"]);
    assert!(images.asked.lock().unwrap().is_empty());
    let sent = String::from_utf8(client.transport.requests()[0].body.clone()).unwrap();
    assert!(sent.contains("An image was here") && !sent.contains("base64"));

    // A request with no stored image never asks the store for anything.
    let images = Arc::new(Images::default());
    let client = Client::with([reply("text")], Some(images.clone()));
    assert_eq!(kinds(&client.run(scenario::text())), ["start", "end"]);
    assert!(images.asked.lock().unwrap().is_empty());
    // And a cancelled request does not load one.
    let cancel = CancelToken::new();
    cancel.cancel();
    let events: Vec<ModelEvent> = block_on(
        client
            .client
            .stream(with_images("a_png", "a_png"), cancel)
            .collect(),
    );
    assert_eq!(final_error(&events).kind, ErrorKind::Interrupted);
}
