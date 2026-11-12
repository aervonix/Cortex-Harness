//! The retry layer against the fake clock: every row of the shared
//! classification table (`docs/architecture/04-providers.md`, "Error
//! classification"), the server's waits, the maximum wait, the attempt
//! limit and the two time limits on a response.
//!
//! Every wait here is one the client asked the fake timer for. The tests
//! read the waits back and read the clock; none of them takes time.

use cortex_base::{CancelToken, ErrorKind};
use cortex_llm::error::{TransportError, TransportErrorKind};
use cortex_llm::{ModelClient, ModelEvent, RetryPolicy};
use cortex_testkit::protocol::sse;
use cortex_testkit::{Reply, block_on, fixtures, join};
use serde_json::json;

use crate::{
    HELLO, Harness, START, assert_kind, assert_well_formed, at, failure, final_error, hello, names,
    retries, secs,
};

/// A stream that waited once and then succeeded.
const RETRIED_ONCE: [&str; 6] = ["retry", "start", "block_start", "text", "block_end", "end"];

/// Runs one failing reply followed by a good one, and returns the events,
/// the waits and how many requests were sent.
fn fail_then_succeed(first: Reply) -> (Vec<ModelEvent>, Vec<std::time::Duration>, usize) {
    let harness = Harness::new([first, hello()]);
    let events = harness.run();
    assert_well_formed(&events);
    (
        events,
        harness.timer.sleeps(),
        harness.transport.requests().len(),
    )
}

/// Runs one failing reply with a good one behind it, and asserts that the
/// failure was reported at once: one request, no wait.
fn fails_at_once(first: Reply, kind: ErrorKind, code: &str) {
    let harness = Harness::new([first, hello()]);
    let events = harness.run();
    assert_eq!(names(&events), ["error"], "{code}");
    assert_kind(final_error(&events), kind, code);
    assert_eq!(
        harness.transport.requests().len(),
        1,
        "{code}: not sent again"
    );
    assert!(harness.timer.sleeps().is_empty(), "{code}: no wait");
    assert_eq!(harness.now(), at(START), "{code}: no time passed");
}

// Row: connection refused or reset, DNS failure, TLS failure, timeout.

#[test]
fn a_connection_failure_is_retried_with_backoff() {
    for kind in [TransportErrorKind::Connect, TransportErrorKind::Timeout] {
        let reply = Reply::failure(TransportError::new(kind, "it did not connect"));
        let (events, sleeps, sent) = fail_then_succeed(reply);
        assert_eq!(names(&events), RETRIED_ONCE, "{kind:?}");
        assert_eq!(sent, 2);
        assert_eq!(sleeps.len(), 1);
        assert!(
            sleeps[0] <= secs(1),
            "the first backoff is at most the base: {sleeps:?}"
        );

        let retry = retries(&events)[0];
        assert_eq!((retry.attempt, retry.max_attempts), (2, 5));
        assert_eq!(retry.error.kind, ErrorKind::Transient);
        assert_eq!(retry.wait, sleeps[0]);
        assert_eq!(
            retry.next_attempt_at,
            at(START).checked_add(sleeps[0]).unwrap()
        );
    }
}

#[test]
fn a_connection_lost_part_way_is_retried_and_the_new_response_starts_over() {
    let broken = Reply::ok(sse("start", &json!({"id": "r1", "model": "m"})))
        .chunk(sse("block_start", &json!({"index": 0, "kind": "text"})))
        .chunk(sse("text", &json!({"index": 0, "text": "Hel"})))
        .then_disconnect();
    let (events, sleeps, sent) = fail_then_succeed(broken);
    assert_eq!(
        names(&events),
        [
            "start",
            "block_start",
            "text",
            "retry",
            "start",
            "block_start",
            "text",
            "block_end",
            "end"
        ],
        "what the first attempt said is followed by a retry and a fresh start"
    );
    assert_kind(
        &retries(&events)[0].error,
        ErrorKind::Transient,
        "connection_lost",
    );
    assert_eq!((sent, sleeps.len()), (2, 1));
}

#[test]
fn a_transport_failure_that_cannot_change_is_not_retried() {
    fails_at_once(
        Reply::failure(TransportError::new(
            TransportErrorKind::InvalidRequest,
            "the URL is not one",
        )),
        ErrorKind::Config,
        "invalid_request",
    );
}

// Row: 408, 409, 5xx, provider overload codes.

#[test]
fn server_failures_and_overloads_are_retried() {
    for (status, code) in [
        (408, "request_timeout"),
        (409, "conflict"),
        (500, "server_error"),
        (502, "bad_gateway"),
        (503, "unavailable"),
        (504, "gateway_timeout"),
        (529, "overloaded"),
        (503, "overloaded"),
    ] {
        let (events, sleeps, sent) = fail_then_succeed(failure(status, code));
        assert_eq!(names(&events), RETRIED_ONCE, "{status} {code}");
        let error = &retries(&events)[0].error;
        assert_kind(error, ErrorKind::Transient, code);
        assert_eq!(error.status, Some(status));
        assert_eq!((sent, sleeps.len()), (2, 1), "{status} {code}");
        assert!(sleeps[0] <= secs(1));
    }
}

// Row: 429 with a rate-limit code, with the server's wait.

#[test]
fn a_rate_limit_waits_exactly_as_long_as_the_server_said() {
    let limited = failure(429, "rate_limit").header("retry-after", "7");
    let harness = Harness::new([limited, hello()]);
    let events = harness.run();
    assert_eq!(names(&events), RETRIED_ONCE);
    assert_eq!(
        harness.timer.sleeps(),
        [secs(7)],
        "the server's wait, not a backoff"
    );
    assert_eq!(harness.now(), at("2026-11-12T10:00:07Z"));

    let retry = retries(&events)[0];
    assert_kind(&retry.error, ErrorKind::Transient, "rate_limit");
    assert_eq!(retry.error.retry_after, Some(secs(7)));
    assert_eq!(retry.wait, secs(7));
    assert_eq!(retry.next_attempt_at, at("2026-11-12T10:00:07Z"));
}

#[test]
fn a_wait_given_as_a_date_is_measured_from_the_clock() {
    let limited = failure(429, "rate_limit").header("retry-after", "Thu, 12 Nov 2026 10:01:30 GMT");
    let harness = Harness::new([limited, hello()]);
    harness.run();
    assert_eq!(harness.timer.sleeps(), [secs(90)]);
}

#[test]
fn a_rate_limit_with_no_wait_backs_off() {
    let (events, sleeps, _) = fail_then_succeed(failure(429, "rate_limit"));
    assert_eq!(names(&events), RETRIED_ONCE);
    assert!(sleeps[0] <= secs(1), "{sleeps:?}");
    // A bare 429, with no code the protocol knows, is a rate limit too.
    let (events, _, _) = fail_then_succeed(failure(429, "who_knows"));
    assert_eq!(names(&events), RETRIED_ONCE);
}

#[test]
fn a_wait_longer_than_the_maximum_is_not_slept() {
    // The default maximum is two minutes.
    let harness = Harness::new([
        failure(429, "rate_limit").header("retry-after", "121"),
        hello(),
    ]);
    let events = harness.run();
    assert_eq!(
        names(&events),
        ["error"],
        "no retry notice: nothing is waited for"
    );
    let error = final_error(&events);
    assert_kind(error, ErrorKind::Transient, "rate_limit");
    assert_eq!(error.retry_after, Some(secs(121)));
    assert!(harness.timer.sleeps().is_empty());
    assert_eq!(harness.now(), at(START), "the clock did not move");
    assert_eq!(harness.transport.requests().len(), 1);
    // The error a person sees carries the time, so that they can decide.
    let data = error.to_error_data_at(harness.now());
    assert_eq!(data.retry_at.unwrap().to_string(), "2026-11-12T10:02:01Z");

    // Exactly the maximum is still waited.
    let harness = Harness::new([
        failure(429, "rate_limit").header("retry-after", "120"),
        hello(),
    ]);
    assert_eq!(names(&harness.run()), RETRIED_ONCE);
    assert_eq!(harness.timer.sleeps(), [secs(120)]);

    // The maximum is a setting.
    let patient = RetryPolicy {
        max_wait: secs(600),
        ..RetryPolicy::default()
    };
    let harness = Harness::with_policy(
        [
            failure(429, "rate_limit").header("retry-after", "300"),
            hello(),
        ],
        patient,
    );
    assert_eq!(names(&harness.run()), RETRIED_ONCE);
    assert_eq!(harness.timer.sleeps(), [secs(300)]);
}

// Row: 429 or 402 with a spending, credit or quota code.

#[test]
fn a_spending_limit_is_not_retried_whatever_its_status() {
    fails_at_once(failure(429, "quota"), ErrorKind::Provider, "quota");
    fails_at_once(failure(402, "quota"), ErrorKind::Provider, "quota");
    fails_at_once(
        failure(402, "payment_required"),
        ErrorKind::Provider,
        "payment_required",
    );
    // Even when the provider sends a wait with it.
    fails_at_once(
        failure(429, "quota").header("retry-after", "5"),
        ErrorKind::Provider,
        "quota",
    );
}

// Row: 401, 403.

#[test]
fn a_rejected_credential_is_not_retried() {
    fails_at_once(
        failure(401, "unauthorized"),
        ErrorKind::Auth,
        "unauthorized",
    );
    fails_at_once(failure(403, "forbidden"), ErrorKind::Auth, "forbidden");
    fails_at_once(failure(400, "auth"), ErrorKind::Auth, "auth");
}

// Row: 400 context-length error.

#[test]
fn a_context_overflow_is_reported_for_the_loop_to_compact() {
    fails_at_once(
        failure(400, "context_length"),
        ErrorKind::ContextOverflow,
        "context_length",
    );
    fails_at_once(
        failure(413, "context_length"),
        ErrorKind::ContextOverflow,
        "context_length",
    );
}

// Row: other 4xx.

#[test]
fn other_client_errors_are_not_retried() {
    for (status, code) in [
        (400, "invalid_request"),
        (404, "not_found"),
        (413, "too_large"),
        (422, "unprocessable"),
        (307, "redirect"),
    ] {
        fails_at_once(failure(status, code), ErrorKind::Provider, code);
    }
}

#[test]
fn a_failure_keeps_the_providers_message_status_and_request_id() {
    let harness = Harness::new([Reply::status(404).header("x-request-id", "req_404").chunk(
        cortex_testkit::protocol::failure_body("not_found", "No such model."),
    )]);
    let events = harness.run();
    let error = final_error(&events);
    assert_eq!(error.message, "No such model.");
    assert_eq!(error.status, Some(404));
    assert_eq!(error.request_id.as_deref(), Some("req_404"));

    // A failure whose body says nothing still has its status to go by.
    let harness = Harness::new([Reply::status(500), hello()]);
    let events = harness.run();
    assert_eq!(names(&events), RETRIED_ONCE);
    assert_eq!(retries(&events)[0].error.message, "HTTP 500");

    // And one whose body is far larger than is worth reading.
    let huge = Reply::status(400).chunk(vec![b'x'; 300_000]);
    let harness = Harness::new([huge]);
    assert_kind(
        final_error(&harness.run()),
        ErrorKind::Provider,
        "unknown_error",
    );
}

// Row: an error event after a 200, classified by its code as above.

#[test]
fn an_error_event_after_a_good_status_is_classified_by_its_code() {
    let stream_with = |error: serde_json::Value| {
        Reply::ok(sse("start", &json!({"id": "r1", "model": "m"})))
            .chunk(sse("block_start", &json!({"index": 0, "kind": "text"})))
            .chunk(sse("text", &json!({"index": 0, "text": "Hel"})))
            .chunk(sse("error", &error))
            // A decoder reads on, but nothing after the error counts.
            .chunk(sse("text", &json!({"index": 0, "text": "never"})))
    };
    const PARTIAL: [&str; 3] = ["start", "block_start", "text"];

    // Retried: overload, server failure, rate limit.
    for code in ["overloaded", "server_error", "rate_limit"] {
        let (events, sleeps, sent) = fail_then_succeed(stream_with(json!({"code": code})));
        assert_eq!(
            names(&events)[..4],
            [PARTIAL[0], PARTIAL[1], PARTIAL[2], "retry"],
            "{code}"
        );
        assert_eq!(names(&events)[4..], HELLO, "{code}");
        assert_kind(&retries(&events)[0].error, ErrorKind::Transient, code);
        assert_eq!(retries(&events)[0].error.status, None);
        assert_eq!((sent, sleeps.len()), (2, 1));
    }

    // With the wait the event names.
    let harness = Harness::new([
        stream_with(json!({"code": "rate_limit", "retry_after": 12})),
        hello(),
    ]);
    harness.run();
    assert_eq!(harness.timer.sleeps(), [secs(12)]);

    // Not retried: a spending limit, a credential, a context overflow and
    // anything the protocol does not know.
    for (code, kind) in [
        ("quota", ErrorKind::Provider),
        ("auth", ErrorKind::Auth),
        ("context_length", ErrorKind::ContextOverflow),
        ("mystery", ErrorKind::Provider),
    ] {
        let harness = Harness::new([stream_with(json!({"code": code})), hello()]);
        let events = harness.run();
        assert_eq!(
            names(&events),
            [PARTIAL[0], PARTIAL[1], PARTIAL[2], "error"],
            "{code}"
        );
        assert_kind(final_error(&events), kind, code);
        assert_eq!(harness.transport.requests().len(), 1, "{code}");
        assert!(harness.timer.sleeps().is_empty(), "{code}");
    }
}

// A stream that is not what it should be.

#[test]
fn a_stream_that_ends_early_or_cannot_be_read_is_retried() {
    let unfinished = Reply::ok(sse("start", &json!({"id": "r1", "model": "m"})));
    let (events, _, sent) = fail_then_succeed(unfinished);
    assert_eq!(names(&events)[..2], ["start", "retry"]);
    assert_kind(
        &retries(&events)[0].error,
        ErrorKind::Transient,
        "incomplete_stream",
    );
    assert_eq!(sent, 2);

    let empty = Reply::status(200);
    let (events, _, _) = fail_then_succeed(empty);
    assert_kind(
        &retries(&events)[0].error,
        ErrorKind::Transient,
        "incomplete_stream",
    );

    let garbled = Reply::ok("event: start\ndata: {not json\n\n");
    let (events, _, _) = fail_then_succeed(garbled);
    assert_eq!(names(&events), RETRIED_ONCE);
    assert_kind(
        &retries(&events)[0].error,
        ErrorKind::Transient,
        "malformed_stream",
    );
}

// The attempt limit and the backoff schedule.

#[test]
fn a_request_is_sent_at_most_five_times_and_the_waits_grow() {
    let harness = Harness::new((0..6).map(|_| failure(503, "unavailable")));
    let events = harness.run();
    assert_eq!(
        names(&events),
        ["retry", "retry", "retry", "retry", "error"]
    );
    assert_well_formed(&events);
    assert_eq!(harness.transport.requests().len(), 5);
    assert_eq!(
        harness.transport.remaining(),
        1,
        "the sixth reply was never asked for"
    );

    let attempts: Vec<u32> = retries(&events).iter().map(|retry| retry.attempt).collect();
    assert_eq!(attempts, [2, 3, 4, 5]);

    // Full jitter under a ceiling of 1, 2, 4 and 8 seconds.
    let sleeps = harness.timer.sleeps();
    assert_eq!(sleeps.len(), 4);
    for (sleep, ceiling) in sleeps.iter().zip([1, 2, 4, 8]) {
        assert!(*sleep <= secs(ceiling), "{sleep:?} is over {ceiling} s");
    }
    assert!(sleeps.iter().any(|sleep| !sleep.is_zero()));
    // The clock moved by exactly what was waited.
    let waited: std::time::Duration = sleeps.iter().sum();
    assert_eq!(harness.now(), at(START).checked_add(waited).unwrap());
    // Each notice says when its attempt starts.
    let mut clock = at(START);
    for (retry, sleep) in retries(&events).iter().zip(&sleeps) {
        clock = clock.checked_add(*sleep).unwrap();
        assert_eq!(retry.next_attempt_at, clock);
    }
    assert_kind(final_error(&events), ErrorKind::Transient, "unavailable");
}

#[test]
fn the_schedule_follows_the_policy() {
    let policy = RetryPolicy {
        max_attempts: 3,
        base: secs(10),
        factor: 3.0,
        cap: secs(20),
        ..RetryPolicy::default()
    };
    let harness = Harness::with_policy((0..3).map(|_| failure(500, "server_error")), policy);
    let events = harness.run();
    assert_eq!(names(&events), ["retry", "retry", "error"]);
    let sleeps = harness.timer.sleeps();
    assert!(sleeps[0] <= secs(10) && sleeps[1] <= secs(20), "{sleeps:?}");
    assert!(
        sleeps[0] > secs(1) || sleeps[1] > secs(2),
        "the base is used: {sleeps:?}"
    );
    assert_eq!(retries(&events)[0].max_attempts, 3);
}

#[test]
fn a_failure_after_retries_can_still_succeed_or_fail_for_good() {
    // Transient twice, then a failure that is not: reported at once.
    let harness = Harness::new([
        failure(503, "unavailable"),
        failure(529, "overloaded"),
        failure(401, "unauthorized"),
        hello(),
    ]);
    let events = harness.run();
    assert_eq!(names(&events), ["retry", "retry", "error"]);
    assert_kind(final_error(&events), ErrorKind::Auth, "unauthorized");
    assert_eq!(harness.transport.requests().len(), 3);
}

#[test]
fn cancelling_during_a_backoff_ends_the_request_without_another_send() {
    let harness = Harness::manual([failure(503, "unavailable"), hello()]);
    let cancel = CancelToken::new();
    let mut stream = harness
        .client
        .stream(fixtures::request(fixtures::model()), cancel.clone());
    let (events, ()) = block_on(join(
        async {
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                events.push(event);
            }
            events
        },
        async {
            // The client is waiting out its backoff. Cancel it there.
            harness.timer.sleeping().await;
            cancel.cancel();
        },
    ));
    assert_eq!(names(&events), ["retry", "error"]);
    assert_kind(final_error(&events), ErrorKind::Interrupted, "interrupted");
    assert_eq!(
        harness.transport.requests().len(),
        1,
        "the second attempt was never made"
    );
    assert!(harness.timer.sleeps().is_empty(), "the wait did not finish");
    assert_eq!(harness.now(), at(START));
}

// The two time limits on a response.

#[test]
fn a_stream_that_goes_silent_fails_at_the_idle_limit() {
    let stalled = Reply::ok(sse("start", &json!({"id": "r1", "model": "m"}))).then_stall();
    let harness = Harness::manual([stalled, hello()]);
    let mut stream = harness
        .client
        .stream(fixtures::request(fixtures::model()), CancelToken::new());
    block_on(async {
        assert!(matches!(
            stream.next().await,
            Some(ModelEvent::Start { .. })
        ));

        // The response has begun, so the limit that runs is the idle one:
        // 90 seconds. One second short of it, nothing has happened.
        let (retry, ()) = join(stream.next(), async {
            harness.timer.sleeping().await;
            assert_eq!(harness.timer.running(), [secs(90)]);
            harness.timer.advance(secs(89));
            assert!(
                harness.timer.sleeps().is_empty(),
                "89 seconds is not yet idle"
            );
            harness.timer.advance(secs(1));
        })
        .await;
        let Some(ModelEvent::Retry(retry)) = retry else {
            panic!("{retry:?}");
        };
        assert_kind(&retry.error, ErrorKind::Transient, "idle_timeout");
        assert_eq!(harness.now(), at("2026-11-12T10:01:30Z"));

        // The backoff is a wait too. Let it pass, and the retry succeeds.
        let (next, ()) = join(stream.next(), async {
            harness.timer.sleeping().await;
            harness.timer.advance(secs(1));
        })
        .await;
        assert!(matches!(next, Some(ModelEvent::Start { .. })), "{next:?}");
    });
    assert_eq!(harness.transport.requests().len(), 2);
}

#[test]
fn a_response_that_never_begins_fails_at_the_first_event_limit() {
    // No status at all, and a status with no event after it.
    for silent in [
        Reply::never_answers(),
        Reply::status(200).then_stall(),
        // Keep-alive comments are bytes, and are still not an event.
        Reply::ok(": ping\n\n").then_stall(),
    ] {
        let harness = Harness::manual([silent, hello()]);
        let mut stream = harness
            .client
            .stream(fixtures::request(fixtures::model()), CancelToken::new());
        block_on(async {
            let (retry, ()) = join(stream.next(), async {
                harness.timer.sleeping().await;
                assert_eq!(harness.timer.running(), [secs(60)], "the first-event limit");
                harness.timer.advance(secs(59));
                assert!(harness.timer.sleeps().is_empty());
                harness.timer.advance(secs(1));
            })
            .await;
            let Some(ModelEvent::Retry(retry)) = retry else {
                panic!("{retry:?}");
            };
            assert_kind(&retry.error, ErrorKind::Transient, "first_event_timeout");
        });
    }
}

#[test]
fn a_provider_can_set_its_own_time_limits() {
    use std::sync::Arc;

    use cortex_llm::ProviderClient;
    use cortex_testkit::{FakeClock, FakeTimer, ScriptedTransport, TestProtocol};

    let clock = FakeClock::starting_at(START).unwrap();
    let timer = FakeTimer::manual(&clock);
    let transport = ScriptedTransport::new([
        Reply::never_answers(),
        Reply::ok(sse("start", &json!({"id": "r1", "model": "m"}))).then_stall(),
    ]);
    // A local server that reads a long prompt before it says anything.
    let mut provider = crate::provider();
    provider.first_event_timeout = Some(secs(600));
    provider.idle_timeout = Some(secs(15));
    let client = ProviderClient::builder(
        Arc::new(TestProtocol),
        provider,
        Arc::new(transport),
        Arc::new(clock),
        Arc::new(timer.clone()),
    )
    .policy(RetryPolicy {
        max_attempts: 1,
        ..RetryPolicy::default()
    })
    .build();

    let run = |expect: std::time::Duration, code: &str| {
        let mut stream = client.stream(fixtures::request(fixtures::model()), CancelToken::new());
        block_on(async {
            let mut events = Vec::new();
            loop {
                let (event, ()) = join(stream.next(), async {
                    if events.len() == usize::from(code == "idle_timeout") {
                        timer.sleeping().await;
                        assert_eq!(timer.running(), [expect], "{code}");
                        timer.advance(expect);
                    }
                })
                .await;
                match event {
                    Some(event) => events.push(event),
                    None => break,
                }
            }
            assert_kind(final_error(&events), ErrorKind::Transient, code);
        });
    };
    run(secs(600), "first_event_timeout");
    run(secs(15), "idle_timeout");
}
