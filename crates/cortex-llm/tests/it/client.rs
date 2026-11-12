//! The client's ordinary path: a request goes out as the protocol built it,
//! with the provider's headers and the credential, and the response comes
//! back as events.

use std::sync::Arc;

use cortex_base::{CancelToken, ErrorKind, Secret};
use cortex_llm::{
    ApiKeyAuth, BlockKind, KeyPlacement, ModelClient, ModelEvent, ProviderClient, RetryPolicy,
};
use cortex_protocol::usage::LimitMeasure;
use cortex_protocol::{Speed, StopReason};
use cortex_testkit::protocol::sse;
use cortex_testkit::{
    FakeClock, FakeTimer, Reply, ScriptedTransport, TestProtocol, block_on, fixtures,
};
use serde_json::json;

use crate::{
    HELLO, Harness, assert_kind, assert_well_formed, final_error, final_usage, hello, hello_text,
    names, provider,
};

#[test]
fn a_response_arrives_as_events_whatever_the_chunking() {
    let text = [
        sse("start", &json!({"id": "resp_9", "model": "test-model"})),
        sse("ping", &json!({})),
        sse("block_start", &json!({"index": 0, "kind": "reasoning"})),
        sse("reasoning", &json!({"index": 0, "text": "Think."})),
        sse("block_end", &json!({"index": 0, "raw": "{\"sig\":\"abc\"}"})),
        sse("block_start", &json!({"index": 1, "kind": "tool_call"})),
        sse("tool_call", &json!({"index": 1, "id": "call_1", "name": "read"})),
        sse("tool_args", &json!({"index": 1, "json": "{\"pa"})),
        sse("tool_args", &json!({"index": 1, "json": "th\":\"a\"}"})),
        sse("block_end", &json!({"index": 1, "raw": "{}"})),
        sse("something_new", &json!({"a": 1})),
        sse("usage", &json!({"input": 100, "output": 7})),
        sse(
            "end",
            &json!({"stop": "tool_calls", "raw_stop": "tool_use", "detail": "one call",
                    "input": 100, "output": 9, "cache_read": 50, "reasoning": 4}),
        ),
    ]
    .concat();

    let whole = Harness::new([Reply::ok(&text)]).run();
    assert_eq!(
        names(&whole),
        [
            "start",
            "block_start",
            "reasoning",
            "block_end",
            "block_start",
            "tool_call",
            "tool_args",
            "tool_args",
            "block_end",
            "usage",
            "end"
        ],
        "events the protocol does not know are skipped"
    );
    assert_well_formed(&whole);
    assert_eq!(
        whole[0],
        ModelEvent::Start {
            response_id: Some("resp_9".to_owned()),
            model: "test-model".to_owned()
        }
    );
    assert_eq!(
        whole[1],
        ModelEvent::BlockStart {
            index: 0,
            kind: BlockKind::Reasoning
        }
    );
    let ModelEvent::BlockEnd { raw, .. } = &whole[3] else {
        panic!("{:?}", whole[3]);
    };
    assert_eq!(raw.as_str(), "{\"sig\":\"abc\"}", "the raw block is kept byte for byte");
    let ModelEvent::End {
        stop,
        raw_stop,
        stop_detail,
        usage,
    } = whole.last().unwrap()
    else {
        panic!("no end");
    };
    assert_eq!(*stop, StopReason::ToolCalls);
    assert_eq!(raw_stop, "tool_use");
    assert_eq!(stop_detail.as_deref(), Some("one call"));
    assert_eq!(
        (usage.tokens.input, usage.tokens.output, usage.tokens.cache_read),
        (100, 9, 50)
    );
    assert_eq!(usage.tokens.reasoning, Some(4));
    assert!(usage.raw.is_some(), "the provider's own usage object is kept");

    // The same bytes in pieces of every small size decode to the same events.
    for size in [1, 2, 3, 7, 64] {
        let chunked = Harness::new([Reply::status(200).chunks_of(size, &text)]).run();
        assert_eq!(chunked, whole, "in chunks of {size}");
    }
}

#[test]
fn the_request_is_what_the_protocol_built_plus_headers_and_the_credential() {
    let clock = FakeClock::default();
    let transport = ScriptedTransport::new([hello()]);
    let mut info = provider();
    info.headers.insert("x-org", "acme");
    info.headers.insert("accept", "application/x-override");
    let client = ProviderClient::builder(
        Arc::new(TestProtocol),
        info,
        Arc::new(transport.clone()),
        Arc::new(clock.clone()),
        Arc::new(FakeTimer::auto(&clock)),
    )
    .auth(Arc::new(ApiKeyAuth::new(
        Secret::new("sk-test-secret"),
        KeyPlacement::Header("x-api-key".to_owned()),
        "a test",
    )))
    .build();
    assert_eq!(client.provider().id.as_str(), "test");

    let mut request = fixtures::request(fixtures::model());
    request.max_output = Some(512);
    let events = block_on(client.stream(request, CancelToken::new()).collect());
    assert_eq!(names(&events), HELLO);

    let sent = transport.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, "https://test.invalid/v1/test");
    assert_eq!(sent[0].headers.get("content-type"), Some("application/json"));
    assert_eq!(sent[0].headers.get("x-org"), Some("acme"));
    assert_eq!(
        sent[0].headers.get("accept"),
        Some("application/x-override"),
        "the provider entry's header wins over the protocol's"
    );
    assert_eq!(sent[0].headers.get("x-api-key"), Some("sk-test-secret"));
    assert!(!format!("{:?}", sent[0]).contains("sk-test-secret"));
    assert_eq!(
        transport.bodies()[0],
        json!({
            "model": "test-model", "speed": "standard", "role": "main",
            "system": "You are a test.", "messages": 1, "tools": [], "max_output": 512,
        })
    );
}

#[test]
fn nothing_is_sent_until_the_stream_is_read() {
    let harness = Harness::new([hello()]);
    let stream = harness
        .client
        .stream(fixtures::request(fixtures::model()), CancelToken::new());
    assert!(harness.transport.requests().is_empty());
    drop(stream);
    assert!(harness.transport.requests().is_empty(), "and an unread stream sends nothing");
    assert_eq!(harness.transport.remaining(), 1);
}

#[test]
fn response_headers_give_the_request_id_and_the_limits() {
    let harness = Harness::new([
        hello()
            .header("x-requests-remaining", "41")
            .header("x-fast-requests-remaining", "3"),
        Reply::status(200)
            .header("x-request-id", "req_abc")
            .chunk(sse("start", &json!({"id": "r", "model": "m"})))
            .chunk(sse("error", &json!({"code": "quota", "message": "No credit."}))),
    ]);
    let events = harness.run();
    assert_eq!(names(&events)[0], "limits", "what the headers said comes first");
    let ModelEvent::Limits(limits) = &events[0] else {
        panic!("{:?}", events[0]);
    };
    assert_eq!(limits.len(), 2);
    assert_eq!((limits[0].measure.clone(), limits[0].remaining), (LimitMeasure::Requests, Some(41)));
    assert_eq!(limits[0].speed, None, "the model's ordinary limit");
    assert_eq!(limits[1].speed, Some(Speed::Fast), "the tier's own limit says whose it is");
    assert_eq!(limits[1].remaining, Some(3));

    // An error that arrives inside a stream gets the id from the headers.
    let events = harness.run();
    let error = final_error(&events);
    assert_kind(error, ErrorKind::Provider, "quota");
    assert_eq!(error.request_id.as_deref(), Some("req_abc"));
}

#[test]
fn a_request_the_protocol_cannot_build_fails_without_being_sent() {
    let harness = Harness::new([hello()]);
    let foreign = cortex_llm::Catalog::from_json(
        r#"{"version": "t", "model": [{"provider": "test", "id": "other", "name": "Other",
            "protocol": "some-other-dialect", "context_window": 10, "max_output": 5}]}"#,
    )
    .unwrap();
    let request = fixtures::request(Arc::clone(&foreign.models()[0]));
    let events = harness.run_request(request);
    assert_eq!(names(&events), ["error"]);
    assert_kind(final_error(&events), ErrorKind::Internal, "invalid_request");
    assert!(harness.transport.requests().is_empty());
    assert!(harness.timer.sleeps().is_empty(), "and is not retried");
}

#[test]
fn a_missing_credential_fails_as_auth_without_being_sent() {
    let clock = FakeClock::default();
    let transport = ScriptedTransport::new([hello()]);
    let client = ProviderClient::builder(
        Arc::new(TestProtocol),
        provider(),
        Arc::new(transport.clone()),
        Arc::new(clock.clone()),
        Arc::new(FakeTimer::auto(&clock)),
    )
    .auth(Arc::new(ApiKeyAuth::new(
        Secret::new(""),
        KeyPlacement::Bearer,
        "environment variable TEST_API_KEY",
    )))
    .build();
    let events = block_on(
        client
            .stream(fixtures::request(fixtures::model()), CancelToken::new())
            .collect(),
    );
    let error = final_error(&events);
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(error.message.contains("TEST_API_KEY"), "{error}");
    assert!(transport.requests().is_empty());
}

#[test]
fn a_request_cancelled_before_it_starts_sends_nothing() {
    let harness = Harness::new([hello()]);
    let cancel = CancelToken::new();
    cancel.cancel();
    let events = block_on(
        harness
            .client
            .stream(fixtures::request(fixtures::model()), cancel)
            .collect(),
    );
    assert_eq!(names(&events), ["error"]);
    assert_kind(final_error(&events), ErrorKind::Interrupted, "interrupted");
    assert!(harness.transport.requests().is_empty());
}

#[test]
fn cancelling_a_response_part_way_ends_it_as_interrupted_and_is_not_retried() {
    let harness = Harness::new([
        Reply::ok(sse("start", &json!({"id": "r", "model": "m"}))).then_stall(),
        hello(),
    ]);
    let cancel = CancelToken::new();
    let mut stream = harness
        .client
        .stream(fixtures::request(fixtures::model()), cancel.clone());
    let events = block_on(async {
        let first = stream.next().await.unwrap();
        cancel.cancel();
        let mut events = vec![first];
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    });
    assert_eq!(names(&events), ["start", "error"]);
    assert_kind(final_error(&events), ErrorKind::Interrupted, "interrupted");
    assert_eq!(harness.transport.requests().len(), 1);
    assert!(harness.timer.sleeps().is_empty());
}

#[test]
fn a_new_policy_applies_from_the_next_request() {
    let harness = Harness::new([
        crate::failure(503, "server_error"),
        hello(),
        crate::failure(503, "server_error"),
    ]);
    assert_eq!(harness.client.policy().max_attempts, 5);
    assert_eq!(names(&harness.run()), ["retry", "start", "block_start", "text", "block_end", "end"]);

    harness.client.set_policy(RetryPolicy {
        max_attempts: 1,
        ..RetryPolicy::default()
    });
    assert_eq!(harness.client.policy().max_attempts, 1);
    let events = harness.run();
    assert_eq!(names(&events), ["error"], "one attempt and no retry");
    assert_eq!(harness.timer.sleeps().len(), 1, "only the first request waited");
}

#[test]
fn usage_carries_the_speed_and_a_stated_cost() {
    let harness = Harness::new([Reply::ok(hello_text(
        &json!({"input": 3, "output": 4, "cost_nanos": 1234}),
    ))]);
    let events = harness.run();
    let usage = final_usage(&events);
    assert_eq!(usage.speed, Speed::Standard);
    assert_eq!(usage.speed_requested, None);
    assert_eq!(usage.provider_cost, Some(1234));
    assert!(harness.timer.sleeps().is_empty());
}
