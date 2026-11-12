//! The real HTTP transport against the scripted SSE server on the loopback
//! interface: what only a real socket can show.
//!
//! These tests use real sockets and take no fixed time. The server and the
//! test take turns through gates, and the time limits are on the fake
//! clock, which the test moves. The async runtime is a real one, because
//! the HTTP client needs it, with one thread and no clock of its own.

use std::sync::Arc;

use cortex_base::{CancelToken, ErrorKind, Secret};
use cortex_llm::error::TransportErrorKind;
use cortex_llm::{
    ApiKeyAuth, HttpOptions, HttpRequest, HttpTransport, KeyPlacement, ModelClient, ModelEvent,
    ProviderClient, ProviderInfo, RetryPolicy, Transport,
};
use cortex_testkit::protocol::{sse, text_response};
use cortex_testkit::{
    FakeClock, FakeTimer, Gate, ServerReply, SseServer, TestProtocol, fixtures, join,
};
use serde_json::json;

use crate::{START, assert_kind, final_error, names, secs};

/// Runs a test's async body on a runtime of its own.
fn run<F: Future>(body: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(body)
}

fn post(url: String, body: &str) -> HttpRequest {
    let mut request = HttpRequest::post(url, body.as_bytes().to_vec());
    request.headers.insert("content-type", "application/json");
    request
}

/// A provider client over the real transport, talking to `server`, with a
/// clock and a timer the test controls.
fn client(server: &SseServer, timer: &FakeTimer, clock: &FakeClock) -> ProviderClient {
    ProviderClient::builder(
        Arc::new(TestProtocol),
        ProviderInfo::new(fixtures::PROVIDER, fixtures::PROTOCOL, server.base_url()),
        Arc::new(HttpTransport::default()),
        Arc::new(clock.clone()),
        Arc::new(timer.clone()),
    )
    .auth(Arc::new(ApiKeyAuth::new(
        Secret::new("sk-loopback-test"),
        KeyPlacement::Header("x-api-key".to_owned()),
        "a test",
    )))
    .policy(RetryPolicy {
        max_attempts: 1,
        ..RetryPolicy::default()
    })
    .build()
}

#[test]
fn the_client_is_created_on_first_use_and_not_before() {
    let transport = HttpTransport::new(HttpOptions::default());
    assert!(!transport.is_created(), "making a transport makes no client");
    let server = SseServer::start();
    server.push(ServerReply::sse().send("data: x\n\n"));
    run(async {
        assert!(!transport.is_created());
        let mut response = transport
            .send(post(server.url("/v1/test"), "{}"), CancelToken::new())
            .await
            .unwrap();
        assert!(transport.is_created(), "the first request made it");
        assert_eq!(response.status, 200);
        while response.body.next().await.is_some() {}
    });

    // Or ahead of the first request, off the start-up path.
    let warmed = HttpTransport::default();
    warmed.warm().unwrap();
    assert!(warmed.is_created());
}

#[test]
fn a_request_arrives_whole_and_the_response_comes_back() {
    let server = SseServer::start();
    server.push(
        ServerReply::sse()
            .header("x-request-id", "req_real")
            .send("data: one\n\n")
            .send("data: two\n\n"),
    );
    let transport = HttpTransport::default();
    let key = Secret::new("sk-on-the-wire");
    let (status, headers, body) = run(async {
        let mut request = post(server.url("/v1/test?x=1"), r#"{"model":"m","speed":"fast"}"#);
        request.headers.insert("x-custom", "value");
        request.headers.insert_secret("x-api-key", &key);
        let mut response = transport.send(request, CancelToken::new()).await.unwrap();
        let mut body = Vec::new();
        while let Some(chunk) = response.body.next().await {
            body.extend_from_slice(&chunk.unwrap());
        }
        (response.status, response.headers, body)
    });
    assert_eq!(status, 200);
    assert_eq!(headers.get("content-type"), Some("text/event-stream"));
    assert_eq!(headers.get("X-Request-Id"), Some("req_real"));
    assert_eq!(body, b"data: one\n\ndata: two\n\n", "the chunked framing is gone");

    let received = server.requests();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "POST");
    assert_eq!(received[0].path, "/v1/test?x=1");
    assert_eq!(received[0].header("content-type"), Some("application/json"));
    assert_eq!(received[0].header("x-custom"), Some("value"));
    assert_eq!(received[0].header("x-api-key"), Some("sk-on-the-wire"), "a secret is sent");
    assert!(received[0].header("user-agent").unwrap().starts_with("cortex-harness/"));
    assert_eq!(received[0].body, br#"{"model":"m","speed":"fast"}"#);
}

#[test]
fn the_body_is_streamed_as_it_arrives() {
    let server = SseServer::start();
    let (first_sent, send_the_rest) = (Gate::new(), Gate::new());
    server.push(
        ServerReply::sse()
            .send("data: first\n\n")
            .mark(&first_sent)
            .wait(&send_the_rest)
            .send("data: second\n\n"),
    );
    let transport = HttpTransport::default();
    run(async {
        let mut response = transport
            .send(post(server.url("/"), "{}"), CancelToken::new())
            .await
            .unwrap();
        // The first piece is here while the server has yet to send the
        // second: the body is not collected before it is handed over.
        let first = response.body.next().await.unwrap().unwrap();
        assert_eq!(first, b"data: first\n\n");
        assert!(first_sent.is_open());
        assert!(!send_the_rest.is_open());

        send_the_rest.open();
        let mut rest = Vec::new();
        while let Some(chunk) = response.body.next().await {
            rest.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(rest, b"data: second\n\n");
    });
}

#[test]
fn a_failure_status_is_a_response_and_a_redirect_is_not_followed() {
    let server = SseServer::start();
    server.push(
        ServerReply::status(429)
            .header("retry-after", "7")
            .send(r#"{"code":"rate_limit","message":"Slow down."}"#),
    );
    server.push(
        ServerReply::status(302)
            .header("location", &server.url("/elsewhere"))
            .send("moved"),
    );
    let transport = HttpTransport::default();
    run(async {
        let mut limited = transport
            .send(post(server.url("/"), "{}"), CancelToken::new())
            .await
            .expect("a 429 is a response, not a transport error");
        assert_eq!(limited.status, 429);
        assert_eq!(limited.headers.get("retry-after"), Some("7"));
        let body = limited.body.next().await.unwrap().unwrap();
        assert_eq!(body, br#"{"code":"rate_limit","message":"Slow down."}"#);

        let mut request = post(server.url("/"), "{}");
        request.headers.insert_secret("x-api-key", &Secret::new("sk-stays-here"));
        let redirected = transport.send(request, CancelToken::new()).await.unwrap();
        assert_eq!(redirected.status, 302);
    });
    assert_eq!(
        server.requests().len(),
        2,
        "the redirect was not followed, so the credential went nowhere else"
    );
}

#[test]
fn cancelling_closes_the_connection() {
    let server = SseServer::start();
    let closed = Gate::new();
    server.push(
        ServerReply::sse()
            .send("data: first\n\n")
            .hang_until_closed(&closed),
    );
    let transport = HttpTransport::default();
    let cancel = CancelToken::new();
    run(async {
        let mut response = transport
            .send(post(server.url("/"), "{}"), cancel.clone())
            .await
            .unwrap();
        assert!(response.body.next().await.unwrap().is_ok());
        assert!(!closed.is_open(), "the server is holding the response open");

        cancel.cancel();
        let ended = response.body.next().await.unwrap().unwrap_err();
        assert_eq!(ended.kind, TransportErrorKind::Cancelled);
        assert!(response.body.next().await.is_none());
        drop(response);
        // The server sees the client hang up. Nothing here waits for a
        // time: the gate opens when the server's read returns.
        closed.opened().await;
    });
}

#[test]
fn cancelling_before_the_response_begins_ends_the_send() {
    let server = SseServer::start();
    let never = Gate::new();
    server.push(ServerReply::sse().hold_until(&never).send("data: x\n\n"));
    let transport = HttpTransport::default();
    let cancel = CancelToken::new();
    run(async {
        let (result, ()) = join(
            transport.send(post(server.url("/"), "{}"), cancel.clone()),
            async { cancel.cancel() },
        )
        .await;
        assert_eq!(result.unwrap_err().kind, TransportErrorKind::Cancelled);
    });
}

#[test]
fn a_connection_that_breaks_is_a_transport_error_of_the_right_kind() {
    let transport = HttpTransport::default();

    // Nothing listens on a port that was just given back. Only where the
    // refusal is immediate: Windows tries a refused loopback connection
    // again for two seconds before it reports it.
    #[cfg(unix)]
    {
        let unused = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.local_addr().unwrap()
        };
        let refused =
            run(transport.send(post(format!("http://{unused}/"), "{}"), CancelToken::new()));
        assert_eq!(refused.unwrap_err().kind, TransportErrorKind::Connect);
    }

    // A server that takes the request and hangs up without an answer.
    let server = SseServer::start();
    server.push(ServerReply::hang_up());
    let hung_up = run(transport.send(post(server.url("/"), "{}"), CancelToken::new()));
    assert_eq!(hung_up.unwrap_err().kind, TransportErrorKind::Connect);

    // A body that stops before its end.
    server.push(ServerReply::sse().send("data: half\n\n").disconnect());
    run(async {
        let mut response = transport
            .send(post(server.url("/"), "{}"), CancelToken::new())
            .await
            .unwrap();
        let mut last = None;
        while let Some(chunk) = response.body.next().await {
            last = Some(chunk);
        }
        let error = last.unwrap().unwrap_err();
        assert_eq!(error.kind, TransportErrorKind::Body, "{error}");
    });

    // A URL that is not one, and a header HTTP cannot carry. Neither is
    // worth sending again, and neither message shows a value.
    let not_a_url = run(transport.send(post("not a url".to_owned(), "{}"), CancelToken::new()));
    assert_eq!(not_a_url.unwrap_err().kind, TransportErrorKind::InvalidRequest);
    let mut request = post(server.url("/"), "{}");
    request.headers.insert_secret("x-api-key", &Secret::new("line\nbreak-secret"));
    let bad_header = run(transport.send(request, CancelToken::new())).unwrap_err();
    assert_eq!(bad_header.kind, TransportErrorKind::InvalidRequest);
    assert!(bad_header.message.contains("x-api-key"), "{bad_header}");
    assert!(!bad_header.message.contains("secret"), "{bad_header}");
}

#[test]
fn a_whole_request_through_the_client_and_the_real_transport() {
    let server = SseServer::start();
    server.push(
        ServerReply::sse()
            .header("x-request-id", "req_e2e")
            .send(text_response("Hello", &json!({"input": 12, "output": 3, "speed": "standard"}))),
    );
    let clock = FakeClock::starting_at(START).unwrap();
    let timer = FakeTimer::manual(&clock);
    let client = client(&server, &timer, &clock);

    let mut request = fixtures::request(fixtures::model());
    request.speed = cortex_protocol::Speed::Fast;
    let events = run(client.stream(request, CancelToken::new()).collect());
    assert_eq!(
        names(&events),
        ["start", "block_start", "text", "block_end", "speed_fallback", "end"],
        "asked for fast, served standard, over a real socket"
    );
    let received = server.requests();
    assert_eq!(received[0].path, "/v1/test");
    assert_eq!(received[0].header("x-api-key"), Some("sk-loopback-test"));
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["speed"], "fast");
    assert!(timer.running().is_empty(), "no time limit is left running");
}

#[test]
fn a_stalled_response_ends_at_the_idle_limit_and_the_connection_is_closed() {
    let server = SseServer::start();
    let closed = Gate::new();
    server.push(
        ServerReply::sse()
            .send(sse("start", &json!({"id": "r1", "model": "m"})))
            .hang_until_closed(&closed),
    );
    let clock = FakeClock::starting_at(START).unwrap();
    let timer = FakeTimer::manual(&clock);
    let client = client(&server, &timer, &clock);

    run(async {
        let mut stream = client.stream(fixtures::request(fixtures::model()), CancelToken::new());
        assert!(matches!(stream.next().await, Some(ModelEvent::Start { .. })));

        // The server has gone silent. Ninety seconds pass on the fake clock.
        let (last, ()) = join(stream.next(), async {
            timer.sleeping().await;
            assert_eq!(timer.running(), [secs(90)]);
            timer.advance(secs(90));
        })
        .await;
        let events = [last.unwrap()];
        assert_kind(final_error(&events), ErrorKind::Transient, "idle_timeout");
        assert!(stream.next().await.is_none());

        // Giving the response up closed the connection.
        closed.opened().await;
    });
}

#[test]
fn cancelling_a_request_through_the_client_closes_the_connection() {
    let server = SseServer::start();
    let closed = Gate::new();
    server.push(
        ServerReply::sse()
            .send(sse("start", &json!({"id": "r1", "model": "m"})))
            .hang_until_closed(&closed),
    );
    let clock = FakeClock::starting_at(START).unwrap();
    let timer = FakeTimer::manual(&clock);
    let client = client(&server, &timer, &clock);
    let cancel = CancelToken::new();

    run(async {
        let mut stream = client.stream(fixtures::request(fixtures::model()), cancel.clone());
        assert!(matches!(stream.next().await, Some(ModelEvent::Start { .. })));
        cancel.cancel();
        let last = [stream.next().await.unwrap()];
        assert_kind(final_error(&last), ErrorKind::Interrupted, "interrupted");
        assert!(stream.next().await.is_none());
        closed.opened().await;
    });
    assert_eq!(server.requests().len(), 1);
}
