//! The decoder, from recorded and hand-written streams.
//!
//! Every fixture whose response is a stream is decoded here whole, cut in
//! two at every byte, one byte at a time, and in pieces of several fixed
//! sizes. The events must be the same each time. What they are is in a
//! snapshot beside the tests, one for each fixture, and the cases the build
//! plan names are also checked by hand below.
//!
//! **The work is bounded.** Cutting a stream of `n` bytes in two at every
//! byte decodes `n` bytes `n` times. The longest recorded stream is 10 KB,
//! and that would take longer than a test may on a slow runner. So a stream
//! is cut in two at every byte of its first [`EVERY_BYTE`] bytes and at
//! every eighth byte after that. The pass that delivers one byte at a time
//! still puts a boundary at every byte of every stream, whatever its
//! length.

use cortex_base::ErrorKind;
use cortex_llm::error::Throttle;
use cortex_llm::{BlockKind, ModelEvent, ModelRequest};
use cortex_protocol::usage::CacheWriteTokens;
use cortex_protocol::{Speed, StopReason};
use cortex_testkit::cases;
use serde_json::{Value, json};

use super::scenario;
use super::{assert_snapshot, decode, decode_whole, load};

/// The request a fixture's response answers, as far as the decoder cares:
/// the model and the speed it was sent at.
fn request_of(name: &str) -> ModelRequest {
    if name.contains("fast") {
        scenario::fast()
    } else {
        scenario::text()
    }
}

/// A line for each event, for a snapshot a person can read.
pub(crate) fn render(events: &[ModelEvent]) -> String {
    let mut out = String::new();
    let usage = |usage: &cortex_llm::Usage| {
        let tokens = &usage.tokens;
        let classes: Vec<String> = tokens
            .cache_write_classes
            .iter()
            .map(|class| format!("{}={}", class.class, class.tokens))
            .collect();
        format!(
            "input={} cache_read={} cache_write={}[{}] output={} reasoning={} speed={} asked={}",
            tokens.input,
            tokens.cache_read,
            tokens.cache_write,
            classes.join(","),
            tokens.output,
            tokens
                .reasoning
                .map_or_else(|| "-".to_owned(), |count| count.to_string()),
            usage.speed.as_str(),
            usage.speed_requested.as_ref().map_or("-", Speed::as_str),
        )
    };
    for event in events {
        let line = match event {
            ModelEvent::Start { response_id, model } => format!(
                "start id={} model={model}",
                response_id.as_deref().unwrap_or("-")
            ),
            ModelEvent::BlockStart { index, kind } => format!("block_start {index} {kind:?}"),
            ModelEvent::TextDelta { index, text } => format!("text {index} {text:?}"),
            ModelEvent::ReasoningDelta { index, text } => format!("reasoning {index} {text:?}"),
            ModelEvent::ToolCallStart { index, id, name } => {
                format!("tool_call {index} id={id} name={name}")
            }
            ModelEvent::ToolArgsDelta { index, json } => format!("tool_args {index} {json:?}"),
            ModelEvent::BlockEnd { index, raw } => format!("block_end {index} {}", raw.as_str()),
            ModelEvent::Usage(used) => format!("usage {}", usage(used)),
            ModelEvent::End {
                stop,
                raw_stop,
                stop_detail,
                usage: used,
            } => format!(
                "end stop={} raw={raw_stop} detail={} {}",
                stop.as_str(),
                stop_detail.as_deref().unwrap_or("-"),
                usage(used)
            ),
            ModelEvent::Error(error) => format!(
                "error kind={} code={} status={:?} throttle={:?} tier={} retry_after={:?} id={} message={:?}",
                error.kind,
                error.code,
                error.status,
                error.throttle,
                error.tier.as_ref().map_or("-", Speed::as_str),
                error.retry_after,
                error.request_id.as_deref().unwrap_or("-"),
                error.message,
            ),
            other => format!("{other:?}"),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// How far into a stream it is cut in two at every single byte.
const EVERY_BYTE: usize = 4096;

/// Decodes a fixture whole, cut in two at every byte, a byte at a time and
/// in pieces of fixed sizes, checks they all agree, and compares the events
/// with the snapshot.
fn every_cut(name: &str) {
    let fixture = load(name);
    assert_eq!(fixture.status, 200, "{name} is not a stream");
    let request = request_of(name);
    let stream = &fixture.response;
    let whole = decode_whole(&request, stream);
    assert!(!whole.is_empty(), "{name}: nothing was decoded");

    let cuts = (1..stream.len()).filter(|cut| *cut <= EVERY_BYTE || cut % 8 == 0);
    for cut in cuts {
        let cut_in_two = decode(&request, stream, &[cut]);
        assert!(
            cut_in_two.as_ref() == Ok(&whole),
            "{name}: cut at byte {cut} of {}, the events differ:\n{}\n--- whole\n{}",
            stream.len(),
            cut_in_two.map_or_else(|error| error, |events| render(&events)),
            render(&whole),
        );
    }
    // One byte at a time: a boundary at every byte of the stream. Then
    // pieces of other sizes, which put the boundaries elsewhere in each
    // event.
    for size in [1, 2, 3, 5, 7, 13, 64, 500] {
        let pieces: Vec<usize> = (1..stream.len()).filter(|at| at % size == 0).collect();
        assert!(
            decode(&request, stream, &pieces).as_ref() == Ok(&whole),
            "{name}: in pieces of {size} bytes"
        );
    }
    assert_snapshot(&format!("anthropic/decode/{name}.txt"), &render(&whole));
}

/// One test for each stream, so that they run side by side and a failure
/// names its fixture.
macro_rules! every_cut {
    ($($test:ident => $fixture:literal,)+) => {
        mod at_every_cut {
            $(
                #[test]
                fn $test() {
                    super::every_cut($fixture);
                }
            )+
        }

        /// The fixtures the tests above cover.
        const STREAMS: &[&str] = &[$($fixture),+];
    };
}

every_cut! {
    text => "text",
    reasoning => "reasoning",
    redacted_thinking => "redacted-thinking",
    tool_call => "tool-call",
    tool_calls => "tool-calls",
    tool_call_cut => "tool-call-cut",
    server_tool => "server-tool",
    unknown_event => "unknown-event",
    error_after_200 => "error-after-200",
    error_after_200_fast => "error-after-200-fast",
    max_tokens => "max-tokens",
    stop_refusal => "stop-refusal",
    stop_pause_turn => "stop-pause-turn",
    stop_sequence => "stop-sequence",
    stop_context_window => "stop-context-window",
    stop_unknown => "stop-unknown",
    fast => "fast",
    standard_named => "standard-named",
    conversation_1 => "conversation-1",
    conversation_2 => "conversation-2",
    conversation_3 => "conversation-3",
    conversation_4 => "conversation-4",
    switch_1 => "switch-1",
    switch_2 => "switch-2",
    switch_3 => "switch-3",
}

#[test]
fn every_stream_in_the_fixtures_directory_is_replayed() {
    let mut on_disk: Vec<String> = Vec::new();
    #[expect(
        clippy::disallowed_methods,
        reason = "a test lists its own fixture directory"
    )]
    let entries = std::fs::read_dir(super::fixtures_directory()).expect("the fixtures directory");
    for entry in entries {
        let name = entry
            .expect("an entry")
            .file_name()
            .into_string()
            .expect("a name");
        if load(&name).status == 200 {
            on_disk.push(name);
        }
    }
    on_disk.sort();
    let mut covered: Vec<&str> = STREAMS.to_vec();
    covered.sort_unstable();
    assert_eq!(
        on_disk, covered,
        "a stream fixture with no test, or a test with no fixture"
    );
}

fn events_of(name: &str) -> Vec<ModelEvent> {
    decode_whole(&request_of(name), &load(name).response)
}

fn raw_blocks(events: &[ModelEvent]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::BlockEnd { raw, .. } => {
                Some(serde_json::from_str(raw.as_str()).expect("a raw block is JSON"))
            }
            _ => None,
        })
        .collect()
}

fn kinds(events: &[ModelEvent]) -> Vec<BlockKind> {
    events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::BlockStart { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect()
}

fn end_of(events: &[ModelEvent]) -> (&StopReason, &str, Option<&str>, &cortex_llm::Usage) {
    match events.last() {
        Some(ModelEvent::End {
            stop,
            raw_stop,
            stop_detail,
            usage,
        }) => (stop, raw_stop, stop_detail.as_deref(), usage),
        other => panic!("the last event is not an end: {other:?}"),
    }
}

/// What every complete response must look like: one start first, each block
/// started once and ended once, and exactly one terminal event, last.
fn assert_well_formed(events: &[ModelEvent]) {
    assert!(
        matches!(events.first(), Some(ModelEvent::Start { .. })),
        "{events:?}"
    );
    let terminals = events.iter().filter(|event| event.is_terminal()).count();
    assert_eq!(terminals, 1);
    assert!(events.last().is_some_and(ModelEvent::is_terminal));
    let mut open: Vec<u32> = Vec::new();
    let mut closed: Vec<u32> = Vec::new();
    for event in events {
        match event {
            ModelEvent::BlockStart { index, .. } => {
                assert!(
                    !open.contains(index) && !closed.contains(index),
                    "{index} twice"
                );
                open.push(*index);
            }
            ModelEvent::BlockEnd { index, .. } => {
                assert!(open.contains(index), "{index} ended before it started");
                open.retain(|other| other != index);
                closed.push(*index);
            }
            ModelEvent::TextDelta { index, .. }
            | ModelEvent::ReasoningDelta { index, .. }
            | ModelEvent::ToolCallStart { index, .. }
            | ModelEvent::ToolArgsDelta { index, .. } => {
                assert!(open.contains(index), "{index} is not open");
            }
            _ => {}
        }
    }
    if matches!(events.last(), Some(ModelEvent::End { .. })) {
        assert!(open.is_empty(), "blocks left open: {open:?}");
    }
}

#[test]
fn text_is_its_deltas_and_the_block_the_provider_would_have_sent() {
    let events = events_of("text");
    assert_well_formed(&events);
    assert_eq!(kinds(&events), [BlockKind::Text]);
    assert!(matches!(
        &events[0],
        ModelEvent::Start { response_id: Some(id), model } if id == "msg_fixture0002" && model == "claude-haiku-5-5"
    ));
    assert_eq!(
        raw_blocks(&events),
        [json!({"type": "text", "text": "Hello, test suite!"})]
    );
    let (stop, raw, detail, usage) = end_of(&events);
    assert_eq!((stop, raw, detail), (&StopReason::End, "end_turn", None));
    // The provider's input count leaves the cache out already: nothing is
    // subtracted. The later count of output tokens replaces the first.
    assert_eq!(
        (
            usage.tokens.input,
            usage.tokens.output,
            usage.tokens.reasoning
        ),
        (29, 10, Some(0))
    );
    assert_eq!(
        (&usage.speed, &usage.speed_requested),
        (&Speed::Standard, &None)
    );
    let raw: Value = serde_json::from_str(usage.raw.as_ref().unwrap().as_str()).unwrap();
    assert_eq!(raw["output_tokens"], 10);
    assert_eq!(
        raw["service_tier"], "standard",
        "what only the first part said is kept"
    );
}

#[test]
fn reasoning_keeps_its_signature_byte_for_byte() {
    let events = events_of("reasoning");
    assert_well_formed(&events);
    assert_eq!(kinds(&events), [BlockKind::Reasoning, BlockKind::Text]);
    let blocks = raw_blocks(&events);
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["signature"], "fixture-signature-01");
    // The visible summary is what the deltas added up to.
    let shown: String = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!shown.is_empty());
    assert_eq!(blocks[0]["thinking"], shown.as_str());
    // The stored block has the provider's members in the provider's order.
    let ModelEvent::BlockEnd { raw, .. } = events
        .iter()
        .find(|event| matches!(event, ModelEvent::BlockEnd { index: 0, .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert!(
        raw.as_str()
            .starts_with(r#"{"type":"thinking","thinking":""#),
        "{raw}"
    );
    assert!(
        raw.as_str()
            .ends_with(r#","signature":"fixture-signature-01"}"#),
        "{raw}"
    );
    assert!(
        end_of(&events)
            .3
            .tokens
            .reasoning
            .is_some_and(|count| count > 0)
    );

    // A block that arrives whole goes back exactly as it came, and so does
    // one whose text is empty.
    let events = events_of("redacted-thinking");
    assert_well_formed(&events);
    assert_eq!(
        kinds(&events),
        [BlockKind::Reasoning, BlockKind::Reasoning, BlockKind::Text]
    );
    let raws: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::BlockEnd { raw, .. } => Some(raw.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        raws[0],
        r#"{"type":"redacted_thinking","data":"fixture-data-92"}"#
    );
    assert_eq!(
        raws[1],
        r#"{"type":"thinking","thinking":"","signature":"fixture-signature-93"}"#
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelEvent::ReasoningDelta { .. })),
        "empty reasoning text is not an event"
    );
}

#[test]
fn tool_calls_are_named_and_their_arguments_are_the_models_own_text() {
    let events = events_of("tool-call");
    assert_well_formed(&events);
    let started: Vec<(u32, &str, &str)> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ToolCallStart { index, id, name } => {
                Some((*index, id.as_str(), name.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].2, "get_weather");
    let arguments: String = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ToolArgsDelta { json, .. } => Some(json.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&arguments).unwrap(),
        json!({"city": "Paris"})
    );
    let block = raw_blocks(&events).pop().unwrap();
    assert_eq!(block["type"], "tool_use");
    assert_eq!(block["input"], json!({"city": "Paris"}));
    assert_eq!(block["id"], started[0].1);
    assert_eq!(end_of(&events).0, &StopReason::ToolCalls);

    // Several calls in one response: each has its own index, id and
    // arguments, and none is mixed with another.
    let events = events_of("tool-calls");
    assert_well_formed(&events);
    let mut cities = Vec::new();
    let mut ids = Vec::new();
    for block in raw_blocks(&events) {
        if block["type"] == "tool_use" {
            cities.push(block["input"]["city"].as_str().unwrap().to_owned());
            ids.push(block["id"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(cities, ["Paris", "Tokyo", "Lima"]);
    ids.dedup();
    assert_eq!(ids.len(), 3);
    for index in events.iter().filter_map(|event| match event {
        ModelEvent::ToolCallStart { index, .. } => Some(*index),
        _ => None,
    }) {
        let own: String = events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::ToolArgsDelta { index: at, json } if *at == index => {
                    Some(json.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(
            serde_json::from_str::<Value>(&own).is_ok(),
            "{index}: {own}"
        );
    }
}

#[test]
fn a_tool_call_that_was_cut_off_is_stored_as_something_that_can_be_sent_back() {
    let events = events_of("tool-call-cut");
    assert_well_formed(&events);
    // The loop gets the model's own text, cut as it was.
    let arguments: String = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ToolArgsDelta { json, .. } => Some(json.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(arguments, r#"{"city": "Par"#);
    // The stored block is valid JSON with an object for its arguments, and
    // keeps what the provider sent with the call. The provider never closed
    // the block: the end of the response did.
    let block = raw_blocks(&events).pop().unwrap();
    assert_eq!(block["input"], json!({"INVALID_JSON": r#"{"city": "Par"#}));
    assert_eq!(block["caller"], json!({"type": "direct"}));
    assert_eq!(end_of(&events).0, &StopReason::MaxOutput);
}

#[test]
fn a_server_side_tool_is_kept_as_provider_blocks() {
    let events = events_of("server-tool");
    assert_well_formed(&events);
    assert_eq!(
        kinds(&events),
        [
            BlockKind::Text,
            BlockKind::ProviderBlock,
            BlockKind::ProviderBlock,
            BlockKind::Text
        ]
    );
    // The server's own tool call is not one of the harness's: no call is
    // announced and no arguments are streamed to the loop.
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelEvent::ToolCallStart { .. } | ModelEvent::ToolArgsDelta { .. }
    )));
    let blocks = raw_blocks(&events);
    assert_eq!(
        blocks[1],
        json!({"type": "server_tool_use", "id": "srvtoolu_fixture9002", "name": "web_search",
               "input": {"query": "weather in Oslo today"}})
    );
    assert_eq!(blocks[2]["type"], "web_search_tool_result");
    assert_eq!(
        blocks[2]["content"][0]["encrypted_content"],
        "fixture-encrypted_content-91"
    );
    assert_eq!(blocks[2]["content"][0]["page_age"], Value::Null);
    // The citation that arrived as a delta is in the stored text block.
    assert_eq!(blocks[3]["text"], "It is clear in Oslo, 18 degrees.");
    assert_eq!(blocks[3]["citations"][0]["url"], "https://example.com/oslo");
    assert_eq!(blocks[3]["citations"].as_array().unwrap().len(), 1);
}

#[test]
fn what_is_not_known_is_skipped_and_nothing_else_is() {
    let events = events_of("unknown-event");
    assert_well_formed(&events);
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, test suite!");
    assert_eq!(
        raw_blocks(&events),
        [json!({"type": "text", "text": "Hello, test suite!"})]
    );
    // The usage is the one the response reported, not the unknown event's.
    assert_eq!(end_of(&events).3.tokens.output, 10);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelEvent::BlockStart { .. }))
            .count(),
        1
    );
}

#[test]
fn an_error_after_a_good_status_is_classified_like_a_failed_response() {
    let events = events_of("error-after-200");
    let Some(ModelEvent::Error(error)) = events.last() else {
        panic!("{events:?}");
    };
    assert_eq!(error.kind, ErrorKind::Transient);
    assert_eq!(error.code, "overloaded_error");
    assert_eq!(error.throttle, Some(Throttle::Overload));
    assert_eq!(error.status, None);
    assert_eq!(
        error.tier, None,
        "sent at standard: the model's, not a tier's"
    );
    // What arrived before it is still there: the client decides what to do.
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelEvent::TextDelta { .. }))
    );
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);

    // In a response to a request at fast speed, it is the tier's.
    let events = events_of("error-after-200-fast");
    let Some(ModelEvent::Error(error)) = events.last() else {
        panic!("{events:?}");
    };
    assert_eq!(error.tier, Some(Speed::Fast));
    assert_eq!(error.request_id.as_deref(), Some("req_fixture9003"));
}

#[test]
fn every_stop_reason_arrives_in_its_neutral_form() {
    let rows: &[(&str, StopReason, &str, Option<&str>)] = &[
        ("text", StopReason::End, "end_turn", None),
        ("tool-call", StopReason::ToolCalls, "tool_use", None),
        ("max-tokens", StopReason::MaxOutput, "max_tokens", None),
        (
            "stop-sequence",
            StopReason::End,
            "stop_sequence",
            Some("END"),
        ),
        ("stop-pause-turn", StopReason::Paused, "pause_turn", None),
        (
            "stop-refusal",
            StopReason::Refusal,
            "refusal",
            Some("cyber"),
        ),
        (
            "stop-context-window",
            StopReason::ContextOverflow,
            "model_context_window_exceeded",
            None,
        ),
    ];
    for (name, stop, raw, detail) in rows {
        let events = events_of(name);
        assert_well_formed(&events);
        assert_eq!(end_of(&events).0, stop, "{name}");
        assert_eq!(end_of(&events).1, *raw, "{name}");
        assert_eq!(end_of(&events).2, *detail, "{name}");
    }
    // A value this build does not know is not an error, and is not taken
    // for one of the values it does know.
    let events = events_of("stop-unknown");
    let (stop, raw, ..) = end_of(&events);
    assert!(!stop.is_known());
    assert_eq!(raw, "handed_off");
}

#[test]
fn the_usage_reports_the_speed_that_served_the_request() {
    // Asked for fast, served fast: nothing more to say.
    let events = events_of("fast");
    assert_well_formed(&events);
    let usage = end_of(&events).3;
    assert_eq!(
        (&usage.speed, &usage.speed_requested),
        (&Speed::Fast, &None)
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelEvent::SpeedFallback(_)))
    );
    let raw: Value = serde_json::from_str(usage.raw.as_ref().unwrap().as_str()).unwrap();
    assert_eq!(raw["speed"], "fast");

    // The same response to a request that was sent at standard: the record
    // says what the response says, never what was sent.
    let fixture = load("fast");
    let events = decode_whole(&scenario::standard_named(), &fixture.response);
    let usage = end_of(&events).3;
    assert_eq!(usage.speed, Speed::Fast);
    assert_eq!(usage.speed_requested, Some(Speed::Standard));

    // Asked for fast and served standard is said, once, as the provider's
    // doing, before the first usage that carries it.
    let fixture = load("standard-named");
    let events = decode_whole(&scenario::fast(), &fixture.response);
    let usage = end_of(&events).3;
    assert_eq!(usage.speed, Speed::Standard);
    assert_eq!(usage.speed_requested, Some(Speed::Fast));
    let moves: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::SpeedFallback(moved) => Some(moved),
            _ => None,
        })
        .collect();
    assert_eq!(moves.len(), 1);
    assert_eq!(
        (moves[0].requested.clone(), moves[0].served.clone()),
        (Speed::Fast, Speed::Standard)
    );
    assert_eq!(
        moves[0].by,
        cortex_protocol::speed::SpeedFallbackBy::Provider
    );
    assert_eq!(moves[0].cooldown_until, None);

    // A response that says nothing of the speed ran at the speed it was
    // sent at: the decoder does not make one up.
    let silent = String::from_utf8(load("fast").response)
        .unwrap()
        .replace(r#","speed":"fast""#, "");
    assert!(!silent.contains("\"speed\""));
    let events = decode_whole(&scenario::fast(), silent.as_bytes());
    let usage = end_of(&events).3;
    assert_eq!(
        (&usage.speed, &usage.speed_requested),
        (&Speed::Fast, &None)
    );
    let events = decode_whole(&scenario::standard_named(), silent.as_bytes());
    assert_eq!(end_of(&events).3.speed, Speed::Standard);
}

#[test]
fn cache_tokens_are_reported_by_class() {
    let events = events_of("conversation-1");
    let tokens = &end_of(&events).3.tokens;
    assert_eq!(tokens.cache_write, 1496);
    assert_eq!(
        tokens.cache_write_classes,
        [CacheWriteTokens {
            class: "5m".to_owned(),
            tokens: 1496
        }]
    );
    assert_eq!(tokens.cache_read, 0);
    let events = events_of("conversation-2");
    let tokens = &end_of(&events).3.tokens;
    assert_eq!(
        (tokens.input, tokens.cache_read, tokens.cache_write),
        (2, 1496, 284)
    );
}

/// A stream short enough to try every pair of cuts near each other: a tool
/// call whose arguments hold a character of more than one byte, with CRLF
/// line endings.
fn small_stream() -> Vec<u8> {
    let events: [(&str, Value); 6] = [
        (
            "message_start",
            json!({"type":"message_start","message":{"id":"m","model":"x","usage":{"input_tokens":1}}}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t","name":"n","input":{}}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":\"é\"}"}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":1}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ];
    let mut stream = String::new();
    for (name, data) in events {
        stream.push_str(&format!("event: {name}\r\ndata: {data}\r\n\r\n"));
    }
    stream.into_bytes()
}

#[test]
fn a_small_stream_decodes_the_same_at_every_pair_of_nearby_cuts() {
    let request = scenario::text();
    let stream = small_stream();
    assert!(stream.len() < 800, "{}: keep this one short", stream.len());
    let whole = decode_whole(&request, &stream);
    assert_well_formed(&whole);
    // Two cuts within 48 bytes of each other: every way a line ending, a
    // character or a token can be split in three.
    for first in 1..stream.len() {
        for second in first + 1..(first + 48).min(stream.len()) {
            let events = decode(&request, &stream, &[first, second]);
            assert!(
                events.as_ref() == Ok(&whole),
                "cuts at {first} and {second}"
            );
        }
    }
}

#[test]
fn a_stream_that_stops_early_has_no_end_and_loses_nothing_that_arrived() {
    let request = scenario::text();
    for name in ["reasoning", "tool-calls", "conversation-1"] {
        let stream = load(name).response;
        let whole = decode_whole(&request, &stream);
        // Stopped at the end of each event, and in the middle of each.
        let mut ends: Vec<usize> = stream
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair == b"\n\n")
            .map(|(at, _)| at + 2)
            .collect();
        ends.pop();
        for end in ends {
            for stop in [end, end + 7] {
                let stop = stop.min(stream.len() - 1);
                let events = decode(&request, &stream[..stop], &[]).unwrap();
                assert!(
                    !events.iter().any(ModelEvent::is_terminal),
                    "{name}: stopped at {stop}, and the response is called complete"
                );
                // What was decoded is the beginning of the whole response.
                assert_eq!(
                    events[..],
                    whole[..events.len()],
                    "{name}: stopped at {stop}"
                );
            }
        }
    }
}

#[test]
fn a_stream_that_breaks_the_dialect_is_an_error_and_not_a_guess() {
    let request = scenario::text();
    let event = |name: &str, data: Value| format!("event: {name}\ndata: {data}\n\n");
    let start = event(
        "message_start",
        json!({"type":"message_start","message":{"id":"m","model":"x","usage":{"input_tokens":1,"output_tokens":1}}}),
    );
    let text_start = event(
        "content_block_start",
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
    );
    let stop = event("message_stop", json!({"type":"message_stop"}));
    let broken: Vec<(&str, String)> = vec![
        ("a block before the message", text_start.clone()),
        ("a second message", format!("{start}{start}")),
        (
            "an addition to a block that has not started",
            format!(
                "{start}{}",
                event(
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":3,"delta":{"type":"text_delta","text":"x"}})
                )
            ),
        ),
        (
            "the end of a block that has not started",
            format!(
                "{start}{}",
                event(
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":0})
                )
            ),
        ),
        (
            "a block that starts twice",
            format!("{start}{text_start}{text_start}"),
        ),
        ("an end with no stop reason", format!("{start}{stop}")),
        (
            "a known event that is not JSON",
            format!(
                "{start}event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\n\n"
            ),
        ),
        (
            "a block with no index",
            format!(
                "{start}{}",
                event(
                    "content_block_start",
                    json!({"type":"content_block_start","content_block":{"type":"text","text":""}})
                )
            ),
        ),
        (
            "an index that is not a number",
            format!(
                "{start}{}",
                event(
                    "content_block_start",
                    json!({"type":"content_block_start","index":"0","content_block":{"type":"text","text":""}})
                )
            ),
        ),
        (
            "a block that is not an object",
            format!(
                "{start}{}",
                event(
                    "content_block_start",
                    json!({"type":"content_block_start","index":0,"content_block":"text"})
                )
            ),
        ),
        (
            "a usage that is not counts",
            format!(
                "{start}{}",
                event(
                    "message_delta",
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":"many"}})
                )
            ),
        ),
        (
            "a message start with no message",
            event("message_start", json!({"type":"message_start"})),
        ),
    ];
    for (what, stream) in broken {
        let result = decode(&request, stream.as_bytes(), &[]);
        assert!(result.is_err(), "{what}: decoded as {result:?}");
    }
    // An end that follows an error is not a second ending.
    let after_error = format!(
        "{start}{}{}{stop}",
        event(
            "error",
            json!({"type":"error","error":{"type":"api_error","message":"x"}})
        ),
        event(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}})
        ),
    );
    let events = decode(&request, after_error.as_bytes(), &[]).unwrap();
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    assert!(matches!(events.last(), Some(ModelEvent::Error(_))));
}

#[test]
fn no_bytes_make_the_decoder_panic_or_end_a_response_twice() {
    // The pieces of real streams, in any order, with pieces damaged.
    let mut pieces: Vec<Vec<u8>> = Vec::new();
    for name in [
        "reasoning",
        "tool-calls",
        "server-tool",
        "unknown-event",
        "error-after-200",
    ] {
        let stream = load(name).response;
        let mut from = 0;
        for at in 0..stream.len().saturating_sub(1) {
            if &stream[at..at + 2] == b"\n\n" {
                pieces.push(stream[from..at + 2].to_vec());
                from = at + 2;
            }
        }
    }
    let request = scenario::text();
    cases::check(
        "anthropic decoder soup",
        150,
        |rng| {
            use cortex_testkit::Rng as _;
            let mut stream = Vec::new();
            for _ in 0..1 + cases::small(rng, 14) {
                let mut piece = cases::pick(rng, &pieces).clone();
                match rng.below(6) {
                    // Cut the piece short.
                    0 => {
                        piece.truncate(usize::try_from(rng.below(piece.len() as u64)).unwrap_or(0))
                    }
                    // Change one byte.
                    1 => {
                        let at = usize::try_from(rng.below(piece.len() as u64)).unwrap_or(0);
                        piece[at] = u8::try_from(rng.below(256)).unwrap_or(0);
                    }
                    // Drop one byte.
                    2 => {
                        let at = usize::try_from(rng.below(piece.len() as u64)).unwrap_or(0);
                        piece.remove(at);
                    }
                    _ => {}
                }
                stream.extend_from_slice(&piece);
            }
            let mut cuts = Vec::new();
            let mut at = 0;
            while at < stream.len() {
                at = (at + 1 + cases::small(rng, 200)).min(stream.len());
                cuts.push(at);
            }
            (stream, cuts)
        },
        |(stream, cuts)| {
            // The same bytes give the same result however they are cut.
            let cut = decode(&request, stream, cuts);
            let whole = decode(&request, stream, &[]);
            if cut != whole {
                return Err("the cuts changed the result".into());
            }
            if let Ok(events) = whole {
                let terminals = events.iter().filter(|event| event.is_terminal()).count();
                if terminals > 1 {
                    return Err(format!("{terminals} endings").into());
                }
                if terminals == 1 && !events.last().is_some_and(ModelEvent::is_terminal) {
                    return Err("an event after the ending".into());
                }
                for event in &events {
                    if let ModelEvent::BlockEnd { raw, .. } = event
                        && serde_json::from_str::<Value>(raw.as_str()).is_err()
                    {
                        return Err(format!("a stored block is not JSON: {raw}").into());
                    }
                }
            }
            Ok(())
        },
    );
}
