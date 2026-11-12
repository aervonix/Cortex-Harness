//! A wire dialect made for tests.
//!
//! `cortex-llm` has no real protocol until the first provider step. Its
//! client still has to be tested: building a request, decoding a stream,
//! classifying a failure. [`TestProtocol`] is a dialect small enough to
//! write by hand in a test and complete enough to say everything a real one
//! can. It is not a model of any provider's API.
//!
//! # The request
//!
//! `POST <base_url>/v1/test` with a JSON body: `model`, `speed` (always
//! sent, also for `standard`), `role`, `system`, `messages` (the count),
//! `tools` (the names), and `max_output` when one is set.
//!
//! # The response stream
//!
//! Server-sent events. The event's name says what it is and its data is a
//! JSON object:
//!
//! | Event | Data |
//! |---|---|
//! | `start` | `id`, `model` |
//! | `block_start` | `index`, `kind`: `text`, `reasoning`, `tool_call` or `provider_block` |
//! | `text`, `reasoning` | `index`, `text` |
//! | `tool_call` | `index`, `id`, `name` |
//! | `tool_args` | `index`, `json` |
//! | `block_end` | `index`, `raw` (a string) |
//! | `usage` | the usage fields below |
//! | `end` | `stop`, `raw_stop`, `detail`, and the usage fields |
//! | `error` | `code`, `message`, `retry_after` (seconds), `tier` (true when the refusal is the speed tier's own) |
//!
//! Usage fields: `input`, `output`, `cache_read`, `cache_write`,
//! `reasoning`, and `speed`, the tier that served the request, **only when
//! the script puts it there**. Any other event is skipped.
//!
//! # A failed response
//!
//! A JSON body with `code`, `message` and optionally `tier`. The codes are
//! `rate_limit`, `quota`, `overloaded`, `server_error`, `context_length`
//! and `auth`; anything else leaves the status to decide. The wait comes
//! from the `retry-after` header and the request id from `x-request-id`.
//!
//! # Response headers
//!
//! `x-request-id`; `x-served-speed`, the tier that served the request;
//! `x-requests-remaining`, a rate limit; and `x-fast-requests-remaining`,
//! the `fast` tier's own.

use std::time::Duration;

use cortex_base::UtcTime;
use cortex_llm::error::{BuildError, DecodeError, ProviderError, Signal};
use cortex_llm::http::parse_retry_after;
use cortex_llm::{
    BlockKind, EventSink, Headers, HttpFailure, HttpRequest, ModelRequest, Protocol, ProviderInfo,
    ReportedUsage, SseEvent, StreamDecoder,
};
use cortex_protocol::usage::{LimitMeasure, LimitWindow, TokenUsage};
use cortex_protocol::{RawJson, Speed, StopReason};
use serde_json::{Value, json};

use crate::fixtures::PROTOCOL;

/// The test dialect. See the [module](self) for what it sends and reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct TestProtocol;

/// One event of the test dialect, as the text a server sends.
#[must_use]
pub fn sse(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// A whole response that says `text` and ends: `start`, one text block,
/// and `end` with the usage given.
#[must_use]
pub fn text_response(text: &str, usage: &Value) -> String {
    let mut end = json!({"stop": "end", "raw_stop": "done"});
    merge(&mut end, usage);
    [
        sse("start", &json!({"id": "resp_1", "model": "test-model"})),
        sse("block_start", &json!({"index": 0, "kind": "text"})),
        sse("text", &json!({"index": 0, "text": text})),
        sse(
            "block_end",
            &json!({"index": 0, "raw": json!({"type": "text", "text": text}).to_string()}),
        ),
        sse("end", &end),
    ]
    .concat()
}

/// The body of a failed response.
#[must_use]
pub fn failure_body(code: &str, message: &str) -> Vec<u8> {
    json!({"code": code, "message": message})
        .to_string()
        .into_bytes()
}

/// The body of a response that failed for the speed tier's own limit.
#[must_use]
pub fn tier_failure_body(code: &str, message: &str) -> Vec<u8> {
    json!({"code": code, "message": message, "tier": true})
        .to_string()
        .into_bytes()
}

fn merge(into: &mut Value, from: &Value) {
    if let (Some(into), Some(from)) = (into.as_object_mut(), from.as_object()) {
        for (key, value) in from {
            into.insert(key.clone(), value.clone());
        }
    }
}

fn signal(code: &str) -> Signal {
    match code {
        "rate_limit" => Signal::RateLimit,
        "quota" => Signal::Quota,
        "overloaded" => Signal::Overload,
        "server_error" => Signal::Server,
        "context_length" => Signal::ContextLength,
        "auth" => Signal::Auth,
        _ => Signal::None,
    }
}

impl Protocol for TestProtocol {
    fn name(&self) -> &str {
        PROTOCOL
    }

    fn build(
        &self,
        request: &ModelRequest,
        provider: &ProviderInfo,
    ) -> Result<HttpRequest, BuildError> {
        if request.model.info.protocol != PROTOCOL {
            return Err(BuildError::new(format!(
                "the model `{}` speaks `{}`, not `{PROTOCOL}`",
                request.model.info.id, request.model.info.protocol
            )));
        }
        let mut body = json!({
            "model": request.model.info.id.as_str(),
            "speed": request.speed.as_str(),
            "role": request.role.as_str(),
            "system": request.system,
            "messages": request.messages.len(),
            "tools": request.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        });
        if let Some(max_output) = request.max_output {
            body["max_output"] = json!(max_output);
        }
        let mut http = HttpRequest::post(
            format!("{}/v1/test", provider.base_url),
            body.to_string().into_bytes(),
        );
        http.headers.insert("content-type", "application/json");
        http.headers.insert("accept", "text/event-stream");
        Ok(http)
    }

    fn decoder(&self, request: &ModelRequest, _provider: &ProviderInfo) -> Box<dyn StreamDecoder> {
        Box::new(TestDecoder {
            sent: request.speed.clone(),
        })
    }

    fn classify(&self, failure: &HttpFailure<'_>, request: &ModelRequest) -> ProviderError {
        let body: Value = serde_json::from_slice(failure.body).unwrap_or(Value::Null);
        let code = body["code"].as_str().unwrap_or("unknown_error");
        let message = body["message"]
            .as_str()
            .map_or_else(|| format!("HTTP {}", failure.status), str::to_owned);
        let wait = failure
            .headers
            .get("retry-after")
            .and_then(|value| parse_retry_after(value, failure.now));
        let error = ProviderError::classified(Some(failure.status), signal(code), code, message)
            .with_retry_after(wait)
            .with_request_id(failure.headers.get("x-request-id").map(str::to_owned));
        if body["tier"] == json!(true) {
            error.for_tier(request.speed.clone())
        } else {
            error
        }
    }

    fn read_headers(&self, headers: &Headers, _now: UtcTime, out: &mut EventSink) {
        if let Some(id) = headers.get("x-request-id") {
            out.request_id(id);
        }
        if let Some(speed) = headers.get("x-served-speed") {
            out.served_at(Speed::from_wire(speed));
        }
        let mut limits = Vec::new();
        for (header, speed) in [
            ("x-requests-remaining", None),
            ("x-fast-requests-remaining", Some(Speed::Fast)),
        ] {
            if let Some(remaining) = headers.get(header).and_then(|value| value.parse().ok()) {
                limits.push(LimitWindow {
                    measure: LimitMeasure::Requests,
                    speed,
                    limit: None,
                    remaining: Some(remaining),
                    resets_at: None,
                });
            }
        }
        out.limits(limits);
    }
}

struct TestDecoder {
    /// The speed the request was sent at: what an error's `tier` refers to.
    sent: Speed,
}

fn index(data: &Value) -> Result<u32, DecodeError> {
    data["index"]
        .as_u64()
        .and_then(|index| u32::try_from(index).ok())
        .ok_or_else(|| DecodeError::new("an event has no `index`"))
}

fn text<'a>(data: &'a Value, key: &str) -> Result<&'a str, DecodeError> {
    data[key]
        .as_str()
        .ok_or_else(|| DecodeError::new(format!("an event has no `{key}`")))
}

fn usage(data: &Value) -> ReportedUsage {
    let count = |key: &str| data[key].as_u64();
    let tokens = TokenUsage {
        input: count("input").unwrap_or(0),
        cache_read: count("cache_read").unwrap_or(0),
        cache_write: count("cache_write").unwrap_or(0),
        cache_write_classes: Vec::new(),
        output: count("output").unwrap_or(0),
        reasoning: count("reasoning"),
    };
    ReportedUsage {
        tokens,
        // Only what the response says. Never the speed that was sent.
        speed: data["speed"].as_str().map(Speed::from_wire),
        provider_cost: count("cost_nanos"),
        raw: Some(RawJson::new(data.to_string())),
    }
}

impl StreamDecoder for TestDecoder {
    fn feed(&mut self, event: SseEvent<'_>, out: &mut EventSink) -> Result<(), DecodeError> {
        let known = matches!(
            event.event,
            "start"
                | "block_start"
                | "text"
                | "reasoning"
                | "tool_call"
                | "tool_args"
                | "block_end"
                | "usage"
                | "end"
                | "error"
        );
        if !known {
            return Ok(());
        }
        let data: Value = serde_json::from_str(event.data).map_err(|error| {
            DecodeError::new(format!("the `{}` event is not JSON: {error}", event.event))
        })?;
        match event.event {
            "start" => out.start(
                data["id"].as_str().map(str::to_owned),
                data["model"].as_str().unwrap_or_default(),
            ),
            "block_start" => {
                let kind = match text(&data, "kind")? {
                    "text" => BlockKind::Text,
                    "reasoning" => BlockKind::Reasoning,
                    "tool_call" => BlockKind::ToolCall,
                    "provider_block" => BlockKind::ProviderBlock,
                    other => {
                        return Err(DecodeError::new(format!("unknown block kind `{other}`")));
                    }
                };
                out.block_start(index(&data)?, kind);
            }
            "text" => out.text_delta(index(&data)?, text(&data, "text")?),
            "reasoning" => out.reasoning_delta(index(&data)?, text(&data, "text")?),
            "tool_call" => {
                out.tool_call_start(index(&data)?, text(&data, "id")?, text(&data, "name")?);
            }
            "tool_args" => out.tool_args_delta(index(&data)?, text(&data, "json")?),
            "block_end" => out.block_end(index(&data)?, RawJson::new(text(&data, "raw")?)),
            "usage" => out.usage(usage(&data)),
            "end" => out.end(
                StopReason::from_wire(text(&data, "stop")?),
                data["raw_stop"].as_str().unwrap_or_default(),
                data["detail"].as_str().map(str::to_owned),
                usage(&data),
            ),
            _ => {
                let code = text(&data, "code")?;
                let message = data["message"].as_str().unwrap_or("The stream failed.");
                let wait = data["retry_after"].as_u64().map(Duration::from_secs);
                let error = ProviderError::classified(None, signal(code), code, message)
                    .with_retry_after(wait);
                out.error(if data["tier"] == json!(true) {
                    error.for_tier(self.sent.clone())
                } else {
                    error
                });
            }
        }
        Ok(())
    }

    fn finish(&mut self, _out: &mut EventSink) -> Result<(), DecodeError> {
        Ok(())
    }
}
